//! The share picker with fake Hyprland state and no capture.

mod share_common;

use std::assert_matches;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use deskshell::panel::share::{Message, Screen, SharePicker, key_message};
use deskshell::panel::{self, Contents, Effect, Env, Sources};
use deskshell::panels::{Manager, Op, Panel};
use deskshell::pickers::{Caller, Chosen, ShareReply};
use deskshell_core::share::Arrow;
use deskshell_hypr::{Address, State};
use deskshell_ui::Theme;
use deskshell_ui::anim::Lerp;
use deskshell_wl::Target;
use iced::keyboard::key::Named;
use iced::keyboard::{Key, Modifiers};
use share_common::{A1, A2, B1, B2, C1, LEFT, RIGHT, list, screens, state};
use tokio::sync::watch;

struct Desk {
    theme: Theme,
    sources: Sources,
    screens: Vec<Screen>,
    sender: watch::Sender<State>,
    receiver: watch::Receiver<State>,
}

impl Desk {
    fn new(state: State) -> Self {
        let (sender, receiver) = watch::channel(state);
        Self {
            theme: Theme::default(),
            sources: Sources::default(),
            screens: screens(),
            sender,
            receiver,
        }
    }

    fn env(&self) -> Env<'_> {
        let mut env = self.sources.env(&self.theme);
        env.hypr_state = Some(&self.receiver);
        env.screens = &self.screens;
        env
    }

    fn open(&self, picker: &mut SharePicker, purpose: &str) -> Vec<Effect> {
        picker.prepare(list(), purpose.to_owned());
        let _ = picker.open(&self.env());
        self.send(picker, Message::Opened)
    }

    fn send(&self, picker: &mut SharePicker, message: Message) -> Vec<Effect> {
        let mut effects = Vec::new();
        let _ = picker.update(message, &self.env(), &mut effects);
        effects
    }

    fn send_at(&self, picker: &mut SharePicker, message: Message, now: Instant) {
        let _ = picker.update_at(message, &self.env(), &mut Vec::new(), now);
    }

    fn press(&self, picker: &mut SharePicker, keys: &[Press]) -> usize {
        for key in keys {
            let message = match key {
                Press::Up => Message::Move(Arrow::Up),
                Press::Down => Message::Move(Arrow::Down),
                Press::Left => Message::Move(Arrow::Left),
                Press::Right => Message::Move(Arrow::Right),
                Press::Tab => Message::Tab { backwards: false },
                Press::Back => Message::Tab { backwards: true },
            };
            let _ = self.send(picker, message);
        }
        picker.selected()
    }
}

#[derive(Debug, Clone, Copy)]
enum Press {
    Up,
    Down,
    Left,
    Right,
    Tab,
    Back,
}

const FIRST_WINDOW: usize = 3;

fn ids(picker: &SharePicker) -> Vec<&str> {
    picker
        .windows()
        .iter()
        .map(|window| window.id.as_str())
        .collect()
}

fn reply(effects: &[Effect]) -> Option<&str> {
    match effects {
        [Effect::ShareReply(line), Effect::Close] => Some(line),
        _ => None,
    }
}

#[test]
fn the_focused_window_is_selected_at_open() {
    let desk = Desk::new(state());
    let mut picker = SharePicker::default();
    let effects = desk.open(&mut picker, "share");
    assert_eq!(
        ids(&picker),
        ["105", "101", "102", "104", "103", "106"]
    );
    assert_eq!(
        picker.addresses().collect::<Vec<_>>(),
        [Some(C1), Some(A1), Some(A2), Some(B2), Some(B1), None]
    );
    assert_eq!(
        picker.selected(),
        FIRST_WINDOW + 4,
        "0xb1 is active"
    );
    assert!(!picker.recording());
    assert!(!picker.remember());
    assert_eq!(
        effects,
        [
            Effect::RefreshWindows,
            Effect::CaptureTargets(vec![
                Target::Output(String::from(LEFT)),
                Target::Output(String::from(RIGHT)),
                Target::Window(C1),
                Target::Window(A1),
                Target::Window(A2),
                Target::Window(B2),
                Target::Window(B1),
            ]),
        ],
        "every screen and every known window, not the unknown one"
    );
}

#[test]
fn the_first_screen_is_selected_without_the_focused_window() {
    let mut state = state();
    state.active_window = Some(Address(0xfff));
    let desk = Desk::new(state);
    let mut picker = SharePicker::default();
    let _ = desk.open(&mut picker, "share");
    assert_eq!(picker.selected(), 0);

    let mut state = share_common::state();
    state.active_window = None;
    let desk = Desk::new(state);
    let _ = desk.open(&mut picker, "share");
    assert_eq!(picker.selected(), 0);
}

#[test]
fn keys_map() {
    let none = Modifiers::empty();
    let key = |named| Key::Named(named);
    assert_matches!(
        key_message(&key(Named::Enter), none),
        Some(Message::Choose)
    );
    assert_matches!(
        key_message(&key(Named::Tab), none),
        Some(Message::Tab { backwards: false })
    );
    assert_matches!(
        key_message(&key(Named::Tab), Modifiers::SHIFT),
        Some(Message::Tab { backwards: true })
    );
    assert_matches!(
        key_message(&key(Named::ArrowLeft), none),
        Some(Message::Move(Arrow::Left))
    );
    assert_matches!(
        key_message(&key(Named::ArrowRight), none),
        Some(Message::Move(Arrow::Right))
    );
    assert_matches!(
        key_message(&key(Named::ArrowUp), none),
        Some(Message::Move(Arrow::Up))
    );
    assert_matches!(
        key_message(&key(Named::ArrowDown), none),
        Some(Message::Move(Arrow::Down))
    );
    assert_matches!(
        key_message(&key(Named::Space), none),
        Some(Message::Remember)
    );
    assert!(
        key_message(&key(Named::Escape), none).is_none(),
        "Esc is the shell's"
    );
    assert!(key_message(&Key::Character("a".into()), none).is_none());
}

#[test]
fn arrows_and_tab_move_through_the_items() {
    use Press::{Back, Down, Left, Right, Tab, Up};
    let desk = Desk::new(state());
    let mut picker = SharePicker::default();
    let _ = desk.open(&mut picker, "share");
    assert_eq!(
        picker.grid(&desk.env()).columns,
        3,
        "a 2560 px output fits three columns"
    );
    let steps: [(&[Press], usize, &str); 15] = [
        (&[Up], 4, "up a row"),
        (&[Up], 4, "the top row"),
        (&[Left], 3, "left"),
        (
            &[Left],
            0,
            "the first column goes to the first screen",
        ),
        (&[Left], 0, "the left edge"),
        (&[Up], 0, "the top edge"),
        (&[Down, Down], 2, "the region"),
        (&[Down], 2, "the bottom edge"),
        (&[Right], 3, "into the grid"),
        (
            &[Right, Right, Right],
            6,
            "right runs on to the next row",
        ),
        (&[Down], 6, "the bottom row"),
        (&[Right, Right], 8, "right"),
        (&[Right], 8, "the last window"),
        (&[Tab], 0, "Tab wraps"),
        (&[Back], 8, "Backtab wraps"),
    ];
    for (keys, expected, what) in steps {
        assert_eq!(desk.press(&mut picker, keys), expected, "{what}");
    }
    let _ = desk.send(&mut picker, Message::Select(1));
    assert_eq!(picker.selected(), 1, "a click selects");
    let _ = desk.send(&mut picker, Message::Select(99));
    assert_eq!(picker.selected(), 1, "past the end");
}

#[test]
fn enter_replies_with_the_selection_and_closes() {
    let desk = Desk::new(state());
    let mut picker = SharePicker::default();
    let _ = desk.open(&mut picker, "share");
    assert_eq!(
        reply(&desk.send(&mut picker, Message::Choose)),
        Some("/window:103")
    );
    let _ = desk.send(&mut picker, Message::Select(1));
    assert_eq!(
        reply(&desk.send(&mut picker, Message::Choose)),
        Some("/screen:DP-2")
    );
    let _ = desk.send(&mut picker, Message::Select(2));
    assert_eq!(
        reply(&desk.send(&mut picker, Message::Choose)),
        Some("/region")
    );
    assert_eq!(
        reply(&desk.send(&mut picker, Message::Confirm(FIRST_WINDOW + 5))),
        Some("/window:106"),
        "a double click chooses, also a window Hyprland does not know"
    );
}

#[test]
fn space_toggles_remember_unless_recording() {
    let desk = Desk::new(state());
    let mut picker = SharePicker::default();
    let _ = desk.open(&mut picker, "share");
    let _ = desk.send(&mut picker, Message::Remember);
    assert!(picker.remember());
    assert_eq!(
        reply(&desk.send(&mut picker, Message::Choose)),
        Some("r/window:103")
    );
    let _ = desk.send(&mut picker, Message::Select(0));
    assert_eq!(
        reply(&desk.send(&mut picker, Message::Choose)),
        Some("r/screen:DP-1")
    );
    let _ = desk.send(&mut picker, Message::Remember);
    assert!(!picker.remember());
    let _ = desk.send(&mut picker, Message::SetRemember(true));
    assert!(picker.remember(), "the switch");
    let _ = desk.open(&mut picker, "share");
    assert!(!picker.remember(), "open resets it");

    let _ = desk.open(&mut picker, "record");
    assert!(picker.recording());
    let _ = desk.send(&mut picker, Message::Remember);
    let _ = desk.send(&mut picker, Message::SetRemember(true));
    assert!(!picker.remember(), "never while recording");
    assert_eq!(
        reply(&desk.send(&mut picker, Message::Choose)),
        Some("/window:103")
    );
}

#[test]
fn the_labels_follow_the_selection_and_the_purpose() {
    let desk = Desk::new(state());
    let mut picker = SharePicker::default();
    let _ = desk.open(&mut picker, "share");
    let grid = picker.grid(&desk.env());
    assert_eq!(
        grid.label(picker.selected(), picker.recording()),
        "Share window"
    );
    assert_eq!(grid.label(0, false), "Share screen");
    assert_eq!(grid.label(2, false), "Select region");
    let _ = desk.open(&mut picker, "record");
    assert_eq!(
        grid.label(picker.selected(), picker.recording()),
        "Record window"
    );
}

#[test]
fn a_window_that_closes_loses_its_preview() {
    let desk = Desk::new(state());
    let mut picker = SharePicker::default();
    let _ = desk.open(&mut picker, "share");
    desk.sender.send_modify(|state| {
        state
            .windows
            .retain(|window| window.address.0 != B2);
    });
    let effects = desk.send(&mut picker, Message::Changed);
    assert_eq!(
        effects,
        [Effect::CaptureTargets(vec![
            Target::Output(String::from(LEFT)),
            Target::Output(String::from(RIGHT)),
            Target::Window(C1),
            Target::Window(A1),
            Target::Window(A2),
            Target::Window(B1),
        ])]
    );
    assert_eq!(
        ids(&picker),
        ["105", "101", "102", "104", "103", "106"],
        "the list stays"
    );
    assert!(
        desk.send(&mut picker, Message::Changed)
            .is_empty(),
        "nothing new to ask for"
    );
}

#[test]
fn screens_that_go_away_clamp_the_selection() {
    let mut desk = Desk::new(state());
    let mut picker = SharePicker::default();
    let _ = desk.open(&mut picker, "share");
    let _ = desk.send(&mut picker, Message::Select(8));
    desk.screens.truncate(1);
    let effects = desk.send(&mut picker, Message::Screens);
    assert_eq!(
        picker.selected(),
        7,
        "one screen, the region and six windows"
    );
    assert_matches!(&effects[..], [Effect::CaptureTargets(targets)] if targets.len() == 6);
}

struct Fake {
    name: &'static str,
    log: Rc<RefCell<Vec<(&'static str, String)>>>,
}

impl Caller for Fake {
    fn answer(self, line: String) {
        self.log
            .borrow_mut()
            .push((self.name, line));
    }

    fn gone(&self) -> bool {
        false
    }
}

#[test]
fn the_reply_on_each_way_of_closing() {
    let desk = Desk::new(state());
    let env = desk.env();
    let mut contents = Contents::default();
    let mut manager: Manager<u32> = Manager::default();
    let mut replies: ShareReply<Fake> = ShareReply::default();
    let log = Rc::new(RefCell::new(Vec::new()));
    let next = Cell::new(0);
    let id = || {
        next.set(next.get() + 1);
        next.get()
    };
    let lines = || -> Vec<(&'static str, String)> { log.borrow_mut().drain(..).collect() };
    let open = |contents: &mut Contents,
                manager: &mut Manager<u32>,
                replies: &mut ShareReply<Fake>,
                name: &'static str| {
        replies.open(Fake { name, log: Rc::clone(&log) });
        contents
            .share
            .prepare(list(), String::from("share"));
        let actions = manager.command(Panel::SharePicker, Op::Show, None, &id);
        let _ = contents.opened(Panel::SharePicker, &env);
        actions
    };
    let update = |contents: &mut Contents, message: panel::Message| {
        let mut effects = Vec::new();
        let _ = contents.update(message, &env, &mut effects);
        effects
    };

    let actions = open(&mut contents, &mut manager, &mut replies, "a");
    assert!(lines().is_empty());
    let effects = update(
        &mut contents,
        panel::Message::Share(Message::Choose),
    );
    let Some(line) = reply(&effects) else { panic!("{effects:?}") };
    assert_eq!(replies.choose(line.to_owned()), Chosen::Answered);
    assert_eq!(lines(), [("a", String::from("/window:103"))]);
    let picker = actions.open.unwrap().id;
    assert_eq!(manager.dismiss(picker), Some(picker));
    assert!(!replies.closed(None));

    let actions = open(&mut contents, &mut manager, &mut replies, "b");
    assert_eq!(
        manager.dismiss(actions.open.unwrap().id),
        actions.open.map(|open| open.id)
    );
    assert!(replies.closed(manager.current().map(|open| open.panel)));
    assert_eq!(lines(), [("b", String::new())]);
    let _ = open(&mut contents, &mut manager, &mut replies, "c");
    assert_eq!(
        update(&mut contents, panel::Message::Backdrop),
        [Effect::Close]
    );
    assert_eq!(
        update(
            &mut contents,
            panel::Message::Share(Message::Cancel)
        ),
        [Effect::Close]
    );
    assert!(replies.closed(None));
    assert_eq!(lines(), [("c", String::new())]);
    assert!(!replies.empty(), "one line per pick");

    let _ = manager.command(Panel::SharePicker, Op::Hide, None, &id);
    let _ = open(&mut contents, &mut manager, &mut replies, "e");
    let actions = manager.command(Panel::Launcher, Op::Show, None, &id);
    assert!(actions.close.is_some());
    assert!(replies.closed(manager.current().map(|open| open.panel)));
    assert_eq!(lines(), [("e", String::new())]);

    let actions = open(&mut contents, &mut manager, &mut replies, "f");
    assert!(lines().is_empty());
    assert!(actions.close.is_some(), "the launcher closes");
    assert!(!replies.closed(manager.current().map(|open| open.panel)));
    assert!(replies.is_pending());

    let _ = open(&mut contents, &mut manager, &mut replies, "g");
    assert_eq!(lines(), [("f", String::new())]);

    let _ = replies.choose(String::from("r/region"));
    let picker = manager.current().unwrap().id;
    assert_eq!(manager.dismiss(picker), Some(picker));
    assert!(!replies.closed(None), "the region keeps the pick");
    assert!(lines().is_empty());
    assert!(replies.region_chosen("region:DP-1@0,0,10,10"));
    assert_eq!(
        lines(),
        [("g", String::from("r/region:DP-1@0,0,10,10"))]
    );

    let _ = open(&mut contents, &mut manager, &mut replies, "h");
    let picker = manager.current().unwrap().id;
    assert!(manager.closed(picker));
    assert!(replies.closed(None));
    assert_eq!(lines(), [("h", String::new())]);
}

#[test]
fn card_borders_fade_with_the_selection_and_the_hover() {
    const MS: Duration = Duration::from_millis(1);
    let mut desk = Desk::new(state());
    let colors = desk.theme.colors;
    let mut picker = SharePicker::default();
    let _ = desk.open(&mut picker, "share");
    let focused = FIRST_WINDOW + 4;
    assert_eq!(picker.selected(), focused);
    assert!(!picker.animating(), "nothing animates on open");
    assert_eq!(picker.border(focused), Some(colors.accent));
    assert_eq!(picker.border(0), Some(colors.border_muted));

    let start = Instant::now();
    desk.send_at(&mut picker, Message::Hover(0, true), start);
    assert!(picker.animating());
    assert_eq!(
        picker.border(0),
        Some(colors.border_muted),
        "the colour moves only on a frame"
    );
    desk.send_at(
        &mut picker,
        Message::Animate(start + 60 * MS),
        start + 60 * MS,
    );
    assert_eq!(
        picker.border(0),
        Some(
            colors
                .border_muted
                .lerp(colors.border, 0.5)
        )
    );
    desk.send_at(
        &mut picker,
        Message::Animate(start + 120 * MS),
        start + 120 * MS,
    );
    assert_eq!(picker.border(0), Some(colors.border));
    assert!(!picker.animating(), "the frames stop at the end");

    let click = start + 200 * MS;
    desk.send_at(&mut picker, Message::Select(0), click);
    assert!(picker.animating());
    desk.send_at(
        &mut picker,
        Message::Animate(click + 120 * MS),
        click + 120 * MS,
    );
    assert_eq!(picker.border(0), Some(colors.accent));
    assert_eq!(picker.border(focused), Some(colors.border_muted));
    assert!(!picker.animating());

    desk.send_at(
        &mut picker,
        Message::Hover(2, true),
        click + 200 * MS,
    );
    assert!(picker.animating());
    desk.screens.truncate(1);
    desk.send_at(&mut picker, Message::Screens, click + 210 * MS);
    assert!(!picker.animating());
    assert_eq!(picker.border(0), Some(colors.accent));
}
