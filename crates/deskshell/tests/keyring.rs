//! The keyring prompt over the real prompter on a private `dbus-daemon` with a gcr-like client.

#[allow(
    dead_code,
    reason = "shared by several test binaries, each using part of it"
)]
#[path = "common/private_bus.rs"]
mod private_bus;

use std::collections::HashMap;
use std::time::Duration;

use deskshell::auth::Secret;
use deskshell::auth::keyring::service::{self, Command, Kind, State};
use deskshell_keyring::exchange::Exchange;
use deskshell_keyring::{BUS_NAME, Config};
use private_bus::PrivateBus;
use tokio::sync::{mpsc, watch};
use zbus::proxy::CacheProperties;
use zbus::{Connection, OwnedVariant, Variant, interface};

const TIMEOUT: Duration = Duration::from_secs(5);
const QUIET: Duration = Duration::from_millis(300);
const CALLBACK: &str = "/org/gnome/keyring/Prompt/p1";

struct Callback {
    tx: mpsc::UnboundedSender<(String, String)>,
}

#[interface(name = "org.gnome.keyring.internal.Prompter.Callback")]
impl Callback {
    fn prompt_ready(
        &self,
        reply: String,
        _properties: HashMap<String, OwnedVariant>,
        exchange: String,
    ) {
        let _ = self.tx.send((reply, exchange));
    }

    fn prompt_done(&self) {}
}

struct Client {
    conn: Connection,
    rx: mpsc::UnboundedReceiver<(String, String)>,
    exchange: Exchange,
}

impl Client {
    async fn new(bus: &PrivateBus) -> Self {
        let conn = bus.connect().await;
        let (tx, rx) = mpsc::unbounded_channel();
        conn.object_server()
            .at(CALLBACK, Callback { tx })
            .await
            .unwrap();
        Self { conn, rx, exchange: Exchange::new().unwrap() }
    }

    async fn call<B>(&self, method: &str, body: &B)
    where
        B: serde::Serialize + zbus::DynamicType,
    {
        zbus::proxy::Builder::<zbus::Proxy<'_>>::new(&self.conn)
            .destination(BUS_NAME)
            .path("/org/gnome/keyring/Prompter")
            .interface("org.gnome.keyring.internal.Prompter")
            .cache_properties(CacheProperties::No)
            .build()
            .await
            .unwrap()
            .call::<_, _, ()>(method, body)
            .await
            .unwrap();
    }

    async fn begin(&mut self) {
        let path = zbus::ObjectPath::try_from(CALLBACK).unwrap();
        self.call("BeginPrompting", &(path,))
            .await;
        assert_eq!(self.ready().await, (String::new(), None));
    }

    async fn perform(&self, props: &[(&str, &str)]) {
        let path = zbus::ObjectPath::try_from(CALLBACK).unwrap();
        let props: HashMap<&str, Variant<'_>> = props
            .iter()
            .map(|(name, value)| (*name, Variant::from(*value)))
            .collect();
        self.call(
            "PerformPrompt",
            &(path, "password", props, self.exchange.begin()),
        )
        .await;
    }

    async fn ready(&mut self) -> (String, Option<Vec<u8>>) {
        let (reply, exchange) = tokio::time::timeout(TIMEOUT, self.rx.recv())
            .await
            .expect("timed out")
            .unwrap();
        let secret = self
            .exchange
            .receive(&exchange)
            .unwrap()
            .map(|secret| secret.to_vec());
        (reply, secret)
    }

    async fn nothing(&mut self) {
        if let Ok(Some(call)) = tokio::time::timeout(QUIET, self.rx.recv()).await {
            panic!("unexpected {call:?}");
        }
    }
}

async fn changed(state: &mut watch::Receiver<State>) -> State {
    tokio::time::timeout(TIMEOUT, state.changed())
        .await
        .expect("timed out")
        .unwrap();
    state.borrow_and_update().clone()
}

async fn serving(bus: &PrivateBus) {
    let conn = bus.connect().await;
    let dbus = zbus::fdo::DBusProxy::new(&conn)
        .await
        .unwrap();
    tokio::time::timeout(TIMEOUT, async {
        while !dbus
            .name_has_owner(zbus::names::BusName::try_from(BUS_NAME).unwrap())
            .await
            .unwrap()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the prompter never owned its name");
}

#[tokio::test]
async fn the_prompt_over_the_prompter_with_tokens() {
    let bus = PrivateBus::start("dsh-keyring-shell");
    let (mut service, stop) = service::spawn(Config {
        address: Some(bus.address().to_owned()),
        bus_name: BUS_NAME.to_owned(),
        call_timeout: Duration::from_secs(1),
        yubikey: None,
    });
    serving(&bus).await;
    let mut client = Client::new(&bus).await;
    client.begin().await;
    client
        .perform(&[("message", "Authentication required"), ("description", "keyring “A &amp; B”")])
        .await;
    let first = changed(&mut service.state).await;
    let prompt = first.prompt.clone().unwrap();
    assert_eq!(prompt.kind, Kind::Password);
    assert_eq!(
        prompt.description, "keyring “A & B”",
        "unescaped for the form"
    );
    assert!(!first.pending);

    service
        .commands
        .send(Command::Submit {
            token: prompt.token.clone(),
            password: Secret::new("hunter2"),
            choice: false,
        })
        .unwrap();
    assert!(
        changed(&mut service.state)
            .await
            .pending
    );
    assert_eq!(
        client.ready().await,
        (String::from("yes"), Some(b"hunter2".to_vec()))
    );

    client
        .perform(&[("warning", "The unlock password was incorrect")])
        .await;
    let retry = changed(&mut service.state).await;
    let again = retry.prompt.clone().unwrap();
    assert!(!retry.pending, "a new prompt ends pending");
    assert_eq!(again.warning, "The unlock password was incorrect");
    assert_ne!(again.token, prompt.token);

    service
        .commands
        .send(Command::Submit {
            token: prompt.token.clone(),
            password: Secret::new("late"),
            choice: false,
        })
        .unwrap();
    client.nothing().await;
    assert_eq!(*service.state.borrow(), retry);

    service
        .commands
        .send(Command::Cancel { token: again.token.clone() })
        .unwrap();
    assert!(
        !changed(&mut service.state)
            .await
            .active(),
        "a cancel hides at once"
    );
    assert_eq!(client.ready().await, (String::from("no"), None));

    stop.stop().await;
    let dbus = zbus::fdo::DBusProxy::new(&client.conn)
        .await
        .unwrap();
    assert!(
        !dbus
            .name_has_owner(zbus::names::BusName::try_from(BUS_NAME).unwrap())
            .await
            .unwrap(),
        "the stop released the name"
    );
}

#[tokio::test]
async fn stopping_with_a_prompt_open_hides_it() {
    let bus = PrivateBus::start("dsh-keyring-shell");
    let (mut service, stop) = service::spawn(Config {
        address: Some(bus.address().to_owned()),
        bus_name: BUS_NAME.to_owned(),
        call_timeout: Duration::from_secs(1),
        yubikey: None,
    });
    serving(&bus).await;
    let mut client = Client::new(&bus).await;
    client.begin().await;
    client.perform(&[]).await;
    assert!(
        changed(&mut service.state)
            .await
            .active()
    );
    stop.stop().await;
    assert!(
        !service
            .state
            .borrow_and_update()
            .active()
    );
}
