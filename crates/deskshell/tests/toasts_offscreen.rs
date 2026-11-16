//! Offscreen renders of the toasts and the notification entry with made-up notifications.

use std::borrow::Cow;
use std::env;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use deskshell::notification::{self, Body, Content, Settings};
use deskshell::toasts::{self, Message, Toasts};
use deskshell_apps::Snapshot;
use deskshell_notify::{Action, Entry, Image, Pixels, Urgency};
use deskshell_ui::{Theme, fonts};
use iced::advanced::graphics::text::font_system;
use iced::widget::{column, container};
use iced::{Background, Element, Length, Padding, Settings as IcedSettings, Size};
use iced_test::Simulator;
use image::{Rgba, RgbaImage};

fn main() {
    // SAFETY: this test has no harness, so no other thread exists yet.
    unsafe {
        for key in ["WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY"] {
            env::remove_var(key);
        }
        env::set_var("ICED_TEST_BACKEND", "tiny-skia");
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("offscreen/toasts");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    {
        let mut system = font_system().write().unwrap();
        for font in fonts::all() {
            system.load_font(Cow::Borrowed(font));
        }
    }
    let icon = dir.join("app-icon.png");
    RgbaImage::from_fn(64, 64, |x, y| {
        let inside = (x as i32 - 32).pow(2) + (y as i32 - 32).pow(2) < 28 * 28;
        if inside { Rgba([0x6e, 0xc8, 0x8a, 255]) } else { Rgba([0, 0, 0, 0]) }
    })
    .save(&icon)
    .unwrap();

    let theme = Theme::default();
    let apps = Snapshot::default();
    let popups = popups(&icon);
    let mut toasts = Toasts::default();
    let start = Instant::now();
    let _ = toasts.sync(&popups, &theme, &apps, start);
    toasts.load_now(&theme);
    let mut requests = Vec::new();
    let settled = start + Duration::from_millis(400);
    toasts.update(
        Message::Frame(settled),
        &theme,
        settled,
        &mut requests,
    );
    assert!(!toasts.animating());
    assert_eq!(toasts.hidden(), 2);
    assert!(
        toasts.toasts()[2].truncated(),
        "the long body gets the expand button"
    );
    assert!(!toasts.toasts()[1].truncated());
    render(
        &dir,
        "toasts",
        (470.0, 560.0),
        toasts::view(&toasts, &theme),
    );

    let first = toasts.toasts()[0].key();
    for message in [notification::Message::ToggleExpand, notification::Message::ShowAllActions] {
        toasts.update(
            Message::Entry(first, message),
            &theme,
            settled,
            &mut requests,
        );
    }
    render(
        &dir,
        "toasts-expanded",
        (470.0, 640.0),
        toasts::view(&toasts, &theme),
    );
    assert!(requests.is_empty());

    let hub = hub_entry();
    let body = Body::parse(&hub.body);
    // Images are square, because tiny-skia ignores the clip bounds that crop a covering image.
    let image = notification::pixels::handle(&checker(96, 96), 128).unwrap();
    let state =
        notification::State { hovered: true, reply: String::new(), show_all_actions: false };
    let label = notification::time_label("Chat", "5m");
    let settings = Settings { time_label: &label, close_on_hover: true, ..Settings::default() };
    let truncated = notification::truncated(&theme, &hub, &body, 400.0, 480.0, 3);
    let entry = notification::view(
        &theme,
        Content { entry: &hub, body: &body, image: Some(&image), truncated },
        &state,
        &settings,
        1.0,
    );
    let card = container(entry)
        .padding(Padding { top: 12.0, right: 8.0, bottom: 12.0, left: 12.0 })
        .width(560)
        .style(move |_| container::Style {
            background: Some(Background::Color(theme.colors.glass)),
            border: iced::Border { radius: 22.0.into(), ..iced::Border::default() },
            ..container::Style::default()
        });
    let card: Element<'_, notification::Message> = card.into();
    snapshot(
        &dir,
        "entry-hub",
        (600.0, 320.0),
        on_wallpaper(card.map(|_| ())),
    );
    println!("offscreen renders in {}", dir.display());
}

fn entry(uid: &str, app: &str, summary: &str, body: &str) -> Entry {
    Entry {
        uid: uid.to_owned(),
        server_id: 1,
        received_at: 1,
        app_name: app.to_owned(),
        app_icon: String::new(),
        desktop_entry: String::new(),
        category: String::new(),
        summary: summary.to_owned(),
        body: body.to_owned(),
        image: None,
        urgency: Urgency::Normal,
        actions: Vec::new(),
        inline_reply: None,
        live: true,
    }
}

fn action(identifier: &str, label: &str) -> Action {
    Action { identifier: identifier.to_owned(), label: label.to_owned() }
}

fn checker(width: u32, height: u32) -> Pixels {
    let mut data = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let pixel = if (x / 16 + y / 16) % 2 == 0 {
                [0x3d, 0x8b, 0xd9, 255]
            } else {
                [0xe8, 0xa8, 0x38, 255]
            };
            data.extend_from_slice(&pixel);
        }
    }
    Pixels {
        width,
        height,
        rowstride: width * 4,
        has_alpha: true,
        channels: 4,
        data: data.into_boxed_slice(),
    }
}

fn popups(icon: &Path) -> Vec<Arc<Entry>> {
    let mut chat = entry(
        "5",
        "Chat",
        "แชทกลุ่ม: Lunch plans",
        "Anna: <b>Are we still on</b> for noon? &amp; bring the <a href=\"https://example.org\">menu</a>",
    );
    chat.image = Some(Image::Pixels(Arc::new(checker(96, 96))));
    chat.app_icon = icon.to_string_lossy().into_owned();
    chat.actions = vec![
        action("default", "Open"),
        action("reply", "Reply"),
        action("mark", "Mark as read"),
        action("mute", "Mute"),
    ];
    let mut battery = entry(
        "4",
        "Power",
        "Battery low",
        "5% left. Plug in the charger.",
    );
    battery.urgency = Urgency::Critical;
    let long = entry(
        "3",
        "อัปเดต",
        "System updates are ready to install on this machine tonight",
        "Twelve packages can be updated. The kernel and the graphics driver are among them, \
         so a restart follows. Save your work before you start, because open windows close \
         during the restart, and the update itself takes a few minutes.",
    );
    vec![
        Arc::new(chat),
        Arc::new(battery),
        Arc::new(long),
        Arc::new(entry("2", "Mail", "Hidden one", "")),
        Arc::new(entry("1", "Mail", "Hidden two", "")),
    ]
}

fn hub_entry() -> Entry {
    let mut hub = entry(
        "9",
        "Chat",
        "Anna sent a photo",
        "Look at this view from the hill!<br>We should go again next week.",
    );
    hub.actions = vec![action("default", "Open"), action("like", "Like")];
    hub.inline_reply = Some(String::from("Reply to Anna…"));
    hub
}

fn render(dir: &Path, name: &str, size: (f32, f32), element: Element<'_, Message>) {
    let element: Element<'_, ()> = element.map(|_| ());
    let placed = container(element)
        .padding(Padding { top: 20.0, right: 20.0, bottom: 20.0, left: 20.0 })
        .width(Length::Fill)
        .height(Length::Fill);
    snapshot(dir, name, size, on_wallpaper(placed.into()));
}

fn on_wallpaper<'a>(content: Element<'a, ()>) -> Element<'a, ()> {
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

fn snapshot(dir: &Path, name: &str, size: (f32, f32), element: Element<'_, ()>) {
    let settings = IcedSettings {
        fonts: fonts::all()
            .iter()
            .map(|font| Cow::Borrowed(*font))
            .collect(),
        default_font: fonts::PLEX,
        ..IcedSettings::default()
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
