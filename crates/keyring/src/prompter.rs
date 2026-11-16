//! `org.gnome.keyring.internal.Prompter`, the interface gcr's clients call.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tracing::warn;
use zbus::fdo::{self, DBusProxy};
use zbus::message::Header;
use zbus::names::{BusName, UniqueName};
use zbus::object_server::{ResponseDispatchListener, ResponseDispatchNotifier};
use zbus::{OwnedObjectPath, OwnedVariant, Variant, interface};

const LOOKUP: Duration = Duration::from_secs(1);
const SECRETS: &str = "org.freedesktop.secrets";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Key {
    pub sender: Arc<str>,
    pub callback: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Prop {
    Str(String),
    Bool(bool),
}

pub(crate) type Reply = oneshot::Sender<fdo::Result<()>>;

pub(crate) enum Incoming {
    Begin {
        key: Key,
        reply: Reply,
        sent: ResponseDispatchListener,
    },
    Perform {
        key: Key,
        kind: String,
        props: Vec<(String, Prop)>,
        exchange: String,
        secrets_owner: bool,
        reply: Reply,
    },
    Stop {
        key: Key,
        reply: Reply,
    },
}

pub(crate) struct Prompter {
    pub incoming: mpsc::UnboundedSender<Incoming>,
    pub bus: DBusProxy<'static>,
    pub uid: u32,
    pub yubikey: bool,
}

impl Prompter {
    async fn forward(&self, make: impl FnOnce(Reply) -> Incoming) -> fdo::Result<()> {
        let (reply, answer) = oneshot::channel();
        self.incoming
            .send(make(reply))
            .map_err(|_| stopped())?;
        answer
            .await
            .unwrap_or_else(|_| Err(stopped()))
    }

    async fn same_user(&self, sender: &UniqueName<'_>) -> bool {
        let lookup = self
            .bus
            .get_connection_credentials(BusName::Unique(sender.as_ref()));
        match tokio::time::timeout(LOOKUP, lookup).await {
            Ok(Ok(credentials)) => credentials.unix_user_id() == Some(self.uid),
            Ok(Err(error)) => {
                warn!(%error, "cannot read a prompt caller's credentials");
                false
            }
            Err(_) => {
                warn!("reading a prompt caller's credentials timed out");
                false
            }
        }
    }

    async fn owns_secrets(&self, sender: &UniqueName<'_>) -> bool {
        let Ok(name) = BusName::try_from(SECRETS) else {
            return false;
        };
        match tokio::time::timeout(LOOKUP, self.bus.get_name_owner(name)).await {
            Ok(Ok(owner)) => owner.as_str() == sender.as_str(),
            _ => false,
        }
    }
}

fn stopped() -> fdo::Error {
    fdo::Error::Failed("The prompter is stopping.".to_owned())
}

fn key(header: &Header<'_>, callback: &OwnedObjectPath) -> fdo::Result<(Key, UniqueName<'static>)> {
    let sender = header
        .sender()
        .ok_or_else(|| fdo::Error::Failed("A call with no sender.".to_owned()))?
        .to_owned();
    let key = Key { sender: Arc::from(sender.as_str()), callback: Arc::from(callback.as_str()) };
    Ok((key, sender))
}

fn props(properties: &HashMap<String, OwnedVariant>) -> Vec<(String, Prop)> {
    properties
        .iter()
        .filter_map(|(name, value)| {
            let prop = match &**value {
                Variant::Str(text) => Prop::Str(text.as_str().to_owned()),
                Variant::Bool(value) => Prop::Bool(*value),
                _ => return None,
            };
            Some((name.clone(), prop))
        })
        .collect()
}

// One call at a time, so calls reach the task in bus order, as in gcr's single main loop.
#[interface(name = "org.gnome.keyring.internal.Prompter", spawn = false)]
impl Prompter {
    async fn begin_prompting(
        &self,
        #[zbus(header)] header: Header<'_>,
        callback: OwnedObjectPath,
    ) -> fdo::Result<ResponseDispatchNotifier<()>> {
        let (key, sender) = key(&header, &callback)?;
        if !self.same_user(&sender).await {
            warn!(sender = %sender, "a prompt from another user, refused");
            return Err(fdo::Error::AccessDenied(
                "Only this user's programs may prompt here.".to_owned(),
            ));
        }
        // gcr's client sets up for PromptReady only when it has read this reply, and leaves a
        // PromptReady that comes first unanswered. The task waits for the reply to go out.
        let (notifier, sent) = ResponseDispatchNotifier::new(());
        self.forward(|reply| Incoming::Begin { key, reply, sent })
            .await?;
        Ok(notifier)
    }

    async fn perform_prompt(
        &self,
        #[zbus(header)] header: Header<'_>,
        callback: OwnedObjectPath,
        r#type: String,
        properties: HashMap<String, OwnedVariant>,
        exchange: String,
    ) -> fdo::Result<()> {
        let (key, sender) = key(&header, &callback)?;
        // The card decrypts the keyring password for the caller, so only the owner of
        // `org.freedesktop.secrets` may get the card path.
        let secrets_owner =
            self.yubikey && r#type == "password" && self.owns_secrets(&sender).await;
        let props = props(&properties);
        self.forward(|reply| Incoming::Perform {
            key,
            kind: r#type,
            props,
            exchange,
            secrets_owner,
            reply,
        })
        .await
    }

    async fn stop_prompting(
        &self,
        #[zbus(header)] header: Header<'_>,
        callback: OwnedObjectPath,
    ) -> fdo::Result<()> {
        let (key, _) = key(&header, &callback)?;
        self.forward(|reply| Incoming::Stop { key, reply })
            .await
    }
}
