//! Offscreen renders of the bar, OSD, tooltips, tray menu and bar transitions.

use std::borrow::Cow;
use std::env;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use deskshell::bar::{self, Bars, Highlight, Surface};
use deskshell_apps::Index;
use deskshell_hypr::{Address, State, Window, Workspace, WorkspaceRef};
use deskshell_media::{Audio, Device, DeviceKind, OsdEvent};
use deskshell_tray::{
    Category, Icon, Item, Menu, MenuEntry, Pixmap, Status as TrayStatus, Toggle, ToggleState,
    Tooltip,
};
use deskshell_ui::{Theme, fonts};
use iced::advanced::graphics::text::font_system;
use iced::widget::container;
use iced::widget::image::Handle;
use iced::{Background, Element, Length, Settings, Size, window};
use iced_test::Simulator;

type Message = bar::Message;

const BAR: (f32, f32) = (1260.0, 48.0);
const WIDE: (f32, f32) = (1920.0, 48.0);

fn main() {
    // SAFETY: this test has no harness, so no other thread exists yet.
    unsafe {
        for key in ["WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY"] {
            env::remove_var(key);
        }
        env::set_var("ICED_TEST_BACKEND", "tiny-skia");
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("offscreen/bar");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    {
        let mut system = font_system().write().unwrap();
        for font in fonts::all() {
            system.load_font(Cow::Borrowed(font));
        }
    }
    let theme = Theme::default();
    let surface = Surface::new(
        window::Id::unique(),
        1,
        Some(String::from("Virtual-1")),
    );

    let mut busy = busy_bars();
    render(
        &dir,
        "bar",
        BAR,
        bar::view(&busy, &surface, &theme, Highlight::default()),
    );
    render(
        &dir,
        "bar-wide",
        (2540.0, 48.0),
        bar::view(
            &busy,
            &surface,
            &theme,
            Highlight { hub: true, media: true, updates: true },
        ),
    );

    busy.osd
        .show(&OsdEvent { kind: DeviceKind::Sink, volume: 0.64, muted: false });
    assert_eq!(
        busy.osd_place(),
        bar::osd::Place::Pill(String::from("Virtual-1"))
    );
    render(
        &dir,
        "bar-osd",
        BAR,
        bar::view(&busy, &surface, &theme, Highlight::default()),
    );

    busy.osd
        .show(&OsdEvent { kind: DeviceKind::Source, volume: 0.3, muted: true });
    render(
        &dir,
        "osd-popup",
        (
            bar::osd::POPUP_WIDTH,
            bar::osd::POPUP_HEIGHT as f32,
        ),
        bar::tabs::popup(&busy.osd, &theme, 1.0, busy.osd.level),
    );

    render(
        &dir,
        "tooltip",
        (360.0, bar::tabs::TIP_HEIGHT as f32),
        bar::tabs::tooltip(
            "แผนที่เส้นทาง - ทริปเชียงใหม่",
            true,
            "Viewer · off-screen, right",
            &theme,
        ),
    );

    let mut menu = bar::menu::MenuView::new(fake_menu());
    let loads = menu.icon_loads();
    assert_eq!(loads.len(), 2);
    let loaded = loads
        .iter()
        .map(|load| match &load.source {
            bar::menu::IconSource::Data(_) => (load.id, bar::menu::resolve(load, None)),
            bar::menu::IconSource::Name { .. } => {
                (load.id, Some(square(32, [0x6e, 0xc8, 0x8a, 255])))
            }
        })
        .collect();
    assert!(menu.icons_loaded(loaded));
    let size = menu.size(|label| {
        bar::text_width(
            &theme,
            label,
            bar::menu::LABEL_SIZE,
            iced::font::Weight::Normal,
        )
    });
    render(
        &dir,
        "menu",
        size,
        bar::menu::view(&menu, &theme).map(|_| bar::Message::Redraw),
    );
    menu.update(bar::menu::Message::Entry(3));
    let size = menu.size(|label| {
        bar::text_width(
            &theme,
            label,
            bar::menu::LABEL_SIZE,
            iced::font::Weight::Normal,
        )
    });
    render(
        &dir,
        "menu-submenu",
        size,
        bar::menu::view(&menu, &theme).map(|_| bar::Message::Redraw),
    );
    let tip = bar::tip_width(
        &theme,
        "แผนที่เส้นทาง - ทริปเชียงใหม่",
        "Viewer · off-screen, right",
    );
    assert!(tip > 100.0 && tip < 360.0, "{tip}");

    let mut idle = idle_bars();
    render(
        &dir,
        "bar-idle",
        BAR,
        bar::view(&idle, &surface, &theme, Highlight::default()),
    );

    in_use(&dir, &theme, &surface, &mut busy, &mut idle);

    transitions(&dir, &theme);

    println!("offscreen renders in {}", dir.display());
}

fn in_use(dir: &Path, theme: &Theme, surface: &Surface, busy: &mut Bars, idle: &mut Bars) {
    let names = |list: &[&str]| -> Vec<String> {
        list.iter()
            .map(|name| (*name).to_owned())
            .collect()
    };
    busy.osd.showing = false;
    assert!(
        busy.in_use
            .set(names(&["Discord"]), names(&["Brave"]))
    );
    render(
        dir,
        "bar-in-use",
        BAR,
        bar::view(busy, surface, theme, Highlight::default()),
    );
    let width = bar::in_use::tip_width(theme, &busy.in_use);
    assert!(width > 60.0 && width < 200.0, "{width}");
    render(
        dir,
        "in-use-tooltip",
        (width, bar::tabs::TIP_HEIGHT as f32),
        bar::in_use::tooltip(&busy.in_use, theme),
    );
    assert!(
        busy.in_use
            .set(names(&["Discord", "Brave"]), Vec::new())
    );
    let width = bar::in_use::tip_width(theme, &busy.in_use);
    render(
        dir,
        "in-use-tooltip-mic",
        (width, bar::tabs::TIP_HEIGHT as f32),
        bar::in_use::tooltip(&busy.in_use, theme),
    );
    assert!(
        idle.in_use
            .set(Vec::new(), names(&["Brave"]))
    );
    render(
        dir,
        "bar-in-use-camera",
        BAR,
        bar::view(idle, surface, theme, Highlight::default()),
    );
}

const MS: Duration = Duration::from_millis(1);
const FRAME: Duration = Duration::from_millis(16);

fn sync(surface: &mut Surface, bars: &mut Bars, theme: &Theme, now: Instant) -> bool {
    bars.measure(theme);
    let place = bars.osd_place();
    surface.sync_anim(bars, &place, now)
}

fn run_out(surface: &mut Surface, bars: &Bars, from: Instant) -> Instant {
    let mut now = from;
    while surface.settle_anim(bars, now) {
        now += FRAME;
        assert!(
            now - from < Duration::from_secs(2),
            "a transition never ends"
        );
    }
    now
}

fn max_width(messages: &[Message]) -> Option<f32> {
    messages
        .iter()
        .find_map(|message| match message {
            Message::MaxWidth(width) => Some(*width),
            _ => None,
        })
}

fn assert_same(dir: &Path, natural: &str, synced: &str) {
    let load = |name: &str| {
        image::open(dir.join(format!("{name}-tiny-skia.png")))
            .unwrap()
            .to_rgba8()
    };
    let (a, b) = (load(natural), load(synced));
    assert_eq!(a.dimensions(), b.dimensions());
    let differ: Vec<(u32, u32)> = a
        .enumerate_pixels()
        .filter(|(x, y, pixel)| *pixel != b.get_pixel(*x, *y))
        .map(|(x, y, _)| (x, y))
        .collect();
    assert!(
        differ.is_empty(),
        "{synced} differs from {natural} in {} pixels, the first at {:?}",
        differ.len(),
        differ.first()
    );
}

fn transitions(dir: &Path, theme: &Theme) {
    fn view<'a>(bars: &'a Bars, surface: &'a Surface, theme: &'a Theme) -> Element<'a, Message> {
        bar::view(bars, surface, theme, Highlight::default())
    }
    let mut bars = busy_bars();
    let mut surface = Surface::new(
        window::Id::unique(),
        1,
        Some(String::from("Virtual-1")),
    );
    let messages = render(
        dir,
        "rest-natural",
        WIDE,
        view(&bars, &surface, theme),
    );
    surface.max_width = max_width(&messages);
    assert!(
        surface.max_width.is_some(),
        "the bar reports its maxWidth"
    );
    let start = Instant::now();
    assert!(sync(&mut surface, &mut bars, theme, start));
    assert!(
        !surface.anim.moving(),
        "a starting bar takes its targets at once"
    );
    render(
        dir,
        "rest-end",
        WIDE,
        view(&bars, &surface, theme),
    );
    assert_same(dir, "rest-natural", "rest-end");

    let mut narrow = Surface::new(
        window::Id::unique(),
        1,
        Some(String::from("Virtual-1")),
    );
    let messages = render(
        dir,
        "rest-narrow-natural",
        BAR,
        view(&bars, &narrow, theme),
    );
    narrow.max_width = max_width(&messages);
    let tabs = &bars
        .hypr
        .monitor("Virtual-1")
        .unwrap()
        .tabs;
    let title = deskshell_core::bar::title_max_width(
        narrow.max_width.unwrap_or_default(),
        tabs.tiled.len(),
        tabs.floating.len(),
    );
    assert!(title < 320.0, "the title is cut at {title}");
    sync(&mut narrow, &mut bars, theme, start);
    render(
        dir,
        "rest-narrow-end",
        BAR,
        view(&bars, &narrow, theme),
    );
    assert_same(dir, "rest-narrow-natural", "rest-narrow-end");

    let mut state = hypr_state();
    state.active_window = Some(Address(0xa3));
    bars.hypr
        .sync(&state, &Index::default());
    assert!(sync(&mut surface, &mut bars, theme, start));
    assert!(surface.anim.moving());
    surface.settle_anim(&bars, start + 70 * MS);
    render(
        dir,
        "focus-mid",
        WIDE,
        view(&bars, &surface, theme),
    );
    let end = run_out(&mut surface, &bars, start + 70 * MS);
    let span = end - start;
    assert!(
        span >= 280 * MS && span < 280 * MS + 2 * FRAME,
        "the pill ends a full duration after the chips: {span:?}"
    );
    render(
        dir,
        "focus-end",
        WIDE,
        view(&bars, &surface, theme),
    );
    let fresh = Surface::new(
        window::Id::unique(),
        1,
        Some(String::from("Virtual-1")),
    );
    render(
        dir,
        "focus-natural",
        WIDE,
        view(&bars, &fresh, theme),
    );
    assert_same(dir, "focus-natural", "focus-end");

    let start = end + Duration::from_secs(1);
    surface.hovered = Some(bar::Hover::Chip(0xa1));
    assert!(sync(&mut surface, &mut bars, theme, start));
    surface.settle_anim(&bars, start + 70 * MS);
    render(
        dir,
        "hover-mid",
        WIDE,
        view(&bars, &surface, theme),
    );
    let end = run_out(&mut surface, &bars, start + 70 * MS);
    assert!(end - start < 140 * MS + 2 * FRAME);
    surface.hovered = None;
    assert!(sync(&mut surface, &mut bars, theme, end));
    let end = run_out(&mut surface, &bars, end);

    let start = end + Duration::from_secs(1);
    bars.osd
        .show(&OsdEvent { kind: DeviceKind::Sink, volume: 0.64, muted: false });
    assert!(sync(&mut surface, &mut bars, theme, start));
    surface.settle_anim(&bars, start + 70 * MS);
    render(dir, "osd-mid", WIDE, view(&bars, &surface, theme));
    let end = run_out(&mut surface, &bars, start + 70 * MS);
    render(dir, "osd-end", WIDE, view(&bars, &surface, theme));
    render(
        dir,
        "osd-natural",
        WIDE,
        view(&bars, &fresh, theme),
    );
    assert_same(dir, "osd-natural", "osd-end");
    let start = end + Duration::from_secs(1);
    let generation = bars
        .osd
        .show(&OsdEvent { kind: DeviceKind::Sink, volume: 0.2, muted: false });
    assert!(bars.osd.hide(generation));
    assert!(sync(&mut surface, &mut bars, theme, start));
    surface.settle_anim(&bars, start + 70 * MS);
    render(
        dir,
        "osd-back-mid",
        WIDE,
        view(&bars, &surface, theme),
    );
    let end = run_out(&mut surface, &bars, start + 70 * MS);
    render(
        dir,
        "osd-back-end",
        WIDE,
        view(&bars, &surface, theme),
    );
    assert_same(dir, "focus-natural", "osd-back-end");

    let start = end + Duration::from_secs(1);
    state.active_workspace = Some(3);
    state.active_window = None;
    bars.hypr
        .sync(&state, &Index::default());
    assert!(sync(&mut surface, &mut bars, theme, start));
    surface.settle_anim(&bars, start + 70 * MS);
    render(
        dir,
        "workspace-mid",
        WIDE,
        view(&bars, &surface, theme),
    );
    let end = run_out(&mut surface, &bars, start + 70 * MS);
    render(
        dir,
        "workspace-end",
        WIDE,
        view(&bars, &surface, theme),
    );
    render(
        dir,
        "workspace-natural",
        WIDE,
        view(&bars, &fresh, theme),
    );
    assert_same(dir, "workspace-natural", "workspace-end");

    let start = end + Duration::from_secs(1);
    surface.anim.flash.restart(start);
    surface.settle_anim(&bars, start + 120 * MS);
    assert!((surface.anim.flash.value() - 1.0).abs() < 1e-6);
    render(
        dir,
        "flash-peak",
        WIDE,
        view(&bars, &surface, theme),
    );
    let end = run_out(&mut surface, &bars, start + 120 * MS);
    assert!(end - start >= 770 * MS && end - start < 770 * MS + FRAME);

    let start = Instant::now();
    let mut popup = bar::anim::Popup::new(0.3, start);
    popup.settle(start + 70 * MS);
    assert!(popup.opacity() > 0.5 && popup.opacity() < 1.0);
    render(
        dir,
        "popup-mid",
        (
            bar::osd::POPUP_WIDTH,
            bar::osd::POPUP_HEIGHT as f32,
        ),
        bar::tabs::popup(&bars.osd, theme, popup.opacity(), popup.level()),
    );
}

fn workspace_ref(id: i64) -> WorkspaceRef {
    WorkspaceRef { id: Some(id), name: id.to_string(), ..WorkspaceRef::default() }
}

fn window(address: u64, x: i32, width: i32, class: &str, title: &str) -> Window {
    Window {
        address: Address(address),
        at: [x, 40],
        size: [width, 640],
        workspace: workspace_ref(2),
        class: class.to_owned(),
        title: title.to_owned(),
        ..Window::default()
    }
}

fn hypr_state() -> State {
    let mut float = window(
        0xf1,
        300,
        500,
        "org.example.Calculator",
        "Calculator",
    );
    float.floating = true;
    let mut float2 = window(0xf2, 360, 500, "foot", "notes");
    float2.floating = true;
    State {
        connected: true,
        monitors: vec![deskshell_hypr::Monitor {
            name: String::from("Virtual-1"),
            x: 0,
            width: 1280,
            height: 720,
            scale: 1.0,
            focused: true,
            active_workspace: Some(workspace_ref(2)),
            ..deskshell_hypr::Monitor::default()
        }],
        workspaces: [1, 2, 3, 6]
            .into_iter()
            .map(|id| Workspace { id: Some(id), name: id.to_string(), ..Workspace::default() })
            .collect(),
        windows: vec![
            window(0xa1, -620, 600, "kitty", "build"),
            window(0xa2, 0, 640, "foot", "~/src/deskshell - Foot"),
            window(
                0xa3,
                660,
                600,
                "org.example.Viewer",
                "แผนที่ - Org.example.viewer",
            ),
            window(0xa4, 1290, 600, "vesktop", "Chat"),
            float,
            float2,
        ],
        active_workspace: Some(2),
        focused_monitor: Some(String::from("Virtual-1")),
        active_window: Some(Address(0xa2)),
        keyboard_layout: String::from("Thai"),
    }
}

fn square(side: u32, rgba: [u8; 4]) -> Handle {
    let mut pixels = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            let inside = (x as i32 * 2 - side as i32).pow(2) + (y as i32 * 2 - side as i32).pow(2)
                < (side as i32 - 2).pow(2);
            pixels.extend_from_slice(if inside { &rgba } else { &[0, 0, 0, 0] });
        }
    }
    Handle::from_rgba(side, side, pixels)
}

fn tray_item(key: &str, rgba: [u8; 4], status: TrayStatus) -> Arc<Item> {
    let side = 32;
    let mut pixels = Vec::with_capacity(side * side * 4);
    for y in 0..side {
        for x in 0..side {
            let ring = (x as i32 - 16).pow(2) + (y as i32 - 16).pow(2);
            pixels.extend_from_slice(if ring < 225 { &rgba } else { &[0, 0, 0, 0] });
        }
    }
    Arc::new(Item {
        key: key.to_owned(),
        id: key.to_owned(),
        title: String::new(),
        status,
        category: Category::ApplicationStatus,
        icon: Icon::Pixmaps(Arc::from(vec![Pixmap {
            width: 32,
            height: 32,
            rgba: Arc::from(pixels),
        }])),
        attention: Icon::None,
        overlay: Icon::None,
        tooltip: Tooltip::default(),
        item_is_menu: false,
        menu: None,
    })
}

fn busy_bars() -> Bars {
    let mut bars = Bars::default();
    let state = hypr_state();
    bars.hypr
        .sync(&state, &Index::default());
    let colors = [
        [0x6e, 0xc8, 0x8a, 255],
        [0xd9, 0x8b, 0x3d, 255],
        [0x3d, 0x8b, 0xd9, 255],
        [0xc8, 0x6e, 0xb4, 255],
    ];
    let names = bars.icon_loads();
    assert!(names.is_empty(), "no icon theme, nothing to load");
    for (n, name) in ["kitty", "foot", "org.example.Viewer", "vesktop", "org.example.Calculator"]
        .into_iter()
        .enumerate()
    {
        bars.icons.loaded(
            bars.icons.generation(),
            name.to_owned(),
            Some(square(36, colors[n % 4])),
        );
    }
    bars.tray.sync(&[
        tray_item(":1.4/chat", colors[2], TrayStatus::Active),
        tray_item(":1.5/sync", colors[0], TrayStatus::Passive),
        tray_item(":1.6/vpn", colors[1], TrayStatus::NeedsAttention),
    ]);
    bars.media.active = true;
    bars.media.art = Some(square(56, [0xe8, 0xa8, 0x38, 255]));
    bars.media.levels = [0.1, 0.8, 0.55, 0.3, 0.9, 0.0, 0.42, 0.01];
    let mut net = deskshell_net::State::default();
    net.network.wifi_enabled = true;
    net.network.wifi_hardware_enabled = true;
    bars.status.sync_net(&net);
    bars.status.sync_audio(&Audio {
        connected: true,
        sink: Some(Device { volume: 0.7, ..Device::default() }),
        source: Some(Device { muted: true, ..Device::default() }),
        ..Audio::default()
    });
    bars.status.sync_notify(true, 4);
    bars.keep_awake = true;
    let install = deskshell_system::install::State {
        run_id: String::from("1"),
        unit_active: true,
        job: deskshell_system::install::Job {
            id: Some(String::from("1")),
            phase: Some(deskshell_system::install::Phase::Running),
            step: String::from("install"),
            done: Some(5),
            total: Some(12),
            ..deskshell_system::install::Job::default()
        },
        ..deskshell_system::install::State::default()
    };
    let mut view = deskshell::updates::View::default();
    view.write(&install, true);
    bars.update.set(&view);
    let now = "2026-11-13T09:41:00+07:00[Asia/Bangkok]"
        .parse()
        .unwrap();
    bars.set_time(&now);
    let monitor = bars.hypr.monitor("Virtual-1").unwrap();
    assert!(
        monitor.tabs.show_bracket(),
        "kitty and vesktop lie off screen"
    );
    assert!(monitor.tabs.show_stack());
    bars
}

fn entry(id: i32, label: &str) -> MenuEntry {
    MenuEntry { id, label: label.to_owned(), ..MenuEntry::default() }
}

fn png() -> Vec<u8> {
    let square = image::RgbaImage::from_pixel(16, 16, image::Rgba([0xd9, 0x8b, 0x3d, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    square
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

fn fake_menu() -> Arc<Menu> {
    let mut open = entry(1, "Open window");
    open.icon_name = String::from("window-new");
    let mut mute = entry(2, "Mute sounds");
    mute.toggle = Toggle::Checkmark;
    mute.toggle_state = ToggleState::On;
    mute.icon_data = Some(Arc::from(png()));
    let mut status = entry(3, "สถานะ Status");
    status.submenu = true;
    let mut online = entry(31, "Online");
    online.toggle = Toggle::Radio;
    online.toggle_state = ToggleState::On;
    let mut away = entry(32, "Away");
    away.toggle = Toggle::Radio;
    let mut busy = entry(33, "Do not disturb");
    busy.toggle = Toggle::Radio;
    busy.enabled = false;
    status.children = vec![online, away, busy];
    let mut updates = entry(4, "Check for updates");
    updates.enabled = false;
    let mut quit = entry(7, "Quit");
    quit.shortcut = vec![vec![String::from("Control"), String::from("q")]];
    Arc::new(Menu {
        item: String::from(":1.4/chat"),
        icon_theme_path: Vec::new(),
        root: MenuEntry {
            id: 0,
            children: vec![
                open,
                mute,
                status,
                updates,
                MenuEntry { id: 6, separator: true, ..MenuEntry::default() },
                quit,
            ],
            ..MenuEntry::default()
        },
    })
}

fn idle_bars() -> Bars {
    let mut bars = Bars::default();
    let mut state = hypr_state();
    state.windows.clear();
    state.active_window = None;
    bars.hypr
        .sync(&state, &Index::default());
    let now = "2026-11-13T23:05:00+07:00[Asia/Bangkok]"
        .parse()
        .unwrap();
    bars.set_time(&now);
    bars
}

fn render<'a>(
    dir: &Path,
    name: &str,
    size: (f32, f32),
    element: Element<'a, Message>,
) -> Vec<Message> {
    let element = container(element)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(Background::Color(iced::Color::from_rgb8(
                0x3a, 0x4a, 0x6b,
            ))),
            ..container::Style::default()
        });
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
    simulator.into_messages().collect()
}
