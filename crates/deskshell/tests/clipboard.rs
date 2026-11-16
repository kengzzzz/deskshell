//! The clipboard panel against made-up store entries. Effects are values, nothing runs.

use std::assert_matches;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use deskshell::panel::clipboard::store::Store;
use deskshell::panel::clipboard::{self, Clipboard, Config, Message, Request, StoreHandle};
use deskshell::panel::{Effect, Sources};
use deskshell_core::clipboard::history::size_str;
use deskshell_core::clipboard::{Details, Kind};
use deskshell_ui::Theme;
use deskshell_ui::image::Slot;
use image::{ImageFormat, Rgba, RgbaImage};

pub struct Fixture {
    dir: PathBuf,
}

fn one(mime: &str, bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    vec![(mime.to_owned(), bytes.to_vec())]
}

impl Fixture {
    pub fn new(name: &str) -> Self {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("clipboard-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut store = Store::open(&dir);
        store.add(0, one("image/webp", b"RIFF made-up WEBP"));
        store.add(0, one("image/png", &png(4, 2)));
        store.add(0, one("text/plain", b"#3d8bd9"));
        store.add(
            0,
            one(
                "text/plain;charset=utf-8",
                b"  https://example.org/docs?page=2\n",
            ),
        );
        store.add(
            0,
            one(
                "text/plain;charset=utf-8",
                b"Some copied words\nand a second line\n",
            ),
        );
        Self { dir }
    }

    pub fn store(&self) -> StoreHandle {
        StoreHandle::start(self.dir.clone()).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

pub fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    RgbaImage::from_pixel(width, height, Rgba([0x3d, 0x8b, 0xd9, 255]))
        .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .unwrap();
    bytes
}

struct Harness {
    theme: Theme,
    sources: Sources,
    panel: Clipboard,
    effects: Vec<Effect>,
}

impl Harness {
    fn new(store: Option<StoreHandle>) -> Self {
        Self {
            theme: Theme::default(),
            sources: Sources { clipboard: Config { store }, ..Sources::default() },
            panel: Clipboard::default(),
            effects: Vec::new(),
        }
    }

    fn send(&mut self, message: Message) -> &[Effect] {
        self.effects.clear();
        let env = self.sources.env(&self.theme);
        let _ = self
            .panel
            .update(message, &env, &mut self.effects);
        &self.effects
    }

    fn open(&mut self) {
        let env = self.sources.env(&self.theme);
        let _ = self.panel.open(&env);
    }

    async fn opened(&mut self, store: &StoreHandle) {
        self.open();
        let entries = clipboard::list(store.clone()).await;
        self.send(Message::Listed { sequence: 1, entries });
    }

    fn ids(&self) -> Vec<&str> {
        self.panel
            .filtered()
            .iter()
            .map(|&index| self.panel.entries()[index].id())
            .collect()
    }
}

#[tokio::test]
async fn the_list_loads_and_classifies() {
    let fixture = Fixture::new("list");
    let entries = clipboard::list(fixture.store()).await;
    let kinds: Vec<(&str, Kind)> = entries
        .iter()
        .map(|entry| (entry.id(), entry.kind))
        .collect();
    assert_eq!(
        kinds,
        [
            ("5", Kind::Text),
            ("4", Kind::Link),
            ("3", Kind::Color),
            ("2", Kind::Image),
            ("1", Kind::Binary)
        ]
    );
    assert_eq!(
        entries[0].preview(),
        "Some copied words and a second line"
    );
    assert_eq!(
        entries[1].preview(),
        "https://example.org/docs?page=2"
    );
    let mut size = String::new();
    size_str(png(4, 2).len(), &mut size);
    assert_eq!(
        entries[3].preview(),
        format!("[[ binary data {size} png 4x2 ]]")
    );
    assert_eq!(
        entries[4].preview(),
        "[[ binary data 17 B webp ]]"
    );

    let empty = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("clipboard-empty-{}", std::process::id()));
    let _ = fs::remove_dir_all(&empty);
    let store = StoreHandle::start(empty.clone()).unwrap();
    assert!(
        clipboard::list(store).await.is_empty(),
        "a new store lists nothing"
    );
    let _ = fs::remove_dir_all(&empty);
}

#[tokio::test]
async fn filters_search_and_selection() {
    let fixture = Fixture::new("filters");
    let store = fixture.store();
    let mut harness = Harness::new(Some(store.clone()));
    harness.opened(&store).await;
    assert_eq!(harness.ids(), ["5", "4", "3", "2", "1"]);
    assert_eq!(
        harness
            .panel
            .selected()
            .map(|entry| entry.id()),
        Some("5")
    );

    harness.send(Message::Move(-1));
    assert_eq!(
        harness.panel.selected_index(),
        4,
        "moving up wraps"
    );
    harness.send(Message::SetKind(Some(Kind::Image)));
    assert_eq!(harness.ids(), ["2"]);
    assert_eq!(
        harness.panel.selected_index(),
        0,
        "a kind resets the selection"
    );
    harness.send(Message::SetKind(Some(Kind::Text)));
    assert_eq!(
        harness.ids(),
        ["5"],
        "binary shows only under all"
    );
    harness.send(Message::SetKind(None));
    harness.send(Message::Input(String::from("  EXAMPLE ")));
    assert_eq!(harness.ids(), ["4"]);
    harness.send(Message::Input(String::from("binary data")));
    assert_eq!(harness.ids(), ["2", "1"]);
    harness.send(Message::Select(1));
    assert_eq!(
        harness
            .panel
            .selected()
            .map(|entry| entry.id()),
        Some("1")
    );
    harness.send(Message::Select(9));
    assert_eq!(
        harness.panel.selected_index(),
        1,
        "a row past the end changes nothing"
    );
    harness.send(Message::Input(String::from("nothing like this")));
    assert!(harness.ids().is_empty());
    assert!(harness.panel.selected().is_none());

    harness.open();
    assert_eq!(harness.panel.query(), "");
    assert_eq!(harness.panel.kind(), None);
    assert_eq!(
        harness.ids().len(),
        5,
        "the old list shows until the new one arrives"
    );
}

#[tokio::test]
async fn effects_copy_open_delete_and_clear() {
    let fixture = Fixture::new("effects");
    let store = fixture.store();
    let mut harness = Harness::new(Some(store.clone()));
    harness.opened(&store).await;

    assert_eq!(
        harness.send(Message::Copy),
        [Effect::Close, Effect::Clipboard(Request::Copy(String::from("5")))]
    );
    assert_eq!(
        harness.send(Message::CopyRow(2)),
        [Effect::Close, Effect::Clipboard(Request::Copy(String::from("3")))],
        "a double click copies its own row"
    );
    assert!(
        harness
            .send(Message::OpenLink)
            .is_empty(),
        "only links open"
    );

    harness.send(Message::Move(1));
    assert_eq!(
        harness.send(Message::OpenLink),
        [
            Effect::Close,
            Effect::Spawn {
                argv: vec![
                    String::from("deskshell"),
                    String::from("launch"),
                    String::from("--"),
                    String::from("xdg-open"),
                    String::from("https://example.org/docs?page=2")
                ],
                cwd: None
            }
        ],
        "the list preview until the text loads"
    );
    let text = clipboard::decode_text(store.clone(), String::from("4")).await;
    harness.send(Message::TextLoaded { id: String::from("4"), text });
    assert!(harness.panel.text_ready());
    assert_eq!(
        harness.send(Message::OpenLink)[1],
        Effect::Spawn {
            argv: vec![
                String::from("deskshell"),
                String::from("launch"),
                String::from("--"),
                String::from("xdg-open"),
                String::from("https://example.org/docs?page=2")
            ],
            cwd: None
        },
        "the loaded text, trimmed"
    );

    assert_eq!(
        harness.send(Message::Delete),
        [Effect::Clipboard(Request::Delete(String::from("4")))]
    );
    assert_eq!(
        harness.ids().len(),
        5,
        "the list changes when the store lists again"
    );

    assert_eq!(
        harness.send(Message::Clear),
        [Effect::Clipboard(Request::Clear)]
    );
    assert!(harness.panel.entries().is_empty());
    assert!(harness.send(Message::Delete).is_empty());
    assert!(harness.send(Message::Copy).is_empty());
}

#[tokio::test]
async fn without_a_store_nothing_lists_or_deletes() {
    let mut harness = Harness::new(None);
    harness.open();
    assert!(harness.ids().is_empty());
    harness.send(Message::Listed { sequence: 1, entries: clipboard::parse_list(b"7\tmade-up\n") });
    assert!(harness.ids().is_empty(), "no list was asked for");
    harness.send(Message::Listed { sequence: 0, entries: clipboard::parse_list(b"7\tmade-up\n") });
    assert!(
        harness.send(Message::Delete).is_empty(),
        "nothing to delete from"
    );
}

#[tokio::test]
async fn a_list_from_before_a_clear_is_dropped() {
    let fixture = Fixture::new("clear-list");
    let store = fixture.store();
    let mut harness = Harness::new(Some(store.clone()));
    harness.open();
    let pending = clipboard::list(store.clone()).await;
    harness.send(Message::Clear);
    harness.send(Message::Listed { sequence: 1, entries: pending });
    assert!(
        harness.panel.entries().is_empty(),
        "the list the open asked for came after the clear"
    );
}

#[tokio::test]
async fn debug_shows_no_clipboard_text() {
    let fixture = Fixture::new("debug");
    let store = fixture.store();
    let entries = clipboard::list(store.clone()).await;
    let shown = format!(
        "{:?} {:?} {:?}",
        Message::Listed { sequence: 1, entries },
        Message::TextLoaded { id: String::from("5"), text: String::from("Some copied words") },
        Request::CopyText(String::from("made-up answer")),
    );
    assert!(!shown.contains("copied"), "{shown}");
    assert!(!shown.contains("made-up"), "{shown}");
    let mut harness = Harness::new(Some(store.clone()));
    harness.opened(&store).await;
    let shown = format!("{:?}", harness.panel);
    assert!(!shown.contains("copied"), "{shown}");
}

#[tokio::test]
async fn a_reload_after_delete_takes_only_the_latest_list() {
    let fixture = Fixture::new("reload");
    let store = fixture.store();
    let mut harness = Harness::new(Some(store.clone()));
    harness.opened(&store).await;
    let stale = clipboard::list(store.clone()).await;
    assert!(store.delete(String::from("4")).await);
    assert!(store.delete(String::from("2")).await);
    assert!(store.delete(String::from("1")).await);
    harness.send(Message::Reload);
    harness.send(Message::Listed { sequence: 1, entries: stale });
    assert_eq!(harness.ids().len(), 5, "an older list is dropped");
    let fresh = clipboard::list(store.clone()).await;
    harness.send(Message::Listed { sequence: 2, entries: fresh });
    assert_eq!(harness.ids(), ["5", "3"]);
}

#[tokio::test]
async fn previews_for_text_image_and_color() {
    let fixture = Fixture::new("previews");
    let store = fixture.store();
    let mut harness = Harness::new(Some(store.clone()));
    harness.opened(&store).await;

    assert_eq!(harness.panel.meta(), "Text");
    assert_eq!(
        harness.panel.preview_text(),
        "Some copied words and a second line"
    );
    let text = clipboard::decode_text(store.clone(), String::from("5")).await;
    assert_eq!(text, "Some copied words\nand a second line\n");
    harness.send(Message::TextLoaded { id: String::from("5"), text });
    assert_eq!(harness.panel.meta(), "3 lines · 36 chars");
    assert_eq!(
        harness.panel.preview_text(),
        "Some copied words\nand a second line\n"
    );

    harness.send(Message::Move(2));
    let color = harness.panel.selected().unwrap();
    assert_matches!(
        &color.details,
        Details::Color { hex, rgb, hsl, .. }
            if hex == "#3d8bd9" && rgb == "rgb(61, 139, 217)" && hsl == "hsl(210, 67%, 55%)"
    );
    assert_eq!(harness.panel.meta(), "Color · rgb(61, 139, 217)");

    harness.send(Message::Move(1));
    let mut size = String::new();
    size_str(png(4, 2).len(), &mut size);
    assert_eq!(harness.panel.meta(), format!("4 × 2 · {size}"));
    assert!(harness.panel.image().is_none(), "decoding");
    let image = clipboard::decode_image(store.clone(), String::from("2")).await;
    harness.send(Message::ImageLoaded { id: String::from("2"), image });
    match harness.panel.image() {
        Some(Slot::Ready(image)) => assert_eq!((image.width, image.height), (4, 2)),
        other => panic!("no image: {other:?}"),
    }

    harness.send(Message::Move(1));
    assert_eq!(harness.panel.meta(), "Binary data");
    assert!(
        harness.panel.image().is_none(),
        "binary data has no image"
    );

    harness.panel.release();
    assert!(!harness.panel.text_ready());
    harness.send(Message::Move(-1));
    assert!(
        harness.panel.image().is_none(),
        "closing drops the decoded images"
    );
}

#[tokio::test]
async fn text_previews_stop_at_64_kib() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("clipboard-long-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let mut long = "ก".repeat(30_000).into_bytes();
    long.extend_from_slice(&[b'a'; 10]);
    Store::open(&dir).add(0, one("text/plain", &long));
    let store = StoreHandle::start(dir.clone()).unwrap();
    let text = clipboard::decode_text(store.clone(), String::from("1")).await;
    assert_eq!(
        text.len(),
        65_535 + 3,
        "a cut character becomes U+FFFD"
    );
    assert!(text.ends_with('\u{fffd}'));
    assert_eq!(
        clipboard::decode_text(store, String::from("404")).await,
        "",
        "a missing entry gives no text"
    );
    let _ = fs::remove_dir_all(&dir);
}
