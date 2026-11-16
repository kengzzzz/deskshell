//! Offscreen renders of the updates popover, pill, Hub row, list, ring and auth sheet.

mod hub_common;

use std::borrow::Cow;
use std::env;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use deskshell::auth::{self, Auth, SheetKind, Surface as AuthSurface};
use deskshell::bar::{self, Bars, Highlight, Surface};
use deskshell::panel::Sources;
use deskshell::panel::hub::Hub;
use deskshell::panel::updates::Updates;
use deskshell::updates::View;
use deskshell::updates::list::List;
use deskshell_system::install::{self, Job, PamStage, Phase};
use deskshell_system::{Package, Updates as Pending, is_reboot_package};
use deskshell_ui::{Theme, fonts};
use iced::advanced::clipboard;
use iced::advanced::graphics::text::font_system;
use iced::advanced::mouse;
use iced::advanced::renderer::{self, Headless as _};
use iced::widget::container;
use iced::window::{self, RedrawRequest};
use iced::{Background, Color, Element, Event, Length, Padding, Pixels, Settings, Size};
use iced_test::Simulator;
use iced_test::futures::futures::executor::block_on;
use iced_test::runtime::user_interface::{self, UserInterface};

const WALLPAPER: Color = Color::from_rgb8(0x3a, 0x4a, 0x6b);
const MARGIN: f32 = 20.0;

fn main() {
    // SAFETY: this test has no harness, so no other thread exists yet.
    unsafe {
        for key in ["WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY"] {
            env::remove_var(key);
        }
        env::set_var("ICED_TEST_BACKEND", "tiny-skia");
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("offscreen/updates");
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

    let launch = install::State {
        run_id: "1".into(),
        unit_active: true,
        launching: true,
        ..install::State::default()
    };
    let mut pin = job(Phase::Auth, "", 0, 12);
    pin.pam_stage = PamStage::Pin;
    pin.auth_attempt = Some(1);
    let mut touch = pin.clone();
    touch.pam_stage = PamStage::Touch;
    touch.submitted_attempt = Some(1);
    let mut sync = job(Phase::Running, "sync", 0, 12);
    sync.furthest_step = 0;
    let mut installing = job(Phase::Running, "install", 5, 12);
    installing.job.current = "mesa".into();
    installing.furthest_step = 2;
    let mut build = job(Phase::Running, "build", 11, 12);
    build.job.current = "paru-bin".into();
    build.furthest_step = 4;

    let popovers = [
        ("popover-launch", &launch),
        ("popover-pin", &pin),
        ("popover-touch", &touch),
        ("popover-sync", &sync),
        ("popover-install", &installing),
        ("popover-build", &build),
    ];
    for (name, state) in popovers {
        let view = view_of(state);
        let mut env = sources.env(&theme);
        env.updates = Some(&view);
        let panel = Updates;
        render(
            &dir,
            name,
            Size::new(580.0, 206.0),
            panel.view(&env).map(|_| ()),
        );
    }
    let install_view = view_of(&installing);
    assert_eq!(install_view.subtitle, "Upgrading mesa · 5 of 12");
    assert_eq!(install_view.percent, "42");

    for (name, state) in [("bar-auth", &pin), ("bar-install", &installing)] {
        let mut bars = Bars::default();
        bars.update.set(&view_of(state));
        let now = "2026-11-13T09:41:00+07:00[Asia/Bangkok]"
            .parse()
            .unwrap();
        bars.set_time(&now);
        let surface = Surface::new(window::Id::unique(), 1, None);
        render(
            &dir,
            name,
            Size::new(1260.0, 48.0),
            bar::view(&bars, &surface, &theme, Highlight::default()).map(|_| ()),
        );
    }

    let mut download = job(Phase::Running, "download", 0, 12);
    download.job.download = Some(64);
    download.furthest_step = 1;
    let download = view_of(&download);
    assert_eq!(download.hub_text, "Updating · Downloading · 64%");
    let full = hub_common::full();
    let mut env = sources.env(&theme);
    env.updates = Some(&download);
    let mut hub = Hub::default();
    let _ = hub.open(&env, full.inputs(), &hub_common::now());
    hub.load_now(&theme);
    let _ = hub.drain_effects().count();
    render(
        &dir,
        "hub-install",
        Size::new(1560.0, 640.0),
        hub.view(&env, full.inputs())
            .map(|_| ()),
    );
    crop(
        &dir.join("hub-install.png"),
        &dir.join("hub-install-row.png"),
        1560.0,
        (500, 530, 640, 110),
    );

    let mut list = List::default();
    let checked = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    list.sync(
        &pending(checked),
        checked + Duration::from_secs(720),
    );
    render(
        &dir,
        "list",
        Size::new(620.0, 320.0),
        list.view(&theme, ()),
    );
    let failed = Pending {
        error: Some(String::from(
            "no server of extra answered: Connection refused (os error 111)",
        )),
        ..pending(checked)
    };
    list.sync(&failed, checked + Duration::from_secs(7200));
    render(
        &dir,
        "list-error",
        Size::new(620.0, 340.0),
        list.view(&theme, ()),
    );

    rings(&dir, &theme);
    sheets(&dir, &theme, checked);
}

fn job(phase: Phase, step: &str, done: i64, total: i64) -> install::State {
    install::State {
        run_id: "1".into(),
        unit_active: true,
        job: Job {
            id: Some("1".into()),
            phase: Some(phase),
            step: step.into(),
            done: Some(done),
            total: Some(total),
            download: Some(-1),
            ..Job::default()
        },
        runner_can_stop: phase == Phase::Auth,
        ..install::State::default()
    }
}

fn view_of(state: &install::State) -> View {
    let mut view = View::default();
    view.write(state, true);
    view
}

fn package(name: &str, old: &str, new: &str, aur: bool) -> Package {
    Package {
        name: name.into(),
        old: old.into(),
        new: new.into(),
        aur,
        reboot: is_reboot_package(name),
    }
}

fn pending(checked: SystemTime) -> Pending {
    Pending {
        ready: true,
        total: 7,
        official: 5,
        aur: 2,
        packages: vec![
            package("linux", "6.17.8.arch1-1", "6.17.9.arch1-1", false),
            package("mesa", "1:25.2.6-1", "1:25.3.0-1", false),
            package("firefox", "145.0-1", "145.0.1-1", false),
            package("python", "3.13.7-1", "3.13.7-2", false),
            package(
                "a-package-with-a-very-long-name-that-is-cut",
                "1.0-1",
                "1.1-1",
                false,
            ),
            package("paru", "2.1.0-1", "2.1.0-2", true),
            package(
                "visual-studio-code-bin",
                "1.105.0-1",
                "1.106.2-1",
                true,
            ),
        ],
        checked_at: Some(checked),
        ..Pending::default()
    }
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

fn render(dir: &Path, name: &str, size: Size, element: Element<'_, ()>) {
    let placed = container(
        container(element)
            .width(size.width)
            .height(size.height),
    )
    .padding(Padding::new(MARGIN))
    .width(Length::Fill)
    .height(Length::Fill)
    .style(|_| container::Style {
        background: Some(Background::Color(WALLPAPER)),
        ..container::Style::default()
    });
    let full = Size::new(
        size.width + 2.0 * MARGIN,
        size.height + 2.0 * MARGIN,
    );
    let mut simulator = Simulator::with_size(settings(), full, placed);
    let snapshot = simulator
        .snapshot(&iced::Theme::Dark)
        .expect("snapshot");
    assert!(
        snapshot
            .matches_image(dir.join(name))
            .expect("write PNG")
    );
    let rendered = dir.join(format!("{name}-tiny-skia.png"));
    fs::rename(&rendered, dir.join(format!("{name}.png"))).expect("rename the render");
}

fn crop(from: &Path, to: &Path, logical_width: f32, (x, y, width, height): (u32, u32, u32, u32)) {
    let image = image::open(from)
        .expect("read the render")
        .to_rgba8();
    let margin = MARGIN as u32;
    let scale = image.width() / (logical_width + 2.0 * MARGIN) as u32;
    image::imageops::crop_imm(
        &image,
        (x + margin) * scale,
        (y + margin) * scale,
        width * scale,
        height * scale,
    )
    .to_image()
    .save(to)
    .expect("write a crop");
}

fn rings(dir: &Path, theme: &Theme) {
    const SCALE: f32 = 8.0;
    let mut renderer = block_on(iced::Renderer::new(
        fonts::PLEX,
        Pixels(16.0),
        Some("tiny-skia"),
    ))
    .expect("a tiny-skia renderer");
    let colors = &theme.colors;
    let (track, arc) = (colors.container_highest, colors.accent);
    let size = Size::new(bar::update::RING + 8.0, bar::update::RING + 8.0);
    let mut cache = user_interface::Cache::new();
    let start = Instant::now();
    let mut frame = |name: &str, progress: f32, indeterminate: bool, at: Duration| {
        let view: Element<'_, ()> = container(bar::update::ring(
            progress,
            indeterminate,
            true,
            false,
            track,
            arc,
        ))
        .center(Length::Fill)
        .style(|_| container::Style {
            background: Some(Background::Color(WALLPAPER)),
            ..container::Style::default()
        })
        .into();
        let mut ui = UserInterface::build(
            view,
            size,
            std::mem::replace(&mut cache, user_interface::Cache::new()),
            &mut renderer,
        );
        let (state, _) = ui.update(
            &[Event::Window(window::Event::RedrawRequested(start + at))],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut clipboard::Null,
            &mut Vec::new(),
        );
        let redraw = match state {
            user_interface::State::Updated { redraw_request, .. } => redraw_request,
            user_interface::State::Outdated => RedrawRequest::NextFrame,
        };
        ui.draw(
            &mut renderer,
            &iced::Theme::Dark,
            &renderer::Style { text_color: Color::WHITE },
            mouse::Cursor::Unavailable,
        );
        let (width, height) = (
            (size.width * SCALE) as u32,
            (size.height * SCALE) as u32,
        );
        let rgba = renderer.screenshot(Size::new(width, height), SCALE, WALLPAPER);
        image::save_buffer(
            dir.join(format!("{name}.png")),
            &rgba,
            width,
            height,
            image::ColorType::Rgba8,
        )
        .expect("write PNG");
        cache = ui.into_cache();
        redraw
    };
    assert_eq!(
        frame("ring-0ms", 0.0, true, Duration::ZERO),
        RedrawRequest::NextFrame
    );
    frame(
        "ring-275ms",
        0.0,
        true,
        Duration::from_millis(275),
    );
    frame(
        "ring-550ms",
        0.0,
        true,
        Duration::from_millis(550),
    );
    assert_eq!(
        frame(
            "ring-sweep-0ms",
            0.75,
            false,
            Duration::from_millis(600)
        ),
        RedrawRequest::NextFrame
    );
    frame(
        "ring-sweep-100ms",
        0.75,
        false,
        Duration::from_millis(700),
    );
    assert_eq!(
        frame(
            "ring-sweep-end",
            0.75,
            false,
            Duration::from_millis(900)
        ),
        RedrawRequest::Wait
    );
}

fn sheets(dir: &Path, theme: &Theme, checked: SystemTime) {
    let mut renderer = block_on(iced::Renderer::new(
        fonts::PLEX,
        Pixels(16.0),
        Some("tiny-skia"),
    ))
    .expect("a tiny-skia renderer");
    let mut pin = job(Phase::Auth, "", 0, 7);
    pin.pam_stage = PamStage::Pin;
    pin.auth_attempt = Some(1);
    let mut failed = pin.clone();
    failed.auth_attempt = Some(2);
    let queued = install::State { queued: true, ..job(Phase::Auth, "", 0, 7) };
    let mut touch = pin.clone();
    touch.pam_stage = PamStage::Touch;
    touch.submitted_attempt = Some(1);
    for (name, state) in [
        ("sheet-pin", &pin),
        ("sheet-failed", &failed),
        ("sheet-queued", &queued),
        ("sheet-touch", &touch),
    ] {
        let mut auth = Auth::default();
        auth.updates.set_job(state);
        auth.updates.set_updates(
            &pending(checked),
            checked + Duration::from_secs(720),
        );
        let _ = auth.event(auth::Event::Updates, &mut Vec::new());
        assert!(
            auth.visible(AuthSurface::Sheet(SheetKind::Updates)),
            "{name}"
        );
        render_sheet(dir, name, &mut auth, theme, &mut renderer);
    }
}

fn render_sheet(
    dir: &Path,
    name: &str,
    auth: &mut Auth,
    theme: &Theme,
    renderer: &mut iced::Renderer,
) {
    const SCALE: f32 = 2.0;
    let surface = AuthSurface::Sheet(SheetKind::Updates);
    let width = surface.width() as f32;
    let mut cache = user_interface::Cache::new();
    let now = Instant::now();
    let mut passes = 0;
    let (mut ui, size) = loop {
        passes += 1;
        assert!(passes < 6, "{name}: the height never settles");
        let height = auth.height(surface) as f32;
        let size = Size::new(width + 2.0 * MARGIN, height + 2.0 * MARGIN);
        let view = container(
            container(auth.view(surface, theme))
                .width(width)
                .height(height),
        )
        .padding(MARGIN)
        .style(|_| container::Style {
            background: Some(Background::Color(WALLPAPER)),
            ..container::Style::default()
        });
        let mut ui = UserInterface::build(view, size, cache, renderer);
        let mut messages = Vec::new();
        let _ = ui.update(
            &[Event::Window(window::Event::RedrawRequested(now))],
            mouse::Cursor::Unavailable,
            renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        if messages.is_empty() {
            break (ui, size);
        }
        cache = ui.into_cache();
        for message in messages {
            auth.message(surface, message, &mut Vec::new());
        }
    };
    ui.draw(
        renderer,
        &iced::Theme::Dark,
        &renderer::Style { text_color: theme.colors.text_primary },
        mouse::Cursor::Unavailable,
    );
    let pixels = Size::new(
        (size.width * SCALE).round() as u32,
        (size.height * SCALE).round() as u32,
    );
    let rgba = renderer.screenshot(pixels, SCALE, WALLPAPER);
    image::save_buffer(
        dir.join(format!("{name}.png")),
        &rgba,
        pixels.width,
        pixels.height,
        image::ColorType::Rgba8,
    )
    .expect("write PNG");
}
