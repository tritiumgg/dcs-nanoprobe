//! Helpers shared by `inspect` and `top`.

use nanoprobe_transport::golden;
use nanoprobe_transport::layout::status as s;
use nanoprobe_transport::mem::Memory;
use nanoprobe_transport::status::{Metrics, Snapshot, read_metrics};

/// Tries made to read a consistent snapshot or the metrics.
const RETRIES: usize = 8;

/// The status block as a private copy: a consistent snapshot when one can be
/// read, otherwise the field-by-field copy.
pub fn snapshot(m: &impl Memory) -> Snapshot {
    for _ in 0..RETRIES {
        if let Some(snap) = Snapshot::read(m) {
            return snap;
        }
        std::thread::yield_now();
    }
    Snapshot::copy(m)
}

/// The metrics read under the sequence lock, retried a few times. `None` when
/// the agent kept writing them.
pub fn metrics(m: &impl Memory) -> Option<Metrics> {
    for _ in 0..RETRIES {
        if let Some(x) = read_metrics(m) {
            return Some(x);
        }
        std::thread::yield_now();
    }
    None
}

/// A code with its name: `RUNNING(3)`, or `unknown(9)`.
pub fn named(section: &str, code: u64) -> String {
    match golden::code_name(section, code) {
        Some(name) => format!("{name}({code})"),
        None => format!("unknown({code})"),
    }
}

/// The names of the bits set in `bits`, joined by `|`, then the number in
/// hex: `ATTACHED|RAW_LUA(0x9)`. Bits with no name show as hex.
pub fn flag_names(section: &str, bits: u64) -> String {
    let mut names: Vec<String> = Vec::new();
    let mut known = 0;
    for c in golden::codes(section) {
        if c.code != 0 && bits & c.code == c.code {
            names.push(c.name.to_string());
            known |= c.code;
        }
    }
    if bits & !known != 0 {
        names.push(format!("{:#x}", bits & !known));
    }
    if names.is_empty() {
        names.push("none".into());
    }
    format!("{}({bits:#x})", names.join("|"))
}

/// Bytes as lowercase hex pairs separated by spaces, at most `max` of them.
pub fn hex_bytes(bytes: &[u8], max: usize) -> String {
    let mut out: Vec<String> = bytes.iter().take(max).map(|b| format!("{b:02x}")).collect();
    if bytes.len() > max {
        out.push(format!("..(+{})", bytes.len() - max));
    }
    out.join(" ")
}

/// `text` cut to at most `max` characters, with `...` when it was cut.
pub fn clip(text: String, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{}...", &text[..end]),
        None => text,
    }
}

/// The bucket that holds the `num/den` quantile of a histogram, `None` when it
/// is empty.
pub fn percentile(hist: &[u64; s::HIST_BUCKETS], num: u128, den: u128) -> Option<usize> {
    let total: u128 = hist.iter().map(|&c| u128::from(c)).sum();
    if total == 0 {
        return None;
    }
    let target = (total * num).div_ceil(den).max(1);
    let mut seen = 0u128;
    for (i, &c) in hist.iter().enumerate() {
        seen += u128::from(c);
        if seen >= target {
            return Some(i);
        }
    }
    None
}

/// The highest bucket that holds a count.
pub fn max_bucket(hist: &[u64; s::HIST_BUCKETS]) -> Option<usize> {
    hist.iter().rposition(|&c| c != 0)
}

/// The upper bound of a bucket: `<2us`, or `>=1073.7s` for the last one.
pub fn bound_text(bucket: usize) -> String {
    match bucket {
        0 => "<1us".into(),
        b if b >= s::HIST_BUCKETS - 1 => format!(">={}", duration_us((1u64 << (b - 1)) as f64)),
        b => format!("<{}", duration_us((1u64 << b) as f64)),
    }
}

/// Microseconds as `850us`, `1.5ms` or `2.0s`.
pub fn duration_us(us: f64) -> String {
    if us < 1_000.0 {
        format!("{us:.0}us")
    } else if us < 1_000_000.0 {
        format!("{:.1}ms", us / 1_000.0)
    } else {
        format!("{:.1}s", us / 1_000_000.0)
    }
}

/// `p50 <2ms  p99 <8ms  max <16ms` for a histogram, or `no samples`.
pub fn hist_summary(hist: &[u64; s::HIST_BUCKETS]) -> String {
    let (Some(p50), Some(p99), Some(max)) = (
        percentile(hist, 50, 100),
        percentile(hist, 99, 100),
        max_bucket(hist),
    ) else {
        return "no samples".into();
    };
    format!(
        "p50 {}  p99 {}  max {}",
        bound_text(p50),
        bound_text(p99),
        bound_text(max)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_and_bounds() {
        let mut h = [0u64; s::HIST_BUCKETS];
        assert_eq!(percentile(&h, 50, 100), None);
        h[3] = 90;
        h[10] = 9;
        h[12] = 1;
        assert_eq!(percentile(&h, 50, 100), Some(3));
        assert_eq!(percentile(&h, 99, 100), Some(10));
        assert_eq!(max_bucket(&h), Some(12));
        assert_eq!(bound_text(3), "<8us");
        assert_eq!(bound_text(0), "<1us");
        assert_eq!(bound_text(10), "<1.0ms");
        assert_eq!(bound_text(31), ">=1073.7s");
        assert_eq!(hist_summary(&[0; 32]), "no samples");
    }

    #[test]
    fn flags_decode() {
        assert_eq!(flag_names("agent_flags", 0), "none(0x0)");
        assert_eq!(flag_names("agent_flags", 9), "ATTACHED|RAW_LUA(0x9)");
        assert_eq!(flag_names("agent_flags", 0x101), "ATTACHED|0x100(0x101)");
        assert_eq!(named("lifecycle", 3), "RUNNING(3)");
        assert_eq!(named("lifecycle", 99), "unknown(99)");
    }

    #[test]
    fn clip_cuts_on_characters() {
        assert_eq!(clip("abcdef".into(), 3), "abc...");
        assert_eq!(clip("ab".into(), 3), "ab");
        assert_eq!(clip("ééé".into(), 2), "éé...");
    }
}
