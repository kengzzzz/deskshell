//! The hyprsunset client against a fake that serves one connection at a time, like 0.4.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::TestDir;
use deskshell_hypr::sunset::{Status, Sunset};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::task::JoinHandle;

#[derive(Debug, Clone)]
struct Profile {
    temperature: u32,
    gamma: f32,
    identity: bool,
}

#[derive(Debug, Clone)]
struct Sun {
    temperature: u32,
    gamma: f32,
    identity: bool,
    profile: Option<Profile>,
}

impl Sun {
    fn new(temperature: u32, identity: bool, profile: Option<Profile>) -> Self {
        Self { temperature, gamma: 1.0, identity, profile }
    }

    /// hyprsunset's `mainThreadParseRequest` for the commands the client sends.
    fn handle(&mut self, request: &str) -> String {
        let arg = request
            .split_once(' ')
            .map(|(_, arg)| arg);
        if request.starts_with("temperature") {
            return match arg {
                None => self.temperature.to_string(),
                Some(arg) => match arg.parse() {
                    Ok(kelvin) if (1000..=20000).contains(&kelvin) => {
                        self.temperature = kelvin;
                        self.identity = false;
                        "ok".into()
                    }
                    _ => "Invalid temperature (should be an integer in range 1000-20000)".into(),
                },
            };
        }
        if request.starts_with("gamma") {
            return match arg {
                None => format!("{:.6}", self.gamma * 100.0),
                Some(arg) => {
                    self.gamma = arg.parse::<f32>().unwrap() / 100.0;
                    "ok".into()
                }
            };
        }
        if request.starts_with("identity") {
            return match arg {
                None => {
                    self.identity = true;
                    "ok".into()
                }
                Some("get") => self.identity.to_string(),
                Some("true") => {
                    self.identity = true;
                    "ok".into()
                }
                Some("false") => {
                    self.identity = false;
                    "ok".into()
                }
                Some(_) => "Invalid identity value (should be true or false)".into(),
            };
        }
        if request == "reset" {
            if let Some(profile) = &self.profile {
                self.temperature = profile.temperature;
                self.gamma = profile.gamma;
                self.identity = profile.identity;
            }
            return "ok".into();
        }
        if request.starts_with("profile") {
            return match &self.profile {
                Some(p) => format!(
                    "Time: 20:00\nTemperature: {}\nGamma: {}\nIdentity: {}",
                    p.temperature, p.gamma, p.identity
                ),
                None => "No profile is currently loaded".into(),
            };
        }
        "invalid command".into()
    }
}

struct FakeSunset {
    sun: Arc<Mutex<Sun>>,
    connections: Arc<Mutex<Vec<Vec<String>>>>,
    task: JoinHandle<()>,
}

impl FakeSunset {
    fn start(dir: &TestDir, sun: Sun) -> Self {
        let listener = UnixListener::bind(dir.paths().sunset()).unwrap();
        let sun = Arc::new(Mutex::new(sun));
        let connections = Arc::new(Mutex::new(Vec::new()));
        let task = tokio::spawn({
            let sun = Arc::clone(&sun);
            let connections = Arc::clone(&connections);
            async move {
                loop {
                    let Ok((mut stream, _)) = listener.accept().await else {
                        return;
                    };
                    let index = {
                        let mut connections = connections.lock().unwrap();
                        connections.push(Vec::new());
                        connections.len() - 1
                    };
                    let mut buf = [0u8; 1024];
                    loop {
                        let read = stream.read(&mut buf).await.unwrap_or(0);
                        if read == 0 {
                            break;
                        }
                        let request = String::from_utf8_lossy(&buf[..read]).into_owned();
                        let reply = sun.lock().unwrap().handle(&request);
                        connections.lock().unwrap()[index].push(request);
                        if stream
                            .write_all(reply.as_bytes())
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });
        Self { sun, connections, task }
    }

    fn requests(&self) -> Vec<String> {
        let connections = self.connections.lock().unwrap();
        for requests in connections.iter() {
            assert_eq!(
                requests.len(),
                1,
                "one request per connection: {connections:?}"
            );
        }
        connections
            .iter()
            .flatten()
            .cloned()
            .collect()
    }

    fn clear(&self) {
        self.connections.lock().unwrap().clear();
    }

    fn sun(&self) -> Sun {
        self.sun.lock().unwrap().clone()
    }
}

impl Drop for FakeSunset {
    fn drop(&mut self) {
        self.task.abort();
    }
}

const READS: [&str; 4] = ["temperature", "identity get", "gamma", "profile"];

fn warm_profile() -> Option<Profile> {
    Some(Profile { temperature: 4000, gamma: 0.8, identity: false })
}

fn identity_profile() -> Option<Profile> {
    Some(Profile { temperature: 6000, gamma: 1.0, identity: true })
}

#[tokio::test]
async fn status_reads_on_one_connection_each() {
    let dir = TestDir::new();
    let fake = FakeSunset::start(&dir, Sun::new(6000, true, None));
    let sunset = Sunset::for_instance(&dir.paths());

    let status = sunset.status().await;
    assert_eq!(
        status,
        Status { available: true, enabled: false, scheduled: false, temperature: 6000 }
    );
    assert_eq!(fake.requests(), READS);
    assert!(
        !fake
            .requests()
            .iter()
            .any(|request| request == "identity"),
        "a bare identity would turn identity on"
    );
    assert!(fake.sun().identity);
}

#[tokio::test]
async fn status_follows_the_profile() {
    let dir = TestDir::new();
    let fake = FakeSunset::start(&dir, Sun::new(4000, false, warm_profile()));
    fake.sun.lock().unwrap().gamma = 0.8;
    let sunset = Sunset::for_instance(&dir.paths());
    assert_eq!(
        sunset.status().await,
        Status { available: true, enabled: true, scheduled: true, temperature: 4000 }
    );

    fake.sun.lock().unwrap().temperature = 4500;
    assert!(!sunset.status().await.scheduled);

    *fake.sun.lock().unwrap() = Sun::new(5000, true, identity_profile());
    let status = sunset.status().await;
    assert!(status.scheduled && !status.enabled);
}

#[tokio::test]
async fn toggle_on_and_off_without_a_profile() {
    let dir = TestDir::new();
    let fake = FakeSunset::start(&dir, Sun::new(6500, true, None));
    let sunset = Sunset::for_instance(&dir.paths());

    sunset.toggle().await;
    let mut expected = READS.to_vec();
    expected.push("temperature 4500");
    assert_eq!(fake.requests(), expected);
    let sun = fake.sun();
    assert!(!sun.identity);
    assert_eq!(sun.temperature, 4500);

    fake.clear();
    sunset.toggle().await;
    let mut expected = READS.to_vec();
    expected.extend(["identity true", "gamma 100"]);
    assert_eq!(fake.requests(), expected);
    assert!(fake.sun().identity);

    fake.sun.lock().unwrap().temperature = 3500;
    fake.clear();
    sunset.toggle().await;
    assert_eq!(
        fake.requests()
            .last()
            .map(String::as_str),
        Some("temperature 3500")
    );
    assert!(!fake.sun().identity);
}

#[tokio::test]
async fn toggle_with_profiles_resets() {
    let dir = TestDir::new();
    let fake = FakeSunset::start(&dir, Sun::new(6000, true, warm_profile()));
    let sunset = Sunset::for_instance(&dir.paths());
    sunset.toggle().await;
    assert_eq!(
        fake.requests()
            .last()
            .map(String::as_str),
        Some("reset")
    );
    let sun = fake.sun();
    assert!(!sun.identity);
    assert_eq!(sun.temperature, 4000);

    // Off with a warm profile cannot reset, so it sets identity and full gamma.
    fake.clear();
    sunset.toggle().await;
    assert_eq!(
        &fake.requests()[4..],
        ["identity true", "gamma 100"]
    );

    let dir = TestDir::new();
    let fake = FakeSunset::start(&dir, Sun::new(4000, false, identity_profile()));
    let sunset = Sunset::for_instance(&dir.paths());
    sunset.toggle().await;
    assert_eq!(&fake.requests()[4..], ["reset"]);
    assert!(fake.sun().identity);
}

#[tokio::test]
async fn resume_schedule_resets() {
    let dir = TestDir::new();
    let fake = FakeSunset::start(&dir, Sun::new(6500, true, warm_profile()));
    let sunset = Sunset::for_instance(&dir.paths());
    sunset.resume_schedule().await;
    assert_eq!(&fake.requests()[4..], ["reset"]);
    assert_eq!(fake.sun().temperature, 4000);
}

#[tokio::test]
async fn a_missing_socket_is_unavailable() {
    let dir = TestDir::new();
    let sunset = Sunset::for_instance(&dir.paths());
    assert_eq!(sunset.status().await, Status::UNAVAILABLE);
    sunset.toggle().await;
    sunset.resume_schedule().await;
    assert!(
        sunset
            .request("temperature")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_temperature_that_is_not_a_number_stops_after_one_read() {
    let dir = TestDir::new();
    let listener = UnixListener::bind(dir.paths().sunset()).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let task = tokio::spawn({
        let requests = Arc::clone(&requests);
        async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 64];
                let read = stream.read(&mut buf).await.unwrap();
                requests
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf[..read]).into_owned());
                let _ = stream
                    .write_all(b"invalid command")
                    .await;
            }
        }
    });
    let sunset = Sunset::for_instance(&dir.paths());
    assert_eq!(sunset.status().await, Status::UNAVAILABLE);
    sunset.toggle().await;
    assert_eq!(
        *requests.lock().unwrap(),
        ["temperature", "temperature"]
    );
    task.abort();
}

#[tokio::test]
async fn a_silent_hyprsunset_times_out() {
    let dir = TestDir::new();
    let listener = UnixListener::bind(dir.paths().sunset()).unwrap();
    let task = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    let sunset = Sunset::for_instance(&dir.paths()).with_timeout(Duration::from_millis(100));
    let start = std::time::Instant::now();
    assert_eq!(sunset.status().await, Status::UNAVAILABLE);
    assert!(start.elapsed() < Duration::from_secs(1));
    task.abort();
}
