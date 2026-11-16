//! The update ring, progress bar and popover buttons through iced's `UserInterface`.

use std::time::{Duration, Instant};

use deskshell::bar::update::ring;
use deskshell::panel::updates::progress::progress_bar;
use deskshell::panel::updates::{self as popover, Updates};
use deskshell::panel::{Effect, Sources};
use deskshell::updates::{self, Request, View};
use deskshell_system::install::{self, Job, PamStage, Phase};
use deskshell_ui::{Theme, fonts};
use iced::advanced::clipboard;
use iced::advanced::mouse;
use iced::advanced::renderer::Headless as _;
use iced::window::{self, RedrawRequest};
use iced::{Color, Element, Event, Pixels, Size};
use iced_test::futures::futures::executor::block_on;
use iced_test::runtime::user_interface::{self, UserInterface};

const FRAME: Duration = Duration::from_micros(16_667);

struct Surface {
    renderer: iced::Renderer,
    cache: user_interface::Cache,
}

impl Surface {
    fn new() -> Self {
        let renderer = block_on(iced::Renderer::new(
            fonts::PLEX,
            Pixels(16.0),
            Some("tiny-skia"),
        ))
        .expect("a tiny-skia renderer");
        Self { renderer, cache: user_interface::Cache::new() }
    }

    fn frame(&mut self, view: Element<'_, ()>, now: Instant) -> RedrawRequest {
        let cache = std::mem::replace(&mut self.cache, user_interface::Cache::new());
        let mut ui = UserInterface::build(
            view,
            Size::new(200.0, 60.0),
            cache,
            &mut self.renderer,
        );
        let mut messages = Vec::new();
        let (state, _) = ui.update(
            &[Event::Window(window::Event::RedrawRequested(now))],
            mouse::Cursor::Unavailable,
            &mut self.renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        self.cache = ui.into_cache();
        match state {
            user_interface::State::Updated { redraw_request, .. } => redraw_request,
            user_interface::State::Outdated => RedrawRequest::NextFrame,
        }
    }

    fn frames_until_rest(
        &mut self,
        view: impl Fn() -> Element<'static, ()>,
        start: Instant,
        limit: u32,
    ) -> u32 {
        let mut frames = 0;
        let mut now = start;
        while self.frame(view(), now) == RedrawRequest::NextFrame {
            frames += 1;
            now += FRAME;
            if frames >= limit {
                break;
            }
        }
        frames
    }
}

fn ring_at(progress: f32, indeterminate: bool, covered: bool) -> Element<'static, ()> {
    ring(
        progress,
        indeterminate,
        true,
        covered,
        Color::from_rgba(1.0, 1.0, 1.0, 0.12),
        Color::WHITE,
    )
}

#[test]
fn an_idle_ring_asks_for_no_frames() {
    let mut surface = Surface::new();
    let start = Instant::now();
    assert_eq!(
        surface.frame(ring_at(0.5, false, false), start),
        RedrawRequest::Wait
    );
    assert_eq!(
        surface.frame(ring_at(0.5, false, false), start + FRAME),
        RedrawRequest::Wait
    );
}

#[test]
fn a_progress_change_sweeps_for_300_ms_then_stops() {
    let mut surface = Surface::new();
    let start = Instant::now();
    assert_eq!(
        surface.frame(ring_at(0.25, false, false), start),
        RedrawRequest::Wait
    );
    let frames = surface.frames_until_rest(|| ring_at(0.5, false, false), start, 100);
    assert!(
        (17..=19).contains(&frames),
        "{frames} frames for 300 ms at 60 Hz"
    );
    assert_eq!(
        surface.frame(
            ring_at(0.5, false, false),
            start + Duration::from_secs(1)
        ),
        RedrawRequest::Wait
    );
}

#[test]
fn indeterminate_spins_on_every_frame() {
    let mut surface = Surface::new();
    let start = Instant::now();
    let frames = surface.frames_until_rest(|| ring_at(0.0, true, false), start, 200);
    assert_eq!(frames, 200, "the spin never rests");
    let after = start + Duration::from_secs(10);
    let frames = surface.frames_until_rest(|| ring_at(0.4, false, false), after, 100);
    assert!((17..=19).contains(&frames), "{frames}");
}

#[test]
fn a_covered_ring_asks_for_nothing() {
    let mut surface = Surface::new();
    let start = Instant::now();
    assert_eq!(
        surface.frame(ring_at(0.0, true, true), start),
        RedrawRequest::Wait,
        "no spin under a fullscreen window"
    );
    assert_eq!(
        surface.frame(ring_at(0.7, false, true), start + FRAME),
        RedrawRequest::Wait,
        "the sweep jumps"
    );
    assert_eq!(
        surface.frame(ring_at(0.7, false, false), start + 2 * FRAME),
        RedrawRequest::Wait,
        "uncovered, it is at its target already"
    );
    assert_eq!(
        surface.frame(ring_at(0.0, true, false), start + 3 * FRAME),
        RedrawRequest::NextFrame,
        "the spin starts again"
    );
}

#[test]
fn a_ring_that_is_not_shown_does_not_spin() {
    let mut surface = Surface::new();
    let view = ring(
        0.0,
        true,
        false,
        false,
        Color::BLACK,
        Color::WHITE,
    );
    assert_eq!(
        surface.frame(view, Instant::now()),
        RedrawRequest::Wait
    );
}

fn bar(progress: f32, indeterminate: bool) -> Element<'static, ()> {
    progress_bar(
        progress,
        indeterminate,
        Color::BLACK,
        Color::WHITE,
    )
}

#[test]
fn the_progress_bar_slides_only_while_indeterminate() {
    let mut surface = Surface::new();
    let start = Instant::now();
    assert_eq!(
        surface.frame(bar(0.3, false), start),
        RedrawRequest::Wait
    );
    let frames = surface.frames_until_rest(|| bar(0.6, false), start, 100);
    assert!(
        (17..=19).contains(&frames),
        "{frames} frames for the 300 ms width"
    );
    let frames = surface.frames_until_rest(
        || bar(0.0, true),
        start + Duration::from_secs(1),
        150,
    );
    assert_eq!(frames, 150, "the slide loops");
    let back = start + Duration::from_secs(5);
    let frames = surface.frames_until_rest(|| bar(0.9, false), back, 100);
    assert!((17..=19).contains(&frames), "{frames}");
}

#[test]
fn the_slide_and_its_easing() {
    use popover::progress::{fill_width, in_out_quad, slide_x};
    assert_eq!(in_out_quad(0.0), 0.0);
    assert_eq!(in_out_quad(0.25), 0.125);
    assert_eq!(in_out_quad(0.5), 0.5);
    assert_eq!(in_out_quad(0.75), 0.875);
    assert_eq!(in_out_quad(1.0), 1.0);
    assert_eq!(fill_width(540.0, 0.5, true), 162.0);
    assert_eq!(fill_width(540.0, 0.5, false), 270.0);
    assert_eq!(fill_width(540.0, 0.0, false), 8.0);
    assert_eq!(slide_x(Duration::ZERO, 162.0, 540.0), -162.0);
    assert_eq!(
        slide_x(Duration::from_millis(650), 162.0, 540.0),
        189.0
    );
}

fn running_view(can_stop: bool) -> View {
    let mut state = install::State {
        run_id: "1".into(),
        unit_active: true,
        job: Job {
            id: Some("1".into()),
            phase: Some(if can_stop { Phase::Auth } else { Phase::Running }),
            ..Job::default()
        },
        runner_can_stop: can_stop,
        ..install::State::default()
    };
    state.pam_stage = PamStage::Pin;
    let mut view = View::default();
    view.write(&state, true);
    view
}

#[test]
fn the_popover_buttons() {
    let theme = Theme::default();
    let sources = Sources::default();
    let mut env = sources.env(&theme);
    let auth = running_view(true);
    env.updates = Some(&auth);
    let mut popover = Updates;
    let mut effects = Vec::new();
    popover.update(popover::Message::Stop, &env, &mut effects);
    popover.update(popover::Message::Hide, &env, &mut effects);
    popover.update(popover::Message::Log, &env, &mut effects);
    popover.update(popover::Message::Redraw, &env, &mut effects);
    assert_eq!(
        effects,
        [
            Effect::Updates(Request::Cancel),
            Effect::Close,
            Effect::Spawn { argv: updates::log_argv(&updates::log_path()), cwd: None },
        ]
    );
    let running = running_view(false);
    env.updates = Some(&running);
    effects.clear();
    popover.update(popover::Message::Stop, &env, &mut effects);
    assert!(effects.is_empty(), "no Stop while pacman runs");
}
