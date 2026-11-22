//! The screenshot selector, its countdown pill, and the native screenshot flow.

pub mod clearance;
pub mod copy;
pub mod flow;
pub mod frame;
pub mod notice;
pub mod save;
mod selection;
mod view;

use std::fmt::Write as _;
use std::time::Duration;

use deskshell_core::screenshot::{self as logic, Drag, Region};
use deskshell_ui::image::{Error, Image};
use iced::keyboard::{self, Key, key};
use iced::{Event, event, window};
use tracing::{info, warn};

pub use view::{pill, view};

pub const NAMESPACE: &str = "deskshell-screenshot";
pub const TICK: Duration = Duration::from_secs(1);
pub const PILL_TOP: i32 = 24;
pub const PILL_SIZE: (u32, u32) = (180, 48);

#[derive(Debug, Clone)]
pub enum Message {
    /// A frame finished converting.
    Decoded {
        generation: u64,
        surface: window::Id,
        frame: Result<Image, Error>,
    },
    Resized(f32, f32),
    Press(f32, f32),
    Move(f32, f32),
    Release,
    RightPress,
    Full,
    Delay(u32),
    Cancel,
    Tick,
}

impl Message {
    pub fn redraws(&self) -> bool {
        !matches!(self, Self::Decoded { .. } | Self::Resized(..))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Open {
        id: window::Id,
        output: u32,
    },
    OpenPill {
        id: window::Id,
        output: u32,
    },
    Close(window::Id),
    /// Turn the flow's frame of `output` into an image off the UI thread.
    Convert {
        generation: u64,
        surface: window::Id,
        output: String,
    },
    Chosen(u64, Choice),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    Region {
        output: String,
        region: Region,
    },
    Full(String),
    /// The countdown ended and its pill is gone. The caller captures again.
    Delay,
    Cancel,
}

#[derive(Debug)]
struct Surface {
    id: window::Id,
    output: u32,
    name: String,
    frame: Option<Image>,
    size: Option<(f32, f32)>,
    drag: Drag,
    dragging: bool,
    label: String,
}

impl Surface {
    fn new(id: window::Id, output: u32, name: &str) -> Self {
        Self {
            id,
            output,
            name: name.to_owned(),
            frame: None,
            size: None,
            drag: Drag::default(),
            dragging: false,
            label: String::new(),
        }
    }

    fn scale(&self) -> f64 {
        logic::pixel_scale(
            self.frame
                .as_ref()
                .map_or(0, |frame| frame.width),
            self.size
                .map_or(0.0, |(width, _)| f64::from(width)),
        )
    }

    fn region(&self) -> Region {
        Region::of(self.drag.rect(), self.scale())
    }
}

#[derive(Debug, Clone, Copy)]
struct Pill {
    id: window::Id,
    output: u32,
}

#[derive(Debug, Default)]
pub struct Selector {
    pending: Option<u64>,
    live: bool,
    generation: u64,
    surfaces: Vec<Surface>,
    countdown: u32,
    pill: Option<Pill>,
    // The pill closed at zero. `delay` waits for its surface to be gone, because the flow
    // captures again as soon as it gets it.
    delay_after: Option<Pill>,
    region_only: bool,
}

impl Selector {
    pub fn show<'a>(
        &mut self,
        run: u64,
        frames: &[&str],
        outputs: impl IntoIterator<Item = (u32, Option<&'a str>)>,
        requests: &mut Vec<Request>,
    ) -> usize {
        self.show_with(run, frames, outputs, false, requests)
    }

    pub fn show_region<'a>(
        &mut self,
        run: u64,
        frames: &[&str],
        outputs: impl IntoIterator<Item = (u32, Option<&'a str>)>,
        requests: &mut Vec<Request>,
    ) -> usize {
        self.show_with(run, frames, outputs, true, requests)
    }

    fn show_with<'a>(
        &mut self,
        run: u64,
        frames: &[&str],
        outputs: impl IntoIterator<Item = (u32, Option<&'a str>)>,
        region_only: bool,
        requests: &mut Vec<Request>,
    ) -> usize {
        self.finish(Choice::Cancel, requests);
        self.region_only = region_only;
        self.generation += 1;
        self.pending = Some(run);
        self.live = true;
        for (output, name) in outputs {
            let Some(name) = name.filter(|name| frames.contains(name)) else {
                warn!(
                    output,
                    ?name,
                    "an output without a frame gets no selector"
                );
                continue;
            };
            let id = window::Id::unique();
            requests.push(Request::Convert {
                generation: self.generation,
                surface: id,
                output: name.to_owned(),
            });
            self.surfaces
                .push(Surface::new(id, output, name));
        }
        info!(
            run,
            frames = self.surfaces.len(),
            "screenshot selection"
        );
        let shown = self.surfaces.len();
        if shown == 0 {
            self.finish(Choice::Cancel, requests);
        }
        shown
    }

    pub fn cancel(&mut self, requests: &mut Vec<Request>) -> bool {
        let pending = self.pending.is_some();
        self.finish(Choice::Cancel, requests);
        pending
    }

    fn finish(&mut self, choice: Choice, requests: &mut Vec<Request>) {
        let Some(run) = self.pending.take() else {
            return;
        };
        self.live = false;
        self.countdown = 0;
        self.close_surfaces(requests);
        if let Some(pill) = self.pill.take() {
            requests.push(Request::Close(pill.id));
        }
        self.delay_after = None;
        requests.push(Request::Chosen(run, choice));
    }

    fn close_surfaces(&mut self, requests: &mut Vec<Request>) {
        for surface in self.surfaces.drain(..) {
            if surface.frame.is_some() {
                requests.push(Request::Close(surface.id));
            }
        }
    }

    pub fn update(
        &mut self,
        from: Option<window::Id>,
        message: Message,
        requests: &mut Vec<Request>,
    ) {
        match message {
            Message::Decoded { generation, surface, frame } => {
                self.decoded(generation, surface, frame, requests);
            }
            Message::Tick => self.tick(requests),
            Message::Cancel => self.finish(Choice::Cancel, requests),
            message => {
                if let Some(id) = from {
                    self.surface_message(id, &message, requests);
                }
            }
        }
    }

    fn decoded(
        &mut self,
        generation: u64,
        id: window::Id,
        frame: Result<Image, Error>,
        requests: &mut Vec<Request>,
    ) {
        if generation != self.generation || !self.live {
            return;
        }
        let Some(surface) = self
            .surfaces
            .iter_mut()
            .find(|surface| surface.id == id)
        else {
            return;
        };
        match frame {
            Ok(frame) => {
                info!(
                    output = surface.name,
                    width = frame.width,
                    height = frame.height,
                    "screenshot frame ready"
                );
                surface.frame = Some(frame);
                requests.push(Request::Open { id, output: surface.output });
            }
            Err(err) => {
                warn!(output = surface.name, %err, "screenshot frame failed, selection cancelled");
                self.finish(Choice::Cancel, requests);
            }
        }
    }

    fn surface_message(&mut self, id: window::Id, message: &Message, requests: &mut Vec<Request>) {
        let Some(index) = self
            .surfaces
            .iter()
            .position(|surface| surface.id == id && surface.frame.is_some())
        else {
            return;
        };
        if self.region_only && matches!(message, Message::Full | Message::Delay(_)) {
            return;
        }
        let surface = &mut self.surfaces[index];
        match *message {
            Message::Resized(width, height) => surface.size = Some((width, height)),
            Message::Press(x, y) => {
                surface.dragging = true;
                surface.drag = Drag::at(f64::from(x), f64::from(y));
                describe(surface);
            }
            Message::Move(x, y) if surface.dragging => {
                let (width, height) = surface
                    .size
                    .unwrap_or((f32::MAX, f32::MAX));
                surface.drag.move_to(
                    f64::from(x),
                    f64::from(y),
                    f64::from(width),
                    f64::from(height),
                );
                describe(surface);
            }
            Message::Release if surface.dragging => {
                surface.dragging = false;
                let region = surface.region();
                if region.big_enough() {
                    let output = surface.name.clone();
                    self.finish(Choice::Region { output, region }, requests);
                } else {
                    surface.drag = Drag::default();
                    describe(surface);
                }
            }
            Message::RightPress => self.finish(Choice::Cancel, requests),
            Message::Full => {
                let output = surface.name.clone();
                self.finish(Choice::Full(output), requests);
            }
            Message::Delay(seconds) => {
                let output = surface.output;
                self.start_delay(seconds, output, requests);
            }
            _ => {}
        }
    }

    fn start_delay(&mut self, seconds: u32, output: u32, requests: &mut Vec<Request>) {
        info!(seconds, "screenshot delay");
        self.live = false;
        self.close_surfaces(requests);
        self.countdown = seconds;
        if let Some(pill) = self.pill.take() {
            requests.push(Request::Close(pill.id));
        }
        let id = window::Id::unique();
        self.pill = Some(Pill { id, output });
        requests.push(Request::OpenPill { id, output });
    }

    fn tick(&mut self, requests: &mut Vec<Request>) {
        if self.countdown == 0 {
            return;
        }
        self.countdown -= 1;
        if self.countdown > 0 {
            return;
        }
        match self.pill.take() {
            Some(pill) => {
                self.delay_after = Some(pill);
                requests.push(Request::Close(pill.id));
            }
            None => self.finish(Choice::Delay, requests),
        }
    }

    pub fn closed(&mut self, id: window::Id) -> Option<Request> {
        if self
            .delay_after
            .is_some_and(|pill| pill.id == id)
        {
            self.delay_after = None;
            return self.take_delay();
        }
        if self
            .pill
            .is_some_and(|pill| pill.id == id)
        {
            self.pill = None;
        }
        self.surfaces
            .retain(|surface| surface.id != id);
        None
    }

    pub fn output_removed(&mut self, output: u32) -> Option<Request> {
        self.surfaces
            .retain(|surface| surface.output != output);
        if self
            .pill
            .is_some_and(|pill| pill.output == output)
        {
            self.pill = None;
        }
        if self
            .delay_after
            .is_some_and(|pill| pill.output == output)
        {
            self.delay_after = None;
            return self.take_delay();
        }
        None
    }

    fn take_delay(&mut self) -> Option<Request> {
        let run = self.pending.take()?;
        self.live = false;
        self.countdown = 0;
        Some(Request::Chosen(run, Choice::Delay))
    }

    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn region_only(&self) -> bool {
        self.region_only
    }

    pub fn countdown(&self) -> u32 {
        self.countdown
    }

    pub fn ticking(&self) -> bool {
        self.countdown > 0
    }

    pub fn owns(&self, id: window::Id) -> bool {
        self.is_surface(id) || self.is_pill(id)
    }

    pub fn is_surface(&self, id: window::Id) -> bool {
        self.surfaces
            .iter()
            .any(|surface| surface.id == id)
    }

    pub fn is_pill(&self, id: window::Id) -> bool {
        self.pill
            .is_some_and(|pill| pill.id == id)
    }

    pub fn pill_id(&self) -> Option<window::Id> {
        self.pill.map(|pill| pill.id)
    }

    pub fn has_keyboard(&self) -> bool {
        self.surfaces
            .iter()
            .any(|surface| surface.frame.is_some())
    }

    pub fn outputs(&self) -> impl Iterator<Item = &str> {
        self.surfaces
            .iter()
            .map(|surface| surface.name.as_str())
    }

    pub fn region(&self, id: window::Id) -> Option<Region> {
        let surface = self
            .surfaces
            .iter()
            .find(|surface| surface.id == id)?;
        surface
            .drag
            .rect()
            .visible()
            .then(|| surface.region())
    }

    pub fn surfaces(&self) -> impl Iterator<Item = (window::Id, &str, bool)> {
        self.surfaces.iter().map(|surface| {
            (
                surface.id,
                surface.name.as_str(),
                surface.frame.is_some(),
            )
        })
    }
}

fn describe(surface: &mut Surface) {
    let region = surface.region();
    surface.label.clear();
    let _ = write!(
        surface.label,
        "{} × {}",
        region.width, region.height
    );
}

/// Digits go by their Latin key, so they work on a Thai layout.
pub fn key(
    event: Event,
    _status: event::Status,
    window: window::Id,
) -> Option<(window::Id, Message)> {
    let Event::Keyboard(keyboard::Event::KeyPressed { key, physical_key, .. }) = event else {
        return None;
    };
    key_message(&key, physical_key).map(|message| (window, message))
}

pub fn key_message(key: &Key, physical_key: key::Physical) -> Option<Message> {
    if *key == Key::Named(key::Named::Enter) {
        return Some(Message::Full);
    }
    key.to_latin(physical_key)
        .and_then(logic::delay_for_key)
        .map(Message::Delay)
}
