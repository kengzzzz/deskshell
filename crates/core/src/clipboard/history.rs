//! The clipboard history's policy: kept types, caps, dedupe, eviction, offered types and previews.

use std::fmt::Write as _;

/// The type a password manager adds to a secret. An offer that lists it is never read.
pub const SENSITIVE: &str = "x-kde-passwordManagerHint";
pub const URI_LIST: &str = "text/uri-list";
pub const TEXT_UTF8: &str = "text/plain;charset=utf-8";
pub const TEXT_PLAIN: &str = "text/plain";
/// What a copy with text offers besides the text's own type, as `wl-copy` does.
pub const TEXT_ALIASES: [&str; 4] = [TEXT_PLAIN, "TEXT", "STRING", "UTF8_STRING"];
pub const MAX_ENTRIES: usize = 100;
pub const MAX_ENTRY_BYTES: usize = 16 << 20;
pub const MAX_TOTAL_BYTES: u64 = 256 << 20;
pub const PREVIEW_WIDTH: usize = 100;
const PREVIEW_SCAN: usize = 64 << 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Part {
    UriList,
    Text,
    Image,
}

impl Part {
    pub fn of(mime: &str) -> Option<Self> {
        if mime == URI_LIST {
            Some(Self::UriList)
        } else if mime.starts_with("image/") {
            Some(Self::Image)
        } else if is_text(mime) {
            Some(Self::Text)
        } else {
            None
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Image => 0,
            Self::UriList => 1,
            Self::Text => 2,
        }
    }
}

/// `wl-paste`'s `mime_type_is_text`.
pub fn is_text(mime: &str) -> bool {
    mime.starts_with("text/")
        || matches!(mime, "TEXT" | "STRING" | "UTF8_STRING")
        || mime.contains("json")
        || ["script", "xml", "yaml", "csv", "ini", "pgp-keys"]
            .iter()
            .any(|suffix| mime.ends_with(suffix))
        || mime.contains("application/vnd.ms-publisher")
}

pub fn is_sensitive<S: AsRef<str>>(types: &[S]) -> bool {
    types
        .iter()
        .any(|mime| mime.as_ref() == SENSITIVE)
}

/// Nothing for a sensitive offer. Text and images are picked in `wl-paste`'s order of
/// preference.
pub fn pick<S: AsRef<str>>(types: &[S]) -> Vec<&str> {
    if is_sensitive(types) {
        return Vec::new();
    }
    let has = |wanted: &str| {
        types
            .iter()
            .map(AsRef::as_ref)
            .find(|mime| *mime == wanted)
    };
    let first = |test: fn(&str) -> bool| {
        types
            .iter()
            .map(AsRef::as_ref)
            .find(|mime| test(mime))
    };
    let text = has(TEXT_UTF8)
        .or_else(|| has(TEXT_PLAIN))
        .or_else(|| first(|mime| mime != URI_LIST && is_text(mime)));
    let image = has("image/png")
        .or_else(|| has("image/jpeg"))
        .or_else(|| first(|mime| mime.starts_with("image/")));
    has(URI_LIST)
        .into_iter()
        .chain(text)
        .chain(image)
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    Blank,
    TooLarge,
    LongType,
}

/// The longest type name an entry keeps, so a hundred entries always fit the store's index.
pub const MAX_TYPE_NAME: usize = 256;

pub fn keep<M: AsRef<str>, B: AsRef<[u8]>>(read: &[(M, B)]) -> Result<Vec<usize>, Reject> {
    let total: usize = read
        .iter()
        .map(|(_, bytes)| bytes.as_ref().len())
        .sum();
    if total > MAX_ENTRY_BYTES {
        return Err(Reject::TooLarge);
    }
    if read
        .iter()
        .any(|(mime, _)| mime.as_ref().len() > MAX_TYPE_NAME)
    {
        return Err(Reject::LongType);
    }
    let kept: Vec<usize> = read
        .iter()
        .enumerate()
        .filter(|(_, (mime, bytes))| {
            let bytes = bytes.as_ref();
            match Part::of(mime.as_ref()) {
                None => false,
                Some(Part::Image) => !bytes.is_empty(),
                Some(Part::Text | Part::UriList) => !is_blank(bytes),
            }
        })
        .map(|(index, _)| index)
        .collect();
    if kept.is_empty() { Err(Reject::Blank) } else { Ok(kept) }
}

pub fn is_blank(bytes: &[u8]) -> bool {
    bytes.utf8_chunks().all(|chunk| {
        chunk.invalid().is_empty()
            && chunk
                .valid()
                .chars()
                .all(char::is_whitespace)
    })
}

pub fn main_index(parts: impl IntoIterator<Item = Part>) -> Option<usize> {
    parts
        .into_iter()
        .enumerate()
        .min_by_key(|(_, part)| part.rank())
        .map(|(index, _)| index)
}

/// The file list of GTK file managers (Thunar, Nautilus). They paste files from this type,
/// not from `text/uri-list`.
pub const GNOME_COPIED_FILES: &str = "x-special/gnome-copied-files";

/// `None` unless every URI is a `file://` URI, since a file manager pastes only files.
pub fn gnome_copied_files(uri_list: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(uri_list).ok()?;
    let mut uris = text
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .peekable();
    uris.peek()?;
    let mut out = b"copy".to_vec();
    for uri in uris {
        if !uri.starts_with("file://") {
            return None;
        }
        out.push(b'\n');
        out.extend_from_slice(uri.as_bytes());
    }
    Some(out)
}

pub fn offers<'a>(kept: &[&'a str]) -> Vec<(&'a str, usize)> {
    let mut offers: Vec<(&str, usize)> = kept
        .iter()
        .enumerate()
        .map(|(index, mime)| (*mime, index))
        .collect();
    let text = kept
        .iter()
        .position(|mime| Part::of(mime) == Some(Part::Text));
    if let Some(text) = text {
        for alias in TEXT_ALIASES {
            if !offers
                .iter()
                .any(|(mime, _)| *mime == alias)
            {
                offers.push((alias, text));
            }
        }
    }
    offers
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub main: Part,
    pub main_len: u64,
    pub bytes: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Placed<T> {
    Added { evicted: Vec<T> },
    Moved,
    Unchanged,
}

pub fn place<T>(
    list: &mut Vec<T>,
    new: T,
    key: impl Fn(&T) -> Key,
    mut same_bytes: impl FnMut(&T) -> bool,
) -> Placed<T> {
    let wanted = key(&new);
    let found = list.iter().position(|old| {
        let old_key = key(old);
        old_key.main == wanted.main && old_key.main_len == wanted.main_len && same_bytes(old)
    });
    match found {
        Some(0) => return Placed::Unchanged,
        Some(index) => {
            let old = list.remove(index);
            list.insert(0, old);
            return Placed::Moved;
        }
        None => {}
    }
    list.insert(0, new);
    let mut total: u64 = list
        .iter()
        .map(|entry| key(entry).bytes)
        .sum();
    let mut evicted = Vec::new();
    while list.len() > 1 && (list.len() > MAX_ENTRIES || total > MAX_TOTAL_BYTES) {
        if let Some(old) = list.pop() {
            total -= key(&old).bytes;
            evicted.push(old);
        }
    }
    Placed::Added { evicted }
}

/// The preview after the tab of the list line, in `cliphist list`'s form, which `classify`
/// reads.
pub fn preview(mime: &str, bytes: &[u8]) -> String {
    let mut out = String::new();
    if Part::of(mime) == Some(Part::Image) {
        out.push_str("[[ binary data ");
        size_str(bytes.len(), &mut out);
        match image_info(bytes) {
            Some((format, width, height)) => {
                let _ = write!(out, " {format} {width}x{height} ]]");
            }
            None => {
                let format = mime
                    .strip_prefix("image/")
                    .unwrap_or(mime);
                let format: String = format
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                let _ = write!(out, " {format} ]]");
            }
        }
        return out;
    }
    let scan = &bytes[..bytes.len().min(PREVIEW_SCAN)];
    let text = String::from_utf8_lossy(scan);
    // One rune past the width tells a cut preview from one that fits.
    let mut runes = 0;
    'words: for (index, word) in text.split_whitespace().enumerate() {
        let space = (index > 0).then_some(' ');
        for c in space.into_iter().chain(word.chars()) {
            if runes > PREVIEW_WIDTH {
                break 'words;
            }
            out.push(c);
            runes += 1;
        }
    }
    if runes > PREVIEW_WIDTH {
        out.pop();
        out.push('…');
    }
    out
}

pub fn size_str(size: usize, out: &mut String) {
    let mut value = size as f64;
    let mut unit = 0;
    let units = ["B", "KiB", "MiB"];
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let _ = write!(out, "{value:.0} {}", units[unit]);
}

pub fn image_info(bytes: &[u8]) -> Option<(&'static str, u32, u32)> {
    let be32 = |at: usize| {
        Some(u32::from_be_bytes(
            bytes.get(at..at + 4)?.try_into().ok()?,
        ))
    };
    let le16 = |at: usize| {
        Some(u16::from_le_bytes(
            bytes.get(at..at + 2)?.try_into().ok()?,
        ))
    };
    let le32 = |at: usize| {
        Some(i32::from_le_bytes(
            bytes.get(at..at + 4)?.try_into().ok()?,
        ))
    };
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.get(12..16) == Some(b"IHDR") {
        return Some(("png", be32(16)?, be32(20)?));
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(("gif", u32::from(le16(6)?), u32::from(le16(8)?)));
    }
    if bytes.starts_with(b"BM") {
        let header = le32(14)?;
        return if header == 12 {
            Some(("bmp", u32::from(le16(18)?), u32::from(le16(20)?)))
        } else {
            Some((
                "bmp",
                le32(18)?.unsigned_abs(),
                le32(22)?.unsigned_abs(),
            ))
        };
    }
    if bytes.starts_with(&[0xff, 0xd8]) {
        return jpeg_size(bytes).map(|(width, height)| ("jpeg", width, height));
    }
    None
}

fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2;
    loop {
        while *bytes.get(at)? != 0xff {
            at += 1;
        }
        while *bytes.get(at)? == 0xff {
            at += 1;
        }
        let marker = *bytes.get(at)?;
        at += 1;
        if marker == 0x01 || (0xd0..=0xd9).contains(&marker) {
            continue;
        }
        let length = usize::from(u16::from_be_bytes([
            *bytes.get(at)?,
            *bytes.get(at + 1)?,
        ]));
        let frame = (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc);
        if frame {
            let height = u16::from_be_bytes([*bytes.get(at + 3)?, *bytes.get(at + 4)?]);
            let width = u16::from_be_bytes([*bytes.get(at + 5)?, *bytes.get(at + 6)?]);
            return Some((u32::from(width), u32::from(height)));
        }
        if length < 2 {
            return None;
        }
        at += length;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types(list: &[&str]) -> Vec<String> {
        list.iter()
            .map(|mime| (*mime).to_owned())
            .collect()
    }

    #[test]
    fn a_browser_image_with_text() {
        let offer = types(&[
            "text/html",
            "image/webp",
            "image/png",
            "image/jpeg",
            "text/plain;charset=utf-8",
            "text/plain",
            "chromium/x-source-url",
        ]);
        assert_eq!(
            pick(&offer),
            ["text/plain;charset=utf-8", "image/png"]
        );
    }

    #[test]
    fn a_file_manager_copy() {
        let offer = types(&[
            "x-special/gnome-copied-files",
            "text/uri-list",
            "text/plain;charset=utf-8",
            "UTF8_STRING",
        ]);
        assert_eq!(
            pick(&offer),
            ["text/uri-list", "text/plain;charset=utf-8"]
        );
        assert_eq!(
            pick(&types(&["text/uri-list"])),
            ["text/uri-list"],
            "the uri-list is not also the text"
        );
    }

    #[test]
    fn bmp_only_and_utf8_string_only() {
        assert_eq!(
            pick(&types(&["image/bmp", "image/x-bmp"])),
            ["image/bmp"]
        );
        assert_eq!(pick(&types(&["UTF8_STRING"])), ["UTF8_STRING"]);
        assert_eq!(
            pick(&types(&["application/json", "TEXT"])),
            ["application/json"],
            "the first text type in offer order"
        );
        assert_eq!(
            pick(&types(&["image/jpeg", "image/gif"])),
            ["image/jpeg"]
        );
        assert!(pick(&types(&["application/octet-stream"])).is_empty());
        assert!(pick::<String>(&[]).is_empty());
    }

    #[test]
    fn a_sensitive_offer_keeps_nothing() {
        let offer = types(&["text/plain", "UTF8_STRING", SENSITIVE]);
        assert!(is_sensitive(&offer));
        assert!(pick(&offer).is_empty());
    }

    #[test]
    fn text_types_follow_wl_paste() {
        for mime in [
            "text/html",
            "TEXT",
            "STRING",
            "UTF8_STRING",
            "application/json",
            "application/ld+json",
            "application/javascript",
            "application/xml",
            "application/x-yaml",
            "text/csv",
            "application/x-ini",
            "application/pgp-keys",
            "application/vnd.ms-publisher",
        ] {
            assert!(is_text(mime), "{mime}");
        }
        for mime in ["image/png", "application/octet-stream", "UTF8", "text"] {
            assert!(!is_text(mime), "{mime}");
        }
        assert_eq!(Part::of(URI_LIST), Some(Part::UriList));
        assert_eq!(Part::of("image/svg+xml"), Some(Part::Image));
        assert_eq!(Part::of("TEXT"), Some(Part::Text));
        assert_eq!(Part::of("application/zip"), None);
    }

    #[test]
    fn whitespace_only_text_is_not_kept() {
        assert!(is_blank(b""));
        assert!(is_blank(b" \t\r\n\x0b\x0c"));
        assert!(is_blank("\u{a0}\u{3000}\u{85}".as_bytes()));
        assert!(!is_blank(b" a "));
        assert!(
            !is_blank(b"\xff"),
            "bytes that are not UTF-8 are content"
        );
        assert_eq!(
            keep(&[("text/plain", b"  \n".as_slice())]),
            Err(Reject::Blank)
        );
        assert_eq!(
            keep(&[("text/plain", b"".as_slice())]),
            Err(Reject::Blank)
        );
        assert_eq!(
            keep(&[("text/plain", b"  ".as_slice()), ("image/png", b"\x89PNG".as_slice())]),
            Ok(vec![1]),
            "the image stays without its blank text"
        );
        assert_eq!(
            keep(&[
                ("text/uri-list", b"file:///a\r\n".as_slice()),
                ("text/plain", b"file:///a".as_slice()),
                ("image/png", b"".as_slice())
            ]),
            Ok(vec![0, 1]),
            "an empty image goes"
        );
    }

    #[test]
    fn the_entry_cap() {
        let big = vec![b'a'; MAX_ENTRY_BYTES];
        assert_eq!(
            keep(&[("text/plain", big.as_slice())]),
            Ok(vec![0])
        );
        assert_eq!(
            keep(&[("text/plain", big.as_slice()), ("image/png", b"x".as_slice())]),
            Err(Reject::TooLarge),
            "the cap counts every type of the entry"
        );
    }

    #[test]
    fn a_long_type_name_is_not_kept() {
        let long = format!("text/x-{}", "a".repeat(MAX_TYPE_NAME));
        assert_eq!(
            keep(&[(long.as_str(), b"made-up".as_slice())]),
            Err(Reject::LongType)
        );
        let fits = format!("text/x-{}", "a".repeat(MAX_TYPE_NAME - 7));
        assert_eq!(
            keep(&[(fits.as_str(), b"made-up".as_slice())]),
            Ok(vec![0])
        );
    }

    #[test]
    fn the_main_part() {
        assert_eq!(main_index([Part::Text, Part::Image]), Some(1));
        assert_eq!(main_index([Part::Text, Part::UriList]), Some(1));
        assert_eq!(
            main_index([Part::UriList, Part::Text, Part::Image]),
            Some(2)
        );
        assert_eq!(main_index([Part::Text]), Some(0));
        assert_eq!(main_index([]), None);
    }

    #[test]
    fn file_lists_for_file_managers() {
        assert_eq!(
            gnome_copied_files(b"file:///a/b.mp4\r\nfile:///c%20d.png\r\n").as_deref(),
            Some(b"copy\nfile:///a/b.mp4\nfile:///c%20d.png".as_slice())
        );
        assert_eq!(
            gnome_copied_files(b"# comment\r\n\r\nfile:///a\n").as_deref(),
            Some(b"copy\nfile:///a".as_slice())
        );
        assert_eq!(
            gnome_copied_files(b"file:///a\r\nhttps://example.org/\r\n"),
            None,
            "a list with a web link is not a file copy"
        );
        assert_eq!(gnome_copied_files(b""), None);
        assert_eq!(gnome_copied_files(b"# only a comment\r\n"), None);
        assert_eq!(gnome_copied_files(b"file:///\xff"), None);
    }

    #[test]
    fn offers_add_the_text_aliases() {
        assert_eq!(
            offers(&["text/plain;charset=utf-8"]),
            [
                ("text/plain;charset=utf-8", 0),
                ("text/plain", 0),
                ("TEXT", 0),
                ("STRING", 0),
                ("UTF8_STRING", 0)
            ]
        );
        assert_eq!(
            offers(&["text/uri-list", "text/plain"]),
            [
                ("text/uri-list", 0),
                ("text/plain", 1),
                ("TEXT", 1),
                ("STRING", 1),
                ("UTF8_STRING", 1)
            ],
            "the uri-list gets no aliases, and text/plain is offered once"
        );
        assert_eq!(offers(&["image/png"]), [("image/png", 0)]);
        assert_eq!(offers(&["text/uri-list"]), [("text/uri-list", 0)]);
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Item {
        id: u64,
        main: Part,
        bytes: u64,
        content: &'static str,
    }

    fn item(id: u64, main: Part, content: &'static str) -> Item {
        Item { id, main, bytes: content.len() as u64, content }
    }

    fn key(item: &Item) -> Key {
        Key { main: item.main, main_len: item.content.len() as u64, bytes: item.bytes }
    }

    fn add(list: &mut Vec<Item>, new: Item) -> Placed<Item> {
        let content = new.content;
        place(list, new, key, |old| old.content == content)
    }

    fn ids(list: &[Item]) -> Vec<u64> {
        list.iter()
            .map(|item| item.id)
            .collect()
    }

    #[test]
    fn dedupe_moves_to_the_top() {
        let mut list = Vec::new();
        assert_eq!(
            add(&mut list, item(1, Part::Text, "one")),
            Placed::Added { evicted: vec![] }
        );
        add(&mut list, item(2, Part::Text, "two"));
        add(&mut list, item(3, Part::Image, "png"));
        assert_eq!(ids(&list), [3, 2, 1]);
        assert_eq!(
            add(&mut list, item(4, Part::Text, "one")),
            Placed::Moved
        );
        assert_eq!(
            ids(&list),
            [1, 3, 2],
            "the old entry keeps its id"
        );
        assert_eq!(
            add(&mut list, item(5, Part::Text, "one")),
            Placed::Unchanged,
            "a replay of the top entry adds nothing"
        );
        assert_eq!(
            add(&mut list, item(6, Part::Text, "png")),
            Placed::Added { evicted: vec![] },
            "the same bytes as another kind are a new entry"
        );
        assert_eq!(
            add(&mut list, item(7, Part::Text, "tw0")),
            Placed::Added { evicted: vec![] },
            "the same length with other bytes is a new entry"
        );
        assert_eq!(ids(&list), [7, 6, 1, 3, 2]);
    }

    #[test]
    fn at_most_100_entries() {
        let mut list = Vec::new();
        let words: Vec<&'static str> = (0..MAX_ENTRIES + 2)
            .map(|n| &*format!("entry {n}").leak())
            .collect();
        for (n, word) in words.iter().enumerate() {
            let placed = add(&mut list, item(n as u64, Part::Text, word));
            if n < MAX_ENTRIES {
                assert_eq!(placed, Placed::Added { evicted: vec![] });
            } else {
                assert_eq!(
                    placed,
                    Placed::Added {
                        evicted: vec![item(n as u64 - 100, Part::Text, words[n - 100])]
                    },
                    "the oldest goes"
                );
            }
        }
        assert_eq!(list.len(), MAX_ENTRIES);
        assert_eq!(list[0].id, MAX_ENTRIES as u64 + 1);
    }

    #[test]
    fn at_most_256_mib_in_all() {
        let mut list = Vec::new();
        let sized =
            |id: u64, mib: u64| Item { id, main: Part::Image, bytes: mib << 20, content: "" };
        let place_sized = |list: &mut Vec<Item>, new: Item| place(list, new, key, |_| false);
        for id in 0..16 {
            assert_eq!(
                place_sized(&mut list, sized(id, 16)),
                Placed::Added { evicted: vec![] }
            );
        }
        let placed = place_sized(&mut list, sized(16, 1));
        assert_eq!(
            placed,
            Placed::Added { evicted: vec![sized(0, 16)] },
            "one over the budget evicts the oldest"
        );
        let placed = place_sized(&mut list, sized(17, 16));
        assert_eq!(
            placed,
            Placed::Added { evicted: vec![sized(1, 16)] }
        );
        assert_eq!(list.len(), 16);
        let mut lone = vec![];
        assert_eq!(
            place_sized(&mut lone, sized(0, 300)),
            Placed::Added { evicted: vec![] },
            "the top entry always stays"
        );
    }

    #[test]
    fn text_previews_fold_and_cut() {
        assert_eq!(
            preview("text/plain", b"  some\t copied \n\n words  "),
            "some copied words"
        );
        let long = "word ".repeat(30);
        let cut = preview("text/plain", long.as_bytes());
        assert_eq!(cut.chars().count(), PREVIEW_WIDTH + 1);
        assert!(cut.ends_with('…'));
        assert!(cut.starts_with("word word"));
        let exact = "a".repeat(PREVIEW_WIDTH);
        assert_eq!(
            preview("text/plain", exact.as_bytes()),
            exact,
            "exactly the width has no ellipsis"
        );
        let trailing = format!("{exact}   \n");
        assert_eq!(
            preview("text/plain", trailing.as_bytes()),
            exact,
            "trailing space is trimmed first"
        );
        let thai = "ก".repeat(150);
        let cut = preview("UTF8_STRING", thai.as_bytes());
        assert_eq!(cut.chars().count(), PREVIEW_WIDTH + 1);
        assert_eq!(preview("text/plain", b"a\xffb"), "a\u{fffd}b");
        let spaced = format!("{} b", "a".repeat(PREVIEW_WIDTH - 1));
        assert_eq!(
            preview("text/plain", spaced.as_bytes()),
            format!("{}…", "a".repeat(PREVIEW_WIDTH - 1) + " "),
            "a space at the cut stays, as cliphist cuts runes"
        );
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes
    }

    #[test]
    fn image_previews() {
        let mut bytes = png(640, 480);
        bytes.resize(12 << 10, 0);
        assert_eq!(
            preview("image/png", &bytes),
            "[[ binary data 12 KiB png 640x480 ]]"
        );
        let entry = super::super::classify(format!("7\t{}", preview("image/png", &bytes)));
        assert_eq!(entry.kind, super::super::Kind::Image);
        assert_eq!(
            preview("image/webp", b"RIFF....WEBP"),
            "[[ binary data 12 B webp ]]"
        );
        let entry = super::super::classify(format!("7\t{}", preview("image/webp", b"RIFF")));
        assert_eq!(
            entry.kind,
            super::super::Kind::Binary,
            "an image without a size is binary"
        );
    }

    #[test]
    fn sizes_as_cliphist() {
        let mut out = String::new();
        for (size, text) in [
            (0, "0 B"),
            (1023, "1023 B"),
            (1024, "1 KiB"),
            (1536, "2 KiB"),
            (2560, "2 KiB"),
            (5 << 20, "5 MiB"),
            (3 << 30, "3072 MiB"),
        ] {
            out.clear();
            size_str(size, &mut out);
            assert_eq!(out, text, "{size}");
        }
    }

    #[test]
    fn image_sizes() {
        assert_eq!(image_info(&png(3, 2)), Some(("png", 3, 2)));
        assert_eq!(
            image_info(b"GIF89a\x05\x00\x07\x00"),
            Some(("gif", 5, 7))
        );
        let mut bmp = b"BM".to_vec();
        bmp.resize(14, 0);
        bmp.extend_from_slice(&40i32.to_le_bytes());
        bmp.extend_from_slice(&9i32.to_le_bytes());
        bmp.extend_from_slice(&(-4i32).to_le_bytes());
        assert_eq!(
            image_info(&bmp),
            Some(("bmp", 9, 4)),
            "a top-down BMP has a negative height"
        );
        let jpeg = [
            0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x00,
            0x02, 0x00, 0x03, 0x03,
        ];
        assert_eq!(image_info(&jpeg), Some(("jpeg", 3, 2)));
        assert_eq!(image_info(&jpeg[..12]), None, "cut short");
        assert_eq!(image_info(&png(3, 2)[..18]), None);
        assert_eq!(image_info(b"not an image"), None);
        assert_eq!(
            image_info(&[0xff, 0xd8, 0xff, 0xe0, 0x00, 0x00]),
            None,
            "a bad length ends the walk"
        );
    }
}
