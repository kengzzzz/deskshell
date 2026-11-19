//! `deskshell-thumbs`: a live thumbnail of every window and output. Runs only in a `dev/vm` guest.
//!
//! ```sh
//! cargo build --release -p deskshell-wl --example thumbs
//! slot=$(dev/vm/vm up --share "$PWD/target/release/examples")
//! dev/vm/vm ssh "$slot" 'as-tester setsid -f /work/thumbs > /tmp/thumbs.log 2>&1 < /dev/null'
//! ```
//!
//! Options: `--shm`, `--headless-dmabuf [DIR]` (PNGs, no UI), `--poison SECONDS`,
//! `--stop-after SECONDS`, `--stop-submitting SECONDS`, `--max-fps N`, `--log-every SECONDS`.
//! `THUMBS_LOG=debug` logs more.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::ExitCode;
use std::sync::OnceLock;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use deskshell_wl::{Capture, Event, Gpu, Mode, Target, TargetId};
use iced::futures::Stream;
use iced::widget::{column, container, row, text};
use iced::{Color, Element, Length, Subscription, Task, Theme, window};
use iced_exwlshell::daemon;
use iced_exwlshell::redraw::Scope;
use iced_exwlshell::reexport::{
    Anchor, KeyboardInteractivity, Layer, LayerSize, NewLayerShellSettings,
};
use iced_exwlshell::settings::{ExWlSettings, LayerShellSettings, StartMode};
use iced_exwlshell::to_exwlshell_message;
use tracing::{info, warn};

const GUEST_HOSTNAME: &str = "deskshell-vm";
const PANEL_HEIGHT: u32 = 300;

#[derive(Debug, Clone)]
struct Opts {
    shm: bool,
    headless: Option<PathBuf>,
    poison: Option<Duration>,
    stop_after: Option<Duration>,
    stop_submitting: Option<Duration>,
    max_fps: u32,
    log_every: Duration,
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Opts, String> {
    let mut opts = Opts {
        shm: false,
        headless: None,
        poison: None,
        stop_after: None,
        stop_submitting: None,
        max_fps: 30,
        log_every: Duration::from_secs(2),
    };
    let mut args = args.by_ref().peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--shm" => opts.shm = true,
            "--live" => {}
            "--headless-dmabuf" => {
                let dir = match args.peek() {
                    Some(next) if !next.starts_with("--") => args.next().unwrap_or_default(),
                    _ => String::from("/tmp/thumbs"),
                };
                opts.headless = Some(PathBuf::from(dir));
            }
            "--poison" => opts.poison = Some(Duration::from_secs(number(&arg, args.next())?)),
            "--stop-after" => {
                opts.stop_after = Some(Duration::from_secs(number(&arg, args.next())?));
            }
            "--stop-submitting" => {
                opts.stop_submitting = Some(Duration::from_secs(number(&arg, args.next())?));
            }
            "--max-fps" => opts.max_fps = number(&arg, args.next())?,
            "--log-every" => opts.log_every = Duration::from_secs(number(&arg, args.next())?),
            _ => return Err(format!("unknown argument {arg}")),
        }
    }
    Ok(opts)
}

fn number<T: std::str::FromStr>(flag: &str, value: Option<String>) -> Result<T, String> {
    value
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| format!("{flag} needs a number"))
}

fn main() -> ExitCode {
    // `--live` runs on the real session, only with the maintainer present.
    let live = std::env::args()
        .skip(1)
        .any(|arg| arg == "--live");
    let hostname = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
    if hostname.trim() != GUEST_HOSTNAME && !live {
        eprintln!("thumbs captures the screen and only runs in a dev/vm guest (or with --live)");
        return ExitCode::from(2);
    }
    let opts = match parse(std::env::args().skip(1)) {
        Ok(opts) => opts,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    for (key, value) in [("WGPU_BACKEND", "vulkan"), ("ICED_BACKEND", "wgpu")] {
        if std::env::var_os(key).is_none() {
            // SAFETY: no other thread exists yet.
            unsafe { std::env::set_var(key, value) };
        }
    }
    log::install();
    match &opts.headless {
        Some(dir) => headless(&opts, dir),
        None => panel(opts),
    }
}

fn hyprland(request: &str, key: &str) -> Vec<String> {
    let (Some(runtime), Some(signature)) = (
        std::env::var_os("XDG_RUNTIME_DIR"),
        std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE"),
    ) else {
        return Vec::new();
    };
    let socket = PathBuf::from(runtime)
        .join("hypr")
        .join(signature)
        .join(".socket.sock");
    let mut reply = String::new();
    let result = UnixStream::connect(&socket)
        .and_then(|mut stream| {
            stream
                .write_all(request.as_bytes())
                .map(|()| stream)
        })
        .and_then(|mut stream| stream.read_to_string(&mut reply));
    if let Err(error) = result {
        warn!(%error, request, "hyprland ipc");
        return Vec::new();
    }
    top_level_values(&reply, key)
}

fn top_level_values(json: &str, key: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut depth = 0usize;
    let mut chars = json.char_indices().peekable();
    let mut last_key: Option<String> = None;
    let mut expect_value = false;
    while let Some((_, c)) = chars.next() {
        match c {
            '{' | '[' => {
                depth += 1;
                expect_value = false;
            }
            '}' | ']' => depth = depth.saturating_sub(1),
            ':' => expect_value = true,
            ',' => expect_value = false,
            '"' => {
                let mut string = String::new();
                while let Some((_, c)) = chars.next() {
                    match c {
                        '\\' => {
                            if let Some((_, escaped)) = chars.next() {
                                string.push(escaped);
                            }
                        }
                        '"' => break,
                        other => string.push(other),
                    }
                }
                if expect_value {
                    if depth == 2 && last_key.as_deref() == Some(key) {
                        values.push(string);
                    }
                    expect_value = false;
                } else {
                    last_key = Some(string);
                }
            }
            _ => {}
        }
    }
    values
}

fn buffer_fds() -> (usize, usize) {
    let Ok(entries) = std::fs::read_dir("/proc/self/fd") else {
        return (0, 0);
    };
    let mut counts = (0, 0);
    for entry in entries.flatten() {
        let Ok(target) = std::fs::read_link(entry.path()) else {
            continue;
        };
        let target = target.to_string_lossy();
        if target.starts_with("/dmabuf:") {
            counts.0 += 1;
        } else if target.starts_with("/memfd:deskshell-capture") {
            counts.1 += 1;
        }
    }
    counts
}

fn windows() -> Vec<u64> {
    hyprland("j/clients", "address")
        .iter()
        .filter_map(|address| u64::from_str_radix(address.trim_start_matches("0x"), 16).ok())
        .collect()
}

fn outputs() -> Vec<String> {
    hyprland("j/monitors", "name")
}

fn headless(opts: &Opts, dir: &std::path::Path) -> ExitCode {
    let gpu = match Gpu::open_headless() {
        Ok(gpu) => gpu,
        Err(error) => {
            warn!(error, "no headless Vulkan device");
            return ExitCode::FAILURE;
        }
    };
    info!(adapter = ?gpu.adapter_name(), dmabuf = gpu.dmabuf_possible(), reason = ?gpu.dmabuf_unavailable(), "headless device");
    if let Err(error) = std::fs::create_dir_all(dir) {
        warn!(%error, "output directory");
        return ExitCode::FAILURE;
    }
    let (capture, mut events) = Capture::spawn(gpu);
    let mut targets: Vec<(u64, TargetId, Option<Mode>, bool)> = windows()
        .into_iter()
        .map(|address| {
            (
                address,
                capture.start(Target::Window(address), opts.max_fps),
                None,
                false,
            )
        })
        .collect();
    info!(windows = targets.len(), "capturing");
    let deadline = Instant::now() + Duration::from_secs(15);
    let submit_until = opts
        .stop_submitting
        .map(|after| Instant::now() + after);
    let mut context = Context::from_waker(Waker::noop());
    while Instant::now() < deadline && targets.iter().any(|target| !target.3) {
        match Pin::new(&mut events).poll_next(&mut context) {
            Poll::Ready(Some(Event::Mode(id, mode))) => {
                if let Some(target) = targets
                    .iter_mut()
                    .find(|target| target.1 == id)
                {
                    target.2 = Some(mode);
                }
            }
            Poll::Ready(Some(Event::Frame(id))) => {
                let Some(target) = targets
                    .iter_mut()
                    .find(|target| target.1 == id && !target.3)
                else {
                    continue;
                };
                let Some(image) = capture.read_frame(id) else {
                    continue;
                };
                let path = dir.join(format!("window-{:x}.png", target.0));
                match std::fs::write(&path, png::encode(&image)) {
                    Ok(()) => info!(
                        window = format!("{:#x}", target.0),
                        mode = ?target.2,
                        width = image.width,
                        height = image.height,
                        path = %path.display(),
                        "frame written"
                    ),
                    Err(error) => warn!(%error, "png"),
                }
                target.3 = true;
            }
            Poll::Ready(Some(Event::Gone(id))) => {
                warn!(
                    id = id.get(),
                    "window gone before its first frame"
                );
                targets.retain(|target| target.1 != id);
            }
            Poll::Ready(Some(Event::Submit)) => {
                if submit_until.is_none_or(|until| Instant::now() < until) {
                    capture.submit();
                }
            }
            Poll::Ready(Some(other)) => info!(?other, "event"),
            Poll::Ready(None) => break,
            Poll::Pending => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    let written = targets
        .iter()
        .filter(|target| target.3)
        .count();
    let dmabuf = targets
        .iter()
        .filter(|target| target.3 && target.2 == Some(Mode::Dmabuf))
        .count();
    info!(
        written,
        dmabuf,
        total = targets.len(),
        "headless dmabuf done"
    );
    if written == targets.len() && dmabuf == written && written > 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

static OPTS: OnceLock<Opts> = OnceLock::new();

fn panel(opts: Opts) -> ExitCode {
    let hooked = !opts.shm && deskshell_wl::install_device_hook();
    info!(hooked, "deskshell-thumbs");
    let _ = OPTS.set(opts);
    let result = daemon(
        Thumbs::new,
        "deskshell-thumbs",
        Thumbs::update,
        Thumbs::view,
    )
    .wl_settings(ExWlSettings {
        layer_settings: LayerShellSettings {
            start_mode: StartMode::Background,
            ..LayerShellSettings::default()
        },
        ..ExWlSettings::default()
    })
    .subscription(Thumbs::subscription)
    .style(|_: &Thumbs, theme: &Theme| iced::theme::Style {
        background_color: Color::TRANSPARENT,
        text_color: theme.palette().text,
    })
    .theme(|_: &Thumbs, _| Theme::Dark)
    .redraw_scope(redraw_scope)
    .run();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            warn!(%error, "event loop failed");
            ExitCode::FAILURE
        }
    }
}

#[to_exwlshell_message]
#[derive(Debug, Clone)]
enum Message {
    CheckGpu,
    Capture(Event),
    Stats,
    Poison,
    StopAll,
    Redraw,
}

fn redraw_scope(message: &Message) -> Scope {
    match message {
        // A redraw would bring a new capture frame and another submit.
        Message::Capture(Event::Submit) => Scope::None,
        Message::Capture(_) | Message::StopAll | Message::Redraw => Scope::All,
        _ => Scope::None,
    }
}

struct Shown {
    id: TargetId,
    label: String,
    frames: u64,
    gone: bool,
}

struct Thumbs {
    capture: Option<Capture>,
    shown: Vec<Shown>,
    stats_at: Instant,
    rendered: u64,
    poisoned: bool,
    stopped: bool,
    redraws: u32,
    lost_at: Option<Instant>,
    labels: String,
    submit_until: Option<Instant>,
}

impl Thumbs {
    fn new() -> (Self, Task<Message>) {
        let thumbs = Self {
            capture: None,
            shown: Vec::new(),
            stats_at: Instant::now(),
            rendered: 0,
            poisoned: false,
            stopped: false,
            redraws: 0,
            lost_at: None,
            labels: String::new(),
            submit_until: None,
        };
        let settings = NewLayerShellSettings {
            size: LayerSize::fill_width(PANEL_HEIGHT),
            layer: Layer::Top,
            anchor: Anchor::Bottom | Anchor::Left | Anchor::Right,
            exclusive_zone: Some(PANEL_HEIGHT as i32),
            keyboard_interactivity: KeyboardInteractivity::None,
            namespace: Some(String::from("deskshell-thumbs")),
            ..NewLayerShellSettings::default()
        };
        (
            thumbs,
            Task::done(Message::NewLayerShell { settings, id: window::Id::unique() }),
        )
    }

    fn opts() -> &'static Opts {
        OPTS.get()
            .expect("set before the daemon runs")
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "iced passes messages by value"
    )]
    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::CheckGpu => self.check_gpu(),
            Message::Capture(event) => {
                self.event(event);
                Task::none()
            }
            Message::Stats => {
                self.stats();
                Task::none()
            }
            Message::Poison => {
                if !self.poisoned
                    && let Some(capture) = &self.capture
                {
                    self.poisoned = true;
                    warn!("sending a protocol error on the capture connection");
                    capture.poison();
                }
                Task::none()
            }
            Message::StopAll => {
                if !self.stopped
                    && let Some(capture) = &self.capture
                {
                    self.stopped = true;
                    for shown in &mut self.shown {
                        if !shown.gone {
                            capture.stop(shown.id);
                            shown.gone = true;
                        }
                    }
                    info!("stopped every target");
                }
                Task::none()
            }
            Message::Redraw => {
                self.redraws += 1;
                Task::none()
            }
            _ => Task::none(),
        }
    }

    fn check_gpu(&mut self) -> Task<Message> {
        if self.capture.is_some() {
            return Task::none();
        }
        let Some(gpu) = Gpu::from_iced() else {
            return Task::none();
        };
        info!(
            hook_devices = deskshell_wl::hook_devices(),
            extension = gpu.dmabuf_possible(),
            reason = ?gpu.dmabuf_unavailable(),
            adapter = ?gpu.adapter_name(),
            devices = deskshell_wl::device_count(),
            "iced's device"
        );
        let (capture, events) = Capture::spawn(gpu);
        let max_fps = Self::opts().max_fps;
        for address in windows() {
            let id = capture.start(Target::Window(address), max_fps);
            self.shown.push(Shown {
                id,
                label: format!("window {address:x}"),
                frames: 0,
                gone: false,
            });
        }
        for name in outputs() {
            let id = capture.start(Target::Output(name.clone()), max_fps);
            self.shown
                .push(Shown { id, label: format!("output {name}"), frames: 0, gone: false });
        }
        info!(targets = self.shown.len(), "capture started");
        self.submit_until = Self::opts()
            .stop_submitting
            .map(|after| Instant::now() + after);
        self.capture = Some(capture);
        self.stats_at = Instant::now();
        self.rendered = deskshell_wl::rendered_frames();
        Task::run(events, Message::Capture)
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Frame(_) => {}
            Event::Submit => {
                if let Some(capture) = &self.capture
                    && self
                        .submit_until
                        .is_none_or(|until| Instant::now() < until)
                {
                    capture.submit();
                }
            }
            Event::Gone(id) => {
                if let Some(shown) = self
                    .shown
                    .iter_mut()
                    .find(|shown| shown.id == id)
                {
                    shown.gone = true;
                    info!(target = shown.label, "gone");
                }
            }
            Event::Mode(id, mode) => {
                if let Some(shown) = self
                    .shown
                    .iter()
                    .find(|shown| shown.id == id)
                {
                    info!(target = shown.label, ?mode, "mode");
                }
            }
            Event::ConnectionLost => {
                self.lost_at = Some(Instant::now());
                warn!("capture connection lost");
            }
            Event::ConnectionRestored => {
                let after = self
                    .lost_at
                    .take()
                    .map(|at| at.elapsed().as_millis() as u64);
                info!(after_ms = ?after, "capture connection restored");
            }
        }
    }

    fn stats(&mut self) {
        let Some(capture) = &self.capture else {
            return;
        };
        let seconds = self
            .stats_at
            .elapsed()
            .as_secs_f64()
            .max(0.001);
        self.stats_at = Instant::now();
        let rendered = deskshell_wl::rendered_frames();
        let drawn = rendered - self.rendered;
        self.rendered = rendered;
        self.labels.clear();
        for shown in &mut self.shown {
            if shown.gone {
                continue;
            }
            let frames = capture.frames(shown.id);
            let fps = (frames - shown.frames) as f64 / seconds;
            shown.frames = frames;
            let mode = match capture.mode(shown.id) {
                Some(Mode::Dmabuf) => "dmabuf",
                Some(Mode::Shm) => "shm",
                None => "-",
            };
            let _ = write!(self.labels, "{}={mode}/{fps:.1}fps ", shown.label);
        }
        let (dmabufs, memfds) = buffer_fds();
        info!(
            panel_fps = format!("{:.1}", drawn as f64 / seconds),
            devices = deskshell_wl::device_count(),
            dmabufs,
            memfds,
            targets = self.labels.trim_end(),
            "stats"
        );
    }

    fn view(&self, _id: window::Id) -> Element<'_, Message> {
        let mut grid = row![].spacing(8);
        if let Some(capture) = &self.capture {
            for shown in self
                .shown
                .iter()
                .filter(|shown| !shown.gone)
            {
                let label = match capture.mode(shown.id) {
                    Some(Mode::Dmabuf) => "dmabuf",
                    Some(Mode::Shm) => "shm",
                    None => "",
                };
                grid = grid.push(
                    column![
                        container(deskshell_wl::thumbnail(capture, shown.id))
                            .width(220)
                            .height(124),
                        text(format!("{} {label}", shown.label)).size(12),
                    ]
                    .spacing(2),
                );
            }
        }
        container(column![deskshell_wl::gpu_probe(), grid.wrap()].spacing(4))
            .padding(8)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| container::background(Color::from_rgba(0.08, 0.08, 0.1, 0.9)))
            .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        let opts = Self::opts();
        Subscription::batch([
            if self.capture.is_none() {
                iced::time::every(Duration::from_millis(100)).map(|_| Message::CheckGpu)
            } else {
                Subscription::none()
            },
            iced::time::every(opts.log_every).map(|_| Message::Stats),
            match opts.poison {
                Some(after) if !self.poisoned => iced::time::every(after).map(|_| Message::Poison),
                _ => Subscription::none(),
            },
            match opts.stop_after {
                Some(after) if !self.stopped => iced::time::every(after).map(|_| Message::StopAll),
                _ => Subscription::none(),
            },
            // A few redraws let wgpu destroy the textures the widget held.
            if self.stopped && self.redraws < 3 {
                iced::time::every(Duration::from_millis(500)).map(|_| Message::Redraw)
            } else {
                Subscription::none()
            },
        ])
    }
}

mod png {
    use deskshell_wl::Image;
    use iced::wgpu::TextureFormat;

    pub fn encode(image: &Image) -> Vec<u8> {
        let row = image.width as usize * 4;
        let mut raw = Vec::with_capacity((row + 1) * image.height as usize);
        for y in 0..image.height as usize {
            let source = if image.flip_y { image.height as usize - 1 - y } else { y };
            raw.push(0);
            for pixel in image.pixels[source * row..(source + 1) * row]
                .as_chunks::<4>()
                .0
            {
                let (r, g, b) = match image.format {
                    TextureFormat::Bgra8Unorm => (pixel[2], pixel[1], pixel[0]),
                    _ => (pixel[0], pixel[1], pixel[2]),
                };
                let a = if image.opaque { 255 } else { pixel[3] };
                raw.extend_from_slice(&[r, g, b, a]);
            }
        }
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut header = Vec::with_capacity(13);
        header.extend_from_slice(&image.width.to_be_bytes());
        header.extend_from_slice(&image.height.to_be_bytes());
        header.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &header);
        chunk(&mut out, b"IDAT", &zlib_stored(&raw));
        chunk(&mut out, b"IEND", &[]);
        out
    }

    fn zlib_stored(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x78, 0x01];
        let mut blocks = data.chunks(65_535).peekable();
        if blocks.peek().is_none() {
            out.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
        }
        while let Some(block) = blocks.next() {
            out.push(u8::from(blocks.peek().is_none()));
            let len = block.len() as u16;
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&(!len).to_le_bytes());
            out.extend_from_slice(block);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &byte in data {
            a = (a + u32::from(byte)) % 65_521;
            b = (b + a) % 65_521;
        }
        out.extend_from_slice(&(b << 16 | a).to_be_bytes());
        out
    }

    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let crc = crc32(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 { 0xedb8_8320 ^ (crc >> 1) } else { crc >> 1 };
            }
        }
        !crc
    }
}

mod log {
    use std::fmt::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Level, Metadata, Subscriber};

    struct Stderr {
        start: Instant,
        own: Level,
        spans: AtomicU64,
    }

    pub fn install() {
        let own = match std::env::var("THUMBS_LOG").as_deref() {
            Ok("debug") => Level::DEBUG,
            Ok("trace") => Level::TRACE,
            _ => Level::INFO,
        };
        let subscriber = Stderr { start: Instant::now(), own, spans: AtomicU64::new(1) };
        let _ = tracing::subscriber::set_global_default(subscriber);
    }

    struct Fields(String);

    impl Visit for Fields {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                let _ = write!(self.0, " {value:?}");
            } else {
                let _ = write!(self.0, " {}={value:?}", field.name());
            }
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            if field.name() == "message" {
                let _ = write!(self.0, " {value}");
            } else {
                let _ = write!(self.0, " {}={value}", field.name());
            }
        }
    }

    impl Subscriber for Stderr {
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            let ours = metadata
                .target()
                .starts_with("deskshell")
                || metadata.target().starts_with("thumbs");
            *metadata.level() <= if ours { self.own } else { Level::WARN }
        }

        fn new_span(&self, _span: &Attributes<'_>) -> Id {
            Id::from_u64(
                self.spans
                    .fetch_add(1, Ordering::Relaxed),
            )
        }

        fn record(&self, _span: &Id, _values: &Record<'_>) {}

        fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

        fn event(&self, event: &Event<'_>) {
            let mut fields = Fields(String::new());
            event.record(&mut fields);
            let elapsed = self.start.elapsed();
            let thread = std::thread::current();
            eprintln!(
                "{:>4}.{:03} {:5} [{}] {}:{}",
                elapsed.as_secs(),
                elapsed.subsec_millis(),
                event.metadata().level(),
                thread.name().unwrap_or("-"),
                event.metadata().target(),
                fields.0
            );
        }

        fn enter(&self, _span: &Id) {}

        fn exit(&self, _span: &Id) {}
    }
}

#[cfg(test)]
mod tests {
    use deskshell_wl::Image;

    use super::*;

    #[test]
    fn reads_top_level_values() {
        let clients = r#"[{"address": "0x55aa10", "grouped": [], "workspace": {"id": 1, "name": "1"}},
            {"address": "0x55bb20", "title": "a \"quoted\" title"}]"#;
        assert_eq!(
            top_level_values(clients, "address"),
            ["0x55aa10", "0x55bb20"]
        );
        let monitors =
            r#"[{"id": 0, "name": "Virtual-1", "activeWorkspace": {"id": 1, "name": "1"}}]"#;
        assert_eq!(top_level_values(monitors, "name"), ["Virtual-1"]);
    }

    #[test]
    fn writes_a_png() {
        let image = Image {
            width: 2,
            height: 1,
            format: iced::wgpu::TextureFormat::Bgra8Unorm,
            opaque: true,
            flip_y: false,
            pixels: vec![0, 0, 255, 0, 255, 0, 0, 0],
        };
        let bytes = png::encode(&image);
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&bytes[12..16], b"IHDR");
        assert!(
            bytes.ends_with(&[0xae, 0x42, 0x60, 0x82]),
            "IEND crc"
        );
    }
}
