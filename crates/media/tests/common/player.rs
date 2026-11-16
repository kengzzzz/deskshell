//! A fake MPRIS player on the private bus that records the methods called on it.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use zbus::fdo::RequestNameFlags;
use zbus::names::InterfaceName;
use zbus::object_server::SignalEmitter;
use zbus::{Connection, ObjectPath, OwnedVariant, Variant, interface};

use super::{PrivateBus, TIMEOUT};

pub const PATH: &str = "/org/mpris/MediaPlayer2";
pub const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
pub const ROOT_IFACE: &str = "org.mpris.MediaPlayer2";

#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    Play,
    Pause,
    PlayPause,
    Next,
    Previous,
    Stop,
    Seek(i64),
    SetPosition(String, i64),
}

#[derive(Debug, Clone)]
pub struct Props {
    pub identity: String,
    pub desktop_entry: String,
    pub status: String,
    pub track_id: Option<String>,
    pub title: Option<String>,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub art_url: Option<String>,
    pub length: Option<i64>,
    pub rate: f64,
    pub position: i64,
    pub can_control: bool,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_seek: bool,
    pub can_go_next: bool,
    pub can_go_previous: bool,
}

impl Default for Props {
    fn default() -> Self {
        Self {
            identity: "Fake Player".to_owned(),
            desktop_entry: "fake-player".to_owned(),
            status: "Stopped".to_owned(),
            track_id: Some("/org/fake/track/1".to_owned()),
            title: Some("Track One".to_owned()),
            artists: vec!["Artist A".to_owned(), "Artist B".to_owned()],
            album: Some("Album".to_owned()),
            art_url: Some("file:///tmp/art.png".to_owned()),
            length: Some(180_000_000),
            rate: 1.0,
            position: 0,
            can_control: true,
            can_play: true,
            can_pause: true,
            can_seek: true,
            can_go_next: true,
            can_go_previous: true,
        }
    }
}

impl Props {
    pub fn metadata(&self) -> HashMap<String, OwnedVariant> {
        let mut map = HashMap::new();
        let mut put = |key: &str, value: Variant<'_>| {
            map.insert(
                key.to_owned(),
                OwnedVariant::try_from(value).expect("owned"),
            );
        };
        if let Some(id) = &self.track_id {
            put(
                "mpris:trackid",
                ObjectPath::try_from(id.as_str())
                    .expect("path")
                    .into(),
            );
        }
        if let Some(title) = &self.title {
            put("xesam:title", title.as_str().into());
        }
        if !self.artists.is_empty() {
            put("xesam:artist", self.artists.clone().into());
        }
        if let Some(album) = &self.album {
            put("xesam:album", album.as_str().into());
        }
        if let Some(art) = &self.art_url {
            put("mpris:artUrl", art.as_str().into());
        }
        if let Some(length) = self.length {
            put("mpris:length", length.into());
        }
        map
    }

    fn value(&self, name: &str) -> Variant<'static> {
        match name {
            "Identity" => self.identity.clone().into(),
            "DesktopEntry" => self.desktop_entry.clone().into(),
            "PlaybackStatus" => self.status.clone().into(),
            "Metadata" => Variant::from(self.metadata()),
            "Rate" => self.rate.into(),
            "Position" => self.position.into(),
            "CanControl" => self.can_control.into(),
            "CanPlay" => self.can_play.into(),
            "CanPause" => self.can_pause.into(),
            "CanSeek" => self.can_seek.into(),
            "CanGoNext" => self.can_go_next.into(),
            "CanGoPrevious" => self.can_go_previous.into(),
            other => panic!("unknown property {other}"),
        }
    }
}

type Shared = Arc<Mutex<Props>>;

struct Root {
    props: Shared,
}

#[interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) {}

    fn quit(&self) {}

    #[zbus(property)]
    fn identity(&self) -> String {
        self.props
            .lock()
            .unwrap()
            .identity
            .clone()
    }

    #[zbus(property)]
    fn desktop_entry(&self) -> String {
        self.props
            .lock()
            .unwrap()
            .desktop_entry
            .clone()
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

struct PlayerIface {
    props: Shared,
    calls: mpsc::UnboundedSender<Call>,
}

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl PlayerIface {
    fn play(&self) {
        let _ = self.calls.send(Call::Play);
    }

    fn pause(&self) {
        let _ = self.calls.send(Call::Pause);
    }

    fn play_pause(&self) {
        let _ = self.calls.send(Call::PlayPause);
    }

    fn next(&self) {
        let _ = self.calls.send(Call::Next);
    }

    fn previous(&self) {
        let _ = self.calls.send(Call::Previous);
    }

    fn stop(&self) {
        let _ = self.calls.send(Call::Stop);
    }

    fn seek(&self, offset: i64) {
        let _ = self.calls.send(Call::Seek(offset));
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "zbus passes object paths by value"
    )]
    fn set_position(&self, track_id: ObjectPath<'_>, position: i64) {
        let _ = self
            .calls
            .send(Call::SetPosition(track_id.to_string(), position));
    }

    #[zbus(property)]
    fn playback_status(&self) -> String {
        self.props
            .lock()
            .unwrap()
            .status
            .clone()
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedVariant> {
        self.props.lock().unwrap().metadata()
    }

    #[zbus(property)]
    fn rate(&self) -> f64 {
        self.props.lock().unwrap().rate
    }

    #[zbus(property)]
    fn position(&self) -> i64 {
        self.props.lock().unwrap().position
    }

    #[zbus(property)]
    fn can_control(&self) -> bool {
        self.props.lock().unwrap().can_control
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        self.props.lock().unwrap().can_play
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.props.lock().unwrap().can_pause
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.props.lock().unwrap().can_seek
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        self.props.lock().unwrap().can_go_next
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.props
            .lock()
            .unwrap()
            .can_go_previous
    }

    #[zbus(signal)]
    async fn seeked(emitter: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;
}

pub struct FakePlayer {
    pub name: String,
    pub conn: Connection,
    pub props: Shared,
    pub calls: mpsc::UnboundedReceiver<Call>,
}

impl FakePlayer {
    pub async fn start(bus: &PrivateBus, suffix: &str, props: Props) -> Self {
        let conn = bus.connect().await;
        let props = Arc::new(Mutex::new(props));
        let (tx, calls) = mpsc::unbounded_channel();
        let server = conn.object_server();
        server
            .at(PATH, Root { props: Arc::clone(&props) })
            .await
            .expect("serve root");
        server
            .at(
                PATH,
                PlayerIface { props: Arc::clone(&props), calls: tx },
            )
            .await
            .expect("serve player");
        let name = format!("org.mpris.MediaPlayer2.{suffix}");
        conn.request_name_with_flags(name.as_str(), RequestNameFlags::DO_NOT_QUEUE)
            .await
            .expect("own the player name");
        Self { name, conn, props, calls }
    }

    pub fn set(&self, f: impl FnOnce(&mut Props)) {
        f(&mut self.props.lock().unwrap());
    }

    fn emitter(&self) -> SignalEmitter<'_> {
        SignalEmitter::new(&self.conn, PATH).expect("emitter")
    }

    pub async fn emit(&self, iface: &'static str, names: &[&str]) {
        let values: Vec<(String, Variant<'static>)> = {
            let props = self.props.lock().unwrap();
            names
                .iter()
                .map(|n| ((*n).to_owned(), props.value(n)))
                .collect()
        };
        let changed: HashMap<&str, Variant<'_>> = values
            .iter()
            .map(|(n, v)| (n.as_str(), v.clone()))
            .collect();
        zbus::fdo::Properties::properties_changed(
            &self.emitter(),
            InterfaceName::from_static_str_unchecked(iface),
            changed,
            Cow::Borrowed(&[]),
        )
        .await
        .expect("emit");
    }

    pub async fn invalidate(&self, iface: &'static str, names: &[&str]) {
        zbus::fdo::Properties::properties_changed(
            &self.emitter(),
            InterfaceName::from_static_str_unchecked(iface),
            HashMap::new(),
            Cow::Borrowed(names),
        )
        .await
        .expect("emit");
    }

    pub async fn emit_raw(&self, iface: &'static str, changed: HashMap<&str, Variant<'_>>) {
        zbus::fdo::Properties::properties_changed(
            &self.emitter(),
            InterfaceName::from_static_str_unchecked(iface),
            changed,
            Cow::Borrowed(&[]),
        )
        .await
        .expect("emit");
    }

    pub async fn seeked(&self, position: i64) {
        self.set(|p| p.position = position);
        PlayerIface::seeked(&self.emitter(), position)
            .await
            .expect("emit Seeked");
    }

    pub async fn next_call(&mut self) -> Call {
        tokio::time::timeout(TIMEOUT, self.calls.recv())
            .await
            .expect("no call arrived")
            .expect("the player stopped")
    }

    pub async fn no_call_within(&mut self, wait: Duration) {
        if let Ok(Some(call)) = tokio::time::timeout(wait, self.calls.recv()).await {
            panic!("unexpected call {call:?} on {}", self.name);
        }
    }

    pub async fn quit(self) {
        let _ = self
            .conn
            .release_name(self.name.as_str())
            .await;
        drop(self.conn);
    }
}
