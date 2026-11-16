//! IPC routes of the share picker and native screenshot, and the share picker's pending call.

use deskshell_core::screenshot::Region;
use deskshell_ipc::{Command, Reply, Request, Screenshot, SharePicker};

use crate::panels::Panel;
use crate::screenshot::flow::Mode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Skip,
    SharePick,
    Screenshot(Mode),
}

/// The shell asks here before `crate::panels::route`, which drops these targets.
pub fn route(cancelled: bool, command: &Command) -> Option<Route> {
    let route = match command {
        Command::SharePicker(SharePicker::Pick { .. }) => Route::SharePick,
        Command::Screenshot(Screenshot::Area) => Route::Screenshot(Mode::Area),
        Command::Screenshot(Screenshot::Full) => Route::Screenshot(Mode::Full),
        _ => return None,
    };
    Some(if cancelled { Route::Skip } else { route })
}

pub trait Caller {
    fn answer(self, line: String);
    fn gone(&self) -> bool;
}

impl Caller for Request {
    fn answer(self, line: String) {
        self.reply(Reply::Text(line));
    }

    fn gone(&self) -> bool {
        self.is_cancelled()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    Picker,
    Region { flags: String, run: Option<u64> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chosen {
    Answered,
    Region,
    Nothing,
}

#[derive(Debug)]
struct Pending<C> {
    pick: u64,
    caller: C,
    stage: Stage,
}

/// Every pick gets exactly one line, empty for a cancel.
#[derive(Debug)]
pub struct ShareReply<C = Request> {
    pending: Option<Pending<C>>,
    picks: u64,
}

impl<C> Default for ShareReply<C> {
    fn default() -> Self {
        Self { pending: None, picks: 0 }
    }
}

impl<C: Caller> ShareReply<C> {
    pub fn open(&mut self, caller: C) -> u64 {
        self.empty();
        self.picks += 1;
        self.pending = Some(Pending { pick: self.picks, caller, stage: Stage::Picker });
        self.picks
    }

    pub fn choose(&mut self, line: String) -> Chosen {
        let Some(mut pending) = self.pending.take() else {
            return Chosen::Nothing;
        };
        match line.split_once('/') {
            Some((flags, "region")) if pending.stage == Stage::Picker => {
                pending.stage = Stage::Region { flags: flags.to_owned(), run: None };
                self.pending = Some(pending);
                Chosen::Region
            }
            _ => {
                pending.caller.answer(line);
                Chosen::Answered
            }
        }
    }

    pub fn region_run(&mut self, run: u64) {
        if let Some(Pending { stage: Stage::Region { run: slot, .. }, .. }) = &mut self.pending {
            *slot = Some(run);
        }
    }

    pub fn region_chosen(&mut self, target: &str) -> bool {
        match self.pending.take() {
            Some(Pending { caller, stage: Stage::Region { flags, .. }, .. }) => {
                caller.answer(format!("{flags}/{target}"));
                true
            }
            other => {
                self.pending = other;
                false
            }
        }
    }

    pub fn empty(&mut self) -> bool {
        match self.pending.take() {
            Some(pending) => {
                pending.caller.answer(String::new());
                true
            }
            None => false,
        }
    }

    pub fn closed(&mut self, open: Option<Panel>) -> bool {
        match &self.pending {
            Some(Pending { stage: Stage::Picker, .. }) if open != Some(Panel::SharePicker) => {
                self.empty()
            }
            _ => false,
        }
    }

    pub fn hung_up(&mut self, pick: u64) -> Option<Stage> {
        match &self.pending {
            Some(pending) if pending.pick == pick && pending.caller.gone() => self
                .pending
                .take()
                .map(|pending| pending.stage),
            _ => None,
        }
    }

    pub fn stage(&self) -> Option<&Stage> {
        self.pending
            .as_ref()
            .map(|pending| &pending.stage)
    }

    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
}

/// xdph passes the box to wlr-screencopy's `capture_output_region`, which takes logical pixels
/// relative to the output. The box is never empty, because xdph refuses a size of 0.
pub fn region_target(output: &str, region: Region, scale: f64) -> String {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let logical = |pixels: i64| (pixels.max(0) as f64 / scale).round() as i64;
    let (left, top) = (logical(region.x), logical(region.y));
    let right = logical(region.x + region.width).max(left + 1);
    let bottom = logical(region.y + region.height).max(top + 1);
    format!(
        "region:{output}@{left},{top},{},{}",
        right - left,
        bottom - top
    )
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use deskshell_ipc as ipc;

    use super::*;

    fn pick() -> Command {
        Command::parse(
            "sharepicker",
            "pick",
            &["1[HC>]a[HT>]b[HE>]2[HA>]", "share"],
        )
        .unwrap()
    }

    fn area() -> Command {
        Command::Screenshot(Screenshot::Area)
    }

    #[test]
    fn picker_calls_route_here() {
        assert_eq!(route(false, &pick()), Some(Route::SharePick));
        assert_eq!(
            route(false, &area()),
            Some(Route::Screenshot(Mode::Area))
        );
        assert_eq!(
            route(false, &Command::Screenshot(Screenshot::Full)),
            Some(Route::Screenshot(Mode::Full))
        );
        for other in [
            Command::Launcher(ipc::Panel::Toggle),
            Command::Lock(ipc::Lock::IsLocked),
            Command::Windows(ipc::Windows::Refresh),
        ] {
            assert_eq!(route(false, &other), None, "{other:?}");
        }
    }

    #[test]
    fn a_timed_out_call_is_skipped() {
        assert_eq!(route(true, &pick()), Some(Route::Skip));
        assert_eq!(route(true, &area()), Some(Route::Skip));
    }

    struct Fake {
        name: &'static str,
        log: Rc<RefCell<Vec<(&'static str, String)>>>,
        gone: Rc<RefCell<bool>>,
    }

    impl Caller for Fake {
        fn answer(self, line: String) {
            self.log
                .borrow_mut()
                .push((self.name, line));
        }

        fn gone(&self) -> bool {
            *self.gone.borrow()
        }
    }

    type Log = Rc<RefCell<Vec<(&'static str, String)>>>;

    fn fake(log: &Log, name: &'static str) -> (Fake, Rc<RefCell<bool>>) {
        let gone = Rc::new(RefCell::new(false));
        (
            Fake { name, log: Rc::clone(log), gone: Rc::clone(&gone) },
            gone,
        )
    }

    fn lines(log: &Log) -> Vec<(&'static str, String)> {
        log.borrow_mut().drain(..).collect()
    }

    #[test]
    fn a_region_keeps_the_pick_until_the_selector_ends() {
        let log = Log::default();
        let mut replies = ShareReply::default();
        let (a, _) = fake(&log, "a");
        replies.open(a);
        assert_eq!(
            replies.choose(String::from("r/region")),
            Chosen::Region
        );
        assert!(
            !replies.closed(None),
            "the picker's close does not answer"
        );
        replies.region_run(4);
        assert_eq!(
            replies.stage(),
            Some(&Stage::Region { flags: String::from("r"), run: Some(4) })
        );
        assert!(replies.region_chosen("region:DP-1@10,20,300,200"));
        assert_eq!(
            lines(&log),
            [("a", String::from("r/region:DP-1@10,20,300,200"))]
        );
        assert!(!replies.is_pending());
        assert!(!replies.region_chosen("region:DP-1@0,0,1,1"));

        let (b, _) = fake(&log, "b");
        replies.open(b);
        assert_eq!(
            replies.choose(String::from("/region")),
            Chosen::Region
        );
        assert!(replies.empty(), "Esc on the selector");
        assert_eq!(lines(&log), [("b", String::new())]);
    }

    #[test]
    fn a_hangup_drops_only_its_own_pick() {
        let log = Log::default();
        let mut replies = ShareReply::default();
        let (a, a_gone) = fake(&log, "a");
        let first = replies.open(a);
        assert_eq!(replies.hung_up(first), None, "still waiting");
        *a_gone.borrow_mut() = true;
        let (b, _) = fake(&log, "b");
        let second = replies.open(b);
        assert_eq!(lines(&log), [("a", String::new())]);
        assert_eq!(
            replies.hung_up(first),
            None,
            "an older pick's hangup leaves the new one"
        );
        assert!(replies.is_pending());
        assert_eq!(replies.hung_up(second), None);
        assert!(replies.is_pending());
    }

    #[test]
    fn a_hangup_returns_the_stage_to_close() {
        let log = Log::default();
        let mut replies = ShareReply::default();
        let (a, gone) = fake(&log, "a");
        let pick = replies.open(a);
        assert_eq!(
            replies.choose(String::from("/region")),
            Chosen::Region
        );
        replies.region_run(9);
        *gone.borrow_mut() = true;
        assert_eq!(
            replies.hung_up(pick),
            Some(Stage::Region { flags: String::new(), run: Some(9) })
        );
        assert!(lines(&log).is_empty(), "nobody to answer");
        assert!(!replies.is_pending());
    }

    fn region(x: i64, y: i64, width: i64, height: i64) -> Region {
        Region { width, height, x, y }
    }

    #[test]
    fn native_pixels_become_logical_ones() {
        assert_eq!(
            region_target("DP-1", region(100, 50, 640, 380), 1.0),
            "region:DP-1@100,50,640,380"
        );
        assert_eq!(
            region_target("DP-2", region(150, 75, 600, 300), 1.5),
            "region:DP-2@100,50,400,200"
        );
        assert_eq!(
            region_target("DP-3", region(201, 99, 401, 199), 2.0),
            "region:DP-3@101,50,200,99",
            "the edges round to the nearest logical pixel"
        );
        assert_eq!(
            region_target("eDP-1", region(320, 160, 640, 640), 1.6),
            "region:eDP-1@200,100,400,400",
            "a scale without an exact binary form"
        );
    }

    #[test]
    fn an_offset_output_keeps_the_box_relative() {
        let at_origin = region_target("DP-1", region(30, 40, 300, 200), 1.25);
        let offset = region_target("HDMI-A-1", region(30, 40, 300, 200), 1.25);
        assert_eq!(at_origin, "region:DP-1@24,32,240,160");
        assert_eq!(offset, "region:HDMI-A-1@24,32,240,160");
    }

    #[test]
    fn a_box_is_never_empty() {
        assert_eq!(
            region_target("DP-1", region(10, 10, 1, 1), 2.0),
            "region:DP-1@5,5,1,1"
        );
        assert_eq!(
            region_target("DP-1", region(-4, -2, 10, 10), 0.0),
            "region:DP-1@0,0,6,8",
            "a bad scale counts as 1, negative edges as 0"
        );
    }
}
