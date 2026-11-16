//! Lock subscription streams. Each yields only on change, as any message redraws every surface.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use deskshell_auth::faillock::{self, Config, Status};
use deskshell_auth::key;
use deskshell_auth::touch::TouchState;
use deskshell_ipc::Reply;
use iced::futures::stream::{self, BoxStream};
use iced::futures::{Stream, StreamExt};
use tokio::sync::{mpsc, watch};
use tokio::time::sleep;

const KEY_PRESENT: Duration = Duration::from_secs(2);
const KEY_MISSING: Duration = Duration::from_millis(500);
const FAILLOCK: Duration = Duration::from_secs(2);
pub const NOTIFICATIONS: Duration = Duration::from_secs(10);
const NOTIFICATIONS_TIMEOUT: Duration = Duration::from_millis(300);
/// After a second boundary, so the new second has begun when it is read.
const MARGIN: Duration = Duration::from_millis(5);

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// `Subscription::run_with` needs `Hash` data, and receivers are neither `Hash` nor `Clone`, so
/// this hashes by identity and the stream takes the value when it starts.
#[derive(Debug)]
pub struct Feed<T>(Arc<Mutex<Option<T>>>);

impl<T> Feed<T> {
    pub fn new(value: T) -> Self {
        Self(Arc::new(Mutex::new(Some(value))))
    }

    pub fn take(&self) -> Option<T> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

impl<T> Clone for Feed<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> Hash for Feed<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0)
            .cast::<()>()
            .hash(state);
    }
}

fn on_change<T, F, P>(next: F, period: P) -> impl Stream<Item = T>
where
    T: Clone + PartialEq + Send + 'static,
    F: Fn() -> T + Send + 'static,
    P: Fn(&T) -> Duration + Send + 'static,
{
    stream::unfold(
        (next, period, None::<T>),
        |(next, period, last)| async move {
            loop {
                if let Some(last) = &last {
                    sleep(period(last)).await;
                }
                let value = next();
                if last.as_ref() != Some(&value) {
                    return Some((value.clone(), (next, period, Some(value))));
                }
            }
        },
    )
}

pub fn key_presence(_: &()) -> impl Stream<Item = bool> + use<> {
    on_change(
        || key::key_present(Path::new(key::HIDRAW_CLASS)),
        |present| if *present { KEY_PRESENT } else { KEY_MISSING },
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Faillock {
    pub config: Config,
    pub user: String,
}

impl Faillock {
    pub fn status(&self) -> Status {
        faillock::evaluate(
            &self.config,
            &faillock::read_tally(&self.config.dir, &self.user),
            now_secs(),
        )
    }
}

pub fn faillock(source: &Faillock) -> impl Stream<Item = Status> + use<> {
    let source = source.clone();
    on_change(move || source.status(), |_| FAILLOCK)
}

pub fn remaining(until: u64, now: u64) -> Remaining {
    let left = until.saturating_sub(now);
    if left < 60 { Remaining::Seconds(left) } else { Remaining::Minutes(left.div_ceil(60)) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remaining {
    Seconds(u64),
    Minutes(u64),
}

pub fn lockout_ticks(until: &u64) -> impl Stream<Item = ()> + use<> {
    let until = *until;
    stream::unfold(
        remaining(until, now_secs()),
        move |last| async move {
            if last == Remaining::Seconds(0) {
                return None;
            }
            loop {
                let into = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .subsec_nanos();
                let wait = Duration::from_nanos(1_000_000_000 - u64::from(into)) + MARGIN;
                sleep(wait).await;
                let now = remaining(until, now_secs());
                if now != last {
                    return Some(((), now));
                }
            }
        },
    )
}

pub fn notification_count(_: &()) -> impl Stream<Item = Option<u32>> + use<> {
    // The chip starts hidden, so a first `None` changes nothing and is not sent.
    stream::unfold((true, None::<u32>), |(first, last)| async move {
        loop {
            if !first {
                sleep(NOTIFICATIONS).await;
            }
            let count = tokio::task::spawn_blocking(ask_count)
                .await
                .unwrap_or(None);
            if count != last {
                return Some((count, (false, count)));
            }
            if first {
                sleep(NOTIFICATIONS).await;
            }
        }
    })
}

fn ask_count() -> Option<u32> {
    let socket = deskshell_ipc::socket_path().ok()?;
    let reply = deskshell_ipc::call(
        &socket,
        "notifications",
        "count",
        &[],
        NOTIFICATIONS_TIMEOUT,
    )
    .ok()?;
    match reply {
        Reply::Text(text) => text
            .trim()
            .parse()
            .ok()
            .filter(|count| *count > 0),
        _ => None,
    }
}

pub fn touch(feed: &Feed<watch::Receiver<TouchState>>) -> BoxStream<'static, TouchState> {
    let Some(receiver) = feed.take() else {
        return stream::empty().boxed();
    };
    stream::unfold(
        (receiver, true),
        |(mut receiver, first)| async move {
            if !first {
                receiver.changed().await.ok()?;
            }
            let value = *receiver.borrow_and_update();
            Some((value, (receiver, false)))
        },
    )
    .boxed()
}

pub fn layout(feed: &Feed<watch::Receiver<deskshell_hypr::State>>) -> BoxStream<'static, String> {
    let Some(receiver) = feed.take() else {
        return stream::empty().boxed();
    };
    stream::unfold(
        (receiver, None::<String>),
        |(mut receiver, last)| async move {
            loop {
                {
                    let state = receiver.borrow_and_update();
                    if state.connected && last.as_deref() != Some(state.keyboard_layout.as_str()) {
                        let layout = state.keyboard_layout.clone();
                        drop(state);
                        return Some((layout.clone(), (receiver, Some(layout))));
                    }
                }
                receiver.changed().await.ok()?;
            }
        },
    )
    .boxed()
}

pub fn reports<T: Send + 'static>(
    feed: &Feed<mpsc::UnboundedReceiver<T>>,
) -> BoxStream<'static, T> {
    let Some(receiver) = feed.take() else {
        return stream::empty().boxed();
    };
    stream::unfold(receiver, |mut receiver| async move {
        let report = receiver.recv().await?;
        Some((report, receiver))
    })
    .boxed()
}

pub fn avatar_path() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(|home| Path::new(&home).join(".config"))
        })?;
    Some(config.join("deskshell/avatar.png"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaining_reads_like_qs() {
        assert_eq!(remaining(1000, 1000), Remaining::Seconds(0));
        assert_eq!(remaining(1000, 2000), Remaining::Seconds(0));
        assert_eq!(remaining(1000, 941), Remaining::Seconds(59));
        assert_eq!(remaining(1000, 940), Remaining::Minutes(1));
        assert_eq!(remaining(1000, 939), Remaining::Minutes(2));
        assert_eq!(remaining(1000, 400), Remaining::Minutes(10));
    }

    #[test]
    fn on_change_yields_the_first_value_and_then_only_changes() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let values = Arc::new(Mutex::new(vec![1, 1, 1, 2, 2, 3]));
        let source = Arc::clone(&values);
        let next = move || {
            let mut values = source.lock().unwrap();
            if values.len() > 1 { values.remove(0) } else { values[0] }
        };
        let got: Vec<i32> = runtime.block_on(
            on_change(next, |_| Duration::from_millis(1))
                .take(3)
                .collect(),
        );
        assert_eq!(got, [1, 2, 3]);
        assert_eq!(values.lock().unwrap().len(), 1);
    }
}
