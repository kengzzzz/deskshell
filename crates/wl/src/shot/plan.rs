//! The one-shot capture's logic without Wayland: constraints, format choice and buffer layout.

use std::fmt;

use crate::format::{self, ARGB8888, BYTES_PER_PIXEL, Fourcc, XRGB8888};

/// The largest side accepted, so a broken `buffer_size` cannot ask for gigabytes.
pub(crate) const MAX_SIDE: u32 = 16384;

const XRGB2101010: u32 = format::fourcc(b"XR30");
const ARGB2101010: u32 = format::fourcc(b"AR30");
const TEN_BIT: [u32; 4] = [XRGB2101010, ARGB2101010, format::XBGR2101010, format::ABGR2101010];

/// Both are B, G, R, A in memory. XRGB's fourth byte is undefined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Argb8888,
    Xrgb8888,
}

impl Format {
    pub fn fourcc(self) -> u32 {
        match self {
            Self::Argb8888 => ARGB8888,
            Self::Xrgb8888 => XRGB8888,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Connect(String),
    Missing(&'static str),
    NoOutputs,
    TenBitOnly { output: String },
    NoFormat { output: String, offered: Vec<u32> },
    Size { output: String, width: u32, height: u32 },
    Failed { output: String, reason: &'static str },
    Timeout,
    Io(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(err) => write!(f, "no Wayland connection: {err}"),
            Self::Missing(global) => write!(f, "the compositor has no {global}"),
            Self::NoOutputs => f.write_str("no output to capture"),
            Self::TenBitOnly { output } => write!(
                f,
                "{output} offers only 10-bit formats, set misc:screencopy_force_8b to true"
            ),
            Self::NoFormat { output, offered } => {
                write!(f, "{output} offers no 8-bit RGB format:")?;
                offered
                    .iter()
                    .try_for_each(|code| write!(f, " {}", Fourcc(*code)))
            }
            Self::Size { output, width, height } => {
                write!(f, "{output} asks for a {width}x{height} buffer")
            }
            Self::Failed { output, reason } => write!(f, "capture of {output} failed: {reason}"),
            Self::Timeout => f.write_str("the capture took too long"),
            Self::Io(err) => f.write_str(err),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Constraints {
    pub width: u32,
    pub height: u32,
    /// DRM fourcc codes.
    pub shm: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Layout {
    pub format: Format,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub len: usize,
}

/// XRGB8888 first, since an output has no meaningful alpha.
pub(crate) fn plan(output: &str, constraints: &Constraints) -> Result<Layout, Error> {
    let format = if constraints.shm.contains(&XRGB8888) {
        Format::Xrgb8888
    } else if constraints.shm.contains(&ARGB8888) {
        Format::Argb8888
    } else if constraints
        .shm
        .iter()
        .any(|code| TEN_BIT.contains(code))
    {
        return Err(Error::TenBitOnly { output: output.to_owned() });
    } else {
        return Err(Error::NoFormat {
            output: output.to_owned(),
            offered: constraints.shm.clone(),
        });
    };
    let Constraints { width, height, .. } = *constraints;
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(Error::Size { output: output.to_owned(), width, height });
    }
    let stride = width * BYTES_PER_PIXEL;
    let len = stride as usize * height as usize;
    Ok(Layout { format, width, height, stride, len })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Stage {
    Constraints(Constraints),
    Capturing(Layout),
    Ready(Layout),
    Failed,
}

#[derive(Debug, Default)]
pub(crate) struct Shots {
    names: Vec<String>,
    stages: Vec<Stage>,
    error: Option<Error>,
}

impl Shots {
    pub fn add(&mut self, name: String) -> usize {
        self.names.push(name);
        self.stages
            .push(Stage::Constraints(Constraints::default()));
        self.names.len() - 1
    }

    pub fn name(&self, index: usize) -> &str {
        &self.names[index]
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    fn constraints(&mut self, index: usize) -> Option<&mut Constraints> {
        match self.stages.get_mut(index) {
            Some(Stage::Constraints(constraints)) => Some(constraints),
            _ => None,
        }
    }

    pub fn buffer_size(&mut self, index: usize, width: u32, height: u32) {
        if let Some(constraints) = self.constraints(index) {
            constraints.width = width;
            constraints.height = height;
        }
    }

    /// A `wl_shm` format code, where 0 and 1 stand for ARGB8888 and XRGB8888.
    pub fn shm_format(&mut self, index: usize, code: u32) {
        if let Some(constraints) = self.constraints(index) {
            constraints
                .shm
                .push(format::shm_to_drm(code));
        }
    }

    /// `None` when the target is past its constraints (a later batch is ignored) or failed.
    pub fn done(&mut self, index: usize) -> Option<Layout> {
        let Some(Stage::Constraints(constraints)) = self.stages.get(index) else {
            return None;
        };
        match plan(&self.names[index], constraints) {
            Ok(layout) => {
                self.stages[index] = Stage::Capturing(layout);
                Some(layout)
            }
            Err(error) => {
                self.fail(index, error);
                None
            }
        }
    }

    pub fn fail(&mut self, index: usize, error: Error) {
        self.stages[index] = Stage::Failed;
        self.error.get_or_insert(error);
    }

    pub fn ready(&mut self, index: usize) -> Option<Layout> {
        let Some(Stage::Capturing(layout)) = self.stages.get(index) else {
            return None;
        };
        let layout = *layout;
        self.stages[index] = Stage::Ready(layout);
        Some(layout)
    }

    pub fn failed(&mut self, index: usize, reason: &'static str) {
        if matches!(
            self.stages.get(index),
            Some(Stage::Ready(_)) | None
        ) {
            return;
        }
        let output = self.names[index].clone();
        self.fail(index, Error::Failed { output, reason });
    }

    /// `Some(Ok)` once every output is ready, `Some(Err)` at the first failure, else `None`.
    pub fn outcome(&self) -> Option<Result<(), Error>> {
        if let Some(error) = &self.error {
            return Some(Err(error.clone()));
        }
        self.stages
            .iter()
            .all(|stage| matches!(stage, Stage::Ready(_)))
            .then_some(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::*;

    fn constraints(width: u32, height: u32, shm: &[u32]) -> Constraints {
        Constraints { width, height, shm: shm.to_vec() }
    }

    #[test]
    fn xrgb_wins_then_argb() {
        let both = plan(
            "DP-3",
            &constraints(3440, 1440, &[ARGB8888, XRGB8888]),
        )
        .unwrap();
        assert_eq!(
            both,
            Layout {
                format: Format::Xrgb8888,
                width: 3440,
                height: 1440,
                stride: 13760,
                len: 13760 * 1440
            }
        );
        let argb = plan(
            "DP-3",
            &constraints(8, 2, &[ARGB2101010, ARGB8888]),
        )
        .unwrap();
        assert_eq!(argb.format, Format::Argb8888);
        assert_eq!((argb.stride, argb.len), (32, 64));
    }

    #[test]
    fn ten_bit_only_has_its_own_error() {
        let error = plan(
            "DP-3",
            &constraints(10, 10, &[format::XBGR2101010, XRGB2101010]),
        )
        .unwrap_err();
        assert_eq!(error, Error::TenBitOnly { output: "DP-3".into() });
        assert!(
            error
                .to_string()
                .contains("misc:screencopy_force_8b")
        );
        let nv12 = format::fourcc(b"NV12");
        assert_eq!(
            plan("DP-3", &constraints(10, 10, &[nv12])),
            Err(Error::NoFormat { output: "DP-3".into(), offered: vec![nv12] })
        );
    }

    #[test]
    fn empty_and_huge_buffers_are_refused() {
        for (width, height) in [(0, 10), (10, 0), (MAX_SIDE + 1, 10)] {
            assert_eq!(
                plan("X", &constraints(width, height, &[XRGB8888])),
                Err(Error::Size { output: "X".into(), width, height })
            );
        }
    }

    /// Hyprland's batch: the alpha format, then the preferred one, as `wl_shm` codes 0 and 1.
    fn hyprland_batch(shots: &mut Shots, index: usize, width: u32, height: u32) {
        shots.shm_format(index, 0);
        shots.shm_format(index, 1);
        shots.buffer_size(index, width, height);
    }

    #[test]
    fn two_outputs_end_when_both_are_ready() {
        let mut shots = Shots::default();
        let a = shots.add("DP-1".into());
        let b = shots.add("HDMI-A-1".into());
        hyprland_batch(&mut shots, a, 1280, 720);
        hyprland_batch(&mut shots, b, 1080, 1920);
        assert_eq!(shots.outcome(), None);
        let layout_a = shots.done(a).unwrap();
        assert_eq!(layout_a.format, Format::Xrgb8888);
        assert_eq!(shots.done(a), None, "a later batch is ignored");
        let layout_b = shots.done(b).unwrap();
        assert_eq!((layout_b.width, layout_b.height), (1080, 1920));
        assert_eq!(shots.ready(a), Some(layout_a));
        assert_eq!(shots.outcome(), None, "one is still capturing");
        assert_eq!(shots.ready(a), None, "ready once");
        assert_eq!(shots.ready(b), Some(layout_b));
        assert_eq!(shots.outcome(), Some(Ok(())));
    }

    #[test]
    fn one_failure_fails_the_set() {
        let mut shots = Shots::default();
        let a = shots.add("DP-1".into());
        let b = shots.add("DP-2".into());
        hyprland_batch(&mut shots, a, 100, 100);
        hyprland_batch(&mut shots, b, 100, 100);
        shots.done(a);
        shots.done(b);
        shots.ready(a);
        shots.failed(a, "stopped");
        assert_eq!(shots.outcome(), None, "a ready frame stays ready");
        shots.failed(b, "buffer constraints");
        assert_eq!(
            shots.outcome(),
            Some(Err(Error::Failed {
                output: "DP-2".into(),
                reason: "buffer constraints"
            }))
        );
    }

    #[test]
    fn a_bad_constraint_batch_fails_the_set() {
        let mut shots = Shots::default();
        let a = shots.add("DP-1".into());
        shots.shm_format(a, XRGB2101010);
        shots.buffer_size(a, 100, 100);
        assert_eq!(shots.done(a), None);
        assert_eq!(
            shots.outcome(),
            Some(Err(Error::TenBitOnly { output: "DP-1".into() }))
        );
        assert_eq!(shots.ready(a), None);
    }

    #[test]
    fn a_session_stopped_before_its_constraints() {
        let mut shots = Shots::default();
        let a = shots.add("DP-1".into());
        shots.failed(a, "stopped");
        assert_matches!(shots.outcome(), Some(Err(Error::Failed { .. })));
    }
}
