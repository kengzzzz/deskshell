//! Offscreen renders of the clipboard and keybindings panels with fixture data.

use std::borrow::Cow;
use std::env;
use std::fs;
use std::io::Cursor;
use std::path::Path;

use deskshell::panel::clipboard::{self, Clipboard, Message as ClipMessage};
use deskshell::panel::keybindings::{Keybindings, Message as KeyMessage};
use deskshell::panel::{Effect, Sources};
use deskshell_hypr::Bind;
use deskshell_ui::{Theme, fonts};
use iced::advanced::graphics::text::font_system;
use iced::widget::{column, container};
use iced::{Background, Element, Length, Settings, Size};
use iced_test::Simulator;
use image::{ImageFormat, Rgba, RgbaImage};

type Message = ();

const LIST: &str = "9\tแชทกลุ่ม: สวัสดีครับ copied from a chat\n\
    8\thttps://example.org/docs/getting-started?lang=en#install\n\
    7\t#3d8bd9\n\
    6\t[[ binary data 3 KiB png 96x64 ]]\n\
    5\t[[ binary data 12 KiB ]]\n\
    4\tcargo test --workspace\n\
    3\t#f0a\n\
    2\tA much longer paragraph of copied text that runs past the width of its row\n\
    1\thttp://localhost:8080/\n";

fn main() {
    // SAFETY: this test has no harness, so no other thread exists yet.
    unsafe {
        for key in ["WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY"] {
            env::remove_var(key);
        }
        env::set_var("ICED_TEST_BACKEND", "tiny-skia");
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("offscreen/overlays");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    {
        let mut system = font_system().write().unwrap();
        for font in fonts::all() {
            system.load_font(Cow::Borrowed(font));
        }
    }
    let theme = Theme::default();
    let sources = Sources::default();
    let env = sources.env(&theme);
    let mut effects: Vec<Effect> = Vec::new();

    let mut panel = Clipboard::default();
    let _ = panel.open(&env);
    let mut send = |panel: &mut Clipboard, message: ClipMessage| {
        let _ = panel.update(message, &env, &mut effects);
    };
    send(
        &mut panel,
        ClipMessage::Listed { sequence: 0, entries: clipboard::parse_list(LIST.as_bytes()) },
    );
    let small = (1280.0, 720.0);
    let large = (2560.0, 1440.0);
    send(
        &mut panel,
        ClipMessage::TextLoaded {
            id: String::from("9"),
            text: String::from(
                "แชทกลุ่ม: สวัสดีครับ copied from a chat\nwith a second line\n\nand a third",
            ),
        },
    );
    render(&dir, "clipboard-text", small, panel.view(&env));
    render(
        &dir,
        "clipboard-text-1440",
        large,
        panel.view(&env),
    );
    send(&mut panel, ClipMessage::Move(1));
    render(&dir, "clipboard-link", small, panel.view(&env));
    send(&mut panel, ClipMessage::Move(1));
    render(&dir, "clipboard-color", small, panel.view(&env));
    send(&mut panel, ClipMessage::Move(1));
    render(
        &dir,
        "clipboard-image-loading",
        small,
        panel.view(&env),
    );
    send(
        &mut panel,
        ClipMessage::ImageLoaded {
            id: String::from("6"),
            image: clipboard::decode_bytes(&sample_png()),
        },
    );
    render(&dir, "clipboard-image", small, panel.view(&env));
    send(&mut panel, ClipMessage::Move(1));
    render(&dir, "clipboard-binary", small, panel.view(&env));
    send(
        &mut panel,
        ClipMessage::Input(String::from("zzz")),
    );
    render(
        &dir,
        "clipboard-no-match",
        small,
        panel.view(&env),
    );
    send(&mut panel, ClipMessage::Clear);
    render(&dir, "clipboard-empty", small, panel.view(&env));

    let mut sheet = Keybindings::default();
    let _ = sheet.open(&env);
    let _ = sheet.update(KeyMessage::Binds(Some(binds())), &env);
    render(&dir, "keybindings", small, sheet.view(&env));
    render(&dir, "keybindings-1440", large, sheet.view(&env));
    let _ = sheet.update(KeyMessage::Input(String::from("workspace")), &env);
    render(
        &dir,
        "keybindings-search",
        small,
        sheet.view(&env),
    );
    let _ = sheet.update(
        KeyMessage::Input(String::from("nothing at all")),
        &env,
    );
    render(
        &dir,
        "keybindings-no-match",
        small,
        sheet.view(&env),
    );
    assert!(
        effects
            .iter()
            .all(|effect| !matches!(effect, Effect::Pipe { .. })),
        "nothing was deleted"
    );
    println!("offscreen renders in {}", dir.display());
}

fn render(
    dir: &Path,
    name: &str,
    size: (f32, f32),
    element: Element<'_, deskshell::panel::Message>,
) {
    snapshot(dir, name, size, on_wallpaper(element.map(|_| ())));
}

fn sample_png() -> Vec<u8> {
    let image = RgbaImage::from_fn(96, 64, |x, y| {
        if (x / 16 + y / 16) % 2 == 0 {
            Rgba([0x3d, 0x8b, 0xd9, 255])
        } else {
            Rgba([0xe8, 0xa8, 0x38, 255])
        }
    });
    let mut bytes = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .unwrap();
    bytes
}

fn bind(description: &str, modmask: u32, key: &str) -> Bind {
    Bind {
        modmask,
        key: key.to_owned(),
        description: description.to_owned(),
        has_description: true,
        ..Bind::default()
    }
}

fn binds() -> Vec<Bind> {
    let mut binds = vec![
        bind("Open terminal", 64, "RETURN"),
        bind("Open browser", 64, "b"),
        bind("Open file manager", 64, "e"),
        bind("Close window", 64, "q"),
        bind("Toggle fullscreen", 64, "f"),
        bind("Toggle floating", 65, "space"),
        bind("Move focus left", 64, "left"),
        bind("Move focus right", 64, "right"),
        bind("Move focus up", 64, "up"),
        bind("Move focus down", 64, "down"),
        bind("Swap window left", 65, "left"),
        bind("Swap window right", 65, "right"),
        bind("Resize window (drag)", 64, "mouse:273"),
        bind("Open window overview", 64, "tab"),
        bind("Toggle launcher", 64, "SPACE"),
        bind("Open clipboard manager", 64, "v"),
        bind("Show keybindings cheat sheet", 68, "k"),
        bind("Open power menu", 68, "p"),
        bind("Lock screen", 68, "l"),
        bind(
            "Increase volume (Max 100%)",
            0,
            "XF86AudioRaiseVolume",
        ),
        bind("Decrease volume", 0, "XF86AudioLowerVolume"),
        bind("Toggle audio mute", 0, "XF86AudioMute"),
        bind("Play/Pause media", 0, "XF86AudioPlay"),
        bind("Take area screenshot", 64, "Print"),
        bind("Take full screenshot", 0, "Print"),
        bind("Toggle HDR on focused monitor", 72, "h"),
        bind("Do something else", 72, "x"),
        bind("", 64, "mouse:272"),
    ];
    for n in 1..=10 {
        let key = (n % 10).to_string();
        binds.push(bind(
            &format!("Switch to workspace {n}"),
            64,
            &key,
        ));
        binds.push(bind(
            &format!("Move window to workspace {n}"),
            65,
            &key,
        ));
    }
    binds
}

fn on_wallpaper<'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    container(column![content])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(Background::Color(iced::Color::from_rgb8(
                0x3a, 0x4a, 0x6b,
            ))),
            ..container::Style::default()
        })
        .into()
}

fn snapshot(dir: &Path, name: &str, size: (f32, f32), element: Element<'_, Message>) {
    let settings = Settings {
        fonts: fonts::all()
            .iter()
            .map(|font| Cow::Borrowed(*font))
            .collect(),
        default_font: fonts::PLEX,
        ..Settings::default()
    };
    let mut simulator = Simulator::with_size(settings, Size::new(size.0, size.1), element);
    let snapshot = simulator
        .snapshot(&iced::Theme::Dark)
        .expect("snapshot");
    assert!(
        snapshot
            .matches_image(dir.join(name))
            .expect("write PNG")
    );
}
