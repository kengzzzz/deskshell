//! The dmabuf modifiers that the capture session, linux-dmabuf and the Vulkan driver all accept.

use crate::feedback::Feedback;
use crate::format::MOD_INVALID;

/// Keeps the compositor's order. Empty means shm. `feedback` is skipped when none was sent.
pub(crate) fn intersect(
    format: u32,
    compositor: &[u64],
    feedback: Option<&Feedback>,
    driver: &[u64],
    out: &mut Vec<u64>,
) {
    out.clear();
    for &modifier in compositor {
        if modifier == MOD_INVALID || out.contains(&modifier) || !driver.contains(&modifier) {
            continue;
        }
        if feedback.is_some_and(|feedback| !feedback.allows(format, modifier)) {
            continue;
        }
        out.push(modifier);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{ARGB8888, MOD_LINEAR, XRGB8888};

    const NV_A: u64 = 0x0300_0000_0060_6014;
    const NV_B: u64 = 0x0300_0000_0060_6015;
    const NV_C: u64 = 0x0300_0000_0060_6010;

    fn feedback(main: u64, entries: &[(u32, u64)]) -> Feedback {
        let mut feedback = Feedback::default();
        feedback.set_table(entries.to_vec());
        feedback.main_device(&main.to_ne_bytes());
        feedback.tranche_device(&main.to_ne_bytes());
        let indices: Vec<u8> = (0..entries.len() as u16)
            .flat_map(u16::to_ne_bytes)
            .collect();
        feedback.tranche_formats(&indices);
        feedback.tranche_done();
        feedback.done();
        feedback
    }

    #[test]
    fn linear_only() {
        let mut out = Vec::new();
        let fb = feedback(
            1,
            &[(ARGB8888, MOD_LINEAR), (ARGB8888, MOD_INVALID)],
        );
        intersect(
            ARGB8888,
            &[MOD_LINEAR, MOD_INVALID],
            Some(&fb),
            &[MOD_LINEAR],
            &mut out,
        );
        assert_eq!(out, vec![MOD_LINEAR]);
    }

    #[test]
    fn nvidia_block_linear() {
        let mut out = Vec::new();
        let compositor = [NV_A, NV_B, NV_C, MOD_INVALID];
        let fb = feedback(
            1,
            &[(ARGB8888, NV_A), (ARGB8888, NV_B), (ARGB8888, NV_C)],
        );
        let driver = [MOD_LINEAR, NV_C, NV_A];
        intersect(
            ARGB8888,
            &compositor,
            Some(&fb),
            &driver,
            &mut out,
        );
        assert_eq!(out, vec![NV_A, NV_C]);
    }

    #[test]
    fn linear_from_the_driver_is_not_enough_when_the_compositor_rejects_it() {
        let mut out = Vec::new();
        let fb = feedback(1, &[(ARGB8888, NV_A)]);
        intersect(
            ARGB8888,
            &[NV_A, MOD_INVALID],
            Some(&fb),
            &[MOD_LINEAR],
            &mut out,
        );
        assert!(
            out.is_empty(),
            "empty intersection falls back to shm"
        );
    }

    #[test]
    fn feedback_filters_pairs_the_session_lists() {
        let mut out = Vec::new();
        let fb = feedback(1, &[(XRGB8888, NV_A), (ARGB8888, NV_B)]);
        intersect(
            ARGB8888,
            &[NV_A, NV_B],
            Some(&fb),
            &[NV_A, NV_B],
            &mut out,
        );
        assert_eq!(out, vec![NV_B]);
        intersect(
            ARGB8888,
            &[NV_A, NV_B],
            None,
            &[NV_A, NV_B],
            &mut out,
        );
        assert_eq!(out, vec![NV_A, NV_B]);
    }
}
