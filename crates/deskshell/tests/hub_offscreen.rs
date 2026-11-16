//! Offscreen renders of the Hub with made-up service states, plus clicks through its view.

mod hub_common;

use std::assert_matches;
use std::borrow::Cow;
use std::env;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use deskshell::notification::Message as EntryMessage;
use deskshell::panel::hub::{
    ConnectionsMessage, ControlsMessage, Hub, Inputs, Message, NotificationsMessage, Secret,
    Sources, Tab,
};
use deskshell::panel::{Effect, Env};
use deskshell_ui::{Theme, fonts};
use iced::advanced::graphics::text::font_system;
use iced::advanced::renderer::Headless;
use iced::widget::container;
use iced::{Background, Element, Length, Padding, Settings, Size};
use iced_test::Simulator;
use iced_test::futures::futures::executor::block_on;
use iced_test::runtime::user_interface::{self, UserInterface};

const WIDTH: f32 = 1560.0;
const HEIGHT: f32 = 640.0;
const MARGIN: f32 = 20.0;

fn main() {
    // SAFETY: this test has no harness, so no other thread exists yet.
    unsafe {
        for key in ["WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY"] {
            env::remove_var(key);
        }
        env::set_var("ICED_TEST_BACKEND", "tiny-skia");
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("offscreen/hub");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    {
        let mut system = font_system().write().unwrap();
        for font in fonts::all() {
            system.load_font(Cow::Borrowed(font));
        }
    }
    let theme = Theme::default();
    let panel_sources = deskshell::panel::Sources::default();
    let env = panel_sources.env(&theme);
    let now = hub_common::now();
    let icon = dir.join("app-icon.png");
    image::RgbaImage::from_fn(80, 80, |x, y| {
        let inside = (x as i32 - 40).pow(2) + (y as i32 - 40).pow(2) < 34 * 34;
        if inside { image::Rgba([0x6e, 0xc8, 0x8a, 255]) } else { image::Rgba([0, 0, 0, 0]) }
    })
    .save(&icon)
    .unwrap();
    let mut full = hub_common::full();
    let mut mail = (*full.notify.history[3]).clone();
    mail.app_icon = icon.to_string_lossy().into_owned();
    full.notify.history[3] = std::sync::Arc::new(mail);

    let mut hub = Hub::default();
    open(&mut hub, &env, full.inputs());
    render(&dir, "full", &hub, &env, full.inputs(), WIDTH);
    let messages = clicks(&hub, &env, full.inputs(), "Like");
    assert!(
        messages.iter().any(|message| matches!(
            message,
            Message::Notifications(NotificationsMessage::Entry(
                _,
                EntryMessage::Action(1)
            ))
        )),
        "{messages:?}"
    );
    assert!(
        !messages.iter().any(|message| matches!(
            message,
            Message::Notifications(NotificationsMessage::ToggleGroup(_))
        )),
        "an action on a stacked card does not toggle the group: {messages:?}"
    );
    let messages = clicks(&hub, &env, full.inputs(), "Saved");
    assert!(
        messages.iter().any(|message| matches!(
            message,
            Message::Connections(ConnectionsMessage::Network { interface, ssid })
                if interface == "wlan0" && ssid == "Studio"
        )),
        "{messages:?}"
    );
    let messages = clicks(&hub, &env, full.inputs(), "Fullscreen");
    assert_matches!(
        messages.as_slice(),
        [Message::Controls(ControlsMessage::Vrr)],
        "{messages:?}"
    );
    assert_eq!(
        hub.notifications().label("20-2"),
        Some("3h"),
        "the badge image hides the app name"
    );

    let mut empty = Hub::default();
    open(&mut empty, &env, Inputs::idle());
    render(&dir, "empty", &empty, &env, Inputs::idle(), WIDTH);

    let mut effects = Vec::new();
    let mut send = |hub: &mut Hub, inputs: Inputs<'_>, message: Message| {
        let _ = hub.update(message, &env, inputs, &mut effects);
    };
    send(
        &mut hub,
        full.inputs(),
        Message::Connections(ConnectionsMessage::Network {
            interface: "wlan0".into(),
            ssid: "Library".into(),
        }),
    );
    send(
        &mut hub,
        full.inputs(),
        Message::Connections(ConnectionsMessage::Password(Secret::new(
            "letmein",
        ))),
    );
    assert_eq!(hub.pending_network(), Some(("wlan0", "Library")));
    render(&dir, "password", &hub, &env, full.inputs(), WIDTH);

    send(
        &mut hub,
        full.inputs(),
        Message::Connections(ConnectionsMessage::Tab(Tab::Bluetooth)),
    );
    render(
        &dir,
        "bluetooth",
        &hub,
        &env,
        full.inputs(),
        WIDTH,
    );
    let messages = clicks(&hub, &env, full.inputs(), "close");
    assert!(
        messages.iter().any(|message| matches!(
            message,
            Message::Connections(ConnectionsMessage::Forget(path)) if path.ends_with("dev_2")
        )),
        "{messages:?}"
    );
    assert!(
        !messages.iter().any(|message| matches!(
            message,
            Message::Connections(ConnectionsMessage::Device(_))
        )),
        "the forget icon does not click its row: {messages:?}"
    );
    send(
        &mut hub,
        full.inputs(),
        Message::Connections(ConnectionsMessage::Tab(Tab::Audio)),
    );
    render(&dir, "audio", &hub, &env, full.inputs(), WIDTH);

    let mut muted: Sources = full.clone();
    if let Some(sink) = &mut muted.audio.sink {
        sink.muted = true;
    }
    muted.system.cpu_power.mode = None;
    muted.system.updates.total = 0;
    muted.system.updates.error = Some(String::from("no server of core answered"));
    muted.system.display.brightness = None;
    muted.hdr = true;
    muted.notify.dnd = true;
    let _ = hub.sync(&env, muted.inputs(), &now);
    let chat = hub
        .notifications()
        .group_key("Chat")
        .unwrap();
    send(
        &mut hub,
        muted.inputs(),
        Message::Notifications(NotificationsMessage::ToggleGroup(chat)),
    );
    let photo = hub
        .notifications()
        .entry_key("50-5")
        .unwrap();
    send(
        &mut hub,
        muted.inputs(),
        Message::Notifications(NotificationsMessage::Entry(
            photo,
            deskshell::notification::Message::ToggleExpand,
        )),
    );
    send(
        &mut hub,
        muted.inputs(),
        Message::Connections(ConnectionsMessage::Tab(Tab::Network)),
    );
    render(
        &dir,
        "expanded",
        &hub,
        &env,
        muted.inputs(),
        WIDTH,
    );

    let mut tight = full.clone();
    if let (Some(sink), Some(source)) = (&mut tight.audio.sink, &mut tight.audio.source) {
        sink.volume = 0.95;
        source.volume = 0.1;
    }
    if let Some(brightness) = &mut tight.system.display.brightness {
        brightness.value = 50;
    }
    let mut narrow = Hub::default();
    open(&mut narrow, &env, tight.inputs());
    render(
        &dir,
        "narrow",
        &narrow,
        &env,
        tight.inputs(),
        1240.0,
    );
    assert!(
        effects
            .iter()
            .all(|effect| !matches!(effect, Effect::Close)),
        "{effects:?}"
    );
    let mut boost = full.clone();
    boost.system.cpu_power.mode = Some(deskshell_system::PowerMode::Performance);
    boost.system.cpu_power.boosted = true;
    let mut boosted = Hub::default();
    open(&mut boosted, &env, boost.inputs());
    render(
        &dir,
        "boost",
        &boosted,
        &env,
        boost.inputs(),
        WIDTH,
    );
    slide(&dir, &env, &full);
    println!("offscreen renders in {}", dir.display());
}

/// Frames go through `UserInterface` with their own times, because the Simulator's
/// snapshot draws at the wall clock.
fn slide(dir: &Path, env: &Env<'_>, sources: &Sources) {
    let mut renderer = block_on(iced::Renderer::new(
        fonts::PLEX,
        iced::Pixels(16.0),
        Some("tiny-skia"),
    ))
    .expect("a tiny-skia renderer");
    let mut hub = Hub::default();
    open(&mut hub, env, sources.inputs());
    let mut quieter = sources.clone();
    if let Some(sink) = &mut quieter.audio.sink {
        sink.volume = 0.2;
    }
    let size = Size::new(WIDTH + 2.0 * MARGIN, HEIGHT + 2.0 * MARGIN);
    let start = Instant::now();
    let change = start + Duration::from_millis(500);
    let frames = [
        (sources, start, None),
        (&quieter, change, Some("slide-0ms")),
        (
            &quieter,
            change + Duration::from_millis(33),
            Some("slide-33ms"),
        ),
        (
            &quieter,
            change + Duration::from_millis(100),
            Some("slide-end"),
        ),
    ];
    let mut cache = user_interface::Cache::new();
    let mut redraws = Vec::new();
    for (sources, now, name) in frames {
        let view = on_wallpaper(
            hub.view(env, sources.inputs())
                .map(|_| ()),
            WIDTH,
        );
        let mut ui = UserInterface::build(view, size, cache, &mut renderer);
        let (state, _) = ui.update(
            &[iced::Event::Window(iced::window::Event::RedrawRequested(now))],
            iced::mouse::Cursor::Unavailable,
            &mut renderer,
            &mut iced::advanced::clipboard::Null,
            &mut Vec::new(),
        );
        if let user_interface::State::Updated { redraw_request, .. } = state {
            redraws.push(redraw_request);
        }
        if let Some(name) = name {
            ui.draw(
                &mut renderer,
                &iced::Theme::Dark,
                &iced::advanced::renderer::Style { text_color: iced::Color::WHITE },
                iced::mouse::Cursor::Unavailable,
            );
            let scale = 2.0;
            let (width, height) = (
                (size.width * scale) as u32,
                (size.height * scale) as u32,
            );
            let rgba = renderer.screenshot(
                Size::new(width, height),
                scale,
                iced::Color::BLACK,
            );
            let image = image::RgbaImage::from_raw(width, height, rgba).expect("a full frame");
            image
                .save(dir.join(format!("{name}.png")))
                .expect("write PNG");
            let (x, y) = ((500.0 * scale) as u32, (350.0 * scale) as u32);
            image::imageops::crop_imm(&image, x, y, 700 * 2, 180 * 2)
                .to_image()
                .save(dir.join(format!("{name}-sliders.png")))
                .expect("write a crop");
        }
        cache = ui.into_cache();
    }
    use iced::window::RedrawRequest::{NextFrame, Wait};
    assert_eq!(redraws, [Wait, NextFrame, NextFrame, Wait]);
    let pixels = |name: &str| {
        image::open(dir.join(format!("{name}.png")))
            .expect("read PNG")
            .into_rgba8()
    };
    assert!(pixels("slide-0ms") != pixels("slide-33ms"));
    assert!(pixels("slide-33ms") != pixels("slide-end"));
}

fn on_wallpaper<'a>(element: Element<'a, ()>, width: f32) -> Element<'a, ()> {
    container(
        container(element)
            .width(width)
            .height(HEIGHT),
    )
    .padding(Padding::new(MARGIN))
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

fn open(hub: &mut Hub, env: &Env<'_>, inputs: Inputs<'_>) {
    let _ = hub.open(env, inputs, &hub_common::now());
    hub.load_now(env.theme);
    let _ = hub.drain_effects().count();
}

fn settings() -> Settings {
    Settings {
        fonts: fonts::all()
            .iter()
            .map(|font| Cow::Borrowed(*font))
            .collect(),
        default_font: fonts::PLEX,
        ..Settings::default()
    }
}

fn clicks(hub: &Hub, env: &Env<'_>, inputs: Inputs<'_>, target: &str) -> Vec<Message> {
    let mut simulator = Simulator::with_size(
        settings(),
        Size::new(WIDTH, HEIGHT),
        hub.view(env, inputs),
    );
    simulator
        .click(target)
        .unwrap_or_else(|err| panic!("{target}: {err}"));
    simulator.into_messages().collect()
}

fn render(dir: &Path, name: &str, hub: &Hub, env: &Env<'_>, inputs: Inputs<'_>, width: f32) {
    let placed = on_wallpaper(hub.view(env, inputs).map(|_| ()), width);
    let size = Size::new(width + 2.0 * MARGIN, HEIGHT + 2.0 * MARGIN);
    let mut simulator = Simulator::with_size(settings(), size, placed);
    let snapshot = simulator
        .snapshot(&iced::Theme::Dark)
        .expect("snapshot");
    assert!(
        snapshot
            .matches_image(dir.join(name))
            .expect("write PNG")
    );
    let path = dir.join(format!("{name}-tiny-skia.png"));
    let image = image::open(&path)
        .expect("read the render")
        .to_rgba8();
    let scale = image.width() / size.width as u32;
    let middle = width as u32 - 880;
    let right = size.width as u32 - 460;
    let crops = [("left", 0, 540), ("middle", 500, middle), ("right", right, 460)];
    for (part, x, width) in crops {
        let x = x * scale;
        let width = (width * scale).min(image.width() - x);
        image::imageops::crop_imm(&image, x, 0, width, image.height())
            .to_image()
            .save(dir.join(format!("{name}-{part}.png")))
            .expect("write a crop");
    }
}
