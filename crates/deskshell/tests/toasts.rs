//! The toasts with fake popups and a clock passed in.

use std::sync::Arc;
use std::time::{Duration, Instant};

use deskshell::notification;
use deskshell::toasts::{
    CRITICAL_LIFETIME, LIFETIME, Message, Request, Swipe, TICK, Toasts, WIDTH,
};
use deskshell_apps::Snapshot;
use deskshell_notify::{Action, Command, Entry, Urgency};
use deskshell_ui::Theme;

fn entry(uid: &str, summary: &str) -> Entry {
    Entry {
        uid: uid.to_owned(),
        server_id: 1,
        received_at: 1,
        app_name: String::from("Chat"),
        app_icon: String::new(),
        desktop_entry: String::new(),
        category: String::new(),
        summary: summary.to_owned(),
        body: String::from("Body text"),
        image: None,
        urgency: Urgency::Normal,
        actions: Vec::new(),
        inline_reply: None,
        live: true,
    }
}

fn with_default(mut entry: Entry) -> Entry {
    entry
        .actions
        .push(Action { identifier: String::from("default"), label: String::from("Open") });
    entry
}

fn hide(uid: &str) -> Request {
    Request::Notify(Command::HidePopup(uid.to_owned()))
}

struct Rig {
    toasts: Toasts,
    theme: Theme,
    apps: Snapshot,
    start: Instant,
    requests: Vec<Request>,
}

impl Rig {
    fn new() -> Self {
        Self {
            toasts: Toasts::default(),
            theme: Theme::default(),
            apps: Snapshot::default(),
            start: Instant::now(),
            requests: Vec::new(),
        }
    }

    fn at(&self, ms: u64) -> Instant {
        self.start + Duration::from_millis(ms)
    }

    fn sync(&mut self, popups: &[Arc<Entry>], ms: u64) -> bool {
        let now = self.at(ms);
        self.toasts
            .sync(popups, &self.theme, &self.apps, now)
            .0
    }

    fn send(&mut self, message: Message, ms: u64) -> bool {
        let now = self.at(ms);
        self.toasts
            .update(message, &self.theme, now, &mut self.requests)
    }

    fn key(&self, index: usize) -> u64 {
        self.toasts.toasts()[index].key()
    }

    fn tick(&mut self, from: u64, to: u64) -> Option<u64> {
        let step = TICK.as_millis() as u64;
        let mut ms = from + step;
        while ms <= to {
            let redrew = self.send(Message::Tick(self.at(ms)), ms);
            assert!(!redrew, "a tick changes nothing that shows");
            if self
                .requests
                .iter()
                .any(|request| matches!(request, Request::Notify(Command::HidePopup(_))))
            {
                return Some(ms);
            }
            ms += step;
        }
        None
    }

    fn take(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }
}

#[test]
fn a_toast_ends_after_six_seconds() {
    let mut rig = Rig::new();
    rig.sync(&[Arc::new(entry("a", "Hello"))], 0);
    assert!(rig.toasts.ticking());
    let ended = rig
        .tick(0, 7000)
        .expect("the toast ends");
    assert_eq!(ended, LIFETIME.as_millis() as u64);
    assert_eq!(rig.take(), [hide("a")]);
    assert!(!rig.toasts.ticking(), "an ended countdown stops");
    assert_eq!(rig.tick(ended, ended + 500), None, "it asks once");
}

#[test]
fn a_critical_toast_ends_after_ten_seconds() {
    let mut rig = Rig::new();
    let mut critical = entry("c", "Battery low");
    critical.urgency = Urgency::Critical;
    rig.sync(&[Arc::new(critical)], 0);
    assert_eq!(rig.tick(0, 9950), None);
    assert_eq!(
        rig.tick(9950, 11000),
        Some(CRITICAL_LIFETIME.as_millis() as u64)
    );
}

#[test]
fn hover_pauses_the_countdown() {
    let mut rig = Rig::new();
    rig.sync(&[Arc::new(entry("a", "Hello"))], 0);
    assert_eq!(rig.tick(0, 2000), None);
    let key = rig.key(0);
    assert!(
        !rig.send(Message::Hover(key, true), 2000),
        "hover draws nothing"
    );
    assert!(!rig.toasts.ticking());
    assert_eq!(
        rig.tick(2000, 20000),
        None,
        "no end while hovered"
    );
    rig.send(Message::Hover(key, false), 20000);
    assert_eq!(rig.tick(20000, 30000), Some(24000));
}

#[test]
fn a_swipe_pauses_the_countdown() {
    let mut rig = Rig::new();
    rig.sync(&[Arc::new(entry("a", "Hello"))], 0);
    assert_eq!(rig.tick(0, 1000), None);
    let key = rig.key(0);
    rig.send(Message::Swipe(key, Swipe::Start), 1000);
    rig.send(Message::Swipe(key, Swipe::Move(40.0)), 1100);
    assert_eq!(rig.tick(1000, 9000), None);
    rig.send(Message::Swipe(key, Swipe::End(40.0)), 9000);
    assert!(
        rig.take().is_empty(),
        "a short swipe hides nothing"
    );
    assert_eq!(rig.tick(9000, 20000), Some(14000));
}

#[test]
fn expanding_pauses_the_countdown() {
    let mut rig = Rig::new();
    rig.sync(&[Arc::new(entry("a", "Hello"))], 0);
    assert_eq!(rig.tick(0, 1000), None);
    let key = rig.key(0);
    assert!(rig.send(
        Message::Entry(key, notification::Message::ToggleExpand),
        1000
    ));
    assert!(
        rig.toasts
            .expanded(&rig.toasts.toasts()[0])
    );
    assert_eq!(rig.tick(1000, 30000), None);
    rig.send(
        Message::Entry(key, notification::Message::ToggleExpand),
        30000,
    );
    assert_eq!(rig.tick(30000, 40000), Some(35000));
}

#[test]
fn a_replaced_entry_starts_the_countdown_again() {
    let mut rig = Rig::new();
    rig.sync(&[Arc::new(entry("a", "first"))], 0);
    for frame in (0..=300).step_by(16) {
        rig.send(Message::Frame(rig.at(frame)), frame);
    }
    assert_eq!(rig.toasts.toasts()[0].x(), 0.0);
    let key = rig.key(0);
    assert_eq!(rig.tick(0, 4000), None);
    assert!(
        rig.sync(&[Arc::new(entry("a", "second"))], 4000),
        "a replace shows"
    );
    let toast = &rig.toasts.toasts()[0];
    assert_eq!(toast.key(), key, "the same toast");
    assert_eq!(toast.entry().summary, "second");
    assert_eq!(
        toast.x(),
        0.0,
        "the enter slide does not run again"
    );
    assert!(!rig.toasts.animating());
    assert_eq!(rig.tick(4000, 20000), Some(10000));

    let mut rig = Rig::new();
    let same = Arc::new(entry("a", "first"));
    rig.sync(std::slice::from_ref(&same), 0);
    assert!(
        !rig.sync(std::slice::from_ref(&same), 100),
        "the same Arc changes nothing"
    );
}

#[test]
fn a_swipe_past_thirty_percent_hides_the_toast() {
    let mut rig = Rig::new();
    rig.sync(&[Arc::new(entry("a", "Hello"))], 0);
    let key = rig.key(0);
    rig.send(Message::Swipe(key, Swipe::Start), 1000);
    assert!(rig.send(Message::Swipe(key, Swipe::Move(200.0)), 1050));
    assert_eq!(
        rig.toasts.toasts()[0].x(),
        200.0,
        "the card follows the pointer"
    );
    rig.send(
        Message::Swipe(key, Swipe::End(WIDTH * 0.3 + 1.0)),
        1100,
    );
    assert_eq!(rig.take(), [hide("a")]);
}

#[test]
fn a_short_swipe_slides_back() {
    let mut rig = Rig::new();
    rig.sync(&[Arc::new(entry("a", "Hello"))], 0);
    for frame in (0..=300).step_by(16) {
        rig.send(Message::Frame(rig.at(frame)), frame);
    }
    let key = rig.key(0);
    rig.send(Message::Swipe(key, Swipe::Start), 1000);
    rig.send(Message::Swipe(key, Swipe::Move(-30.0)), 1020);
    assert_eq!(
        rig.toasts.toasts()[0].x(),
        0.0,
        "a swipe never moves left"
    );
    rig.send(
        Message::Swipe(key, Swipe::Move(WIDTH * 0.3)),
        1050,
    );
    rig.send(Message::Swipe(key, Swipe::End(WIDTH * 0.3)), 1100);
    assert!(rig.take().is_empty(), "exactly 30% stays");
    assert!(rig.toasts.animating());
    rig.send(Message::Frame(rig.at(1180)), 1180);
    let halfway = rig.toasts.toasts()[0].x();
    assert!(
        halfway > 0.0 && halfway < WIDTH * 0.3,
        "{halfway}"
    );
    rig.send(Message::Frame(rig.at(1260)), 1260);
    assert_eq!(rig.toasts.toasts()[0].x(), 0.0);
    assert!(
        !rig.toasts.animating(),
        "frames stop with the slide"
    );
}

#[test]
fn the_enter_slide_and_fade() {
    let mut rig = Rig::new();
    rig.sync(&[Arc::new(entry("a", "Hello"))], 0);
    let toast = &rig.toasts.toasts()[0];
    assert_eq!((toast.x(), toast.alpha()), (60.0, 0.0));
    assert!(rig.toasts.animating());
    rig.send(Message::Frame(rig.at(120)), 120);
    let toast = &rig.toasts.toasts()[0];
    assert!(toast.x() > 0.0 && toast.x() < 60.0);
    assert!(toast.alpha() > 0.0 && toast.alpha() < 1.0);
    rig.send(Message::Frame(rig.at(240)), 240);
    let toast = &rig.toasts.toasts()[0];
    assert_eq!((toast.x(), toast.alpha()), (0.0, 1.0));
    assert!(!rig.toasts.animating());
}

#[test]
fn a_click_invokes_the_default_action_of_a_live_entry() {
    let mut rig = Rig::new();
    rig.sync(&[Arc::new(with_default(entry("a", "Hello")))], 0);
    rig.send(Message::Click(rig.key(0)), 500);
    assert_eq!(
        rig.take(),
        [
            Request::Notify(Command::Invoke(
                String::from("a"),
                String::from("default")
            )),
            hide("a"),
        ]
    );
}

#[test]
fn a_click_without_a_default_action_opens_the_hub() {
    let mut rig = Rig::new();
    let mut other = entry("a", "Hello");
    other
        .actions
        .push(Action { identifier: String::from("reply"), label: String::from("Reply") });
    rig.sync(&[Arc::new(other)], 0);
    rig.send(Message::Click(rig.key(0)), 500);
    assert_eq!(rig.take(), [Request::ShowHub, hide("a")]);
}

#[test]
fn a_click_on_a_loaded_entry_opens_the_hub() {
    let mut rig = Rig::new();
    let mut loaded = with_default(entry("a", "Hello"));
    loaded.live = false;
    rig.sync(&[Arc::new(loaded)], 0);
    rig.send(Message::Click(rig.key(0)), 500);
    assert_eq!(rig.take(), [Request::ShowHub, hide("a")]);
}

#[test]
fn entry_buttons() {
    let mut rig = Rig::new();
    let mut busy = with_default(entry("a", "Hello"));
    busy.actions
        .push(Action { identifier: String::from("mark"), label: String::from("Mark") });
    rig.sync(&[Arc::new(busy)], 0);
    let key = rig.key(0);
    rig.send(
        Message::Entry(key, notification::Message::Action(1)),
        100,
    );
    assert_eq!(
        rig.take(),
        [
            Request::Notify(Command::Invoke(
                String::from("a"),
                String::from("mark")
            )),
            hide("a"),
        ],
        "an action hides the toast, as actionTriggered does"
    );
    rig.send(
        Message::Entry(key, notification::Message::Close),
        200,
    );
    assert_eq!(rig.take(), [hide("a")]);
}

#[test]
fn three_toasts_at_most_and_the_pill_counts_the_rest() {
    let mut rig = Rig::new();
    let popups: Vec<Arc<Entry>> = (1..=5)
        .rev()
        .map(|n| Arc::new(entry(&format!("{n}"), &format!("Message {n}"))))
        .collect();
    rig.sync(&popups, 0);
    let shown: Vec<&str> = rig
        .toasts
        .toasts()
        .iter()
        .map(|toast| toast.entry().uid.as_str())
        .collect();
    assert_eq!(shown, ["5", "4", "3"], "newest first");
    assert_eq!(rig.toasts.hidden(), 2);
    assert_eq!(rig.toasts.more_label(), "2 more");
    rig.send(Message::More, 100);
    assert_eq!(rig.take(), [Request::ShowHub]);

    rig.sync(&popups[..3], 200);
    assert_eq!(rig.toasts.hidden(), 0);
    assert_eq!(rig.toasts.more_label(), "");
}

#[test]
fn a_toast_keeps_its_state_when_a_newer_one_arrives() {
    let mut rig = Rig::new();
    let a = Arc::new(entry("a", "older"));
    rig.sync(std::slice::from_ref(&a), 0);
    let key = rig.key(0);
    rig.send(Message::Hover(key, true), 100);
    let b = Arc::new(entry("b", "newer"));
    rig.sync(&[Arc::clone(&b), Arc::clone(&a)], 1000);
    let toasts = rig.toasts.toasts();
    assert_eq!(
        toasts[1].key(),
        key,
        "the older toast moved down with its state"
    );
    assert!(!toasts[1].running(), "still hovered");
    assert!(toasts[0].running());
}

#[test]
fn a_toast_pushed_out_starts_over_when_it_returns() {
    let mut rig = Rig::new();
    let entries: Vec<Arc<Entry>> = (0..4)
        .map(|n| Arc::new(entry(&format!("{n}"), "x")))
        .collect();
    rig.sync(&entries[..3], 0);
    assert_eq!(rig.tick(0, 3000), None);
    let old = rig.key(2);
    rig.sync(
        &[
            Arc::clone(&entries[3]),
            Arc::clone(&entries[0]),
            Arc::clone(&entries[1]),
            Arc::clone(&entries[2]),
        ],
        3000,
    );
    rig.sync(&entries[..3], 3500);
    let back = &rig.toasts.toasts()[2];
    assert_ne!(back.key(), old, "a new toast");
    assert_eq!(back.elapsed(), Duration::ZERO);
}

#[test]
fn ticks_change_nothing_that_shows() {
    let mut rig = Rig::new();
    rig.sync(&[Arc::new(entry("a", "Hello"))], 0);
    for frame in (0..=300).step_by(16) {
        rig.send(Message::Frame(rig.at(frame)), frame);
    }
    let before: Vec<(f32, f32, String)> = rig
        .toasts
        .toasts()
        .iter()
        .map(|toast| (toast.x(), toast.alpha(), toast.label().to_owned()))
        .collect();
    assert!(!Message::Tick(rig.at(0)).redraws());
    assert_eq!(rig.tick(300, 3000), None);
    let after: Vec<(f32, f32, String)> = rig
        .toasts
        .toasts()
        .iter()
        .map(|toast| (toast.x(), toast.alpha(), toast.label().to_owned()))
        .collect();
    assert_eq!(before, after);
    assert!(
        !rig.toasts.animating(),
        "no frames once the toast stands"
    );
}

#[test]
fn the_time_label_shows_the_app_name_without_an_icon() {
    let mut rig = Rig::new();
    let mut critical = entry("c", "Low");
    critical.urgency = Urgency::Critical;
    rig.sync(
        &[Arc::new(entry("a", "Hello")), Arc::new(critical)],
        0,
    );
    assert_eq!(rig.toasts.toasts()[0].label(), "Chat · now");
    assert_eq!(rig.toasts.toasts()[1].label(), "Chat · critical");
}

#[test]
fn a_lost_surface_forgets_hover_and_swipe() {
    let mut rig = Rig::new();
    rig.sync(
        &[Arc::new(entry("a", "Hello")), Arc::new(entry("b", "Other"))],
        0,
    );
    let (a, b) = (rig.key(0), rig.key(1));
    assert_eq!(rig.tick(0, 1000), None);
    rig.send(Message::Hover(a, true), 1000);
    rig.send(Message::Swipe(b, Swipe::Start), 1000);
    rig.send(Message::Swipe(b, Swipe::Move(50.0)), 1050);
    assert!(!rig.toasts.ticking(), "both paused");
    rig.toasts.surface_lost(rig.at(2000));
    assert!(
        rig.toasts
            .toasts()
            .iter()
            .all(|toast| toast.running())
    );
    assert_eq!(
        rig.toasts.toasts()[1].x(),
        0.0,
        "the swiped card is back in place"
    );
    assert_eq!(
        rig.tick(2000, 10000),
        Some(7000),
        "the countdowns go on"
    );
}
