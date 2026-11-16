//! The OutExpo easing curve that eases each wheel step.

// Qt's curve. It is scaled by 1.001 so it reaches 1 without the textbook curve's jump at the
// end, and is exactly 1 at `t = 1`.
pub fn out_expo(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    if t == 1.0 { 1.0 } else { 1.001 * (1.0 - 2f64.powf(-10.0 * t)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-12
    }

    #[test]
    fn matches_easing_cpp() {
        assert_eq!(out_expo(0.0), 0.0);
        assert!(close(out_expo(0.1), 1.001 * 0.5));
        assert!(close(out_expo(0.5), 1.001 * (1.0 - 1.0 / 32.0)));
        assert_eq!(out_expo(1.0), 1.0);
        assert_eq!(out_expo(-0.5), 0.0);
        assert_eq!(out_expo(2.0), 1.0);
    }

    #[test]
    fn passes_one_just_before_the_end() {
        assert!(out_expo(0.99) < 1.0);
        assert!(out_expo(0.99995) > 1.0);
        assert!(out_expo(0.999_999) < 1.000_023);
    }
}
