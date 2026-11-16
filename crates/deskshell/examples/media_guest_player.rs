//! A made-up MPRIS player for media panel checks in a `dev/vm` guest. It refuses to run elsewhere.
//!
//! ```sh
//! cargo build -p deskshell --example media_guest_player
//! slot=$(dev/vm/vm up --share "$PWD/target")
//! dev/vm/vm ssh "$slot" 'as-tester setsid -f /work/debug/examples/media_guest_player \
//!     --name One --art file > /tmp/one.log 2>&1 < /dev/null'
//! ```
//!
//! Options: `--name NAME`, `--title TEXT`, `--artist TEXT`, `--paused`, `--live`, `--no-seek`
//! and `--art none|file|http|broken`.

use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use std::net::TcpListener;
use std::process::ExitCode;
use std::time::Instant;

use image::{ImageFormat, Rgba, RgbaImage};
use zbus::object_server::SignalEmitter;
use zbus::{ObjectPath, OwnedVariant, Variant, interface};

const GUEST_HOSTNAME: &str = "deskshell-vm";
const PATH: &str = "/org/mpris/MediaPlayer2";
const LENGTH: i64 = 200_000_000;
const TRACKS: usize = 3;

struct Options {
    name: String,
    title: String,
    artist: String,
    playing: bool,
    live: bool,
    seek: bool,
    art: String,
}

fn options() -> Result<Options, String> {
    let mut options = Options {
        name: String::from("Guest"),
        title: String::from("Guest Track"),
        artist: String::from("Sample Artist"),
        playing: true,
        live: false,
        seek: true,
        art: String::from("none"),
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--name" => options.name = value()?,
            "--title" => options.title = value()?,
            "--artist" => options.artist = value()?,
            "--art" => options.art = value()?,
            "--paused" => options.playing = false,
            "--live" => options.live = true,
            "--no-seek" => options.seek = false,
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(options)
}

fn cover(name: &str) -> Vec<u8> {
    let hue = name.bytes().fold(0_u32, |sum, byte| {
        sum.wrapping_mul(31)
            .wrapping_add(u32::from(byte))
    });
    let base = [(hue % 200 + 40) as u8, (hue / 7 % 200 + 40) as u8, (hue / 49 % 200 + 40) as u8];
    let image = RgbaImage::from_fn(480, 320, |x, y| {
        let ring = ((x as f32 - 240.0).hypot(y as f32 - 160.0) / 24.0) as u32;
        if ring.is_multiple_of(2) {
            Rgba([base[0], base[1], base[2], 255])
        } else {
            Rgba([255 - base[0] / 2, 240, 220, 255])
        }
    });
    let mut bytes = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .expect("encode the cover");
    bytes
}

fn art_url(options: &Options) -> String {
    match options.art.as_str() {
        "file" => {
            let path = format!("/tmp/media guest {}.png", options.name);
            std::fs::write(&path, cover(&options.name)).expect("write the cover");
            format!("file://{}", path.replace(' ', "%20"))
        }
        "http" => {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1");
            let address = listener.local_addr().expect("address");
            let body = cover(&options.name);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    let mut request = [0; 2048];
                    let _ = stream.read(&mut request);
                    println!("http art request");
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(&body);
                }
            });
            format!("http://{address}/cover?player={}", options.name)
        }
        "broken" => format!("file:///tmp/missing-{}.png", options.name),
        _ => String::new(),
    }
}

struct Root {
    identity: String,
}

#[interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) {}

    fn quit(&self) {}

    #[zbus(property)]
    fn identity(&self) -> String {
        self.identity.clone()
    }

    #[zbus(property)]
    fn desktop_entry(&self) -> String {
        String::new()
    }

    #[zbus(property)]
    fn can_quit(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn can_raise(&self) -> bool {
        false
    }
}

struct Player {
    title: String,
    artist: String,
    art: String,
    live: bool,
    seek: bool,
    playing: bool,
    track: usize,
    position: i64,
    read_at: Instant,
}

impl Player {
    fn track_id(&self) -> String {
        format!("/org/deskshell/guest/track/{}", self.track)
    }

    fn length(&self) -> i64 {
        if self.live { i64::MAX } else { LENGTH }
    }

    fn now(&self) -> i64 {
        let elapsed = if self.playing {
            i64::try_from(self.read_at.elapsed().as_micros()).unwrap_or(i64::MAX)
        } else {
            0
        };
        self.position
            .saturating_add(elapsed)
            .min(self.length())
    }

    fn set_playing(&mut self, playing: bool) {
        self.position = self.now();
        self.read_at = Instant::now();
        self.playing = playing;
    }

    fn jump(&mut self, position: i64) {
        self.position = position.clamp(0, self.length());
        self.read_at = Instant::now();
    }

    async fn status_changed(&self, emitter: &SignalEmitter<'_>) {
        println!(
            "status {}",
            if self.playing { "Playing" } else { "Paused" }
        );
        let _ = self
            .playback_status_changed(emitter)
            .await;
    }

    async fn track_changed(&mut self, step: usize, emitter: &SignalEmitter<'_>) {
        self.track = (self.track + step) % TRACKS;
        self.jump(0);
        println!("track {}", self.track);
        let _ = self.metadata_changed(emitter).await;
    }
}

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    async fn play(&mut self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        println!("call Play");
        self.set_playing(true);
        self.status_changed(&emitter).await;
    }

    async fn pause(&mut self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        println!("call Pause");
        self.set_playing(false);
        self.status_changed(&emitter).await;
    }

    async fn play_pause(&mut self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        println!("call PlayPause");
        self.set_playing(!self.playing);
        self.status_changed(&emitter).await;
    }

    fn stop(&self) {
        println!("call Stop");
    }

    async fn next(&mut self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        println!("call Next");
        self.track_changed(1, &emitter).await;
    }

    async fn previous(&mut self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        println!("call Previous");
        self.track_changed(TRACKS - 1, &emitter)
            .await;
    }

    async fn seek(&mut self, offset: i64, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        println!("call Seek {offset}");
        self.jump(self.now().saturating_add(offset));
        let _ = Self::seeked(&emitter, self.position).await;
    }

    async fn set_position(
        &mut self,
        track_id: ObjectPath<'_>,
        position: i64,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) {
        println!("call SetPosition {track_id} {position}");
        if track_id.as_str() != self.track_id() || !(0..=self.length()).contains(&position) {
            println!("ignored");
            return;
        }
        self.jump(position);
        let _ = Self::seeked(&emitter, position).await;
    }

    #[zbus(property)]
    fn playback_status(&self) -> String {
        String::from(if self.playing { "Playing" } else { "Paused" })
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedVariant> {
        let mut map = HashMap::new();
        let mut put = |key: &str, value: Variant<'_>| {
            if let Ok(value) = OwnedVariant::try_from(value) {
                map.insert(key.to_owned(), value);
            }
        };
        if let Ok(path) = ObjectPath::try_from(self.track_id()) {
            put("mpris:trackid", path.into());
        }
        put(
            "xesam:title",
            format!("{} {}", self.title, self.track + 1).into(),
        );
        put("xesam:artist", vec![self.artist.clone()].into());
        if !self.art.is_empty() {
            put("mpris:artUrl", self.art.as_str().into());
        }
        put("mpris:length", self.length().into());
        map
    }

    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }

    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        self.now()
    }

    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.seek
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        !self.live
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        !self.live
    }

    #[zbus(signal)]
    async fn seeked(emitter: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let hostname = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
    if hostname.trim() != GUEST_HOSTNAME {
        eprintln!("media_guest_player joins the session's players and only runs in a dev/vm guest");
        return ExitCode::from(2);
    }
    let options = match options() {
        Ok(options) => options,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::from(2);
        }
    };
    let art = art_url(&options);
    println!("art {art}");
    let player = Player {
        title: options.title,
        artist: options.artist,
        art,
        live: options.live,
        seek: options.seek,
        playing: options.playing,
        track: 0,
        position: 30_000_000,
        read_at: Instant::now(),
    };
    let name = format!("org.mpris.MediaPlayer2.{}", options.name);
    let built = zbus::connection::Builder::session()
        .serve_at(
            PATH,
            Root { identity: format!("Player {}", options.name) },
        )
        .serve_at(PATH, player)
        .name(name.as_str())
        .build()
        .await;
    let _conn = match built {
        Ok(conn) => conn,
        Err(err) => {
            eprintln!("cannot serve {name}: {err}");
            return ExitCode::FAILURE;
        }
    };
    println!("serving {name}");
    std::future::pending::<()>().await;
    ExitCode::SUCCESS
}
