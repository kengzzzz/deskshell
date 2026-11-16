//! `org.freedesktop.PolicyKit1.AuthenticationAgent`, the interface polkitd calls.

use std::collections::HashMap;

use tokio::sync::{mpsc, oneshot};
use zbus::interface;
use zbus::message::Header;

use crate::identity::RawIdentity;

// polkitd records `Cancelled` as a dismissal.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.PolicyKit1.Error")]
pub(crate) enum Reply {
    #[zbus(error)]
    ZBus(zbus::Error),
    Failed(String),
    Cancelled(String),
}

pub(crate) struct Begin {
    pub action_id: String,
    pub message: String,
    pub icon_name: String,
    pub cookie: String,
    pub identities: Vec<RawIdentity>,
}

pub(crate) enum Incoming {
    Begin { sender: Option<String>, begin: Begin, reply: oneshot::Sender<Result<(), Reply>> },
    Cancel { sender: Option<String>, cookie: String, reply: oneshot::Sender<Result<(), Reply>> },
}

pub(crate) struct Listener {
    pub incoming: mpsc::UnboundedSender<Incoming>,
}

fn stopped() -> Reply {
    Reply::Cancelled("The authentication agent stopped.".to_owned())
}

// zbus runs every call in its own task (the default `spawn`), so a BeginAuthentication that
// waits for the user never holds up CancelAuthentication.
#[interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl Listener {
    #[expect(
        clippy::too_many_arguments,
        reason = "the D-Bus method takes this many arguments"
    )]
    async fn begin_authentication(
        &self,
        #[zbus(header)] header: Header<'_>,
        action_id: String,
        message: String,
        icon_name: String,
        _details: HashMap<String, String>,
        cookie: String,
        identities: Vec<RawIdentity>,
    ) -> Result<(), Reply> {
        let (reply, result) = oneshot::channel();
        let begin = Begin { action_id, message, icon_name, cookie, identities };
        let sender = header.sender().map(ToString::to_string);
        self.incoming
            .send(Incoming::Begin { sender, begin, reply })
            .map_err(|_| stopped())?;
        result
            .await
            .unwrap_or_else(|_| Err(stopped()))
    }

    async fn cancel_authentication(
        &self,
        #[zbus(header)] header: Header<'_>,
        cookie: String,
    ) -> Result<(), Reply> {
        let (reply, result) = oneshot::channel();
        let sender = header.sender().map(ToString::to_string);
        self.incoming
            .send(Incoming::Cancel { sender, cookie, reply })
            .map_err(|_| stopped())?;
        result
            .await
            .unwrap_or_else(|_| Err(stopped()))
    }
}
