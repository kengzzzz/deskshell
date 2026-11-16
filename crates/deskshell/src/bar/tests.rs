//! The bar with fake state: every click and wheel and what it asks the shell to do.

use deskshell_apps::Index;
use deskshell_hypr::{Address, State, Window, Workspace, WorkspaceRef};
use deskshell_media::{Audio, Device};
use iced::Point;

use super::*;

fn workspace_ref(id: i64) -> WorkspaceRef {
    WorkspaceRef { id: Some(id), name: id.to_string(), ..WorkspaceRef::default() }
}

fn window(address: u64, x: i32, floating: bool) -> Window {
    Window {
        address: Address(address),
        at: [x, 40],
        size: [600, 600],
        workspace: workspace_ref(2),
        floating,
        class: String::from("foot"),
        title: String::from("shell"),
        ..Window::default()
    }
}

fn bars() -> Bars {
    let state = State {
        connected: true,
        monitors: vec![deskshell_hypr::Monitor {
            name: String::from("DP-1"),
            width: 2560,
            height: 1440,
            scale: 1.0,
            focused: true,
            active_workspace: Some(workspace_ref(2)),
            ..deskshell_hypr::Monitor::default()
        }],
        workspaces: [1, 2, 5, 10]
            .into_iter()
            .map(|id| Workspace { id: Some(id), name: id.to_string(), ..Workspace::default() })
            .chain([Workspace {
                id: Some(3),
                name: String::from("chat \"x\""),
                ..Workspace::default()
            }])
            .collect(),
        windows: vec![window(0xa1, 0, false), window(0xb2, 700, true), window(0xc3, 1400, true)],
        active_workspace: Some(2),
        focused_monitor: Some(String::from("DP-1")),
        active_window: Some(Address(0xb2)),
        keyboard_layout: String::from("English (US)"),
    };
    let mut bars = Bars::default();
    bars.hypr
        .sync(&state, &Index::default());
    bars
}

fn update(bars: &Bars, message: Message) -> Option<Request> {
    bars.update(Some("DP-1"), message)
}

fn dispatch(lines: &[&str]) -> Option<Request> {
    Some(Request::Dispatch(
        lines
            .iter()
            .map(|line| (*line).to_owned())
            .collect(),
    ))
}

#[test]
fn launcher_button() {
    let bars = bars();
    assert_eq!(
        update(&bars, Message::Launcher(Button::Left)),
        Some(Request::Panel(Panel::Launcher))
    );
    assert_eq!(
        update(&bars, Message::Launcher(Button::Right)),
        Some(Request::Panel(Panel::Keybindings))
    );
    assert_eq!(
        update(&bars, Message::Launcher(Button::Middle)),
        None
    );
}

#[test]
fn workspace_dots() {
    let bars = bars();
    let ids: Vec<i64> = bars
        .hypr
        .dots
        .iter()
        .map(|dot| dot.id)
        .collect();
    assert_eq!(ids, [1, 2, 3, 5, 10]);
    assert_eq!(
        update(&bars, Message::Workspace(5)),
        dispatch(&[r#"hl.dsp.focus({ workspace = "5" })"#])
    );
    assert_eq!(
        update(&bars, Message::Workspace(3)),
        dispatch(&[r#"hl.dsp.focus({ workspace = "chat \"x\"" })"#])
    );
    assert_eq!(
        update(&bars, Message::Workspace(7)),
        None,
        "no such dot"
    );
    assert_eq!(
        update(&bars, Message::Workspaces(-120.0)),
        dispatch(&[r#"hl.dsp.focus({ workspace = "chat \"x\"" })"#]),
        "down moves to the next id"
    );
    assert_eq!(
        update(&bars, Message::Workspaces(120.0)),
        dispatch(&[r#"hl.dsp.focus({ workspace = "1" })"#])
    );
}

#[test]
fn tabs_clicks_and_wheel() {
    let bars = bars();
    assert_eq!(
        update(&bars, Message::Tab(0xa1)),
        dispatch(&[
            r#"hl.dsp.focus({ window = "address:0xa1" })"#,
            r#"hl.dsp.layout("fit_into_view")"#,
        ])
    );
    assert_eq!(
        update(&bars, Message::Tab(0xb2)),
        dispatch(&[
            r#"hl.dsp.focus({ window = "address:0xb2" })"#,
            r#"hl.dsp.window.alter_zorder({ mode = "top" })"#,
        ])
    );
    assert_eq!(
        update(&bars, Message::Stack),
        dispatch(&[
            r#"hl.dsp.focus({ window = "address:0xc3" })"#,
            r#"hl.dsp.window.alter_zorder({ mode = "top" })"#,
        ]),
        "the stack cycles past the focused float"
    );
    assert_eq!(
        update(&bars, Message::Tabs(-120.0)),
        dispatch(&[r#"hl.dsp.layout("focus r")"#])
    );
    assert_eq!(
        update(&bars, Message::Tabs(120.0)),
        dispatch(&[r#"hl.dsp.layout("focus l")"#])
    );
    assert_eq!(
        bars.update(None, Message::Tab(0xa1)),
        None,
        "a bar without a monitor"
    );
}

#[test]
fn tray_clicks() {
    let bars = bars();
    let bounds = Rectangle { x: 120.0, y: 8.0, width: 20.0, height: 32.0 };
    for (button, tray) in [
        (Button::Left, deskshell_tray::Button::Left),
        (Button::Middle, deskshell_tray::Button::Middle),
        (Button::Right, deskshell_tray::Button::Right),
    ] {
        assert_eq!(
            update(
                &bars,
                Message::Tray(String::from(":1.5/item"), button, bounds)
            ),
            Some(Request::TrayClick { item: String::from(":1.5/item"), button: tray, bounds })
        );
    }
    assert_eq!(
        update(
            &bars,
            Message::TrayWheel(String::from(":1.5/item"), -120.0)
        ),
        Some(Request::Tray(TrayCommand::Scroll {
            item: String::from(":1.5/item"),
            delta: -120,
            orientation: Orientation::Vertical,
        }))
    );
}

#[test]
fn right_group() {
    let mut bars = bars();
    assert_eq!(
        update(&bars, Message::Updates),
        Some(Request::Panel(Panel::Updates))
    );
    assert_eq!(
        update(&bars, Message::Media(Button::Left)),
        Some(Request::Panel(Panel::Media))
    );
    assert_eq!(
        update(&bars, Message::Media(Button::Right)),
        Some(Request::Media(MprisCommand::PlayPause))
    );
    assert_eq!(
        update(&bars, Message::Cluster),
        Some(Request::Panel(Panel::Hub))
    );
    assert_eq!(
        update(&bars, Message::Clock),
        Some(Request::Panel(Panel::Calendar))
    );
    assert_eq!(
        update(&bars, Message::Volume(120.0)),
        None,
        "no sink"
    );
    assert_eq!(update(&bars, Message::Mic), None, "no source");
    bars.status.sync_audio(&Audio {
        connected: true,
        sink: Some(Device { volume: 0.5, ..Device::default() }),
        source: Some(Device::default()),
        ..Audio::default()
    });
    assert_eq!(
        update(&bars, Message::Volume(120.0)),
        Some(Request::Audio(AudioCommand::StepVolume(
            DeviceKind::Sink,
            0.05
        )))
    );
    assert_eq!(
        update(&bars, Message::Volume(-120.0)),
        Some(Request::Audio(AudioCommand::StepVolume(
            DeviceKind::Sink,
            -0.05
        )))
    );
    assert_eq!(
        update(&bars, Message::Mic),
        Some(Request::Audio(AudioCommand::ToggleMuted(
            DeviceKind::Source
        )))
    );
}

#[test]
fn the_bell_chip() {
    let bars = bars();
    assert_eq!(
        update(&bars, Message::Bell(Button::Left)),
        Some(Request::Panel(Panel::Hub))
    );
    assert_eq!(
        update(&bars, Message::Bell(Button::Right)),
        Some(Request::Notify(NotifyCommand::ToggleDnd))
    );
    assert_eq!(
        update(&bars, Message::Bell(Button::Middle)),
        Some(Request::Notify(NotifyCommand::Clear))
    );
}

#[test]
fn tooltips_wait_and_follow_the_pointer() {
    let mut bars = bars();
    let bar = window::Id::unique();
    let bounds = Rectangle::new(
        Point::new(600.0, 8.0),
        iced::Size::new(30.0, 32.0),
    );
    let first = bars
        .hover(bar, Hover::Chip(0xa1), true, bounds)
        .unwrap();
    let tip = bars.tip_due(first, false).unwrap();
    assert_eq!(tip.target, Hover::Chip(0xa1));
    assert!(
        bars.tip_due(first, true).is_none(),
        "not while the OSD shows there"
    );
    let second = bars
        .hover(bar, Hover::Stack, true, bounds)
        .unwrap();
    assert!(
        bars.tip_due(first, false).is_none(),
        "the pointer moved on"
    );
    assert_eq!(
        bars.hover(bar, Hover::Chip(0xa1), false, bounds),
        None,
        "leaving the old chip after entering the new one changes nothing"
    );
    assert!(bars.tip_due(second, false).is_some());
    bars.hide_tip();
    assert!(
        bars.tip_due(second, false).is_none(),
        "a click hides it"
    );
    assert_eq!(
        bars.tip_texts("DP-1", Hover::Chip(0xa1)),
        Some(("shell", "on screen"))
    );
    assert_eq!(
        bars.tip_texts("DP-1", Hover::Stack),
        Some(("shell, shell", "2 floating")),
        "the stack holds the focused float too"
    );
}

#[test]
fn osd_shows_in_the_focused_pill() {
    let mut bars = bars();
    assert_eq!(bars.osd_place(), osd::Place::Hidden);
    bars.osd
        .show(&deskshell_media::OsdEvent { kind: DeviceKind::Sink, volume: 0.5, muted: false });
    assert_eq!(
        bars.osd_place(),
        osd::Place::Pill(String::from("DP-1"))
    );
}

#[test]
fn the_clock() {
    let mut bars = Bars::default();
    let now: Zoned = "2026-11-13T09:41:12+07:00[Asia/Bangkok]"
        .parse()
        .unwrap();
    assert!(bars.set_time(&now));
    assert_eq!(bars.time, "09:41");
    assert_eq!(bars.date, "Fri 13");
    assert!(!bars.set_time(&now));
}

#[test]
fn highlights_follow_the_open_panel() {
    assert_eq!(Highlight::of(None), Highlight::default());
    assert!(Highlight::of(Some(Panel::Hub)).hub);
    assert!(Highlight::of(Some(Panel::Media)).media);
    assert!(Highlight::of(Some(Panel::Updates)).updates);
    assert_eq!(
        Highlight::of(Some(Panel::Launcher)),
        Highlight::default()
    );
}
