//! libalpm's `alpm_pkg_vercmp`, byte for byte.

use std::cmp::Ordering;

pub fn vercmp(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (epoch_a, ver_a, rel_a) = split(a.as_bytes());
    let (epoch_b, ver_b, rel_b) = split(b.as_bytes());
    rpmvercmp(epoch_a, epoch_b)
        .then_with(|| rpmvercmp(ver_a, ver_b))
        .then_with(|| match (rel_a, rel_b) {
            (Some(a), Some(b)) => rpmvercmp(a, b),
            _ => Ordering::Equal,
        })
}

/// `parseEVR`: the epoch is the leading digits before a `:` (`0` without one), the
/// release is after the last `-` that follows those digits.
fn split(evr: &[u8]) -> (&[u8], &[u8], Option<&[u8]>) {
    let digits = evr
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    let rest = &evr[digits..];
    let release_at = rest
        .iter()
        .rposition(|&b| b == b'-')
        .map(|at| digits + at);
    let (head, release) = match release_at {
        Some(at) => (&evr[..at], Some(&evr[at + 1..])),
        None => (evr, None),
    };
    if head.get(digits) == Some(&b':') {
        let epoch = if digits == 0 { b"0".as_slice() } else { &head[..digits] };
        (epoch, &head[digits + 1..], release)
    } else {
        (b"0", head, release)
    }
}

fn rpmvercmp(a: &[u8], b: &[u8]) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (mut one, mut two) = (0, 0);
    let (mut ptr1, mut ptr2) = (0, 0);
    while one < a.len() && two < b.len() {
        while one < a.len() && !a[one].is_ascii_alphanumeric() {
            one += 1;
        }
        while two < b.len() && !b[two].is_ascii_alphanumeric() {
            two += 1;
        }
        if one == a.len() || two == b.len() {
            break;
        }
        // Separators of different lengths decide.
        let (sep1, sep2) = (one - ptr1, two - ptr2);
        if sep1 != sep2 {
            return sep1.cmp(&sep2);
        }
        ptr1 = one;
        ptr2 = two;
        let numeric = a[ptr1].is_ascii_digit();
        let class: fn(&u8) -> bool =
            if numeric { u8::is_ascii_digit } else { u8::is_ascii_alphabetic };
        while ptr1 < a.len() && class(&a[ptr1]) {
            ptr1 += 1;
        }
        while ptr2 < b.len() && class(&b[ptr2]) {
            ptr2 += 1;
        }
        // A numeric segment against an alpha one: numeric is newer.
        if two == ptr2 {
            return if numeric { Ordering::Greater } else { Ordering::Less };
        }
        let (mut seg1, mut seg2) = (&a[one..ptr1], &b[two..ptr2]);
        if numeric {
            seg1 = trim_zeros(seg1);
            seg2 = trim_zeros(seg2);
            let by_len = seg1.len().cmp(&seg2.len());
            if by_len != Ordering::Equal {
                return by_len;
            }
        }
        let by_bytes = seg1.cmp(seg2);
        if by_bytes != Ordering::Equal {
            return by_bytes;
        }
        one = ptr1;
        two = ptr2;
    }
    let (rest1, rest2) = (&a[one.min(a.len())..], &b[two.min(b.len())..]);
    if rest1.is_empty() && rest2.is_empty() {
        return Ordering::Equal;
    }
    // A remaining alpha string never beats an empty one.
    let first_alpha = |rest: &[u8]| {
        rest.first()
            .is_some_and(u8::is_ascii_alphabetic)
    };
    if (rest1.is_empty() && !first_alpha(rest2)) || first_alpha(rest1) {
        Ordering::Less
    } else {
        Ordering::Greater
    }
}

fn trim_zeros(digits: &[u8]) -> &[u8] {
    let zeros = digits
        .iter()
        .take_while(|&&b| b == b'0')
        .count();
    &digits[zeros..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits() {
        assert_eq!(
            split(b"1:2.0-3"),
            (
                b"1".as_slice(),
                b"2.0".as_slice(),
                Some(b"3".as_slice())
            )
        );
        assert_eq!(
            split(b"2.0"),
            (b"0".as_slice(), b"2.0".as_slice(), None)
        );
        assert_eq!(
            split(b":2.0-1"),
            (
                b"0".as_slice(),
                b"2.0".as_slice(),
                Some(b"1".as_slice())
            )
        );
        assert_eq!(
            split(b"1.0-a-b"),
            (
                b"0".as_slice(),
                b"1.0-a".as_slice(),
                Some(b"b".as_slice())
            )
        );
    }
}
