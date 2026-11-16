//! Turns mono samples into 16 spectrum bars, modelled on cava. It counts samples and has no clock.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

pub const BARS: usize = 16;
pub type Frame = [f32; BARS];

pub const FRAME_RATE: u32 = 60;
pub const LOW_CUTOFF: f32 = 50.0;
pub const HIGH_CUTOFF: f32 = 10_000.0;
pub const NOISE_REDUCTION: f32 = 0.8;
pub const SLEEP_SECONDS: u32 = 2;
/// At 48 kHz this is 11.7 Hz per bin, so the lowest bar still gets one bin of its own.
pub const FFT_LEN: usize = 4096;

/// Not exactly zero, so it also absorbs denormals.
const SILENCE: f32 = 1.0e-6;
const FALL_STEP: f32 = 0.028;
const AUTOSENS_DOWN: f32 = 0.98;
const AUTOSENS_UP: f32 = 1.001;
const AUTOSENS_INIT: f32 = 1.1;

pub fn band_edges() -> [f32; BARS + 1] {
    let ratio = HIGH_CUTOFF / LOW_CUTOFF;
    std::array::from_fn(|n| LOW_CUTOFF * ratio.powf(n as f32 / BARS as f32))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    None,
    Frame,
}

pub struct Analyser {
    rate: u32,
    hop: usize,
    sleep_after: usize,
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    ring: Vec<f32>,
    pos: usize,
    input: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    bins: [(usize, usize); BARS],
    // The bar's lower edge in kHz, so the bars rise with frequency and music comes out level.
    eq: [f32; BARS],
    since_frame: usize,
    hop_loud: bool,
    silent_run: usize,
    asleep: bool,
    sens: f32,
    sens_init: bool,
    gravity: f32,
    peak: [f32; BARS],
    fall: [f32; BARS],
    prev: [f32; BARS],
    memory: [f32; BARS],
    frame: Frame,
}

impl Analyser {
    pub fn new(rate: u32) -> Self {
        let rate = rate.max(FRAME_RATE);
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_LEN);
        let window = (0..FFT_LEN)
            .map(|i| {
                let phase = std::f32::consts::TAU * i as f32 / (FFT_LEN - 1) as f32;
                0.5 * (1.0 - phase.cos())
            })
            .collect();
        let spectrum = fft.make_output_vec();
        let scratch = fft.make_scratch_vec();
        let edges = band_edges();
        let bin_hz = rate as f32 / FFT_LEN as f32;
        let last_bin = spectrum.len() - 1;
        let mut bins = [(0, 0); BARS];
        let mut next = 1;
        for (n, range) in bins.iter_mut().enumerate() {
            let lo = ((edges[n] / bin_hz).ceil() as usize)
                .max(next)
                .min(last_bin);
            let hi = ((edges[n + 1] / bin_hz).ceil() as usize)
                .saturating_sub(1)
                .clamp(lo, last_bin);
            *range = (lo, hi);
            next = hi + 1;
        }
        let eq = std::array::from_fn(|n| edges[n] / 1000.0);
        let fps = rate as f32 / (rate / FRAME_RATE) as f32;
        let gravity = ((60.0 / fps).powf(2.5) * 1.54 / NOISE_REDUCTION).max(1.0);
        Self {
            rate,
            hop: (rate / FRAME_RATE) as usize,
            sleep_after: (rate * SLEEP_SECONDS) as usize,
            fft,
            window,
            ring: vec![0.0; FFT_LEN],
            pos: 0,
            input: vec![0.0; FFT_LEN],
            spectrum,
            scratch,
            bins,
            eq,
            since_frame: 0,
            hop_loud: false,
            silent_run: 0,
            asleep: false,
            sens: 1.0,
            sens_init: true,
            gravity,
            peak: [0.0; BARS],
            fall: [0.0; BARS],
            prev: [0.0; BARS],
            memory: [0.0; BARS],
            frame: [0.0; BARS],
        }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn bins(&self) -> &[(usize, usize); BARS] {
        &self.bins
    }

    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    pub fn is_asleep(&self) -> bool {
        self.asleep
    }

    pub fn reset(&mut self) {
        self.ring.fill(0.0);
        self.pos = 0;
        self.since_frame = 0;
        self.hop_loud = false;
        self.silent_run = 0;
        self.asleep = false;
        self.sens = 1.0;
        self.sens_init = true;
        self.clear_bars();
    }

    pub fn sleep(&mut self) -> bool {
        let changed = self.frame.iter().any(|&bar| bar != 0.0);
        self.asleep = true;
        self.clear_bars();
        changed
    }

    fn clear_bars(&mut self) {
        self.peak = [0.0; BARS];
        self.fall = [0.0; BARS];
        self.prev = [0.0; BARS];
        self.memory = [0.0; BARS];
        self.frame = [0.0; BARS];
    }

    pub fn push(&mut self, sample: f32) -> Output {
        self.ring[self.pos] = sample;
        self.pos = (self.pos + 1) % FFT_LEN;
        if sample.abs() > SILENCE {
            self.hop_loud = true;
            self.silent_run = 0;
            self.asleep = false;
        } else {
            self.silent_run = self.silent_run.saturating_add(1);
        }
        self.since_frame += 1;
        if self.since_frame < self.hop {
            return Output::None;
        }
        self.since_frame = 0;
        let loud = std::mem::take(&mut self.hop_loud);
        if self.asleep {
            return Output::None;
        }
        if self.silent_run >= self.sleep_after {
            self.sleep();
            return Output::Frame;
        }
        self.compute(loud);
        Output::Frame
    }

    fn compute(&mut self, loud: bool) {
        let (older, newer) = self.ring.split_at(self.pos);
        for ((slot, &sample), &w) in self
            .input
            .iter_mut()
            .zip(newer.iter().chain(older))
            .zip(&self.window)
        {
            *slot = sample * w;
        }
        if self
            .fft
            .process_with_scratch(
                &mut self.input,
                &mut self.spectrum,
                &mut self.scratch,
            )
            .is_err()
        {
            return;
        }

        // A full scale sine on a bin centre has a magnitude of FFT_LEN / 4 under a Hann window.
        let scale = 4.0 / FFT_LEN as f32;
        let mut overshoot = false;
        for bar in 0..BARS {
            let (lo, hi) = self.bins[bar];
            let sum: f32 = self.spectrum[lo..=hi]
                .iter()
                .map(|c| c.norm())
                .sum();
            let mut value = sum / (hi - lo + 1) as f32 * scale * self.eq[bar] * self.sens;

            if value < self.prev[bar] {
                let fall = self.fall[bar];
                value = (self.peak[bar] * (1.0 - fall * fall * self.gravity)).max(0.0);
                self.fall[bar] += FALL_STEP;
            } else {
                self.peak[bar] = value;
                self.fall[bar] = 0.0;
            }
            self.prev[bar] = value;

            value += self.memory[bar] * NOISE_REDUCTION;
            self.memory[bar] = value;
            overshoot |= value > 1.0;
            self.frame[bar] = value.min(1.0);
        }

        if overshoot {
            self.sens *= AUTOSENS_DOWN;
            self.sens_init = false;
        } else if loud {
            self.sens *= AUTOSENS_UP;
            if self.sens_init {
                self.sens *= AUTOSENS_INIT;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn feed_sine(analyser: &mut Analyser, hz: f32, amplitude: f32, seconds: f32, t0: &mut usize) {
        let count = (RATE as f32 * seconds) as usize;
        for _ in 0..count {
            let t = *t0 as f32 / RATE as f32;
            analyser.push(amplitude * (std::f32::consts::TAU * hz * t).sin());
            *t0 += 1;
        }
    }

    fn loudest(frame: &Frame) -> usize {
        let mut best = 0;
        for (i, &v) in frame.iter().enumerate() {
            if v > frame[best] {
                best = i;
            }
        }
        best
    }

    #[test]
    fn band_edges_are_log_spaced() {
        let edges = band_edges();
        assert!((edges[0] - 50.0).abs() < 1e-3);
        assert!((edges[8] - 707.107).abs() < 0.01);
        assert!((edges[16] - 10_000.0).abs() < 0.5);
        let analyser = Analyser::new(RATE);
        let bins = analyser.bins();
        assert_eq!(bins[0], (5, 5));
        assert_eq!(bins[15], (613, 853));
        for pair in bins.windows(2) {
            assert_eq!(
                pair[1].0,
                pair[0].1 + 1,
                "bars share no bins and leave no gaps"
            );
        }
    }

    #[test]
    fn a_sine_lands_in_its_band() {
        let edges = band_edges();
        for bar in [1, 5, 9, 14] {
            let hz = (edges[bar] * edges[bar + 1]).sqrt();
            let mut analyser = Analyser::new(RATE);
            let mut t = 0;
            feed_sine(&mut analyser, hz, 0.5, 1.0, &mut t);
            let frame = *analyser.frame();
            assert_eq!(loudest(&frame), bar, "{hz} Hz: {frame:?}");
            assert!(
                frame[bar] > 0.5,
                "{hz} Hz reaches a visible height: {frame:?}"
            );
        }
    }

    #[test]
    fn frames_come_at_sixty_per_second() {
        let mut analyser = Analyser::new(RATE);
        let frames = (0..RATE).filter(|i| analyser.push((*i as f32).sin() * 0.1) == Output::Frame);
        assert_eq!(frames.count(), 60);
    }

    #[test]
    fn silence_ends_the_frames_after_two_seconds() {
        let mut analyser = Analyser::new(RATE);
        let mut t = 0;
        feed_sine(&mut analyser, 1000.0, 0.5, 0.5, &mut t);
        assert!(
            analyser
                .frame()
                .iter()
                .any(|&v| v > 0.1)
        );

        let mut frames = Vec::new();
        for i in 0..(RATE * 3) as usize {
            if analyser.push(0.0) == Output::Frame {
                frames.push((i, *analyser.frame()));
            }
        }
        let (last_at, last) = frames
            .last()
            .expect("frames during the silence");
        assert_eq!(last, &[0.0; BARS], "the last frame is all zero");
        let two_seconds = (RATE * SLEEP_SECONDS) as usize;
        assert!(
            *last_at >= two_seconds - 1 && *last_at < two_seconds + 800,
            "at {last_at}"
        );
        assert!(analyser.is_asleep());
        assert!(
            frames
                .iter()
                .rev()
                .nth(1)
                .is_some_and(|(_, f)| f.iter().all(|&v| v < 0.01))
        );

        feed_sine(&mut analyser, 1000.0, 0.5, 0.1, &mut t);
        assert!(!analyser.is_asleep());
        assert!(
            analyser
                .frame()
                .iter()
                .any(|&v| v > 0.0)
        );
    }

    #[test]
    fn autosens_keeps_loud_input_in_range() {
        let mut analyser = Analyser::new(RATE);
        let mut t = 0;
        feed_sine(&mut analyser, 440.0, 1.0, 5.0, &mut t);
        let frame = analyser.frame();
        assert!(
            frame
                .iter()
                .all(|v| (0.0..=1.0).contains(v))
        );
        let bar = loudest(frame);
        assert!(frame[bar] > 0.8, "{frame:?}");
    }
}
