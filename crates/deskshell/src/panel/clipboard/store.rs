//! The clipboard history store: a private directory of blob files and a JSON index.

use std::collections::HashSet;
use std::fmt;
use std::fs::{DirBuilder, File};
use std::io::{self, Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use deskshell_core::clipboard::history::{
    self, Key, MAX_ENTRIES, MAX_ENTRY_BYTES, MAX_TOTAL_BYTES, MAX_TYPE_NAME, Part, Placed,
};
use deskshell_wl::clipboard::{Offer, Reservation};
use rustix::fs::{AtFlags, CWD, FileType, Mode, OFlags};
use rustix::io::Errno;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tracing::{debug, warn};

pub const INDEX: &str = "index.json";
const INDEX_TEMP: &str = "index.json.tmp";
const INDEX_CORRUPT: &str = "index.json.corrupt";
const VERSION: u32 = 1;
pub const MAX_INDEX: u64 = 1 << 20;
/// Ids stay below this, the largest integer JSON readers keep exact.
pub const MAX_ID: u64 = 1 << 53;
const PRIVATE: u32 = 0o700;

#[derive(Serialize, Deserialize)]
struct IndexFile<E = IndexEntry> {
    version: u32,
    next: u64,
    entries: Vec<E>,
}

#[derive(Clone, Serialize, Deserialize)]
struct IndexEntry {
    id: u64,
    time: u64,
    types: Vec<TypeEntry>,
    preview: String,
}

impl fmt::Debug for IndexEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IndexEntry")
            .field("id", &self.id)
            .field("time", &self.time)
            .field("types", &self.types)
            .field("preview_len", &self.preview.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TypeEntry {
    mime: String,
    len: u64,
}

struct Item {
    entry: IndexEntry,
    key: Key,
    main: usize,
    memory: Option<Vec<Arc<[u8]>>>,
}

impl fmt::Debug for Item {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Item")
            .field("entry", &self.entry)
            .field("in_memory", &self.memory.is_some())
            .finish()
    }
}

impl Item {
    fn new(entry: IndexEntry) -> Option<Self> {
        if !(1..=3).contains(&entry.types.len())
            || !(1..MAX_ID).contains(&entry.id)
            || entry
                .types
                .iter()
                .any(|kept| kept.mime.len() > MAX_TYPE_NAME)
        {
            return None;
        }
        let parts: Vec<Part> = entry
            .types
            .iter()
            .map(|kept| Part::of(&kept.mime))
            .collect::<Option<_>>()?;
        let main = history::main_index(parts.iter().copied())?;
        let bytes = entry
            .types
            .iter()
            .try_fold(0u64, |sum, kept| sum.checked_add(kept.len))
            .filter(|bytes| *bytes <= MAX_ENTRY_BYTES as u64)?;
        let key = Key { main: parts[main], main_len: entry.types[main].len, bytes };
        Some(Self { entry, key, main, memory: None })
    }

    fn blobs(&self) -> Blobs {
        Blobs { id: self.entry.id, count: self.entry.types.len() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Blobs {
    id: u64,
    count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Added {
    New,
    Moved,
    Unchanged,
    Rejected,
    Stale,
}

impl Added {
    pub fn changed(self) -> bool {
        matches!(self, Self::New | Self::Moved)
    }
}

pub struct Store {
    dir: Option<OwnedFd>,
    items: Vec<Item>,
    next: u64,
    // Blobs the index on disk may still name. They go after the next index write succeeds.
    doomed: Vec<Blobs>,
    committed: HashSet<u64>,
    epoch: u64,
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Store")
            .field("on_disk", &self.dir.is_some())
            .field("entries", &self.items.len())
            .field("next", &self.next)
            .field("epoch", &self.epoch)
            .finish()
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn blob_name(id: u64, index: usize) -> String {
    format!("{id}.{index}")
}

fn blob_pattern(name: &str) -> Option<(u64, usize)> {
    let (id, index) = name.split_once('.')?;
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    if !digits(id) || !digits(index) {
        return None;
    }
    Some((id.parse().ok()?, index.parse().ok()?))
}

/// Opens the store directory with no symlink followed. It and its parent must belong to
/// the user, and its mode is forced to 0700.
pub(crate) fn open_private(path: &Path) -> io::Result<OwnedFd> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(io::Error::other("the store path has no parent"));
    };
    DirBuilder::new()
        .recursive(true)
        .mode(PRIVATE)
        .create(parent)?;
    let directory = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let parent = rustix::fs::openat(CWD, parent, directory, Mode::empty())?;
    let user = rustix::process::geteuid().as_raw();
    if rustix::fs::fstat(&parent)?.st_uid != user {
        return Err(io::Error::other(
            "the store's parent belongs to another user",
        ));
    }
    match rustix::fs::mkdirat(&parent, name, Mode::from_raw_mode(PRIVATE)) {
        Ok(()) | Err(Errno::EXIST) => {}
        Err(errno) => return Err(errno.into()),
    }
    let dir = rustix::fs::openat(&parent, name, directory, Mode::empty())?;
    let stat = rustix::fs::fstat(&dir)?;
    if stat.st_uid != user {
        return Err(io::Error::other(
            "the store belongs to another user",
        ));
    }
    if stat.st_mode & 0o777 != PRIVATE {
        rustix::fs::fchmod(&dir, Mode::from_raw_mode(PRIVATE))?;
    }
    Ok(dir)
}

fn regular_size(dir: &OwnedFd, name: &str) -> Option<u64> {
    let stat = rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW).ok()?;
    (FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile).then_some(stat.st_size as u64)
}

// NONBLOCK so a planted FIFO cannot hang the open. Only a regular file is accepted.
fn open_read(dir: &OwnedFd, name: &str) -> io::Result<File> {
    let fd = rustix::fs::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    if FileType::from_raw_mode(rustix::fs::fstat(&fd)?.st_mode) != FileType::RegularFile {
        return Err(io::Error::other("not a regular file"));
    }
    Ok(File::from(fd))
}

// The old name goes first and the create is O_EXCL, so a planted file or symlink is never
// written through.
fn create(dir: &OwnedFd, name: &str) -> io::Result<File> {
    match rustix::fs::unlinkat(dir, name, AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => {}
        Err(errno) => return Err(errno.into()),
    }
    let fd = rustix::fs::openat(
        dir,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )?;
    Ok(File::from(fd))
}

fn remove(dir: &OwnedFd, name: &str) {
    let _ = rustix::fs::unlinkat(dir, name, AtFlags::empty());
}

impl Store {
    pub fn open(dir: &Path) -> Self {
        let dir = match open_private(dir) {
            Ok(fd) => Some(fd),
            Err(err) => {
                warn!(%err, dir = %dir.display(), "the clipboard store cannot use its directory, the history stays in memory");
                None
            }
        };
        let mut store = Self {
            dir,
            items: Vec::new(),
            next: 1,
            doomed: Vec::new(),
            committed: HashSet::new(),
            epoch: 0,
        };
        store.load();
        store.remove_strays();
        store
    }

    pub fn on_disk(&self) -> bool {
        self.dir.is_some()
    }

    fn load(&mut self) {
        let Some(dir) = &self.dir else {
            return;
        };
        let file = match open_read(dir, INDEX) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return,
            Err(err) => {
                warn!(%err, "the clipboard index cannot be read");
                self.set_aside();
                return;
            }
        };
        let mut text = Vec::new();
        if let Err(err) = file
            .take(MAX_INDEX + 1)
            .read_to_end(&mut text)
        {
            warn!(%err, "the clipboard index cannot be read");
            self.set_aside();
            return;
        }
        if text.len() as u64 > MAX_INDEX {
            warn!(
                limit = MAX_INDEX,
                "the clipboard index is too large"
            );
            self.set_aside();
            return;
        }
        let index = match serde_json::from_slice::<IndexFile>(&text) {
            Ok(index) if index.version == VERSION => index,
            Ok(index) => {
                warn!(
                    version = index.version,
                    "the clipboard index has an unknown version"
                );
                self.set_aside();
                return;
            }
            Err(err) => {
                warn!(%err, "the clipboard index is corrupt");
                self.set_aside();
                return;
            }
        };
        let mut total: u64 = 0;
        for entry in index.entries {
            let id = entry.id;
            if self.items.len() == MAX_ENTRIES {
                warn!(
                    id,
                    "a clipboard entry past the entry count was dropped"
                );
                continue;
            }
            let Some(item) = Item::new(entry) else {
                warn!(
                    id,
                    "a clipboard entry outside the caps was dropped"
                );
                continue;
            };
            let Some(after) = total
                .checked_add(item.key.bytes)
                .filter(|after| *after <= MAX_TOTAL_BYTES)
            else {
                warn!(
                    id,
                    "a clipboard entry past the total cap was dropped"
                );
                continue;
            };
            let sound = !self
                .items
                .iter()
                .any(|other| other.entry.id == id)
                && item
                    .entry
                    .types
                    .iter()
                    .enumerate()
                    .all(|(index, kept)| {
                        regular_size(dir, &blob_name(id, index)) == Some(kept.len)
                    });
            if !sound {
                warn!(
                    id,
                    "a clipboard entry without its blobs was dropped"
                );
                continue;
            }
            total = after;
            self.items.push(item);
        }
        let first_free = self
            .items
            .iter()
            .map(|item| item.entry.id)
            .max()
            .map_or(1, |id| id + 1);
        self.next = if (first_free..=MAX_ID).contains(&index.next) {
            index.next
        } else {
            warn!(
                next = index.next,
                "the clipboard index's next id is out of range, it follows the entries"
            );
            first_free
        };
        self.committed = self
            .items
            .iter()
            .map(|item| item.entry.id)
            .collect();
    }

    fn set_aside(&self) {
        let Some(dir) = &self.dir else {
            return;
        };
        match rustix::fs::renameat(dir, INDEX, dir, INDEX_CORRUPT) {
            Ok(()) => warn!("the clipboard index was renamed aside, the history starts empty"),
            Err(err) => warn!(%err, "the clipboard index cannot be renamed aside"),
        }
    }

    fn remove_strays(&self) {
        let Some(dir) = &self.dir else {
            return;
        };
        let Ok(listing) = rustix::fs::Dir::read_from(dir) else {
            return;
        };
        let mut strays = Vec::new();
        for file in listing.flatten() {
            let Ok(name) = file.file_name().to_str() else {
                continue;
            };
            let ours = match blob_pattern(name) {
                Some((id, index)) => !self
                    .items
                    .iter()
                    .any(|item| item.entry.id == id && index < item.entry.types.len()),
                None => name == INDEX_TEMP,
            };
            if ours && regular_size(dir, name).is_some() {
                strays.push(name.to_owned());
            }
        }
        for name in &strays {
            debug!(name, "stray clipboard file removed");
            remove(dir, name);
        }
    }

    fn write_index(&mut self) -> bool {
        let Some(dir) = &self.dir else {
            return false;
        };
        let written = self
            .encode()
            .and_then(|bytes| {
                if bytes.len() as u64 > MAX_INDEX {
                    return Err(io::Error::other(
                        "the index would be too large to read back",
                    ));
                }
                create(dir, INDEX_TEMP)?.write_all(&bytes)
            })
            .and_then(|()| Ok(rustix::fs::renameat(dir, INDEX_TEMP, dir, INDEX)?));
        if let Err(err) = written {
            warn!(%err, "the clipboard index cannot be written, the change stays in memory");
            if regular_size(dir, INDEX_TEMP).is_some() {
                remove(dir, INDEX_TEMP);
            }
            return false;
        }
        for blobs in std::mem::take(&mut self.doomed) {
            for index in 0..blobs.count {
                remove(dir, &blob_name(blobs.id, index));
            }
        }
        self.committed = self
            .items
            .iter()
            .filter(|item| item.memory.is_none())
            .map(|item| item.entry.id)
            .collect();
        true
    }

    fn encode(&self) -> io::Result<Vec<u8>> {
        let index = IndexFile {
            version: VERSION,
            next: self.next,
            entries: self
                .items
                .iter()
                .filter(|item| item.memory.is_none())
                .map(|item| &item.entry)
                .collect(),
        };
        serde_json::to_vec(&index).map_err(io::Error::other)
    }

    // Blobs of a gone entry wait while the index on disk names them, so a failed index write
    // never loses an entry the old index still names.
    fn doom(&mut self, item: &Item) {
        if item.memory.is_some() {
            return;
        }
        if self.committed.contains(&item.entry.id) {
            self.doomed.push(item.blobs());
        } else if let Some(dir) = &self.dir {
            for index in 0..item.entry.types.len() {
                remove(dir, &blob_name(item.entry.id, index));
            }
        }
    }

    // Only guards the reader's MAX_INDEX limit. The caps on names, previews and entries keep
    // the index below it.
    fn fit_index(&mut self) -> bool {
        while self
            .encode()
            .is_ok_and(|bytes| bytes.len() as u64 > MAX_INDEX)
        {
            let Some(old) = self.items.pop() else {
                return false;
            };
            warn!(
                id = old.entry.id,
                "the clipboard index is full, the oldest entry went"
            );
            self.doom(&old);
        }
        !self.items.is_empty()
    }

    // Never reads more than one entry's cap, whatever the file on disk holds.
    fn read_type(&self, item: &Item, index: usize, limit: u64) -> io::Result<Vec<u8>> {
        let limit = limit.min(MAX_ENTRY_BYTES as u64);
        if let Some(memory) = &item.memory {
            let bytes = memory
                .get(index)
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            return Ok(bytes[..bytes.len().min(limit as usize)].to_vec());
        }
        let dir = self
            .dir
            .as_ref()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        let mut bytes = Vec::new();
        open_read(dir, &blob_name(item.entry.id, index))?
            .take(limit)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    fn read_whole(&self, item: &Item, index: usize) -> io::Result<Vec<u8>> {
        let bytes = self.read_type(item, index, u64::MAX)?;
        if Some(bytes.len() as u64)
            != item
                .entry
                .types
                .get(index)
                .map(|kept| kept.len)
        {
            return Err(io::Error::other("the blob changed its size"));
        }
        Ok(bytes)
    }

    /// `epoch` is the clear count when the read started. A read from before a clear is dropped.
    pub fn add(&mut self, epoch: u64, data: Vec<(String, Vec<u8>)>) -> Added {
        if epoch < self.epoch {
            debug!(epoch, "a selection from before a clear, dropped");
            return Added::Stale;
        }
        let kept = match history::keep(&data) {
            Ok(kept) => kept,
            Err(reject) => {
                debug!(?reject, "selection not recorded");
                return Added::Rejected;
            }
        };
        let mut data: Vec<Option<(String, Vec<u8>)>> = data.into_iter().map(Some).collect();
        let kept: Vec<(String, Vec<u8>)> = kept
            .into_iter()
            .filter_map(|index| data.get_mut(index)?.take())
            .collect();
        let types: Vec<TypeEntry> = kept
            .iter()
            .map(|(mime, bytes)| TypeEntry { mime: mime.clone(), len: bytes.len() as u64 })
            .collect();
        let Some(next) = self
            .next
            .checked_add(1)
            .filter(|next| *next <= MAX_ID)
        else {
            warn!("the clipboard store ran out of ids");
            return Added::Rejected;
        };
        let Some(item) =
            Item::new(IndexEntry { id: self.next, time: now(), types, preview: String::new() })
        else {
            return Added::Rejected;
        };
        let main_bytes = kept[item.main].1.as_slice();
        let mut items = std::mem::take(&mut self.items);
        let placed = history::place(
            &mut items,
            item,
            |item| item.key,
            |old| {
                self.read_whole(old, old.main)
                    .is_ok_and(|bytes| bytes == main_bytes)
            },
        );
        self.items = items;
        match placed {
            Placed::Unchanged => Added::Unchanged,
            Placed::Moved => {
                self.items[0].entry.time = now();
                self.write_index();
                Added::Moved
            }
            Placed::Added { evicted } => {
                self.next = next;
                let main = self.items[0].main;
                self.items[0].entry.preview = history::preview(&kept[main].0, &kept[main].1);
                let id = self.items[0].entry.id;
                let written = match &self.dir {
                    Some(dir) => kept
                        .iter()
                        .enumerate()
                        .try_for_each(|(index, (_, bytes))| {
                            create(dir, &blob_name(id, index))?.write_all(bytes)
                        })
                        .map_err(|err| {
                            warn!(%err, "a clipboard entry cannot be written, it stays in memory");
                            for index in 0..kept.len() {
                                let name = blob_name(id, index);
                                if regular_size(dir, &name).is_some() {
                                    remove(dir, &name);
                                }
                            }
                        }),
                    None => Err(()),
                };
                if written.is_err() {
                    self.items[0].memory = Some(
                        kept.into_iter()
                            .map(|(_, bytes)| Arc::from(bytes))
                            .collect(),
                    );
                }
                for old in &evicted {
                    self.doom(old);
                }
                let kept = self.fit_index();
                self.write_index();
                if kept { Added::New } else { Added::Rejected }
            }
        }
    }

    pub fn list(&self) -> Vec<String> {
        self.items
            .iter()
            .map(|item| format!("{}\t{}", item.entry.id, item.entry.preview))
            .collect()
    }

    pub fn read_main(&self, id: &str, limit: u64, whole: bool) -> Option<Vec<u8>> {
        let item = self.find(id)?;
        if whole && item.key.main_len > limit {
            debug!(id, limit, "clipboard entry too large to preview");
            return None;
        }
        let read = if whole {
            self.read_whole(item, item.main)
        } else {
            self.read_type(item, item.main, limit)
        };
        read.map_err(|err| debug!(id, %err, "cannot read a clipboard entry"))
            .ok()
    }

    /// A uri-list of files also goes out as GNOME copied-files, so a GTK file manager pastes
    /// them as files.
    pub fn offers(&self, id: &str) -> Option<Vec<Offer>> {
        let item = self.find(id)?;
        let mimes: Vec<&str> = item
            .entry
            .types
            .iter()
            .map(|kept| kept.mime.as_str())
            .collect();
        let offers = history::offers(&mimes);
        let mut blocks = Vec::with_capacity(mimes.len());
        for (index, mime) in mimes.iter().enumerate() {
            let data = self
                .read_whole(item, index)
                .map_err(|err| warn!(id, %err, "cannot read a clipboard entry to copy"))
                .ok()?;
            let types = offers
                .iter()
                .filter(|(_, of)| *of == index)
                .map(|(mime, _)| (*mime).to_owned())
                .collect();
            let files = (*mime == history::URI_LIST)
                .then(|| history::gnome_copied_files(&data))
                .flatten();
            blocks.push(Offer { types, data });
            if let Some(files) = files {
                blocks.push(Offer {
                    types: vec![history::GNOME_COPIED_FILES.to_owned()],
                    data: files,
                });
            }
        }
        Some(blocks)
    }

    pub fn delete(&mut self, id: &str) -> bool {
        let Some(at) = self.position(id) else {
            return false;
        };
        let item = self.items.remove(at);
        self.doom(&item);
        self.write_index();
        true
    }

    pub fn clear(&mut self, epoch: u64) {
        self.epoch = self.epoch.max(epoch);
        for item in std::mem::take(&mut self.items) {
            self.doom(&item);
        }
        self.write_index();
    }

    fn position(&self, id: &str) -> Option<usize> {
        let id: u64 = id.parse().ok()?;
        self.items
            .iter()
            .position(|item| item.entry.id == id)
    }

    fn find(&self, id: &str) -> Option<&Item> {
        self.position(id)
            .map(|at| &self.items[at])
    }
}

type Job = Box<dyn FnOnce(&mut Store) + Send>;

#[derive(Clone)]
pub struct StoreHandle {
    jobs: mpsc::Sender<Job>,
}

impl fmt::Debug for StoreHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StoreHandle")
    }
}

impl StoreHandle {
    pub fn start(dir: PathBuf) -> io::Result<Self> {
        let (jobs, receiver) = mpsc::channel::<Job>();
        thread::Builder::new()
            .name(String::from("clipboard-store"))
            .spawn(move || {
                let mut store = Store::open(&dir);
                while let Ok(job) = receiver.recv() {
                    job(&mut store);
                }
            })?;
        Ok(Self { jobs })
    }

    async fn call<R: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Store) -> R + Send + 'static,
    ) -> Option<R> {
        let (sender, receiver) = oneshot::channel();
        self.jobs
            .send(Box::new(move |store| {
                let _ = sender.send(job(store));
            }))
            .ok()?;
        receiver.await.ok()
    }

    /// `reservation` holds the bytes in the watcher's budget until the store is done.
    pub async fn add(
        &self,
        epoch: u64,
        data: Vec<(String, Vec<u8>)>,
        reservation: Reservation,
    ) -> Added {
        self.call(move |store| {
            let added = store.add(epoch, data);
            drop(reservation);
            added
        })
        .await
        .unwrap_or(Added::Rejected)
    }

    pub async fn list(&self) -> Vec<String> {
        self.call(|store| store.list())
            .await
            .unwrap_or_default()
    }

    pub async fn read_main(&self, id: String, limit: u64, whole: bool) -> Option<Vec<u8>> {
        self.call(move |store| store.read_main(&id, limit, whole))
            .await
            .flatten()
    }

    pub async fn offers(&self, id: String) -> Option<Vec<Offer>> {
        self.call(move |store| store.offers(&id))
            .await
            .flatten()
    }

    pub async fn delete(&self, id: String) -> bool {
        self.call(move |store| store.delete(&id))
            .await
            .unwrap_or(false)
    }

    pub async fn clear(&self, epoch: u64) {
        let _ = self
            .call(move |store| store.clear(epoch))
            .await;
    }
}
