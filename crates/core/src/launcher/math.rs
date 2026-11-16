//! The launcher's built-in calculator.

use super::text::{is_js_space, trim, utf16_len};

pub const MAX_LEN: usize = 100;

pub fn calculate(input: &str) -> Option<String> {
    let trimmed = trim(input);
    let source = trim(
        trimmed
            .strip_prefix('=')
            .unwrap_or(trimmed),
    );
    let len = utf16_len(source);
    if len == 0
        || len > MAX_LEN
        || !source.contains(['+', '*', '/', '(', ')', '-'])
        || !source.chars().all(is_allowed)
    {
        return None;
    }

    let mut parser = Parser { source, position: 0 };
    let result = parser.expression();
    parser.skip_spaces();
    (parser.position == source.len() && result.is_finite()).then(|| {
        ryu_js::Buffer::new()
            .format(to_precision_12(result))
            .to_owned()
    })
}

/// Rounds ties away from zero like JS `toPrecision`, not half-to-even like Rust formatting.
pub fn to_precision_12(value: f64) -> f64 {
    let magnitude = round_half_up_12(value.abs()).unwrap_or(value.abs());
    if value < 0.0 { -magnitude } else { magnitude }
}

fn round_half_up_12(magnitude: f64) -> Option<f64> {
    // 800 digits hold the exact decimal expansion of any f64 (at most 767
    // significant digits), so digit 13 is exact and ties are seen as ties.
    let exact = format!("{magnitude:.800e}");
    let (mantissa, exponent) = exact.split_once('e')?;
    let exponent: i32 = exponent.parse().ok()?;
    let mut digits = mantissa
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|d| d - b'0');
    let mut kept = 0u64;
    for _ in 0..12 {
        kept = kept * 10 + u64::from(digits.next()?);
    }
    if digits.next()? >= 5 {
        kept += 1;
    }
    format!("{kept}e{}", exponent - 11)
        .parse()
        .ok()
}

fn is_allowed(c: char) -> bool {
    c.is_ascii_digit() || is_js_space(c) || matches!(c, '.' | '+' | '*' | '/' | '(' | ')' | '-')
}

struct Parser<'a> {
    source: &'a str,
    position: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.source
            .get(self.position..)?
            .chars()
            .next()
    }

    fn advance(&mut self) {
        self.position += self.peek().map_or(1, char::len_utf8);
    }

    fn skip_spaces(&mut self) {
        while self.peek().is_some_and(is_js_space) {
            self.advance();
        }
    }

    fn factor(&mut self) -> f64 {
        self.skip_spaces();
        match self.peek() {
            Some(sign @ ('+' | '-')) => {
                self.advance();
                let value = self.factor();
                if sign == '-' { -value } else { value }
            }
            Some('(') => {
                self.advance();
                let value = self.expression();
                self.skip_spaces();
                let close = self.peek();
                self.advance();
                if close == Some(')') { value } else { f64::NAN }
            }
            _ => self.number(),
        }
    }

    fn number(&mut self) -> f64 {
        let rest = self
            .source
            .get(self.position..)
            .unwrap_or("")
            .as_bytes();
        let digits_from = |start: usize| {
            rest.get(start..).map_or(0, |tail| {
                tail.iter()
                    .take_while(|b| b.is_ascii_digit())
                    .count()
            })
        };
        let whole = digits_from(0);
        let len = match (whole, rest.get(whole)) {
            (0, Some(b'.')) => match digits_from(1) {
                0 => return f64::NAN,
                fraction => 1 + fraction,
            },
            (0, _) => return f64::NAN,
            (_, Some(b'.')) => whole + 1 + digits_from(whole + 1),
            _ => whole,
        };
        let text = &self.source[self.position..self.position + len];
        self.position += len;
        text.parse().unwrap_or(f64::NAN)
    }

    fn term(&mut self) -> f64 {
        let mut value = self.factor();
        loop {
            self.skip_spaces();
            let operator = self.peek();
            if operator != Some('*') && operator != Some('/') {
                return value;
            }
            self.advance();
            let right = self.factor();
            value = if operator == Some('*') { value * right } else { value / right };
        }
    }

    fn expression(&mut self) -> f64 {
        let mut value = self.term();
        loop {
            self.skip_spaces();
            let operator = self.peek();
            if operator != Some('+') && operator != Some('-') {
                return value;
            }
            self.advance();
            let right = self.term();
            value = if operator == Some('+') { value + right } else { value - right };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calc(input: &str) -> String {
        calculate(input).unwrap_or_default()
    }

    #[test]
    fn needs_an_operator_and_only_math_chars() {
        assert_eq!(calc("42"), "");
        assert_eq!(calc(""), "");
        assert_eq!(calc("="), "");
        assert_eq!(calc("2x+1"), "");
        assert_eq!(calc("2^3"), "");
        assert_eq!(calc("1+2"), "3");
        assert_eq!(calc(" = 1 + 2 "), "3");
        assert_eq!(calc("(7)"), "7");
    }

    #[test]
    fn js_number_text() {
        assert_eq!(calc("0.1+0.2"), "0.3");
        assert_eq!(calc("1/3"), "0.333333333333");
        assert_eq!(calc("2/3"), "0.666666666667");
        assert_eq!(calc("1000000000000*1000000000"), "1e+21");
        assert_eq!(calc("1/10000000"), "1e-7");
        assert_eq!(calc("1/1000000"), "0.000001");
        assert_eq!(calc("-0"), "0");
        assert_eq!(calc("0*-1"), "0");
        assert_eq!(calc("1/0"), "");
        assert_eq!(calc("0/0"), "");
        assert_eq!(calc("1/(1/0)"), "0");
    }

    #[test]
    fn to_precision_rounds_ties_up() {
        assert_eq!(calc("123456789012.5+0"), "123456789013");
        assert_eq!(calc("1234567890125*1"), "1234567890130");
        assert_eq!(calc("-1234567890125*1"), "-1234567890130");
        assert_eq!(calc("9999999999995+0"), "10000000000000");
        assert_eq!(to_precision_12(2.5), 2.5);
    }

    #[test]
    fn grammar() {
        assert_eq!(calc("1.+1"), "2");
        assert_eq!(calc(".5+.5"), "1");
        assert_eq!(calc("."), "");
        assert_eq!(calc("1+."), "");
        assert_eq!(calc("--1+0"), "1");
        assert_eq!(calc("2*(3+4"), "");
        assert_eq!(calc("2*(3+4))"), "");
        assert_eq!(calc("1 2+3"), "");
        assert_eq!(calc("1\u{a0}+\u{3000}2\u{feff}"), "3");
        assert_eq!(calc("1\u{85}+2"), "");
    }

    #[test]
    fn length_limit() {
        let at_limit = format!("10{}", "+1".repeat(49));
        assert_eq!(at_limit.len(), MAX_LEN);
        assert_eq!(calc(&at_limit), "59");
        assert_eq!(calc(&format!("{at_limit}0")), "");
        assert_eq!(calc(&format!(" {at_limit}\u{a0}")), "59");
        let wide = format!("1{}\u{a0}+1", "+1".repeat(48));
        assert_eq!(utf16_len(&wide), MAX_LEN);
        assert_eq!(calc(&wide), "50");
    }
}
