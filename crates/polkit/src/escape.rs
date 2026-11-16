//! The helper's `g_strescape`d output lines, decoded as `polkitagentsession.c` decodes them.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Line {
    Prompt {
        text: String,
        echo: bool,
    },
    Error(String),
    Info(String),
    Success,
    Failure,
    /// Anything else, which libpolkit-agent-1 treats as a failed conversation.
    Unknown,
}

pub(crate) fn parse(raw: &[u8]) -> Line {
    let raw = raw.trim_suffix(b"\n");
    // GIOChannel reads lines as UTF-8 and reports invalid input as an error, which ends the
    // conversation as failed. The helper's own output is always ASCII.
    if std::str::from_utf8(raw).is_err() {
        return Line::Unknown;
    }
    let text = compress(raw);
    let text = text.as_slice();
    let message = |rest: &[u8]| String::from_utf8_lossy(rest).into_owned();
    if let Some(rest) = text.strip_prefix(b"PAM_PROMPT_ECHO_OFF ") {
        Line::Prompt { text: message(rest), echo: false }
    } else if let Some(rest) = text.strip_prefix(b"PAM_PROMPT_ECHO_ON ") {
        Line::Prompt { text: message(rest), echo: true }
    } else if let Some(rest) = text.strip_prefix(b"PAM_ERROR_MSG ") {
        Line::Error(message(rest))
    } else if let Some(rest) = text.strip_prefix(b"PAM_TEXT_INFO ") {
        Line::Info(message(rest))
    } else if text.starts_with(b"SUCCESS") {
        Line::Success
    } else if text.starts_with(b"FAILURE") {
        Line::Failure
    } else {
        Line::Unknown
    }
}

/// glib's `g_strcompress`. Octal escapes wrap modulo 256, as glib stores them in a `gchar`,
/// and a NUL or a trailing backslash ends the text, as in glib's C strings.
pub(crate) fn compress(source: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(source.len());
    let mut i = 0;
    while let Some(&byte) = source.get(i) {
        if byte == 0 {
            break;
        }
        i += 1;
        if byte != b'\\' {
            out.push(byte);
            continue;
        }
        let Some(&escaped) = source.get(i) else {
            break;
        };
        match escaped {
            0 => break,
            b'0'..=b'7' => {
                let end = (i + 3).min(source.len());
                let mut value: u8 = 0;
                while i < end && (b'0'..=b'7').contains(&source[i]) {
                    value = value
                        .wrapping_mul(8)
                        .wrapping_add(source[i] - b'0');
                    i += 1;
                }
                out.push(value);
                continue;
            }
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(0x0b),
            other => out.push(other),
        }
        i += 1;
    }
    if let Some(end) = out.iter().position(|&b| b == 0) {
        out.truncate(end);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `g_strescape (s, NULL)`, to check that decoding undoes it.
    fn strescape(source: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for &b in source {
            match b {
                0x08 => out.extend_from_slice(b"\\b"),
                0x0c => out.extend_from_slice(b"\\f"),
                b'\n' => out.extend_from_slice(b"\\n"),
                b'\r' => out.extend_from_slice(b"\\r"),
                b'\t' => out.extend_from_slice(b"\\t"),
                0x0b => out.extend_from_slice(b"\\v"),
                b'\\' => out.extend_from_slice(b"\\\\"),
                b'"' => out.extend_from_slice(b"\\\""),
                b if !(b' '..0x7f).contains(&b) => {
                    out.push(b'\\');
                    out.push(b'0' + ((b >> 6) & 7));
                    out.push(b'0' + ((b >> 3) & 7));
                    out.push(b'0' + (b & 7));
                }
                b => out.push(b),
            }
        }
        out
    }

    #[test]
    fn simple_escapes() {
        assert_eq!(compress(br#"a\nb\tc\\d\"e"#), b"a\nb\tc\\d\"e");
        assert_eq!(compress(br"\b\f\r\v"), b"\x08\x0c\r\x0b");
        assert_eq!(compress(br"\q\ \'"), b"q '");
    }

    #[test]
    fn octal_takes_at_most_three_digits() {
        assert_eq!(compress(br"\101\1012"), b"AA2");
        assert_eq!(compress(br"\7x"), b"\x07x");
        assert_eq!(compress(br"\08"), b"");
        assert_eq!(compress(br"\18"), b"\x018");
        assert_eq!(compress(br"\777"), [0xff]);
        assert_eq!(compress(br"\400"), b"");
    }

    #[test]
    fn nul_and_trailing_backslash_end_the_text() {
        assert_eq!(compress(br"abc\000def"), b"abc");
        assert_eq!(compress(b"abc\0def"), b"abc");
        assert_eq!(compress(br"abc\"), b"abc");
    }

    #[test]
    fn decoding_undoes_g_strescape() {
        let text = "Bitte PIN eingeben (é, ไทย)\t\"ok\"\\\x01\x7f".as_bytes();
        let escaped = strescape(text);
        assert!(escaped.is_ascii());
        assert_eq!(compress(&escaped), text);
    }

    #[test]
    fn lines() {
        assert_eq!(
            parse(b"PAM_PROMPT_ECHO_OFF Please enter the PIN: \n"),
            Line::Prompt { text: "Please enter the PIN: ".into(), echo: false }
        );
        assert_eq!(
            parse(b"PAM_PROMPT_ECHO_ON login:\n"),
            Line::Prompt { text: "login:".into(), echo: true }
        );
        assert_eq!(
            parse(b"PAM_ERROR_MSG nope\n"),
            Line::Error("nope".into())
        );
        assert_eq!(
            parse(b"PAM_TEXT_INFO Please touch the FIDO authenticator.\n"),
            Line::Info("Please touch the FIDO authenticator.".into())
        );
        assert_eq!(parse(b"SUCCESS\n"), Line::Success);
        assert_eq!(parse(b"FAILURE\n"), Line::Failure);
        assert_eq!(parse(b"SUCCESSFUL"), Line::Success);
        assert_eq!(parse(b"HELLO\n"), Line::Unknown);
        assert_eq!(parse(b"PAM_TEXT_INFO\n"), Line::Unknown);
    }

    #[test]
    fn prefixes_match_after_decoding() {
        assert_eq!(
            parse(br"PAM_ERROR_MSG\040x"),
            Line::Error("x".into())
        );
        assert_eq!(
            parse(br"PAM_TEXT_INFO line one\nline two"),
            Line::Info("line one\nline two".into())
        );
        assert_eq!(
            parse(br"PAM_TEXT_INFO caf\303\251"),
            Line::Info("café".into())
        );
        assert_eq!(
            parse(br"PAM_TEXT_INFO \377"),
            Line::Info("\u{fffd}".into())
        );
    }

    #[test]
    fn raw_invalid_utf8_fails() {
        assert_eq!(parse(b"PAM_TEXT_INFO \xff\n"), Line::Unknown);
    }
}
