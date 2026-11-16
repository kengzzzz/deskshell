//! Starts the tray watcher and host and hands out the state and command channels.

use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use zbus::Connection;

use crate::bus::WATCHER_PATH;
use crate::host::Host;
use crate::model::{Command, State};
use crate::watcher::Watcher;

#[derive(Debug, Clone)]
pub struct Config {
    pub watcher: bool,
    pub call_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self { watcher: true, call_timeout: Duration::from_secs(10) }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("this connection already serves an object at {WATCHER_PATH}")]
    PathTaken,
    #[error("the connection has no unique name")]
    NotBus,
    #[error("cannot own the host name {0}: {1}")]
    HostName(String, zbus::Error),
    #[error("D-Bus: {0}")]
    DBus(#[from] zbus::Error),
}

pub struct Service {
    commands: mpsc::UnboundedSender<Command>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
    watcher: Option<Watcher>,
}

impl Service {
    pub async fn start(
        conn: &Connection,
        config: Config,
    ) -> Result<(Self, watch::Receiver<State>), Error> {
        let watcher = if config.watcher { Some(Watcher::start(conn).await?) } else { None };
        let (state, state_rx) = watch::channel(State::default());
        let host = Host::start(conn, config.call_timeout, state).await?;
        let (commands, commands_rx) = mpsc::unbounded_channel();
        let (stop, stop_rx) = oneshot::channel();
        let task = tokio::spawn(host.run(commands_rx, stop_rx));
        let service = Self { commands, stop: Some(stop), task: Some(task), watcher };
        Ok((service, state_rx))
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    pub fn commands(&self) -> mpsc::UnboundedSender<Command> {
        self.commands.clone()
    }

    pub fn watcher(&self) -> Option<&Watcher> {
        self.watcher.as_ref()
    }

    pub async fn shutdown(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
        if let Some(watcher) = self.watcher.take() {
            watcher.shutdown().await;
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}
