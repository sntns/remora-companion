/// Print a human-readable byte size (KiB/MiB/GiB), matching what a human
/// expects to see, not a raw byte count.
pub fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stays_in_bytes_below_one_kib() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1), "1 B");
        assert_eq!(human_size(1023), "1023 B");
    }

    #[test]
    fn steps_up_a_unit_at_each_1024_boundary() {
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(1024 * 1024), "1.0 MiB");
        assert_eq!(human_size(1024 * 1024 * 1024), "1.0 GiB");
        assert_eq!(human_size(1024u64.pow(4)), "1.0 TiB");
    }

    #[test]
    fn rounds_to_one_decimal_place() {
        assert_eq!(human_size(1536), "1.5 KiB"); // 1.5 * 1024
        assert_eq!(human_size(1500), "1.5 KiB"); // 1.46... rounds to 1.5
    }

    #[test]
    fn clamps_at_the_largest_unit_instead_of_introducing_a_new_one() {
        // Comfortably past 1024 TiB -- there's no "PiB" entry, so this must
        // stay expressed in TiB rather than panicking on an out-of-bounds
        // UNITS index.
        assert_eq!(human_size(1024u64.pow(5) * 3), "3072.0 TiB");
    }
}
