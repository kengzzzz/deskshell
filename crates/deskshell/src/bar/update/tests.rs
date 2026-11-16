use deskshell_system::install::{self, Job, Phase};

use super::*;

#[test]
fn sweeps() {
    assert_eq!(sweep(0.0, false), 8.0);
    assert_eq!(sweep(0.5, false), 180.0);
    assert_eq!(sweep(0.2, true), 90.0);
    assert_eq!(sweep(2.0, false), 360.0);
}

#[test]
fn a_turn_takes_1100_ms() {
    assert_eq!(spin_angle(Duration::ZERO), 0.0);
    let quarter = spin_angle(Duration::from_millis(275));
    assert!((quarter - std::f32::consts::FRAC_PI_2).abs() < 1e-4);
    assert!(spin_angle(Duration::from_millis(1100)).abs() < 1e-4);
    let again = spin_angle(Duration::from_millis(1100 + 550));
    assert!((again - std::f32::consts::PI).abs() < 1e-3);
}

fn view(phase: Phase, done: i64, total: i64) -> View {
    let state = install::State {
        run_id: "1".into(),
        unit_active: true,
        job: Job {
            id: Some("1".into()),
            phase: Some(phase),
            step: "install".into(),
            done: Some(done),
            total: Some(total),
            ..Job::default()
        },
        ..install::State::default()
    };
    let mut view = View::default();
    view.write(&state, true);
    view
}

#[test]
fn the_pill_texts() {
    let mut update = Update::default();
    assert!(update.set(&view(Phase::Running, 3, 12)));
    assert!(update.active);
    assert_eq!(update.label(), "Updating");
    assert_eq!(update.count, "3/12");
    assert!(update.shows_count());
    assert!(!update.set(&view(Phase::Running, 3, 12)));
    assert!(update.set(&view(Phase::Auth, 0, 12)));
    assert_eq!(update.label(), "Authorize");
    assert!(!update.shows_count());
    assert!(update.set(&View::default()));
    assert!(!update.active);
}

#[test]
fn the_arc_starts_at_the_top() {
    let Handle::Rgba { width, pixels, .. } = raster(90.0, Color::TRANSPARENT, Color::WHITE) else {
        panic!("an RGBA handle");
    };
    let alpha = |x: u32, y: u32| pixels[((y * width + x) * 4 + 3) as usize];
    assert_eq!(alpha(26, 3), 255, "top");
    assert_eq!(
        alpha(48, 26),
        255,
        "right, at 90 degrees, under the round cap"
    );
    assert_eq!(alpha(26, 48), 0, "bottom");
    assert_eq!(alpha(3, 26), 0, "left");
}
