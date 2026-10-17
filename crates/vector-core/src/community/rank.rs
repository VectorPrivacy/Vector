//! Rank keys (CORD-04 §3): Role order as fractional keys over `0-9A-Za-z`, compared byte by
//! byte, 1 to 32 bytes and never ending in `0`. Between any two keys another always exists,
//! so moving a Role between two neighbours is one edition, on the moved Role alone. Channel
//! and Category positions (CORD-03) use the same keys.

/// The longest key a reader accepts.
pub const MAX_RANK_KEY_BYTES: usize = 32;

const DIGITS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

fn digit(b: u8) -> Option<usize> {
    match b {
        b'0'..=b'9' => Some((b - b'0') as usize),
        b'A'..=b'Z' => Some((b - b'A') as usize + 10),
        b'a'..=b'z' => Some((b - b'a') as usize + 36),
        _ => None,
    }
}

/// A well-formed rank key.
pub fn is_rank_key(key: &str) -> bool {
    let b = key.as_bytes();
    !b.is_empty() && b.len() <= MAX_RANK_KEY_BYTES && b.iter().all(|&c| digit(c).is_some()) && b[b.len() - 1] != b'0'
}

/// A key strictly between `lo` and `hi` (`None` is the open end on that side), or `None`
/// when the keys are malformed, out of order, or the gap no longer fits in 32 bytes, at
/// which point the caller re-keys neighbours.
pub fn between(lo: Option<&str>, hi: Option<&str>) -> Option<String> {
    let lo = lo.unwrap_or("");
    if !lo.is_empty() && !is_rank_key(lo) {
        return None;
    }
    if let Some(h) = hi {
        if !is_rank_key(h) || lo.as_bytes() >= h.as_bytes() {
            return None;
        }
    }
    let key = match hi {
        // Appending is the common move (a new Role starts at the bottom), so it steps up a
        // digit rather than halving the gap: about 61 keys per byte instead of 6.
        None if !lo.is_empty() => after(lo.as_bytes()),
        _ => midpoint(lo.as_bytes(), hi.map(str::as_bytes)),
    };
    (key.len() <= MAX_RANK_KEY_BYTES).then(|| String::from_utf8(key).expect("ascii digits"))
}

/// The shortest step past `lo`: its first digit plus one, or `z` and a step past the rest.
fn after(lo: &[u8]) -> Vec<u8> {
    let Some(&first) = lo.first() else { return vec![DIGITS[1]] };
    let a = digit(first).expect("validated");
    if a + 1 < DIGITS.len() {
        return vec![DIGITS[a + 1]];
    }
    let mut out = vec![first];
    out.extend(after(&lo[1..]));
    out
}

/// The midpoint of two base-62 fractions `0.lo` < `0.hi` (`None` is 1), as the shortest
/// digit string that ends in a non-zero digit.
fn midpoint(lo: &[u8], hi: Option<&[u8]>) -> Vec<u8> {
    if let Some(hi) = hi {
        let mut n = 0;
        while n < hi.len() && lo.get(n).copied().unwrap_or(b'0') == hi[n] {
            n += 1;
        }
        if n > 0 {
            let mut out = hi[..n].to_vec();
            out.extend(midpoint(lo.get(n..).unwrap_or(&[]), Some(&hi[n..])));
            return out;
        }
    }
    let a = lo.first().and_then(|&c| digit(c)).unwrap_or(0);
    let b = hi.map_or(DIGITS.len(), |h| digit(h[0]).expect("validated"));
    if b - a > 1 {
        return vec![DIGITS[(a + b).div_ceil(2)]];
    }
    match hi {
        Some(h) if h.len() > 1 => vec![h[0]],
        _ => {
            let mut out = vec![DIGITS[a]];
            out.extend(midpoint(lo.get(1..).unwrap_or(&[]), None));
            out
        }
    }
}

/// Order-preserving dense ranks for a set of keys: each distinct key maps to 1 + its index
/// in byte order, so equal keys are peers and every comparison a `u32` rank answers is the
/// comparison the keys answer. 0 stays the owner's.
pub fn dense_ranks<'a>(keys: impl IntoIterator<Item = &'a str>) -> std::collections::HashMap<&'a str, u32> {
    let mut sorted: Vec<&str> = keys.into_iter().collect();
    sorted.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    sorted.dedup();
    sorted.into_iter().enumerate().map(|(i, k)| (k, i as u32 + 1)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_bounded_and_never_end_in_zero() {
        assert!(is_rank_key("V") && is_rank_key("0A") && is_rank_key(&"z".repeat(32)));
        assert!(!is_rank_key("") && !is_rank_key("A0") && !is_rank_key("0") && !is_rank_key(&"z".repeat(33)));
        assert!(!is_rank_key("a-b") && !is_rank_key("é"));
    }

    #[test]
    fn between_lands_strictly_inside_every_gap() {
        let cases: [(Option<&str>, Option<&str>); 8] =
            [(None, None), (None, Some("V")), (Some("V"), None), (Some("A"), Some("B")), (Some("A"), Some("A1")), (Some("A1"), Some("A2")), (Some("z"), None), (None, Some("01"))];
        for (lo, hi) in cases {
            let k = between(lo, hi).expect("room");
            assert!(is_rank_key(&k), "{k} is well-formed");
            assert!(lo.is_none_or(|l| l.as_bytes() < k.as_bytes()), "{lo:?} < {k}");
            assert!(hi.is_none_or(|h| k.as_bytes() < h.as_bytes()), "{k} < {hi:?}");
        }
    }

    #[test]
    fn refuses_what_has_no_inside() {
        assert_eq!(between(Some("B"), Some("A")), None, "out of order");
        assert_eq!(between(Some("A"), Some("A")), None, "equal");
        assert_eq!(between(Some("A0"), None), None, "malformed low");
        assert_eq!(between(None, Some("")), None, "malformed high");
    }

    #[test]
    fn repeated_inserts_at_one_spot_stay_ordered_until_the_key_space_runs_out() {
        // Insert between the same low key and the last key minted, over and over.
        let lo = "A";
        let mut hi = "B".to_string();
        let mut minted = 0;
        while let Some(k) = between(Some(lo), Some(&hi)) {
            assert!(lo.as_bytes() < k.as_bytes() && k.as_bytes() < hi.as_bytes());
            hi = k;
            minted += 1;
            assert!(minted < 10_000, "terminates");
        }
        assert!(minted > 150, "about six halvings per byte: {minted}");
        // Appending steps a digit up, so the bottom of a long list is far from running out.
        let mut last = "V".to_string();
        for _ in 0..1_000 {
            let k = between(Some(&last), None).expect("room at the end");
            assert!(is_rank_key(&k) && last.as_bytes() < k.as_bytes());
            last = k;
        }
        assert!(last.len() <= 18, "{last}");
    }

    #[test]
    fn dense_ranks_preserve_order_and_make_equal_keys_peers() {
        let r = dense_ranks(["b", "A", "b", "Az", "0V"]);
        assert_eq!((r["0V"], r["A"], r["Az"], r["b"]), (1, 2, 3, 4));
        assert_eq!(r.len(), 4);
    }
}
