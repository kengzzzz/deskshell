//! The screenshot selector with generated frames from memory and no compositor.

use std::assert_matches;

use deskshell::screenshot::{Choice, Message, Request, Selector, frame, key_message};
use deskshell_core::screenshot::Region;
use deskshell_ui::image::{Error, Image};
use deskshell_wl::shot;
use iced::keyboard::Key;
use iced::keyboard::key::{Code, Named, NativeCode, Physical};
use iced::window;

struct Rig {
    selector: Selector,
    requests: Vec<Request>,
}

type Convert = (u64, window::Id, String);

impl Rig {
    fn new() -> Self {
        Self { selector: Selector::default(), requests: Vec::new() }
    }

    fn pending(
        &mut self,
        run: u64,
        frames: &[shot::Frame],
        outputs: &[(u32, &str)],
    ) -> Vec<Convert> {
        let names: Vec<&str> = frames
            .iter()
            .map(|frame| frame.output.as_str())
            .collect();
        self.selector.show(
            run,
            &names,
            outputs
                .iter()
                .map(|&(id, name)| (id, Some(name))),
            &mut self.requests,
        );
        std::mem::take(&mut self.requests)
            .into_iter()
            .filter_map(|request| match request {
                Request::Convert { generation, surface, output } => {
                    Some((generation, surface, output))
                }
                _ => None,
            })
            .collect()
    }

    fn feed(&mut self, generation: u64, surface: window::Id, frame: Result<Image, Error>) {
        self.selector.update(
            None,
            Message::Decoded { generation, surface, frame },
            &mut self.requests,
        );
    }

    fn show(
        &mut self,
        run: u64,
        frames: &[shot::Frame],
        outputs: &[(u32, &str)],
    ) -> Vec<(window::Id, u32)> {
        for (generation, surface, output) in self.pending(run, frames, outputs) {
            let frame = frames
                .iter()
                .find(|frame| frame.output == output)
                .map(frame::image)
                .unwrap();
            self.feed(generation, surface, frame);
        }
        self.requests
            .drain(..)
            .map(|request| match request {
                Request::Open { id, output } => (id, output),
                other => panic!("expected an open, got {other:?}"),
            })
            .collect()
    }

    fn send(&mut self, id: window::Id, message: Message) -> Vec<Request> {
        self.selector
            .update(Some(id), message, &mut self.requests);
        std::mem::take(&mut self.requests)
    }

    fn drag(&mut self, id: window::Id, from: (f32, f32), to: (f32, f32)) -> Vec<Request> {
        let mut requests = self.send(id, Message::Press(from.0, from.1));
        requests.extend(self.send(
            id,
            Message::Move((from.0 + to.0) / 2.0, (from.1 + to.1) / 2.0),
        ));
        requests.extend(self.send(id, Message::Move(to.0, to.1)));
        requests.extend(self.send(id, Message::Release));
        requests
    }
}

fn chosen(requests: &[Request]) -> Vec<(u64, &Choice)> {
    requests
        .iter()
        .filter_map(|request| match request {
            Request::Chosen(run, choice) => Some((*run, choice)),
            _ => None,
        })
        .collect()
}

fn closes(requests: &[Request]) -> Vec<window::Id> {
    requests
        .iter()
        .filter_map(|request| match request {
            Request::Close(id) => Some(*id),
            _ => None,
        })
        .collect()
}

fn region(output: &str, width: i64, height: i64, x: i64, y: i64) -> Choice {
    Choice::Region { output: output.into(), region: Region { width, height, x, y } }
}

fn captured(output: &str, width: u32, height: u32) -> shot::Frame {
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            pixels.extend_from_slice(&[0x80, (y % 256) as u8, (x % 256) as u8, 0]);
        }
    }
    shot::Frame {
        output: output.to_owned(),
        width,
        height,
        stride: width * 4,
        format: shot::Format::Xrgb8888,
        pixels,
    }
}

#[test]
fn a_captured_frame_becomes_an_opaque_rgba_image() {
    let image = frame::image(&captured("Virtual-1", 3, 2)).unwrap();
    assert_eq!((image.width, image.height), (3, 2));
    let iced::widget::image::Handle::Rgba { pixels, .. } = &image.handle else {
        panic!("an RGBA handle")
    };
    assert_eq!(&pixels[4..8], &[1, 0, 0x80, 0xff]);
    let short = shot::Frame { pixels: vec![0; 8], ..captured("Virtual-1", 3, 2) };
    assert!(frame::image(&short).is_err());
}

#[test]
fn show_opens_a_surface_for_each_output_with_a_frame() {
    let frames = [captured("Virtual-1", 64, 36), captured("Virtual-2", 32, 18)];
    let mut rig = Rig::new();
    let opened = rig.show(
        7,
        &frames,
        &[(31, "Virtual-1"), (44, "Virtual-2"), (50, "Virtual-3")],
    );
    assert_eq!(
        opened
            .iter()
            .map(|&(_, output)| output)
            .collect::<Vec<_>>(),
        [31, 44],
        "an output without a frame gets no surface"
    );
    assert!(rig.selector.has_keyboard());
    assert!(rig.selector.is_pending());
    assert!(
        opened
            .iter()
            .all(|&(id, _)| rig.selector.is_surface(id))
    );
}

#[test]
fn nothing_shows_before_its_frame_converts() {
    let frames = [captured("Virtual-1", 64, 36), captured("Virtual-2", 32, 18)];
    let mut rig = Rig::new();
    let converts = rig.pending(
        7,
        &frames,
        &[(31, "Virtual-1"), (44, "Virtual-2")],
    );
    assert_eq!(
        converts
            .iter()
            .map(|(_, _, output)| output.as_str())
            .collect::<Vec<_>>(),
        ["Virtual-1", "Virtual-2"]
    );
    assert!(!rig.selector.has_keyboard());
    assert!(rig.selector.is_pending());
}

#[test]
fn a_native_drag_hands_the_region_to_the_flow() {
    let frames = [captured("Virtual-1", 2560, 1440)];
    let mut rig = Rig::new();
    let [(id, _)] = rig.show(7, &frames, &[(31, "Virtual-1")])[..] else { panic!() };
    rig.send(id, Message::Resized(1280.0, 720.0));
    let requests = rig.drag(id, (10.0, 20.0), (110.0, 70.0));
    assert_eq!(
        chosen(&requests),
        [(7, &region("Virtual-1", 200, 100, 20, 40))],
        "frame pixels at scale 2"
    );
    assert_eq!(closes(&requests), [id]);
    assert!(!rig.selector.is_pending());
}

#[test]
fn a_drag_chooses_the_region_in_frame_pixels() {
    let frames = [captured("Virtual-1", 1280, 720)];
    let mut rig = Rig::new();
    let [(id, _)] = rig.show(7, &frames, &[(31, "Virtual-1")])[..] else { panic!() };
    assert!(
        rig.send(id, Message::Resized(1280.0, 720.0))
            .is_empty()
    );
    let requests = rig.drag(id, (300.5, 250.25), (100.0, 50.0));
    assert_eq!(
        chosen(&requests),
        [(7, &region("Virtual-1", 201, 200, 100, 50))]
    );
    assert_eq!(closes(&requests), [id], "the surface closes");
    assert!(!rig.selector.is_pending());
    assert!(
        !rig.selector.is_surface(id),
        "the frame is dropped"
    );
}

#[test]
fn a_drag_on_a_scale_two_output_ends_on_the_surface() {
    let frames = [captured("Virtual-1", 2560, 1440)];
    let mut rig = Rig::new();
    let [(id, _)] = rig.show(7, &frames, &[(31, "Virtual-1")])[..] else { panic!() };
    rig.send(id, Message::Resized(1280.0, 720.0));
    let requests = rig.drag(id, (10.0, 20.0), (2000.0, 30.0));
    assert_eq!(
        chosen(&requests),
        [(7, &region("Virtual-1", 2540, 20, 20, 40))],
        "the end is clamped to the surface"
    );
}

#[test]
fn a_drag_under_four_pixels_starts_over() {
    let frames = [captured("Virtual-1", 1280, 720)];
    let mut rig = Rig::new();
    let [(id, _)] = rig.show(7, &frames, &[(31, "Virtual-1")])[..] else { panic!() };
    rig.send(id, Message::Resized(1280.0, 720.0));
    assert!(
        rig.drag(id, (10.0, 10.0), (13.0, 200.0))
            .is_empty()
    );
    assert_eq!(
        rig.selector.region(id),
        None,
        "the selection is gone"
    );
    assert!(rig.selector.is_pending());
    assert!(
        rig.send(id, Message::Release)
            .is_empty(),
        "a release without a press"
    );
    let requests = rig.drag(id, (10.0, 10.0), (14.0, 14.0));
    assert_eq!(
        chosen(&requests),
        [(7, &region("Virtual-1", 4, 4, 10, 10))]
    );
}

#[test]
fn enter_takes_the_whole_screen_it_was_pressed_on() {
    let frames = [captured("Virtual-1", 64, 36), captured("Virtual-2", 64, 36)];
    let mut rig = Rig::new();
    let opened = rig.show(
        7,
        &frames,
        &[(31, "Virtual-1"), (44, "Virtual-2")],
    );
    let requests = rig.send(opened[1].0, Message::Full);
    assert_eq!(
        chosen(&requests),
        [(7, &Choice::Full("Virtual-2".into()))]
    );
    assert_eq!(
        closes(&requests),
        [opened[0].0, opened[1].0],
        "every surface closes"
    );
}

#[test]
fn native_enter_and_cancel() {
    let frames = [captured("Virtual-1", 64, 36)];
    let mut rig = Rig::new();
    let [(id, _)] = rig.show(7, &frames, &[(31, "Virtual-1")])[..] else { panic!() };
    let requests = rig.send(id, Message::Full);
    assert_eq!(
        chosen(&requests),
        [(7, &Choice::Full("Virtual-1".into()))]
    );

    let [(id, _)] = rig.show(8, &frames, &[(31, "Virtual-1")])[..] else { panic!() };
    let requests = rig.send(id, Message::Cancel);
    assert_eq!(chosen(&requests), [(8, &Choice::Cancel)]);

    rig.show(9, &frames, &[(31, "Virtual-1")]);
    let mut requests = Vec::new();
    assert!(rig.selector.cancel(&mut requests));
    assert_eq!(chosen(&requests), [(9, &Choice::Cancel)]);
    assert!(
        !rig.selector.cancel(&mut requests),
        "nothing pending"
    );
}

#[test]
fn esc_close_and_a_right_press_cancel() {
    let frames = [captured("Virtual-1", 64, 36)];
    for message in [Message::Cancel, Message::RightPress] {
        let mut rig = Rig::new();
        let [(id, _)] = rig.show(7, &frames, &[(31, "Virtual-1")])[..] else { panic!() };
        let requests = rig.send(id, message);
        assert_eq!(chosen(&requests), [(7, &Choice::Cancel)]);
        assert_eq!(closes(&requests), [id]);
        assert!(
            rig.send(id, Message::Full).is_empty(),
            "nothing is pending"
        );
    }
}

#[test]
fn a_delay_counts_down_and_hands_back_once_the_pill_is_gone() {
    let frames = [captured("Virtual-1", 64, 36), captured("Virtual-2", 64, 36)];
    let mut rig = Rig::new();
    let opened = rig.show(
        7,
        &frames,
        &[(31, "Virtual-1"), (44, "Virtual-2")],
    );
    let requests = rig.send(opened[1].0, Message::Delay(3));
    assert_eq!(
        closes(&requests),
        [opened[0].0, opened[1].0],
        "the selector closes first"
    );
    let [.., Request::OpenPill { id: pill, output: 44 }] = requests[..] else {
        panic!("the pill opens on the output the delay was picked on: {requests:?}")
    };
    assert!(chosen(&requests).is_empty());
    assert!(!rig.selector.has_keyboard());
    assert_eq!(rig.selector.countdown(), 3);
    assert!(rig.send(pill, Message::Tick).is_empty());
    assert_eq!(rig.selector.countdown(), 2);
    assert!(rig.send(pill, Message::Tick).is_empty());
    let requests = rig.send(pill, Message::Tick);
    assert_eq!(
        requests,
        [Request::Close(pill)],
        "the pill closes, nothing chosen yet"
    );
    assert!(rig.selector.is_pending());
    assert_eq!(
        rig.selector.closed(opened[0].0),
        None,
        "an old surface closing"
    );
    assert_eq!(
        rig.selector.closed(pill),
        Some(Request::Chosen(7, Choice::Delay)),
        "the delay follows the pill's close"
    );
    assert!(!rig.selector.is_pending());

    assert_eq!(
        rig.show(7, &frames, &[(31, "Virtual-1")])
            .len(),
        1
    );
}

#[test]
fn the_pill_goes_with_its_output_and_the_countdown_goes_on() {
    let frames = [captured("Virtual-1", 64, 36)];
    let mut rig = Rig::new();
    let [(id, _)] = rig.show(7, &frames, &[(31, "Virtual-1")])[..] else { panic!() };
    rig.send(id, Message::Delay(3));
    assert_eq!(rig.selector.output_removed(31), None);
    assert_eq!(rig.selector.pill_id(), None);
    let mut requests = Vec::new();
    for _ in 0..3 {
        rig.selector
            .update(None, Message::Tick, &mut requests);
    }
    assert_eq!(chosen(&requests), [(7, &Choice::Delay)]);
}

#[test]
fn show_without_a_matching_output_cancels_at_once() {
    let mut rig = Rig::new();
    let names = ["HDMI-A-1"];
    let shown = rig.selector.show(
        7,
        &names,
        [(31, Some("Virtual-1")), (32, None)],
        &mut rig.requests,
    );
    assert_eq!(shown, 0);
    assert_eq!(chosen(&rig.requests), [(7, &Choice::Cancel)]);
    assert!(!rig.selector.is_pending());
}

#[test]
fn a_frame_that_fails_to_convert_cancels() {
    let frames = [
        captured("Virtual-1", 64, 36),
        captured("Virtual-2", 64, 36),
        captured("Virtual-3", 64, 36),
    ];
    let mut rig = Rig::new();
    let converts = rig.pending(
        7,
        &frames,
        &[(31, "Virtual-1"), (44, "Virtual-2"), (50, "Virtual-3")],
    );
    let [first, second, third] = &converts[..] else { panic!("{converts:?}") };
    rig.feed(first.0, first.1, frame::image(&frames[0]));
    assert_eq!(
        std::mem::take(&mut rig.requests),
        [Request::Open { id: first.1, output: 31 }]
    );
    let short = shot::Frame { pixels: vec![0; 8], ..captured("Virtual-2", 64, 36) };
    let broken = frame::image(&short);
    assert_matches!(broken, Err(Error::Decode(_)));
    rig.feed(second.0, second.1, broken);
    let requests = std::mem::take(&mut rig.requests);
    assert_eq!(chosen(&requests), [(7, &Choice::Cancel)]);
    assert_eq!(
        closes(&requests),
        [first.1],
        "the surface that showed closes"
    );
    rig.feed(third.0, third.1, frame::image(&frames[2]));
    assert!(
        rig.requests.is_empty(),
        "nothing is pending anymore"
    );
}

#[test]
fn a_newer_show_cancels_the_pending_one_first() {
    let frames = [captured("Virtual-1", 64, 36)];
    let mut rig = Rig::new();
    let [(old_generation, old, _)] = &rig.pending(7, &frames, &[(31, "Virtual-1")])[..] else {
        panic!()
    };
    let (old_generation, old) = (*old_generation, *old);
    rig.feed(old_generation, old, frame::image(&frames[0]));
    rig.requests.clear();

    let names = ["Virtual-1"];
    rig.selector.show(
        8,
        &names,
        [(31, Some("Virtual-1"))],
        &mut rig.requests,
    );
    let requests = std::mem::take(&mut rig.requests);
    assert_eq!(chosen(&requests), [(7, &Choice::Cancel)]);
    assert_eq!(closes(&requests), [old]);
    assert_matches!(requests.last(), Some(Request::Convert { .. }));
    assert!(!rig.selector.is_surface(old));

    rig.feed(old_generation, old, frame::image(&frames[0]));
    assert!(rig.requests.is_empty());

    let mut requests = Vec::new();
    assert!(
        rig.selector.cancel(&mut requests),
        "a selection was pending"
    );
    assert_eq!(chosen(&requests), [(8, &Choice::Cancel)]);
    requests.clear();
    assert!(!rig.selector.cancel(&mut requests));
    assert!(requests.is_empty());
}

#[test]
fn the_keys() {
    let physical = |code| Physical::Code(code);
    assert_matches!(
        key_message(&Key::Named(Named::Enter), physical(Code::Enter)),
        Some(Message::Full)
    );
    assert_matches!(
        key_message(
            &Key::Character("3".into()),
            physical(Code::Digit3)
        ),
        Some(Message::Delay(3))
    );
    assert_matches!(
        key_message(
            &Key::Character("5".into()),
            physical(Code::Digit5)
        ),
        Some(Message::Delay(5))
    );
    assert_matches!(
        key_message(
            &Key::Character("0".into()),
            physical(Code::Digit0)
        ),
        Some(Message::Delay(10))
    );
    assert_matches!(
        key_message(
            &Key::Character("ๅ".into()),
            physical(Code::Digit3)
        ),
        Some(Message::Delay(3)),
        "a Thai layout"
    );
    assert!(
        key_message(
            &Key::Character("1".into()),
            physical(Code::Digit1)
        )
        .is_none()
    );
    assert!(
        key_message(
            &Key::Named(Named::Escape),
            Physical::Unidentified(NativeCode::Unidentified)
        )
        .is_none(),
        "Esc is the shell's"
    );
}
