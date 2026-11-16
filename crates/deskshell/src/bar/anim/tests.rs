//! Bar transition targets for each state change, with made-up text widths.

use deskshell_apps::Index;
use deskshell_hypr::{Address, State, Window, Workspace, WorkspaceRef};
use deskshell_media::{DeviceKind, OsdEvent};

use super::*;

const MS: Duration = Duration::from_millis(1);
const FRAME: Duration = Duration::from_millis(16);
const FLAG: f32 = 20.0;
const ONE: f32 = 100.0;
const TWO: f32 = 400.0;
const MAX_WIDTH: Option<f32> = Some(2000.0);

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
}

fn workspace_ref(id: i64) -> WorkspaceRef {
    WorkspaceRef { id: Some(id), name: id.to_string(), ..WorkspaceRef::default() }
}

fn window(address: u64, workspace: i64, x: i32, title: &str) -> Window {
    Window {
        address: Address(address),
        at: [x, 40],
        size: [600, 600],
        workspace: workspace_ref(workspace),
        class: String::from("foot"),
        title: title.to_owned(),
        ..Window::default()
    }
}

fn monitor(name: &str, x: i32, workspace: i64, focused: bool) -> deskshell_hypr::Monitor {
    deskshell_hypr::Monitor {
        name: name.to_owned(),
        x,
        width: 2560,
        height: 1440,
        scale: 1.0,
        focused,
        active_workspace: Some(workspace_ref(workspace)),
        ..deskshell_hypr::Monitor::default()
    }
}

fn state() -> State {
    State {
        connected: true,
        monitors: vec![monitor("DP-1", 0, 2, true), monitor("HDMI-A-1", 2560, 5, false)],
        workspaces: [1, 2, 5]
            .into_iter()
            .map(|id| Workspace { id: Some(id), name: id.to_string(), ..Workspace::default() })
            .collect(),
        windows: vec![
            window(0xa1, 2, 0, "one"),
            window(0xa2, 2, 700, "two"),
            window(0xa3, 2, -900, "three"),
            window(0xb1, 5, 2600, "four"),
        ],
        active_workspace: Some(2),
        focused_monitor: Some(String::from("DP-1")),
        active_window: Some(Address(0xa1)),
        keyboard_layout: String::from("English (US)"),
    }
}

fn bars(state: &State) -> Bars {
    let mut bars = Bars::default();
    bars.hypr.sync(state, &Index::default());
    let widths = &mut bars.widths;
    widths.flag = Some((bars.hypr.flag, FLAG));
    widths.desktop = 50.0;
    widths
        .labels
        .insert(String::from("one"), ONE);
    widths
        .labels
        .insert(String::from("two"), TWO);
    widths
        .counts
        .insert(String::from("1"), 8.0);
    bars
}

fn sync(anim: &mut Anim, bars: &Bars, monitor: &str, hovered: Option<Hover>, now: Instant) -> bool {
    let place = bars.osd_place();
    anim.sync(
        bars,
        bars.hypr.monitor(monitor),
        hovered,
        MAX_WIDTH,
        &place,
        now,
    )
}

fn ready(bars: &Bars, monitor: &str, now: Instant) -> Anim {
    let mut anim = Anim::default();
    assert!(sync(&mut anim, bars, monitor, None, now));
    assert!(!anim.moving());
    anim
}

fn run_out(anim: &mut Anim, bars: &Bars, monitor: &str, from: Instant) -> Instant {
    let monitor = bars.hypr.monitor(monitor);
    let mut now = from;
    while anim.settle(monitor, now) {
        now += FRAME;
        assert!(now - from < Duration::from_secs(2));
    }
    now
}

fn key(address: u64) -> ChipKey {
    ChipKey { address, float: false }
}

fn focused(label: f32, title: f32) -> f32 {
    8.0 + FLAG + 7.0 + 18.0 + 7.0 + label.min(title) + 12.0
}

#[test]
fn a_starting_bar_takes_its_targets_at_once() {
    let bars = bars(&state());
    let now = Instant::now();
    let mut anim = Anim::default();
    let place = bars.osd_place();
    let monitor = bars.hypr.monitor("DP-1");
    assert!(
        anim.sync(&bars, monitor, None, None, &place, now),
        "the first sync draws"
    );
    assert!(!anim.moving());
    assert_eq!(anim.chip(key(0xa1)), Some(focused(ONE, 320.0)));
    anim.sync(&bars, monitor, None, Some(150.0), &place, now);
    assert!(
        !anim.moving(),
        "the first maxWidth still moves at once"
    );
    assert_eq!(anim.chip(key(0xa1)), Some(focused(ONE, 60.0)));
    let mut other = Anim::default();
    other.sync(&bars, None, None, MAX_WIDTH, &place, now);
    assert!(
        !other.ready,
        "a bar without its monitor is not ready"
    );
}

#[test]
fn widths_at_rest() {
    let bars = bars(&state());
    let anim = ready(&bars, "DP-1", Instant::now());
    assert_eq!(anim.dot(2, true), DOT_FOCUSED);
    assert_eq!(anim.dot(1, false), DOT);
    assert_eq!(anim.chip(key(0xa3)), Some(CHIP));
    assert_eq!(anim.chip(key(0xa1)), Some(focused(ONE, 320.0)));
    assert_eq!(anim.chip(key(0xa2)), Some(CHIP));
    assert_eq!(
        anim.chip_opacity(key(0xa3)),
        Some(OFF_SCREEN),
        "three lies off screen"
    );
    assert_eq!(anim.chip_opacity(key(0xa1)), Some(1.0));
    let chips = CHIP + focused(ONE, 320.0) + CHIP;
    assert_eq!(anim.pill(), Some(chips + 2.0 * 3.0 + 8.0));
    assert_eq!(anim.layers(false), (1.0, 0.0));
}

#[test]
fn the_idle_part_the_separator_and_the_stack_count_in_the_pill() {
    let mut state = state();
    state
        .windows
        .push(Window { floating: true, ..window(0xc1, 2, 300, "float") });
    let mut bars = bars(&state);
    let hdmi = ready(&bars, "HDMI-A-1", Instant::now());
    assert_eq!(
        hdmi.pill(),
        Some(9.0 + FLAG + 9.0 + 3.0 + CHIP + 8.0)
    );
    let dp = ready(&bars, "DP-1", Instant::now());
    let chips = CHIP + focused(ONE, 320.0) + CHIP;
    let stack = 9.0 + 18.0 + 5.0 + 8.0 + 9.0;
    assert_eq!(
        dp.pill(),
        Some(chips + SEPARATOR + stack + 4.0 * 3.0 + 8.0)
    );
    let mut empty = self::state();
    empty.active_workspace = Some(1);
    empty.active_window = None;
    bars.hypr
        .sync(&empty, &Index::default());
    let idle = ready(&bars, "DP-1", Instant::now());
    assert_eq!(
        idle.pill(),
        Some(9.0 + 18.0 + 7.0 + 50.0 + 9.0 + 8.0)
    );
}

#[test]
fn a_focus_change_starts_the_old_and_new_chips_and_the_pill_trails_them() {
    let mut state = state();
    let mut bars = bars(&state);
    let start = Instant::now();
    let mut dp = ready(&bars, "DP-1", start);
    let mut hdmi = ready(&bars, "HDMI-A-1", start);

    state.active_window = Some(Address(0xa2));
    bars.hypr
        .sync(&state, &Index::default());
    assert!(sync(&mut dp, &bars, "DP-1", None, start));
    assert!(
        !sync(&mut hdmi, &bars, "HDMI-A-1", None, start),
        "the other bar draws nothing"
    );
    assert!(!hdmi.moving());
    assert!(dp.moving());
    assert_eq!(
        dp.chips.value(&key(0xa2)),
        Some(CHIP),
        "values move on frames"
    );

    let monitor = bars.hypr.monitor("DP-1");
    let before = dp.pill().unwrap_or_default();
    dp.settle(monitor, start + 70 * MS);
    let eased = Easing::OutCubic.apply(0.5);
    let one = focused(ONE, 320.0);
    let two = focused(TWO, 320.0);
    assert!(close(
        dp.chip(key(0xa1)).unwrap_or_default(),
        one + (CHIP - one) * eased
    ));
    assert!(close(
        dp.chip(key(0xa2)).unwrap_or_default(),
        CHIP + (two - CHIP) * eased
    ));
    assert_eq!(
        dp.pill(),
        Some(before),
        "the pill's new target starts from where it is"
    );

    let mut now = start + 70 * MS;
    let mut chips_end = None;
    while dp.settle(monitor, now) {
        if chips_end.is_none() && !dp.chips.moving() {
            chips_end = Some(now);
        }
        now += FRAME;
    }
    let chips_end = chips_end.unwrap_or(now);
    assert!(chips_end - start >= SHORT && chips_end - start < SHORT + FRAME);
    assert!(
        now - chips_end >= SHORT - FRAME,
        "the pill ends a duration after the chips"
    );
    assert_eq!(dp.chip(key(0xa1)), Some(CHIP));
    assert_eq!(dp.chip(key(0xa2)), Some(two));
    assert_eq!(dp.pill(), Some(CHIP + CHIP + two + 6.0 + 8.0));
    assert!(!dp.moving());
}

#[test]
fn a_workspace_change_moves_the_dots_on_every_bar() {
    let mut state = state();
    let mut bars = bars(&state);
    let start = Instant::now();
    let mut dp = ready(&bars, "DP-1", start);
    let mut hdmi = ready(&bars, "HDMI-A-1", start);

    state.active_workspace = Some(1);
    state.active_window = None;
    bars.hypr
        .sync(&state, &Index::default());
    assert!(sync(&mut dp, &bars, "DP-1", None, start));
    assert!(
        sync(&mut hdmi, &bars, "HDMI-A-1", None, start),
        "the dots show on every bar"
    );
    assert!(hdmi.dots.moving() && !hdmi.chips.moving());
    dp.settle(bars.hypr.monitor("DP-1"), start + 70 * MS);
    let eased = Easing::OutCubic.apply(0.5);
    assert!(close(
        dp.dot(2, false),
        DOT_FOCUSED + (DOT - DOT_FOCUSED) * eased
    ));
    assert!(close(
        dp.dot(1, true),
        DOT + (DOT_FOCUSED - DOT) * eased
    ));
    assert_eq!(
        dp.chip(key(0xa1)),
        None,
        "the old workspace's chips go with it"
    );
    let end = run_out(&mut hdmi, &bars, "HDMI-A-1", start + 70 * MS);
    assert!(end - start >= SHORT && end - start < SHORT + FRAME);
    assert_eq!(hdmi.dot(1, true), DOT_FOCUSED);

    state.workspaces.push(Workspace {
        id: Some(7),
        name: String::from("7"),
        ..Workspace::default()
    });
    bars.hypr
        .sync(&state, &Index::default());
    sync(&mut hdmi, &bars, "HDMI-A-1", None, end);
    assert_eq!(hdmi.dots.value(&7), Some(DOT));
    assert!(!hdmi.moving());
}

#[test]
fn hover_brings_an_off_screen_chip_to_full_opacity() {
    let bars = bars(&state());
    let start = Instant::now();
    let mut anim = ready(&bars, "DP-1", start);
    assert!(sync(
        &mut anim,
        &bars,
        "DP-1",
        Some(Hover::Chip(0xa3)),
        start
    ));
    assert!(anim.opacity.moving() && !anim.chips.moving());
    let end = run_out(&mut anim, &bars, "DP-1", start);
    assert_eq!(anim.chip_opacity(key(0xa3)), Some(1.0));
    assert!(end - start < SHORT + FRAME);
    assert!(sync(&mut anim, &bars, "DP-1", None, end));
    run_out(&mut anim, &bars, "DP-1", end);
    assert_eq!(anim.chip_opacity(key(0xa3)), Some(OFF_SCREEN));
    assert!(
        !sync(
            &mut anim,
            &bars,
            "DP-1",
            Some(Hover::Chip(0xa1)),
            end
        ),
        "an on-screen chip is opaque already"
    );
}

#[test]
fn the_osd_crossfades_in_the_focused_pill_only() {
    let mut bars = bars(&state());
    let start = Instant::now();
    let mut dp = ready(&bars, "DP-1", start);
    let mut hdmi = ready(&bars, "HDMI-A-1", start);
    let generation = bars
        .osd
        .show(&OsdEvent { kind: DeviceKind::Sink, volume: 0.6, muted: false });
    assert!(sync(&mut dp, &bars, "DP-1", None, start));
    assert!(
        !sync(&mut hdmi, &bars, "HDMI-A-1", None, start),
        "the level moves at once where the OSD does not draw"
    );
    assert!(!hdmi.moving());
    assert!(close(hdmi.level(0.0), 0.6));

    dp.settle(bars.hypr.monitor("DP-1"), start + 70 * MS);
    let (tabs, osd) = dp.layers(true);
    let eased = Easing::OutCubic.apply(0.5);
    assert!(close(tabs, 1.0 - eased) && close(osd, eased));
    assert!(
        close(
            dp.level(0.0),
            0.6 * Easing::OutCubic.apply(70.0 / 90.0)
        ),
        "the level slides from 0 in 90 ms"
    );
    let end = run_out(&mut dp, &bars, "DP-1", start + 70 * MS);
    assert_eq!(dp.layers(true), (0.0, 1.0));
    assert_eq!(dp.pill(), Some(osd::POPUP_WIDTH));
    assert!(end - start < SHORT + FRAME);

    bars.osd
        .show(&OsdEvent { kind: DeviceKind::Sink, volume: 0.7, muted: false });
    assert!(sync(&mut dp, &bars, "DP-1", None, end));
    dp.settle(bars.hypr.monitor("DP-1"), end + 45 * MS);
    let eased = Easing::OutCubic.apply(0.5);
    assert!(close(dp.level(0.0), 0.6 + 0.1 * eased));
    let end = run_out(&mut dp, &bars, "DP-1", end + 45 * MS);

    assert!(!bars.osd.hide(generation));
    let generation = bars
        .osd
        .show(&OsdEvent { kind: DeviceKind::Sink, volume: 0.7, muted: true });
    assert!(bars.osd.hide(generation));
    assert!(sync(&mut dp, &bars, "DP-1", None, end));
    run_out(&mut dp, &bars, "DP-1", end);
    assert_eq!(dp.layers(false), (1.0, 0.0));
    let rest = ready(&bars, "DP-1", end);
    assert_eq!(dp.pill(), rest.pill());
}

#[test]
fn the_track_flash_rises_in_120_ms_and_falls_in_650() {
    let start = Instant::now();
    let mut flash = Flash::default();
    flash.restart(start);
    assert!(flash.moving());
    assert_eq!(flash.value(), 0.0);
    assert!(flash.settle(start + 60 * MS));
    assert!(close(flash.value(), 0.875));
    assert!(flash.settle(start + 120 * MS));
    assert!(close(flash.value(), 1.0));
    assert!(flash.settle(start + 120 * MS + 325 * MS));
    assert!(close(flash.value(), 0.125));
    flash.restart(start + 500 * MS);
    assert_eq!(flash.value(), 0.0, "a new track starts it from 0");
    assert!(flash.settle(start + 560 * MS));
    assert!(close(flash.value(), 0.875));
    assert!(!flash.settle(start + 500 * MS + 770 * MS));
    assert_eq!(flash.value(), 0.0);
    assert!(!flash.moving());
}

#[test]
fn the_flash_stops_when_the_pill_hides() {
    let mut bars = bars(&state());
    let start = Instant::now();
    let mut anim = ready(&bars, "DP-1", start);
    bars.media.active = true;
    anim.flash.restart(start);
    assert!(anim.moving());
    bars.media.active = false;
    assert!(sync(&mut anim, &bars, "DP-1", None, start));
    assert!(!anim.moving());
}

fn fullscreen(state: &mut State, index: usize) {
    state.windows[index].fullscreen = 2;
    let id = state.windows[index].workspace.id;
    for workspace in &mut state.workspaces {
        workspace.has_fullscreen = workspace.id == id;
    }
}

fn unfullscreen(state: &mut State) {
    for window in &mut state.windows {
        window.fullscreen = 0;
    }
    for workspace in &mut state.workspaces {
        workspace.has_fullscreen = false;
    }
}

#[test]
fn a_covered_bar_takes_its_targets_at_once() {
    let mut state = state();
    fullscreen(&mut state, 0);
    let mut bars = bars(&state);
    let start = Instant::now();
    let mut dp = ready(&bars, "DP-1", start);
    let mut hdmi = ready(&bars, "HDMI-A-1", start);
    assert!(
        bars.hypr
            .monitor("DP-1")
            .unwrap()
            .tabs
            .fullscreen_covered
    );

    state.focused_monitor = Some(String::from("HDMI-A-1"));
    state.monitors[0].focused = false;
    state.monitors[1].focused = true;
    state.active_workspace = Some(5);
    state.active_window = Some(Address(0xb1));
    bars.hypr
        .sync(&state, &Index::default());
    assert!(
        sync(&mut dp, &bars, "DP-1", None, start),
        "the bar draws the new values when it shows again"
    );
    assert!(
        !dp.moving(),
        "no frames come under a fullscreen window"
    );
    assert_eq!(dp.dot(2, false), DOT);
    assert_eq!(dp.dot(5, true), DOT_FOCUSED);
    assert!(sync(&mut hdmi, &bars, "HDMI-A-1", None, start));
    assert!(hdmi.dots.moving(), "the uncovered bar animates");

    state.focused_monitor = Some(String::from("DP-1"));
    state.monitors[0].focused = true;
    state.monitors[1].focused = false;
    state.active_workspace = Some(2);
    state.active_window = Some(Address(0xa2));
    bars.hypr
        .sync(&state, &Index::default());
    assert!(sync(&mut dp, &bars, "DP-1", None, start));
    assert!(!dp.moving());
    let two = focused(TWO, 320.0);
    assert_eq!(dp.chip(key(0xa1)), Some(CHIP));
    assert_eq!(dp.chip(key(0xa2)), Some(two));
    assert_eq!(dp.pill(), Some(CHIP + CHIP + two + 6.0 + 8.0));
    assert_eq!(dp.dot(2, true), DOT_FOCUSED);
}

#[test]
fn a_cover_ends_the_transitions_that_run() {
    let mut state = state();
    let mut bars = bars(&state);
    let start = Instant::now();
    let mut dp = ready(&bars, "DP-1", start);
    state.active_window = Some(Address(0xa2));
    bars.hypr
        .sync(&state, &Index::default());
    assert!(sync(&mut dp, &bars, "DP-1", None, start));
    dp.settle(bars.hypr.monitor("DP-1"), start + 50 * MS);
    assert!(dp.moving());

    fullscreen(&mut state, 1);
    bars.hypr
        .sync(&state, &Index::default());
    assert!(sync(
        &mut dp,
        &bars,
        "DP-1",
        None,
        start + 60 * MS
    ));
    assert!(!dp.moving());
    let rest = ready(&bars, "DP-1", start + 60 * MS);
    assert_eq!(dp.chip(key(0xa1)), rest.chip(key(0xa1)));
    assert_eq!(dp.chip(key(0xa2)), rest.chip(key(0xa2)));
    assert_eq!(dp.pill(), rest.pill());
}

#[test]
fn a_covered_bar_does_not_flash() {
    let mut state = state();
    let mut bars = bars(&state);
    bars.media.active = true;
    let start = Instant::now();
    let mut dp = ready(&bars, "DP-1", start);
    dp.restart_flash(start);
    assert!(dp.flash.moving());

    fullscreen(&mut state, 0);
    bars.hypr
        .sync(&state, &Index::default());
    assert!(
        sync(&mut dp, &bars, "DP-1", None, start + 60 * MS),
        "the flash stops where it is"
    );
    assert!(!dp.moving());
    assert_eq!(dp.flash.value(), 0.0);
    dp.restart_flash(start + 100 * MS);
    assert!(
        !dp.moving(),
        "a track change under the fullscreen window does not flash"
    );
}

#[test]
fn nothing_replays_when_the_cover_ends() {
    let mut state = state();
    fullscreen(&mut state, 0);
    let mut bars = bars(&state);
    let start = Instant::now();
    let mut dp = ready(&bars, "DP-1", start);
    state.active_window = Some(Address(0xa2));
    bars.hypr
        .sync(&state, &Index::default());
    sync(&mut dp, &bars, "DP-1", None, start);

    unfullscreen(&mut state);
    bars.hypr
        .sync(&state, &Index::default());
    assert!(!sync(
        &mut dp,
        &bars,
        "DP-1",
        None,
        start + 500 * MS
    ));
    assert!(!dp.moving());

    state.active_window = Some(Address(0xa1));
    bars.hypr
        .sync(&state, &Index::default());
    assert!(sync(
        &mut dp,
        &bars,
        "DP-1",
        None,
        start + SHORT * 4
    ));
    assert!(dp.chips.moving(), "a shown bar animates again");
}

#[test]
fn the_popup_fades_in_and_out_and_back() {
    let start = Instant::now();
    let mut popup = Popup::new(0.4, start);
    assert!(popup.moving() && popup.shown());
    assert_eq!(popup.opacity(), 0.0, "the card starts clear");
    assert_eq!(popup.level(), 0.4);
    popup.settle(start + 70 * MS);
    assert!(close(
        popup.opacity(),
        Easing::OutCubic.apply(0.5)
    ));
    assert!(!popup.settle(start + SHORT));
    assert_eq!(popup.opacity(), 1.0);

    let hide = start + Duration::from_secs(2);
    assert!(popup.hide(hide));
    assert!(
        !popup.gone(),
        "the surface stays while the card fades"
    );
    popup.settle(hide + 70 * MS);
    let half = 1.0 - Easing::OutCubic.apply(0.5);
    assert!(close(popup.opacity(), half));
    assert!(popup.show(0.5, hide + 70 * MS));
    popup.settle(hide + 70 * MS);
    assert!(close(popup.opacity(), half));
    assert!(popup.settle(hide + 140 * MS));
    assert!(popup.opacity() > half);
    popup.settle(hide + 70 * MS + SHORT);
    assert_eq!(popup.opacity(), 1.0);
    assert_eq!(popup.level(), 0.5);

    let hide = hide + Duration::from_secs(1);
    popup.hide(hide);
    assert!(!popup.settle(hide + SHORT));
    assert!(popup.gone());

    let mut early = Popup::new(0.4, start);
    early.hide(start);
    assert!(
        early.gone(),
        "a popup hidden before its card moved has nothing to fade"
    );
}

#[test]
fn a_window_that_turns_floating_gets_a_new_chip() {
    let mut state = state();
    let mut bars = bars(&state);
    let start = Instant::now();
    let mut anim = ready(&bars, "DP-1", start);
    state.windows[0].floating = true;
    bars.hypr
        .sync(&state, &Index::default());
    sync(&mut anim, &bars, "DP-1", None, start);
    let float = ChipKey { address: 0xa1, float: true };
    assert_eq!(
        anim.chip(float),
        Some(focused(ONE, 320.0)),
        "the float's chip starts at its width"
    );
    assert_eq!(
        anim.chip(key(0xa1)),
        None,
        "the tiled chip went with its row"
    );
    assert!(!anim.chips.moving());
}

#[test]
fn a_new_font_family_makes_the_widths_stale() {
    let bars = bars(&state());
    let mut theme = Theme::default();
    let mut widths = Widths::default();
    assert!(widths.stale(&theme), "nothing is measured yet");
    widths.measure(&bars.hypr, &theme);
    assert!(!widths.stale(&theme));
    theme.colors.accent = iced::Color::WHITE;
    assert!(
        !widths.stale(&theme),
        "a colour does not change a width"
    );
    theme.fonts.ui = "Other Sans";
    assert!(widths.stale(&theme));
    widths.measure(&bars.hypr, &theme);
    assert!(!widths.stale(&theme));
}
