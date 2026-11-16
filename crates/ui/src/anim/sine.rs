//! The InOutSine easing curve.

pub fn in_out_sine(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    -0.5 * ((std::f32::consts::PI * t).cos() - 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn matches_easing_cpp() {
        assert!(close(in_out_sine(0.0), 0.0));
        assert!(close(
            in_out_sine(0.25),
            0.5 - 0.5 * std::f32::consts::FRAC_1_SQRT_2
        ));
        assert!(close(in_out_sine(0.5), 0.5));
        assert!(close(in_out_sine(1.0), 1.0));
        assert!(close(in_out_sine(-1.0), 0.0));
        assert!(close(in_out_sine(2.0), 1.0));
    }
}
