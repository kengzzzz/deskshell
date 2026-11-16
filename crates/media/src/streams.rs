//! A one-shot list of the sink inputs, the apps that play sound now.

use std::path::Path;
use std::time::Duration;

use pulseaudio::protocol::Command;

use crate::pulse::info::{self, prop};
use crate::pulse::{Conn, Message, PulseError};

const LIMIT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkInput {
    pub index: u32,
    pub process_id: Option<u32>,
    pub node_name: Option<String>,
    pub application_name: Option<String>,
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct ListError(PulseError);

pub async fn sink_inputs(socket: &Path, client_name: &str) -> Result<Vec<SinkInput>, ListError> {
    tokio::time::timeout(LIMIT, list(socket, client_name))
        .await
        .unwrap_or(Err(PulseError::Timeout))
        .map_err(ListError)
}

async fn list(socket: &Path, client_name: &str) -> Result<Vec<SinkInput>, PulseError> {
    let mut conn = Conn::connect(socket, client_name).await?;
    let seq = conn
        .send(&Command::GetSinkInputInfoList)
        .await?;
    let version = conn.version();
    loop {
        match conn.next().await? {
            Message::Reply { seq: got, body } if got == seq => {
                let (inputs, error) = info::read_inputs(body, version);
                if let Some(error) = error {
                    tracing::debug!(%error, "a sink input list cut short");
                }
                return Ok(inputs
                    .into_iter()
                    .map(|input| SinkInput {
                        index: input.index,
                        process_id: prop(&input.props, c"application.process.id")
                            .and_then(|pid| pid.trim().parse().ok()),
                        node_name: prop(&input.props, c"node.name").map(str::to_owned),
                        application_name: prop(&input.props, c"application.name")
                            .map(str::to_owned),
                    })
                    .collect());
            }
            Message::Error { seq: got, code } if got == seq => {
                return Err(PulseError::Refused(code));
            }
            _ => {}
        }
    }
}
