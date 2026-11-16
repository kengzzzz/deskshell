//! Starts the NetworkManager and BlueZ tasks and hands out the state and command channels.

use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use zbus::Connection;

use crate::bus::Signals;
use crate::model::{Command, State};
use crate::{bluez, nm};

#[derive(Debug, Clone)]
pub struct Config {
    pub scan_interval: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self { scan_interval: nm::DEFAULT_INTERVAL }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("D-Bus: {0}")]
    DBus(#[from] zbus::Error),
}

pub struct Service {
    commands: mpsc::UnboundedSender<Command>,
    stops: Vec<oneshot::Sender<()>>,
    tasks: Vec<JoinHandle<()>>,
    router: JoinHandle<()>,
}

impl Service {
    pub async fn start(
        conn: &Connection,
        config: Config,
    ) -> Result<(Self, watch::Receiver<State>), Error> {
        let nm_signals = Signals::subscribe(conn, nm::NAME, Some(nm::SIGNAL_NAMESPACE)).await?;
        let bluez_signals = Signals::subscribe(conn, bluez::NAME, None).await?;
        let (state, state_rx) = watch::channel(State::default());

        let (nm_tx, nm_rx) = mpsc::unbounded_channel();
        let (bluez_tx, bluez_rx) = mpsc::unbounded_channel();
        let (nm_stop, nm_stop_rx) = oneshot::channel();
        let (bluez_stop, bluez_stop_rx) = oneshot::channel();
        let nm = nm::Task::new(conn.clone(), state.clone(), config.scan_interval);
        let bluez = bluez::Task::new(conn.clone(), state);
        let tasks = vec![
            tokio::spawn(nm.run(nm_signals, nm_rx, nm_stop_rx)),
            tokio::spawn(bluez.run(bluez_signals, bluez_rx, bluez_stop_rx)),
        ];

        let (commands, mut rx) = mpsc::unbounded_channel::<Command>();
        let router = tokio::spawn(async move {
            while let Some(command) = rx.recv().await {
                let target = if command.is_bluetooth() { &bluez_tx } else { &nm_tx };
                let _ = target.send(command);
            }
        });
        let service = Self { commands, stops: vec![nm_stop, bluez_stop], tasks, router };
        Ok((service, state_rx))
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    pub fn commands(&self) -> mpsc::UnboundedSender<Command> {
        self.commands.clone()
    }

    pub async fn shutdown(mut self) {
        for stop in self.stops.drain(..) {
            let _ = stop.send(());
        }
        for task in self.tasks.drain(..) {
            let _ = task.await;
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        for stop in self.stops.drain(..) {
            let _ = stop.send(());
        }
        self.router.abort();
    }
}

impl Command {
    fn is_bluetooth(&self) -> bool {
        matches!(
            self,
            Self::SetBluetoothPowered(_)
                | Self::Discover(_)
                | Self::BtConnect(_)
                | Self::BtDisconnect(_)
                | Self::BtPair(_)
                | Self::BtForget(_)
        )
    }
}
