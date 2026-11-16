//! Assuan line splitting. Lines can carry session keys or passphrases, so none is logged or kept.

use zeroize::Zeroize;

/// Lines longer than this are dropped. Assuan itself caps lines at 1000 bytes.
pub const MAX_LINE: usize = 64 * 1024;

#[derive(Debug, Default)]
pub struct Lines {
    partial: Vec<u8>,
    /// Set after an overflow, so the tail of an overlong line is never read as a command.
    skipping: bool,
}

impl Lines {
    pub fn feed(&mut self, mut data: &[u8], mut on_line: impl FnMut(&[u8])) {
        while let Some(end) = data.iter().position(|&b| b == b'\n') {
            let piece = &data[..end];
            data = &data[end + 1..];
            if self.skipping {
                self.skipping = false;
            } else if self.partial.is_empty() {
                if piece.len() <= MAX_LINE {
                    on_line(trim_cr(piece));
                }
            } else if self.partial.len() + piece.len() <= MAX_LINE {
                self.append(piece);
                on_line(trim_cr(&self.partial));
            }
            self.clear();
        }
        if self.skipping {
            return;
        }
        if self.partial.len() + data.len() > MAX_LINE {
            self.clear();
            self.skipping = true;
        } else {
            self.append(data);
        }
    }

    /// Grows into a new buffer and zeroes the old one, where `extend_from_slice` would leave
    /// the old bytes in a freed allocation.
    fn append(&mut self, data: &[u8]) {
        let needed = self.partial.len() + data.len();
        if needed > self.partial.capacity() {
            let capacity = needed
                .max(self.partial.capacity() * 2)
                .min(MAX_LINE);
            let mut grown = Vec::with_capacity(capacity);
            grown.extend_from_slice(&self.partial);
            self.partial.zeroize();
            self.partial = grown;
        }
        self.partial.extend_from_slice(data);
    }

    fn clear(&mut self) {
        self.partial.as_mut_slice().zeroize();
        self.partial.clear();
    }
}

impl Drop for Lines {
    fn drop(&mut self) {
        self.partial.zeroize();
    }
}

fn trim_cr(line: &[u8]) -> &[u8] {
    line.trim_suffix(b"\r")
}

pub fn fields(line: &[u8]) -> impl Iterator<Item = &[u8]> {
    line.split(u8::is_ascii_whitespace)
        .filter(|field| !field.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(chunks: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut lines = Lines::default();
        let mut out = Vec::new();
        for chunk in chunks {
            lines.feed(chunk, |line| out.push(line.to_vec()));
        }
        out
    }

    #[test]
    fn lines_split_across_reads() {
        assert_eq!(
            split(&[b"SIG", b"KEY ABC\r\nPKSIGN\nPART", b"IAL\n"]),
            [&b"SIGKEY ABC"[..], b"PKSIGN", b"PARTIAL"]
        );
        let stream = b"OK one\r\nINQUIRE TWO\nERR 3\r\n";
        for cut in 0..=stream.len() {
            let (a, b) = stream.split_at(cut);
            assert_eq!(
                split(&[a, b]),
                [&b"OK one"[..], b"INQUIRE TWO", b"ERR 3"],
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn crlf_and_empty_lines() {
        assert_eq!(
            split(&[b"\r\n\nOK\r", b"\n"]),
            [&b""[..], b"", b"OK"]
        );
        assert_eq!(split(&[b"OK\r\r\n"]), [b"OK\r"]);
        assert_eq!(split(&[b"no newline yet"]), Vec::<Vec<u8>>::new());
    }

    #[test]
    fn overflow_drops_the_whole_line() {
        let long = vec![b'A'; MAX_LINE];
        assert_eq!(
            split(&[&long, b"\nOK\n"]),
            [long.clone(), b"OK".to_vec()],
            "the cap itself fits"
        );

        let mut over = vec![b'B'; MAX_LINE + 1];
        over.extend_from_slice(b"\nOK\n");
        assert_eq!(split(&[&over]), [b"OK"]);

        let tail = b"OK this tail is not a line\nERR 1\n";
        assert_eq!(
            split(&[&[b'C'; 40_000], &[b'C'; 40_000], tail]),
            [b"ERR 1"]
        );
        assert_eq!(
            split(&[&[b'D'; MAX_LINE], b"D", b"PKSIGN\n", b"PKSIGN\n"]),
            [b"PKSIGN"],
            "a split overlong line stays dropped"
        );
    }

    #[test]
    fn a_used_line_is_zeroed() {
        let mut lines = Lines::default();
        lines.feed(b"D secret-", |_| {});
        let first = lines.partial.as_ptr();
        lines.feed(&[b'x'; 300], |_| {});
        assert_eq!(lines.partial.len(), 309);
        assert_ne!(lines.partial.as_ptr(), first, "it grew");
        assert!(lines.partial.capacity() <= MAX_LINE);
        lines.feed(b"\n", |line| assert_eq!(line.len(), 309));
        assert!(lines.partial.is_empty());
        // SAFETY: the allocation is live and its first 309 bytes were written above.
        let spare = unsafe { std::slice::from_raw_parts(lines.partial.as_ptr(), 309) };
        assert!(
            spare.iter().all(|&b| b == 0),
            "the used line was zeroed"
        );
    }

    #[test]
    fn field_split() {
        let got: Vec<&[u8]> = fields(b"  SETKEY  --force\tABC ").collect();
        assert_eq!(got, [&b"SETKEY"[..], b"--force", b"ABC"]);
        assert_eq!(fields(b"   ").count(), 0);
    }
}
