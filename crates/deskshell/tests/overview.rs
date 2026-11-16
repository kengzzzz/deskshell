//! The overview panel with fake Hyprland state and no capture.

mod overview_common;

use std::assert_matches;
use std::borrow::Cow;
use std::time::{Duration, Instant};

use deskshell::panel::overview::{Message, Overview, key_message};
use deskshell::panel::{Effect, Env, Message as PanelMessage, Sources};
use deskshell_core::overview::{Direction, Rect, lua};
use deskshell_hypr::{Address, State};
use deskshell_ui::anim::{Lerp, Smoothed};
use deskshell_ui::{Theme, fonts};
use iced::advanced::clipboard;
use iced::advanced::graphics::text::font_system;
use iced::advanced::renderer::Headless;
use iced::keyboard::key::{Code, Named, NativeCode, Physical};
use iced::keyboard::{Key, Modifiers};
use iced::{Color, Event, Pixels, Point, Size, mouse, window};
use iced_test::futures::futures::executor::block_on;
use iced_test::runtime::user_interface::{self, UserInterface};
use overview_common::{LEFT, RIGHT, state, workspace_ref};
use tokio::sync::watch;

const A1: u64 = 0xa1;
const A2: u64 = 0xa2;
const B1: u64 = 0xb1;
const B2: u64 = 0xb2;
const B3: u64 = 0xb3;
const B4: u64 = 0xb4;
const B5: u64 = 0xb5;
const RIGHT_X: f32 = 2560.0;
const MS: Duration = Duration::from_millis(1);

struct Desk {
    theme: Theme,
    sources: Sources,
    sender: watch::Sender<State>,
    receiver: watch::Receiver<State>,
}

impl Desk {
    fn new(state: State) -> Self {
        let (sender, receiver) = watch::channel(state);
        Self { theme: Theme::default(), sources: Sources::default(), sender, receiver }
    }

    fn env(&self) -> Env<'_> {
        let mut env = self.sources.env(&self.theme);
        env.hypr_state = Some(&self.receiver);
        env
    }

    fn open(&self, overview: &mut Overview) -> Vec<Effect> {
        let _ = overview.open(&self.env());
        self.send(overview, Message::Opened)
    }

    fn send(&self, overview: &mut Overview, message: Message) -> Vec<Effect> {
        let mut effects = Vec::new();
        let _ = overview.update(message, &self.env(), &mut effects);
        effects
    }

    fn send_at(&self, overview: &mut Overview, message: Message, now: Instant) -> Vec<Effect> {
        let mut effects = Vec::new();
        let _ = overview.update_at(message, &self.env(), &mut effects, now);
        effects
    }

    fn frame(&self, overview: &mut Overview, now: Instant) {
        let _ = self.send_at(overview, Message::Animate(now), now);
    }

    fn change(&self, overview: &mut Overview, change: impl FnOnce(&mut State)) -> Vec<Effect> {
        self.sender.send_modify(change);
        self.send(overview, Message::Changed)
    }
}

fn selected(overview: &Overview) -> Option<u64> {
    overview.selected_address()
}

fn card(overview: &Overview, address: u64) -> Rect {
    let index = overview
        .windows()
        .iter()
        .position(|window| window.address == address)
        .expect("a window with this address");
    let (_, slot) = overview
        .lanes()
        .find(index)
        .expect("on a lane");
    overview
        .layout()
        .card_rect(
            overview.lanes(),
            slot,
            overview.list_scroll(),
            overview.lane_shown(),
        )
        .expect("a card")
}

fn lane(overview: &Overview, id: i64) -> usize {
    overview
        .lanes()
        .lanes
        .iter()
        .position(|lane| lane.id == id)
        .expect("a lane for the workspace")
}

fn center(rect: Rect) -> (f32, f32) {
    (
        rect.x + rect.width / 2.0,
        rect.y + rect.height / 2.0,
    )
}

fn drag(desk: &Desk, overview: &mut Overview, address: u64, x: f32, y: f32) -> Vec<Effect> {
    let from = card(overview, address);
    let (start_x, start_y) = center(from);
    let mut effects = desk.send(
        overview,
        Message::DragStart {
            address,
            grab: (from.width / 2.0, from.height / 2.0),
            size: (from.width, from.height),
            pointer: (start_x + 20.0, start_y),
        },
    );
    effects.extend(desk.send(overview, Message::DragMove(x, y)));
    effects.extend(desk.send(overview, Message::Drop(x, y)));
    assert!(overview.drag().is_none(), "a drop ends the drag");
    effects
}

fn dispatches(lua: &[String], refresh: bool) -> Effect {
    Effect::Dispatches { lua: lua.to_vec(), refresh }
}

fn wheel(x: f32, y: f32, lines: f32) -> Message {
    Message::Wheel { x, y, delta: mouse::ScrollDelta::Lines { x: 0.0, y: lines } }
}

#[test]
fn open_selects_the_active_window_and_asks_for_thumbnails() {
    let desk = Desk::new(state());
    let mut overview = Overview::default();
    let effects = desk.open(&mut overview);
    assert_eq!(selected(&overview), Some(B1));
    assert_eq!(
        overview.size(),
        Some((2560.0, 1440.0)),
        "guessed from the focused monitor"
    );
    assert_eq!(
        effects,
        [Effect::RefreshWindows, Effect::Capture(vec![A1, A2, B2, B1, B3, B4, B5])],
        "a fresh window list, and a thumbnail per card in lane order"
    );
    let lanes: Vec<(i64, bool)> = overview
        .lanes()
        .lanes
        .iter()
        .map(|lane| (lane.id, lane.phantom))
        .collect();
    assert_eq!(
        lanes,
        [(1, false), (3, false), (5, false), (6, true)]
    );

    let desk = Desk::new(State { active_window: None, ..state() });
    let mut overview = Overview::default();
    let _ = desk.open(&mut overview);
    assert_eq!(
        selected(&overview),
        Some(A1),
        "no active window: the first one"
    );

    let theme = Theme::default();
    let sources = Sources::default();
    let env = sources.env(&theme);
    let mut overview = Overview::default();
    let _ = overview.open(&env);
    let mut effects = Vec::new();
    let _ = overview.update(Message::Opened, &env, &mut effects);
    assert_eq!(
        effects,
        [Effect::RefreshWindows, Effect::Capture(Vec::new())]
    );
    assert_eq!(overview.lanes().lanes.len(), 1);
    assert_eq!(selected(&overview), None);
}

#[test]
fn keys_map() {
    let none = Modifiers::empty();
    let named = |name| Key::Named(name);
    let character = |c: &str| Key::Character(c.into());
    let unidentified = Physical::Unidentified(NativeCode::Unidentified);
    let message = |key: &Key, modifiers| key_message(key, unidentified, modifiers);
    let pressed = |key: &Key, modifiers| format!("{:?}", message(key, modifiers));

    assert_eq!(
        pressed(&named(Named::Enter), none),
        "Some(Activate)"
    );
    assert_eq!(pressed(&character("3"), none), "Some(MoveTo(3))");
    assert_eq!(pressed(&character("0"), none), "Some(MoveTo(10))");
    assert_eq!(
        pressed(&character("3"), Modifiers::ALT),
        "Some(MoveTo(3))",
        "the digits ignore modifiers"
    );
    assert_eq!(
        pressed(&character("!"), Modifiers::SHIFT),
        "None",
        "Shift+1 is not a digit"
    );
    assert_eq!(
        format!(
            "{:?}",
            key_message(
                &character("ๅ"),
                Physical::Code(Code::Digit1),
                none
            )
        ),
        "Some(MoveTo(1))",
        "the 1 key on a Thai layout"
    );
    assert_eq!(
        pressed(&named(Named::ArrowLeft), none),
        "Some(Select(Left))"
    );
    assert_eq!(
        pressed(&named(Named::ArrowDown), none),
        "Some(Select(Down))"
    );
    assert_eq!(
        pressed(&named(Named::ArrowRight), Modifiers::ALT),
        "Some(Reorder(Right))"
    );
    assert_eq!(
        pressed(&named(Named::ArrowUp), Modifiers::ALT),
        "Some(Reorder(Up))"
    );
    assert_eq!(pressed(&named(Named::Tab), none), "Some(Next)");
    assert_eq!(
        pressed(&named(Named::Tab), Modifiers::SHIFT),
        "None",
        "Backtab in Qt"
    );
    assert_eq!(
        pressed(&named(Named::Escape), none),
        "None",
        "Esc is the shell's"
    );
    assert_eq!(pressed(&character("a"), none), "None");
}

#[test]
fn tab_and_arrows_move_the_selection() {
    let desk = Desk::new(state());
    let mut overview = Overview::default();
    let _ = desk.open(&mut overview);
    let press = |overview: &mut Overview, message: Message| {
        assert!(
            desk.send(overview, message).is_empty(),
            "selection only"
        );
        selected(overview)
    };
    let overview = &mut overview;
    assert_eq!(
        press(overview, Message::Select(Direction::Right)),
        Some(B3)
    );
    assert_eq!(
        press(overview, Message::Select(Direction::Up)),
        Some(A2),
        "the third column clamps to workspace 1's last window"
    );
    assert_eq!(
        press(overview, Message::Select(Direction::Up)),
        Some(A2),
        "no workspace before 1"
    );
    assert_eq!(
        press(overview, Message::Select(Direction::Down)),
        Some(B1),
        "second column"
    );
    assert_eq!(
        press(overview, Message::Select(Direction::Down)),
        Some(B1),
        "workspace 5 has no windows, which stops the move"
    );
    assert_eq!(
        press(overview, Message::Select(Direction::Left)),
        Some(B2)
    );
    assert_eq!(
        press(overview, Message::Select(Direction::Left)),
        Some(B2),
        "the first card"
    );
    assert_eq!(
        press(overview, Message::Select(Direction::Up)),
        Some(A1)
    );
    assert_eq!(
        press(overview, Message::Next),
        Some(B1),
        "Tab follows Hyprland's order"
    );
    for _ in 0..4 {
        press(overview, Message::Next);
    }
    assert_eq!(
        selected(overview),
        Some(0xc1),
        "the special workspace's window"
    );
    assert_eq!(
        press(overview, Message::Select(Direction::Down)),
        Some(A1),
        "a window outside the lanes counts as index -1"
    );
    for _ in 0..7 {
        press(overview, Message::Next);
    }
    assert_eq!(selected(overview), Some(B5));
    assert_eq!(
        press(overview, Message::Next),
        Some(A1),
        "Tab wraps"
    );
}

#[test]
fn keys_and_clicks_dispatch_lua() {
    let desk = Desk::new(state());
    let mut overview = Overview::default();
    let _ = desk.open(&mut overview);

    assert_eq!(
        desk.send(&mut overview, Message::Activate),
        [
            Effect::Close,
            dispatches(
                &[
                    String::from(r#"hl.dsp.focus({ window = "address:0xb1" })"#),
                    String::from(r#"hl.dsp.layout("fit_into_view")"#),
                ],
                false
            ),
        ]
    );
    assert_eq!(
        desk.send(&mut overview, Message::Reorder(Direction::Left)),
        [
            Effect::Close,
            dispatches(
                &[
                    String::from(r#"hl.dsp.focus({ window = "address:0xb1" })"#),
                    String::from(r#"hl.dsp.window.swap({ direction = "left" })"#),
                ],
                false
            ),
        ]
    );
    assert_eq!(
        desk.send(&mut overview, Message::Reorder(Direction::Down)),
        [
            Effect::Close,
            dispatches(
                &[lua::focus(B1), lua::swap(Direction::Down)],
                false
            )
        ]
    );
    assert_eq!(
        desk.send(&mut overview, Message::MoveTo(2)),
        [
            Effect::Close,
            dispatches(
                &[String::from(
                    r#"hl.dsp.window.move({ workspace = 2, follow = false, window = "address:0xb1" })"#
                )],
                false
            ),
        ]
    );
    assert!(
        desk.send(&mut overview, Message::MoveTo(3))
            .is_empty(),
        "its own workspace: nothing, and the overview stays open"
    );
    assert_eq!(
        desk.send(&mut overview, Message::Click(A2)),
        [
            Effect::Close,
            dispatches(
                &[lua::focus(A2), String::from(lua::FIT_ACTIVE)],
                false
            )
        ]
    );
    assert_eq!(
        selected(&overview),
        Some(A2),
        "a click selects first"
    );
}

#[test]
fn drops_move_next_to_windows_and_onto_lanes() {
    let desk = Desk::new(state());
    let mut overview = Overview::default();
    let _ = desk.open(&mut overview);
    let cursor = |x: f32, y: f32| lua::move_cursor(x + RIGHT_X, y);

    let (x, y) = center(card(&overview, B2));
    assert_eq!(
        drag(&desk, &mut overview, B5, x, y),
        [dispatches(
            &[
                lua::focus(B5),
                lua::swap(Direction::Left),
                lua::swap(Direction::Left),
                lua::swap(Direction::Left),
                lua::swap(Direction::Left),
                cursor(x, y),
            ],
            true
        )],
        "the overview stays open and the window list is fetched after the swaps"
    );
    assert!(
        cursor(x, y).contains(&format!("x = {}", (x + RIGHT_X).round())),
        "the cursor goes back to the drop point in global coordinates"
    );
    let (x, y) = center(card(&overview, B3));
    assert_eq!(
        drag(&desk, &mut overview, B1, x, y),
        [dispatches(
            &[lua::focus(B1), lua::swap(Direction::Right), cursor(x, y)],
            true
        )]
    );

    let (x, y) = center(card(&overview, B4));
    assert_eq!(
        drag(&desk, &mut overview, A1, x, y),
        [dispatches(
            &[lua::move_to_workspace(3, A1), cursor(x, y)],
            true
        )]
    );
    let last = card(&overview, A2);
    let (x, y) = (last.x + last.width + 100.0, last.y + 50.0);
    assert_eq!(
        drag(&desk, &mut overview, B2, x, y),
        [dispatches(
            &[lua::move_to_workspace(1, B2), cursor(x, y)],
            true
        )]
    );

    let lane = |index: usize| {
        overview
            .layout()
            .lane_rect(index, overview.list_scroll())
            .unwrap()
    };
    let (own, empty, phantom) = (lane(0), lane(2), lane(3));
    let (x, y) = (empty.x + 300.0, empty.y + 20.0);
    let (px, py) = (phantom.x + 300.0, phantom.y + 20.0);
    assert_eq!(
        drag(&desk, &mut overview, A1, x, y),
        [dispatches(
            &[lua::move_to_workspace(5, A1), cursor(x, y)],
            true
        )]
    );
    assert_eq!(
        drag(&desk, &mut overview, A1, px, py),
        [dispatches(
            &[lua::move_to_workspace(6, A1), cursor(px, py)],
            true
        )]
    );

    let (x, y) = center(card(&overview, A1));
    assert!(drag(&desk, &mut overview, A1, x, y).is_empty());
    assert!(
        drag(
            &desk,
            &mut overview,
            A1,
            own.x + 40.0,
            own.y + 100.0
        )
        .is_empty()
    );
    let header = overview.layout().panel;
    assert!(
        drag(
            &desk,
            &mut overview,
            A1,
            header.x + 200.0,
            header.y + 30.0
        )
        .is_empty()
    );
    assert!(drag(&desk, &mut overview, A1, 5.0, 5.0).is_empty());
}

#[test]
fn drops_on_the_left_monitor_keep_local_coordinates() {
    let mut fixture = state();
    fixture.focused_monitor = Some(String::from(LEFT));
    for monitor in &mut fixture.monitors {
        monitor.focused = monitor.name == LEFT;
    }
    let desk = Desk::new(fixture);
    let mut overview = Overview::default();
    let _ = desk.open(&mut overview);
    assert_eq!(
        overview.global(10.0, 20.0),
        (10.0, 20.0),
        "the layout's origin"
    );
    let (x, y) = center(card(&overview, B3));
    assert_eq!(
        drag(&desk, &mut overview, B1, x, y),
        [dispatches(
            &[lua::focus(B1), lua::swap(Direction::Right), lua::move_cursor(x, y)],
            true
        )]
    );

    let desk = Desk::new(state());
    let _ = desk.open(&mut overview);
    assert_eq!(
        overview.global(10.0, 20.0),
        (2570.0, 20.0),
        "offset by the right monitor's position"
    );
    assert_eq!(state().monitors[1].name, RIGHT);
}

#[test]
fn closing_windows_clamp_the_selection_and_end_a_drag() {
    let desk = Desk::new(state());
    let mut overview = Overview::default();
    let _ = desk.open(&mut overview);
    for _ in 0..6 {
        desk.send(&mut overview, Message::Next);
    }
    assert_eq!(selected(&overview), Some(B5), "the last window");
    let effects = desk.change(&mut overview, |state| {
        state
            .windows
            .retain(|window| window.address != Address(B5));
    });
    assert_eq!(selected(&overview), Some(B4), "clampSelection");
    assert_eq!(
        effects,
        [Effect::Capture(vec![A1, A2, B2, B1, B3, B4])],
        "the closed window's thumbnail stops"
    );
    assert!(
        desk.change(&mut overview, |_| {})
            .is_empty(),
        "an unchanged list asks for nothing"
    );
    assert!(
        desk.change(&mut overview, |state| state.windows[0].title =
            String::from("new title"))
            .is_empty(),
        "a title change keeps the thumbnails"
    );

    desk.send(&mut overview, Message::Click(B2));
    let _ = desk.change(&mut overview, |state| {
        state
            .windows
            .retain(|window| window.address != Address(A1));
    });
    assert_eq!(selected(&overview), Some(B3));

    let from = card(&overview, B3);
    desk.send(
        &mut overview,
        Message::DragStart {
            address: B3,
            grab: (10.0, 10.0),
            size: (from.width, from.height),
            pointer: center(from),
        },
    );
    assert!(overview.drag().is_some());
    desk.send(&mut overview, Message::Hover(Some(B4)));
    let _ = desk.change(&mut overview, |state| {
        state
            .windows
            .retain(|window| window.address != Address(B3));
    });
    assert!(overview.drag().is_none());
    let (x, y) = center(card(&overview, B2));
    assert!(
        desk.send(&mut overview, Message::Drop(x, y))
            .is_empty()
    );
    let _ = desk.change(&mut overview, |state| {
        state
            .windows
            .retain(|window| window.address != Address(B4));
    });
    assert_eq!(
        overview.hovered(),
        None,
        "the hovered card closed"
    );

    let _ = desk.change(&mut overview, |state| state.windows.clear());
    assert_eq!(selected(&overview), None);
    assert!(
        desk.send(&mut overview, Message::Activate)
            .is_empty()
    );
}

#[test]
fn wheel_and_drag_auto_scroll() {
    let desk = Desk::new(state());
    let mut overview = Overview::default();
    let _ = desk.open(&mut overview);
    desk.send(&mut overview, Message::Resized(1280.0, 720.0));
    let layout = overview.layout().clone();
    let area = layout
        .area(overview.lanes(), 1, overview.list_scroll())
        .unwrap();
    let row = layout.lanes[1].row;
    let overflow = row - area.width;
    assert!(overflow > 120.0, "{overflow}");
    assert_eq!(overview.lane_scroll()[1], 0.0, "0xb1 is in view");

    let (x, y) = (area.x + 50.0, area.y + 50.0);
    desk.send(&mut overview, wheel(x, y, -1.0));
    assert_eq!(
        overview.lane_scroll()[1],
        120.0,
        "a notch scrolls the lane"
    );
    desk.send(&mut overview, wheel(x, y, -10.0));
    assert_eq!(
        overview.lane_scroll()[1],
        overflow,
        "up to its end"
    );
    let list = overview.list_scroll();
    let start = Instant::now();
    desk.send_at(&mut overview, wheel(x, y, -1.0), start);
    assert_eq!(overview.lane_scroll()[1], overflow);
    assert_eq!(
        overview.list_motion().target(),
        list + 72.0,
        "at the lane's end the wheel scrolls the list by Qt's notch"
    );
    assert!(overview.animating());
    desk.frame(&mut overview, start + 150 * MS);
    let mid = overview.list_scroll();
    assert!(mid > list && mid < list + 72.0, "{mid}");
    desk.frame(&mut overview, start + 300 * MS);
    assert_eq!(overview.list_scroll(), list + 72.0);
    assert!(!overview.list_motion().moving());
    desk.send(&mut overview, wheel(x, y, 10_000.0 / 120.0));
    assert_eq!(
        overview.lane_scroll()[1],
        0.0,
        "the lane scrolls back first"
    );
    let start = Instant::now();
    desk.send_at(
        &mut overview,
        wheel(x, y, 10_000.0 / 120.0),
        start,
    );
    assert_eq!(
        overview.list_motion().target(),
        0.0,
        "then the list, cut at its top"
    );
    desk.frame(&mut overview, start + 300 * MS);
    assert_eq!(overview.list_scroll(), 0.0);
    desk.send(
        &mut overview,
        Message::Wheel { x, y, delta: mouse::ScrollDelta::Pixels { x: 0.0, y: -30.0 } },
    );
    assert_eq!(
        overview.lane_scroll()[1],
        30.0,
        "the lane takes it first"
    );

    desk.send(&mut overview, wheel(x, y, -10.0));
    assert_eq!(overview.lane_scroll()[1], overflow);
    let area = overview
        .layout()
        .area(overview.lanes(), 1, overview.list_scroll())
        .unwrap();
    desk.send(
        &mut overview,
        Message::DragStart {
            address: B1,
            grab: (10.0, 10.0),
            size: (200.0, 200.0),
            pointer: (area.x + 300.0, area.y + 50.0),
        },
    );
    assert!(!overview.auto_scrolling(), "away from the edges");
    desk.send(
        &mut overview,
        Message::DragMove(area.x + 10.0, area.y + 50.0),
    );
    assert!(overview.auto_scrolling());
    let mut steps = 0;
    while overview.auto_scrolling() {
        let before = overview.lane_scroll()[1];
        desk.send(&mut overview, Message::AutoScroll);
        assert!(overview.lane_scroll()[1] < before);
        steps += 1;
        assert!(steps < 100);
    }
    assert_eq!(overview.lane_scroll()[1], 0.0);
    desk.send(
        &mut overview,
        Message::Drop(area.x + 10.0, area.y + 50.0),
    );
    assert!(
        !overview.auto_scrolling(),
        "the drop stops the timer"
    );

    for _ in 0..3 {
        desk.send(&mut overview, Message::Select(Direction::Right));
    }
    assert_eq!(selected(&overview), Some(B5));
    assert_eq!(overview.lane_scroll()[1], overflow, "revealIndex");
}

#[test]
fn card_borders_fade_with_the_selection_and_the_hover() {
    let desk = Desk::new(state());
    let colors = desk.theme.colors;
    let mut overview = Overview::default();
    let _ = desk.open(&mut overview);
    assert_eq!(selected(&overview), Some(B1));
    assert!(!overview.animating(), "nothing animates on open");
    assert_eq!(overview.card_border(B1), Some(colors.accent));
    assert_eq!(
        overview.card_border(B2),
        Some(colors.border_muted)
    );

    let start = Instant::now();
    desk.send_at(
        &mut overview,
        Message::Select(Direction::Left),
        start,
    );
    assert_eq!(selected(&overview), Some(B2));
    assert!(overview.animating());
    assert_eq!(
        overview.card_border(B1),
        Some(colors.accent),
        "the colour moves only on a frame"
    );
    desk.frame(&mut overview, start + 60 * MS);
    assert_eq!(
        overview.card_border(B1),
        Some(
            colors
                .accent
                .lerp(colors.border_muted, 0.5)
        )
    );
    assert_eq!(
        overview.card_border(B2),
        Some(
            colors
                .border_muted
                .lerp(colors.accent, 0.5)
        )
    );
    desk.frame(&mut overview, start + 120 * MS);
    assert_eq!(
        overview.card_border(B1),
        Some(colors.border_muted)
    );
    assert_eq!(overview.card_border(B2), Some(colors.accent));
    assert!(
        !overview.animating(),
        "the frames stop at the end"
    );

    let hover = start + 500 * MS;
    desk.send_at(&mut overview, Message::Hover(Some(B3)), hover);
    assert!(overview.animating());
    desk.frame(&mut overview, hover + 120 * MS);
    assert_eq!(overview.card_border(B3), Some(colors.border));
    desk.send_at(
        &mut overview,
        Message::Hover(None),
        hover + 200 * MS,
    );
    desk.frame(&mut overview, hover + 260 * MS);
    assert_eq!(
        overview.card_border(B3),
        Some(
            colors
                .border
                .lerp(colors.border_muted, 0.5)
        )
    );
    desk.frame(&mut overview, hover + 320 * MS);
    assert_eq!(
        overview.card_border(B3),
        Some(colors.border_muted)
    );
    assert!(!overview.animating());
}

#[test]
fn lane_borders_fade_with_a_drop_and_the_active_workspace() {
    let desk = Desk::new(state());
    let colors = desk.theme.colors;
    let mut overview = Overview::default();
    let _ = desk.open(&mut overview);
    let (one, five) = (lane(&overview, 1), lane(&overview, 5));
    assert_eq!(
        overview.lane_border(one),
        Some(colors.accent_container)
    );
    assert_eq!(
        overview.lane_border(five),
        Some(colors.container_high)
    );

    let start = Instant::now();
    let card = card(&overview, B1);
    let target = overview
        .layout()
        .lane_rect(five, overview.list_scroll())
        .unwrap();
    desk.send_at(
        &mut overview,
        Message::DragStart {
            address: B1,
            grab: (10.0, 10.0),
            size: (card.width, card.height),
            pointer: center(card),
        },
        start,
    );
    let (x, y) = center(target);
    desk.send_at(&mut overview, Message::DragMove(x, y), start);
    assert!(
        overview.animating(),
        "the lane under a drop lights up"
    );
    desk.frame(&mut overview, start + 120 * MS);
    assert_eq!(overview.lane_border(five), Some(colors.accent));

    let drop = start + 200 * MS;
    desk.send_at(&mut overview, Message::Drop(x, y), drop);
    desk.frame(&mut overview, drop + 120 * MS);
    assert_eq!(
        overview.lane_border(five),
        Some(colors.container_high)
    );
    assert!(!overview.animating());

    let _ = desk.change(&mut overview, |state| {
        state.monitors[0].active_workspace = Some(workspace_ref(5));
    });
    assert!(overview.animating());
    desk.frame(&mut overview, Instant::now() + 120 * MS);
    assert_eq!(
        overview.lane_border(five),
        Some(colors.accent_container)
    );
    assert_eq!(
        overview.lane_border(one),
        Some(colors.container_high)
    );
    assert!(!overview.animating());
}

#[test]
fn a_card_on_another_lane_starts_at_its_colour() {
    let desk = Desk::new(state());
    let colors = desk.theme.colors;
    let mut overview = Overview::default();
    let _ = desk.open(&mut overview);
    desk.send(&mut overview, Message::Hover(Some(B3)));
    assert!(overview.animating());
    let _ = desk.change(&mut overview, |state| {
        state.windows[4].workspace = workspace_ref(1);
    });
    assert_eq!(overview.card_border(B3), Some(colors.border));
    assert!(!overview.animating());
}

#[test]
fn a_new_theme_draws_at_once() {
    let mut desk = Desk::new(state());
    let mut overview = Overview::default();
    let _ = desk.open(&mut overview);
    desk.theme.colors.accent = Color::from_rgb(1.0, 0.0, 0.0);
    desk.send(&mut overview, Message::Hover(Some(B3)));
    assert_eq!(
        overview.card_border(B1),
        Some(Color::from_rgb(1.0, 0.0, 0.0))
    );
    assert_eq!(
        overview.card_border(B3),
        Some(desk.theme.colors.border)
    );
    assert!(!overview.animating());
}

#[test]
fn the_strip_scrolls_with_a_smoothed_animation() {
    let desk = Desk::new(state());
    let mut overview = Overview::default();
    let _ = desk.open(&mut overview);
    desk.send(&mut overview, Message::Resized(1280.0, 720.0));
    assert!(!overview.animating(), "a new size jumps");
    let area = overview
        .layout()
        .area(overview.lanes(), 1, overview.list_scroll())
        .unwrap();
    let (x, y) = (area.x + 50.0, area.y + 50.0);

    let start = Instant::now();
    desk.send_at(&mut overview, wheel(x, y, -1.0), start);
    assert_eq!(overview.lane_scroll()[1], 120.0);
    assert_eq!(overview.lane_shown()[1], 0.0);
    assert!(overview.animating());
    let mut expected = Smoothed::new(0.0, 1100.0, Some(180 * MS));
    expected.set(120.0, start);
    for ms in [16, 33, 50, 66, 83, 100] {
        desk.frame(&mut overview, start + ms * MS);
        expected.settle(start + ms * MS);
        assert_eq!(
            overview.lane_shown()[1],
            expected.value(),
            "{ms} ms"
        );
        assert!(overview.lane_shown()[1] > 0.0 && overview.lane_shown()[1] < 120.0);
    }
    let more = start + 100 * MS;
    desk.send_at(&mut overview, wheel(x, y, -1.0), more);
    expected.set(240.0, more);
    desk.frame(&mut overview, more + 16 * MS);
    expected.settle(more + 16 * MS);
    assert_eq!(overview.lane_shown()[1], expected.value());
    desk.frame(&mut overview, more + 500 * MS);
    assert_eq!(overview.lane_shown()[1], 240.0);
    assert!(!overview.animating());

    let overflow = overview.layout().lanes[1].row - area.width;
    let select = more + 1000 * MS;
    for _ in 0..3 {
        desk.send_at(
            &mut overview,
            Message::Select(Direction::Right),
            select,
        );
    }
    assert_eq!(selected(&overview), Some(B5));
    assert_eq!(overview.lane_scroll()[1], overflow);
    assert_eq!(overview.lane_shown()[1], 240.0);
    desk.frame(&mut overview, select + 1000 * MS);
    assert_eq!(overview.lane_shown()[1], overflow);
    assert!(!overview.animating());

    overview.release();
    let _ = desk.open(&mut overview);
    assert_eq!(overview.lane_shown(), overview.lane_scroll());
    assert!(!overview.animating());
}

#[test]
fn the_drag_auto_scroll_speeds_the_strip_up() {
    let desk = Desk::new(state());
    let mut overview = Overview::default();
    let _ = desk.open(&mut overview);
    desk.send(&mut overview, Message::Resized(1280.0, 720.0));
    let area = overview
        .layout()
        .area(overview.lanes(), 1, overview.list_scroll())
        .unwrap();
    desk.send(
        &mut overview,
        wheel(area.x + 50.0, area.y + 50.0, -10.0),
    );
    desk.frame(&mut overview, Instant::now() + 2000 * MS);
    let start = Instant::now();
    desk.send_at(
        &mut overview,
        Message::DragStart {
            address: B1,
            grab: (10.0, 10.0),
            size: (200.0, 200.0),
            pointer: (area.x + 300.0, area.y + 50.0),
        },
        start,
    );
    desk.send_at(
        &mut overview,
        Message::DragMove(area.x + 10.0, area.y + 50.0),
        start,
    );
    let before = overview.lane_scroll()[1];
    desk.send_at(
        &mut overview,
        Message::AutoScroll,
        start + 16 * MS,
    );
    let intensity = 86.0 / 96.0;
    assert_eq!(
        overview.lane_scroll()[1],
        before - (6.0 + 34.0 * intensity)
    );
    assert_eq!(
        overview.lane_motion()[1].velocity(),
        900.0 + 2400.0 * intensity
    );
    assert!(overview.lane_motion()[1].moving());
    desk.send_at(
        &mut overview,
        Message::Drop(area.x + 10.0, area.y + 50.0),
        start + 20 * MS,
    );
    assert_eq!(
        overview.lane_motion()[1].velocity(),
        1100.0,
        "the timer stops and sets it back"
    );
}

#[test]
fn the_card_under_a_drop_lights_up() {
    let desk = Desk::new(state());
    let colors = desk.theme.colors;
    let mut overview = Overview::default();
    let _ = desk.open(&mut overview);
    let start = Instant::now();
    let dragged = card(&overview, B2);
    desk.send_at(
        &mut overview,
        Message::DragStart {
            address: B2,
            grab: (10.0, 10.0),
            size: (dragged.width, dragged.height),
            pointer: center(dragged),
        },
        start,
    );
    desk.frame(&mut overview, start + 120 * MS);
    assert_eq!(
        overview.card_border(B2),
        Some(colors.border_muted),
        "the dragged card is no drop target"
    );
    let (x, y) = center(card(&overview, B3));
    let moved = start + 200 * MS;
    desk.send_at(&mut overview, Message::DragMove(x, y), moved);
    assert!(overview.animating());
    desk.frame(&mut overview, moved + 60 * MS);
    assert_eq!(
        overview.card_border(B3),
        Some(
            colors
                .border_muted
                .lerp(colors.border, 0.5)
        )
    );
    desk.frame(&mut overview, moved + 120 * MS);
    assert_eq!(overview.card_border(B3), Some(colors.border));
    assert_eq!(overview.card_border(B1), Some(colors.accent));
    assert!(!overview.animating());
}

#[test]
fn a_shorter_lane_eases_back_into_range() {
    let desk = Desk::new(state());
    let mut overview = Overview::default();
    let _ = desk.open(&mut overview);
    desk.send(&mut overview, Message::Resized(1280.0, 720.0));
    let area = overview
        .layout()
        .area(overview.lanes(), 1, overview.list_scroll())
        .unwrap();
    desk.send(
        &mut overview,
        wheel(area.x + 50.0, area.y + 50.0, -10_000.0 / 120.0),
    );
    desk.frame(&mut overview, Instant::now() + 2000 * MS);
    let end = overview.lane_scroll()[1];
    assert_eq!(overview.lane_shown()[1], end);

    let _ = desk.change(&mut overview, |state| {
        state
            .windows
            .retain(|window| window.address != Address(B5));
    });
    let target = overview.lane_scroll()[1];
    assert!(target < end, "{target} < {end}");
    assert_eq!(
        overview.lane_shown()[1],
        end,
        "the strip eases there"
    );
    assert!(overview.lane_motion()[1].moving());
    desk.frame(&mut overview, Instant::now() + 2000 * MS);
    assert_eq!(overview.lane_shown()[1], target);
    assert!(!overview.animating());
}

#[test]
fn a_state_change_keeps_a_moving_strip() {
    let desk = Desk::new(state());
    let mut overview = Overview::default();
    let _ = desk.open(&mut overview);
    desk.send(&mut overview, Message::Resized(1280.0, 720.0));
    let area = overview
        .layout()
        .area(overview.lanes(), 1, overview.list_scroll())
        .unwrap();
    let start = Instant::now();
    desk.send_at(
        &mut overview,
        wheel(area.x + 50.0, area.y + 50.0, -1.0),
        start,
    );
    desk.frame(&mut overview, start + 30 * MS);
    let shown = overview.lane_shown()[1];
    assert!(shown > 0.0 && shown < 120.0);
    let _ = desk.change(&mut overview, |state| {
        state.active_window = Some(Address(B2));
    });
    assert!(overview.lane_motion()[1].moving());
    assert_eq!(overview.lane_shown()[1], shown);
    assert_eq!(overview.lane_scroll()[1], 120.0);
    desk.frame(&mut overview, start + 1000 * MS);
    assert_eq!(overview.lane_shown()[1], 120.0);
    assert!(!overview.animating());
}

struct Surface {
    renderer: iced::Renderer,
    cache: user_interface::Cache,
}

impl Surface {
    fn new() -> Self {
        let mut system = font_system()
            .write()
            .expect("font system");
        for font in fonts::all() {
            system.load_font(Cow::Borrowed(font));
        }
        drop(system);
        let renderer = block_on(iced::Renderer::new(
            fonts::PLEX,
            Pixels(16.0),
            Some("tiny-skia"),
        ))
        .expect("a tiny-skia renderer");
        Self { renderer, cache: user_interface::Cache::new() }
    }

    fn update(
        &mut self,
        overview: &Overview,
        desk: &Desk,
        events: &[Event],
        at: Point,
    ) -> Vec<Message> {
        let env = desk.env();
        let cache = std::mem::replace(&mut self.cache, user_interface::Cache::new());
        let mut ui = UserInterface::build(
            overview.view(&env),
            Size::new(1280.0, 720.0),
            cache,
            &mut self.renderer,
        );
        let mut messages = Vec::new();
        let _ = ui.update(
            events,
            mouse::Cursor::Available(at),
            &mut self.renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        self.cache = ui.into_cache();
        messages
            .into_iter()
            .filter_map(|message| match message {
                PanelMessage::Overview(message) => Some(message),
                _ => None,
            })
            .collect()
    }
}

#[test]
fn a_card_that_scrolls_under_a_still_pointer_takes_the_hover() {
    let desk = Desk::new(state());
    let mut overview = Overview::default();
    let _ = desk.open(&mut overview);
    desk.send(&mut overview, Message::Resized(1280.0, 720.0));
    let first = card(&overview, B2);
    let at = Point::new(first.x + first.width - 20.0, first.y + 20.0);
    let mut surface = Surface::new();
    let entered = surface.update(&overview, &desk, &[moved(at)], at);
    assert_matches!(
        entered.as_slice(),
        [Message::Hover(Some(B2))],
        "{entered:?}"
    );
    desk.send(&mut overview, Message::Hover(Some(B2)));
    let still = surface.update(&overview, &desk, &[redraw(Instant::now())], at);
    assert!(still.is_empty(), "nothing moved: {still:?}");

    let start = Instant::now();
    desk.send_at(&mut overview, wheel(at.x, at.y, -1.0), start);
    let mut now = start;
    let mut hovers = Vec::new();
    while overview.animating() {
        now += 16 * MS;
        desk.frame(&mut overview, now);
        for message in surface.update(&overview, &desk, &[redraw(now)], at) {
            if let Message::Hover(hover) = message {
                hovers.push(hover);
                desk.send(&mut overview, message);
            }
        }
    }
    let under = overview
        .card_at(at.x, at.y)
        .map(|(address, _)| address);
    assert_ne!(
        under,
        Some(B2),
        "the strip moved another card under it"
    );
    assert_eq!(hovers.last(), Some(&under), "{hovers:?}");
    assert_eq!(overview.hovered(), under);
    let rest = surface.update(&overview, &desk, &[redraw(now + 16 * MS)], at);
    assert!(rest.is_empty(), "{rest:?}");
}

#[test]
fn a_press_keeps_the_hover_while_the_strip_moves() {
    let desk = Desk::new(state());
    let mut overview = Overview::default();
    let _ = desk.open(&mut overview);
    desk.send(&mut overview, Message::Resized(1280.0, 720.0));
    let first = card(&overview, B2);
    let at = Point::new(first.x + first.width - 20.0, first.y + 20.0);
    let mut surface = Surface::new();
    let entered = surface.update(
        &overview,
        &desk,
        &[moved(at), Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))],
        at,
    );
    assert_matches!(
        entered.as_slice(),
        [Message::Hover(Some(B2))],
        "{entered:?}"
    );
    let start = Instant::now();
    desk.send_at(&mut overview, wheel(at.x, at.y, -1.0), start);
    desk.frame(&mut overview, start + 1000 * MS);
    assert_ne!(
        overview
            .card_at(at.x, at.y)
            .map(|(address, _)| address),
        Some(B2)
    );
    let frame = surface.update(&overview, &desk, &[redraw(start + 1000 * MS)], at);
    assert!(
        frame.is_empty(),
        "Qt sends no hover while the card holds the grab: {frame:?}"
    );
}

fn moved(position: Point) -> Event {
    Event::Mouse(mouse::Event::CursorMoved { position })
}

fn redraw(now: Instant) -> Event {
    Event::Window(window::Event::RedrawRequested(now))
}
