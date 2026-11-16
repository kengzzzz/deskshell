//! The request socket client and the instance socket paths.

use std::env;
use std::ffi::OsStr;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::de::DeserializeOwned;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::time::timeout;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);

const BATCH_TOKEN: &str = "[[BATCH]]";
const BATCH_DELIMITER: &str = "\n\n\n";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0} is not set")]
    MissingEnv(&'static str),
    #[error("Hyprland IPC I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("Hyprland IPC timed out during {0}")]
    Timeout(&'static str),
    #[error("cannot parse Hyprland JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Hyprland rejected the command: {0}")]
    Command(String),
    #[error("batch reply has {got} parts, expected {expected}")]
    Batch { expected: usize, got: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub request: PathBuf,
    pub events: PathBuf,
}

impl Paths {
    pub fn from_env() -> Result<Self, Error> {
        let runtime = env::var_os("XDG_RUNTIME_DIR")
            .filter(|v| !v.is_empty())
            .ok_or(Error::MissingEnv("XDG_RUNTIME_DIR"))?;
        let signature = env::var_os("HYPRLAND_INSTANCE_SIGNATURE")
            .filter(|v| !v.is_empty())
            .ok_or(Error::MissingEnv("HYPRLAND_INSTANCE_SIGNATURE"))?;
        Ok(Self::for_instance(
            Path::new(&runtime),
            &signature,
        ))
    }

    pub fn for_instance(runtime_dir: &Path, signature: &OsStr) -> Self {
        Self::in_dir(&runtime_dir.join("hypr").join(signature))
    }

    pub fn in_dir(dir: &Path) -> Self {
        Self { request: dir.join(".socket.sock"), events: dir.join(".socket2.sock") }
    }

    pub fn sunset(&self) -> PathBuf {
        self.request
            .with_file_name(".hyprsunset.sock")
    }
}

/// Every request opens its own connection, because Hyprland serves one request per
/// connection and closes it after replying.
#[derive(Debug, Clone)]
pub struct Client {
    paths: Paths,
    timeout: Duration,
}

impl Client {
    pub fn new(paths: Paths) -> Self {
        Self { paths, timeout: DEFAULT_TIMEOUT }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    pub async fn request(&self, cmd: &str) -> Result<String, Error> {
        let mut stream = self
            .connect(&self.paths.request)
            .await?;
        // Hyprland takes everything readable before its first EAGAIN as the whole request,
        // so the command must go out in one write.
        self.step("write", stream.write_all(cmd.as_bytes()))
            .await?;
        let _ = stream.shutdown().await;
        let mut reply = Vec::new();
        self.step("read", stream.read_to_end(&mut reply))
            .await?;
        Ok(String::from_utf8_lossy_owned(reply))
    }

    pub async fn json<T: DeserializeOwned>(&self, query: &str) -> Result<T, Error> {
        let reply = self
            .request(&format!("j/{query}"))
            .await?;
        Ok(serde_json::from_str(&reply)?)
    }

    pub async fn dispatch(&self, lua: &str) -> Result<(), Error> {
        self.expect_ok(&format!("/dispatch {lua}"))
            .await
    }

    pub async fn eval(&self, lua: &str) -> Result<(), Error> {
        self.expect_ok(&format!("/eval {lua}"))
            .await
    }

    /// Hyprland answers a `[[BATCH]]` in one event loop pass, so the replies form a
    /// consistent snapshot.
    pub async fn batch(&self, commands: &[&str]) -> Result<Batch, Error> {
        let mut request = String::from(BATCH_TOKEN);
        for (i, command) in commands.iter().enumerate() {
            if i > 0 {
                request.push(';');
            }
            escape_batch_command(&mut request, command);
        }
        let reply = self.request(&request).await?;
        let got = reply.split(BATCH_DELIMITER).count();
        if got != commands.len() {
            return Err(Error::Batch { expected: commands.len(), got });
        }
        Ok(Batch { reply })
    }

    pub(crate) async fn connect_events(&self) -> Result<UnixStream, Error> {
        self.connect(&self.paths.events).await
    }

    async fn connect(&self, path: &Path) -> Result<UnixStream, Error> {
        self.step("connect", UnixStream::connect(path))
            .await
    }

    async fn step<T>(
        &self,
        stage: &'static str,
        fut: impl Future<Output = io::Result<T>>,
    ) -> Result<T, Error> {
        Ok(timeout(self.timeout, fut)
            .await
            .map_err(|_| Error::Timeout(stage))??)
    }

    async fn expect_ok(&self, cmd: &str) -> Result<(), Error> {
        let reply = self.request(cmd).await?;
        if reply.trim() == "ok" { Ok(()) } else { Err(Error::Command(reply)) }
    }
}

#[derive(Debug, Clone)]
pub struct Batch {
    reply: String,
}

impl Batch {
    pub fn parts(&self) -> impl Iterator<Item = &str> {
        self.reply.split(BATCH_DELIMITER)
    }
}

fn escape_batch_command(out: &mut String, command: &str) {
    for c in command.chars() {
        if c == ';' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_for_instance() {
        let paths = Paths::for_instance(Path::new("/run/user/1000"), OsStr::new("abc_123"));
        assert_eq!(
            paths.request,
            Path::new("/run/user/1000/hypr/abc_123/.socket.sock")
        );
        assert_eq!(
            paths.events,
            Path::new("/run/user/1000/hypr/abc_123/.socket2.sock")
        );
    }

    #[test]
    fn batch_escapes_separators() {
        let mut out = String::new();
        escape_batch_command(&mut out, r#"/eval a(); b("\n")"#);
        assert_eq!(out, r#"/eval a()\; b("\\n")"#);
    }
}
