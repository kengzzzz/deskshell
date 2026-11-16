//! hyprsunset 0.4's IPC client and the night light status and toggle rules.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::time::timeout;

use crate::{Error, Paths};

pub const TIMEOUT: Duration = Duration::from_secs(1);
pub const WARM_TEMPERATURE: u32 = 4500;
/// A live temperature below this counts as warm and is kept when night light turns on.
pub const WARM_LIMIT: u32 = 6000;

const REPLY_MAX: usize = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub available: bool,
    /// Identity is off, so the screen is tinted.
    pub enabled: bool,
    pub scheduled: bool,
    pub temperature: u32,
}

impl Status {
    pub const UNAVAILABLE: Self =
        Self { available: false, enabled: false, scheduled: false, temperature: 0 };
}

/// The live values and the profile's, as the replies spell them.
#[derive(Debug, Clone, Default, PartialEq)]
struct Reading {
    temperature: u32,
    temperature_text: String,
    identity: String,
    gamma: f64,
    profile: Profile,
}

/// Empty fields when no profile is loaded.
#[derive(Debug, Clone, Default, PartialEq)]
struct Profile {
    temperature: String,
    gamma: String,
    identity: String,
}

impl Profile {
    /// The fields of `Time: HH:MM\nTemperature: N\nGamma: G\nIdentity: B`. Any other reply,
    /// `No profile is currently loaded` included, leaves them empty.
    fn parse(reply: &str) -> Self {
        let mut profile = Self::default();
        for line in reply.lines() {
            let Some((key, value)) = line.split_once(": ") else {
                continue;
            };
            let value = value
                .split(": ")
                .next()
                .unwrap_or_default();
            let field = match key {
                "Temperature" => &mut profile.temperature,
                "Gamma" => &mut profile.gamma,
                "Identity" => &mut profile.identity,
                _ => continue,
            };
            if field.is_empty() {
                value.clone_into(field);
            }
        }
        profile
    }

    fn loaded(&self) -> bool {
        !self.identity.is_empty()
    }
}

impl Reading {
    fn status(&self) -> Status {
        Status {
            available: true,
            enabled: self.identity != "true",
            scheduled: self.scheduled(),
            temperature: self.temperature,
        }
    }

    /// Live gamma is a percent and profile gamma a fraction. Both round half up.
    fn scheduled(&self) -> bool {
        let profile = &self.profile;
        profile.loaded()
            && self.identity == profile.identity
            && (self.gamma + 0.5).trunc() == (awk_number(&profile.gamma) * 100.0 + 0.5).trunc()
            && (self.identity == "true" || self.temperature_text == profile.temperature)
    }

    fn toggle(&self) -> &'static [Toggle] {
        if self.identity == "true" { self.turn_on() } else { self.turn_off() }
    }

    fn turn_on(&self) -> &'static [Toggle] {
        if self.profile.loaded() && self.profile.identity == "false" {
            &[Toggle::Reset]
        } else if self.temperature < WARM_LIMIT {
            &[Toggle::KeepTemperature]
        } else {
            &[Toggle::WarmTemperature]
        }
    }

    fn turn_off(&self) -> &'static [Toggle] {
        if self.profile.loaded() && self.profile.identity == "true" {
            &[Toggle::Reset]
        } else {
            &[Toggle::IdentityOn, Toggle::FullGamma]
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Toggle {
    Reset,
    KeepTemperature,
    WarmTemperature,
    IdentityOn,
    FullGamma,
}

/// The leading number as `strtod` reads it, 0 without one. hyprsunset prints a small
/// profile gamma as `1e-05`.
fn awk_number(text: &str) -> f64 {
    let text = text.trim_start();
    let mut end = 0;
    let mut previous = None;
    for (i, &b) in text.as_bytes().iter().enumerate() {
        let sign = matches!(b, b'-' | b'+') && (i == 0 || matches!(previous, Some(b'e' | b'E')));
        if !(b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E') || sign) {
            break;
        }
        end = i + 1;
        previous = Some(b);
    }
    (0..=end)
        .rev()
        .find_map(|len| text[..len].parse::<f64>().ok())
        .unwrap_or(0.0)
}

fn is_missing(error: &Error) -> bool {
    matches!(
        error,
        Error::Io(err)
            if matches!(err.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused)
    )
}

/// hyprsunset serves one connection until the client closes, so a held connection blocks
/// every other client. Each request opens its own connection and closes it.
#[derive(Debug)]
pub struct Sunset {
    socket: PathBuf,
    timeout: Duration,
    missing_logged: AtomicBool,
}

impl Sunset {
    pub fn new(socket: PathBuf) -> Self {
        Self { socket, timeout: TIMEOUT, missing_logged: AtomicBool::new(false) }
    }

    pub fn for_instance(paths: &Paths) -> Self {
        Self::new(paths.sunset())
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// hyprsunset ends a reply with nothing, so this reads once and closes.
    pub async fn request(&self, cmd: &str) -> Result<String, Error> {
        let exchange = async {
            let mut stream = UnixStream::connect(&self.socket).await?;
            stream.write_all(cmd.as_bytes()).await?;
            let mut reply = vec![0; REPLY_MAX];
            let read = stream.read(&mut reply).await?;
            reply.truncate(read);
            Ok::<_, io::Error>(reply)
        };
        let reply = timeout(self.timeout, exchange)
            .await
            .map_err(|_| Error::Timeout("hyprsunset request"))??;
        Ok(String::from_utf8_lossy(&reply)
            .trim()
            .to_owned())
    }

    /// A failed read counts as an empty reply.
    async fn read(&self, cmd: &str) -> Option<String> {
        match self.request(cmd).await {
            Ok(reply) => {
                self.missing_logged
                    .store(false, Ordering::Relaxed);
                Some(reply)
            }
            Err(error) if is_missing(&error) => {
                if self
                    .missing_logged
                    .swap(true, Ordering::Relaxed)
                {
                    tracing::debug!("hyprsunset is still not running: {error}");
                } else {
                    tracing::warn!("hyprsunset is not running, night light is off: {error}");
                }
                None
            }
            Err(error) => {
                tracing::warn!("hyprsunset `{cmd}` failed: {error}");
                None
            }
        }
    }

    async fn reading(&self) -> Option<Reading> {
        let temperature_text = self.read("temperature").await?;
        if temperature_text.is_empty()
            || !temperature_text
                .bytes()
                .all(|b| b.is_ascii_digit())
        {
            tracing::debug!("hyprsunset temperature is not a number: {temperature_text:?}");
            return None;
        }
        let temperature = temperature_text.parse().ok()?;
        // Bare `identity` sets identity, so the read must say `get`.
        let identity = self
            .read("identity get")
            .await
            .unwrap_or_default();
        let gamma = self
            .read("gamma")
            .await
            .map_or(0.0, |gamma| awk_number(&gamma));
        let profile = self
            .read("profile")
            .await
            .map(|reply| Profile::parse(&reply))
            .unwrap_or_default();
        Some(Reading { temperature, temperature_text, identity, gamma, profile })
    }

    pub async fn status(&self) -> Status {
        self.reading()
            .await
            .map_or(Status::UNAVAILABLE, |reading| reading.status())
    }

    pub async fn toggle(&self) {
        let Some(reading) = self.reading().await else {
            return;
        };
        for step in reading.toggle() {
            match step {
                Toggle::Reset => self.command("reset").await,
                Toggle::KeepTemperature => {
                    self.command(&format!("temperature {}", reading.temperature))
                        .await;
                }
                Toggle::WarmTemperature => {
                    self.command(&format!("temperature {WARM_TEMPERATURE}"))
                        .await;
                }
                Toggle::IdentityOn => self.command("identity true").await,
                Toggle::FullGamma => self.command("gamma 100").await,
            }
        }
    }

    pub async fn resume_schedule(&self) {
        if self.reading().await.is_some() {
            self.command("reset").await;
        }
    }

    async fn command(&self, cmd: &str) {
        match self.read(cmd).await {
            Some(reply) if reply == "ok" => {}
            Some(reply) => tracing::warn!("hyprsunset refused `{cmd}`: {reply}"),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(identity: &str, temperature: u32, gamma: f64, profile: &str) -> Reading {
        Reading {
            temperature,
            temperature_text: temperature.to_string(),
            identity: identity.to_owned(),
            gamma,
            profile: Profile::parse(profile),
        }
    }

    const WARM_PROFILE: &str = "Time: 20:00\nTemperature: 4000\nGamma: 0.8\nIdentity: false";
    const DAY_PROFILE: &str = "Time: 07:00\nTemperature: 6000\nGamma: 1\nIdentity: true";
    const NO_PROFILE: &str = "No profile is currently loaded";

    #[test]
    fn profiles() {
        let profile = Profile::parse(WARM_PROFILE);
        assert_eq!(profile.temperature, "4000");
        assert_eq!(profile.gamma, "0.8");
        assert_eq!(profile.identity, "false");
        assert!(profile.loaded());
        assert_eq!(Profile::parse(NO_PROFILE), Profile::default());
        assert!(!Profile::parse("").loaded());
    }

    #[test]
    fn awk_numbers() {
        assert_eq!(awk_number("100"), 100.0);
        assert_eq!(awk_number("100.000000"), 100.0);
        assert_eq!(awk_number(" 80.5x"), 80.5);
        assert_eq!(awk_number("-1"), -1.0);
        assert_eq!(awk_number("1.2.3"), 1.2);
        assert_eq!(awk_number(""), 0.0);
        assert_eq!(awk_number("ok"), 0.0);
        assert_eq!(awk_number("."), 0.0);
        assert_eq!(awk_number("1e-05"), 0.00001);
        assert_eq!(awk_number("2.5E+2"), 250.0);
        assert_eq!(awk_number("1e"), 1.0);
        assert_eq!(awk_number("1e-"), 1.0);
        assert_eq!(awk_number("1e5e3"), 100000.0);
        assert_eq!(awk_number("e5"), 0.0);
        assert_eq!(awk_number("1-2"), 1.0);
    }

    #[test]
    fn status_with_a_scientific_gamma() {
        // hyprsunset formats a profile gamma of 0.00001 with `{}`, which prints `1e-05`.
        let profile = "Time: 22:00\nTemperature: 3000\nGamma: 1e-05\nIdentity: false";
        assert!(reading("false", 3000, 0.001, profile).scheduled());
        assert!(!reading("false", 3000, 100.0, profile).scheduled());
    }

    #[test]
    fn status_without_a_profile() {
        let status = reading("false", 4500, 100.0, NO_PROFILE).status();
        assert_eq!(
            status,
            Status { available: true, enabled: true, scheduled: false, temperature: 4500 }
        );
        let status = reading("true", 6000, 100.0, NO_PROFILE).status();
        assert!(!status.enabled && !status.scheduled);
    }

    #[test]
    fn status_with_an_identity_profile() {
        assert!(reading("true", 6000, 100.0, DAY_PROFILE).scheduled());
        assert!(reading("true", 3000, 100.0, DAY_PROFILE).scheduled());
        assert!(!reading("false", 6000, 100.0, DAY_PROFILE).scheduled());
        assert!(!reading("true", 6000, 90.0, DAY_PROFILE).scheduled());
    }

    #[test]
    fn status_with_a_warm_profile() {
        assert!(reading("false", 4000, 80.0, WARM_PROFILE).scheduled());
        assert!(!reading("false", 4500, 80.0, WARM_PROFILE).scheduled());
        assert!(!reading("true", 4000, 80.0, WARM_PROFILE).scheduled());
    }

    #[test]
    fn gamma_rounds_to_whole_percent() {
        assert!(reading("false", 4000, 80.4, WARM_PROFILE).scheduled());
        assert!(reading("false", 4000, 79.5, WARM_PROFILE).scheduled());
        assert!(!reading("false", 4000, 80.5, WARM_PROFILE).scheduled());
        assert!(!reading("false", 4000, 79.4, WARM_PROFILE).scheduled());
        let profile = "Temperature: 4000\nGamma: 0.795\nIdentity: false";
        assert!(reading("false", 4000, 80.0, profile).scheduled());
    }

    #[test]
    fn turn_on_rules() {
        assert_eq!(
            reading("true", 6000, 100.0, WARM_PROFILE).toggle(),
            [Toggle::Reset]
        );
        assert_eq!(
            reading("true", 5999, 100.0, DAY_PROFILE).toggle(),
            [Toggle::KeepTemperature]
        );
        assert_eq!(
            reading("true", 6000, 100.0, DAY_PROFILE).toggle(),
            [Toggle::WarmTemperature]
        );
        assert_eq!(
            reading("true", 3500, 100.0, NO_PROFILE).toggle(),
            [Toggle::KeepTemperature]
        );
        assert_eq!(
            reading("true", 6500, 100.0, NO_PROFILE).toggle(),
            [Toggle::WarmTemperature]
        );
    }

    #[test]
    fn turn_off_rules() {
        assert_eq!(
            reading("false", 4000, 80.0, DAY_PROFILE).toggle(),
            [Toggle::Reset]
        );
        assert_eq!(
            reading("false", 4000, 80.0, WARM_PROFILE).toggle(),
            [Toggle::IdentityOn, Toggle::FullGamma]
        );
        assert_eq!(
            reading("false", 4000, 80.0, NO_PROFILE).toggle(),
            [Toggle::IdentityOn, Toggle::FullGamma]
        );
        // An identity read that failed counts as off.
        assert_eq!(
            reading("", 4000, 80.0, NO_PROFILE).toggle(),
            [Toggle::IdentityOn, Toggle::FullGamma]
        );
    }
}
