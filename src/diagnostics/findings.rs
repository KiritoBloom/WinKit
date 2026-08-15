//! Deterministic finding scoring (§77).
//!
//! Every score is a pure function of measured values with a documented
//! formula, so two identical machines always produce the same ranking.
//! Formulas are listed in `docs/diagnostics.md` and must stay in sync with
//! the docs when changed.

/// Map a 0-100 score to `(severity, confidence)` bands.
///
/// Bands: `>= 90` critical/high, `>= 70` high/high, `>= 50` medium/medium,
/// otherwise low/low.
pub fn score_bands(score: u8) -> (&'static str, &'static str) {
    match score {
        90..=u8::MAX => ("critical", "high"),
        70..=89 => ("high", "high"),
        50..=69 => ("medium", "medium"),
        _ => ("low", "low"),
    }
}

/// Storage pressure score from percent free.
///
/// Piecewise curve: `<= 1%` free is 100, `<= 5%` is 95, `<= 10%` is 80,
/// `<= 20%` is 60, otherwise 0.
pub fn storage_score(free_percent: f64) -> u8 {
    let score = if free_percent <= 1.0 {
        100.0
    } else if free_percent <= 5.0 {
        95.0
    } else if free_percent <= 10.0 {
        80.0
    } else if free_percent <= 20.0 {
        60.0
    } else {
        0.0
    };
    score as u8
}

/// System memory pressure score: linear from 0 at the configured threshold
/// to 100 at 100% load.
pub fn memory_pressure_score(load_percent: f64, threshold_percent: f64) -> u8 {
    let span = 100.0 - threshold_percent;
    if span <= 0.0 {
        return 100;
    }
    (((load_percent - threshold_percent) / span) * 100.0).clamp(0.0, 100.0) as u8
}

/// Application CPU score: the CPU value itself (already a percent of total
/// system CPU capacity), clamped to 0-100.
pub fn app_cpu_score(cpu_percent: f64) -> u8 {
    cpu_percent.clamp(0.0, 100.0) as u8
}

/// Application memory score: working set relative to the larger of one
/// quarter of physical RAM or the high-memory threshold. An app holding a
/// quarter of RAM scores 100.
pub fn app_memory_score(
    working_set_bytes: u64,
    total_ram_bytes: Option<u64>,
    threshold_bytes: u64,
) -> u8 {
    let denominator = total_ram_bytes
        .map(|t| t / 4)
        .unwrap_or(threshold_bytes)
        .max(threshold_bytes);
    if denominator == 0 {
        return 100;
    }
    ((working_set_bytes as f64 / denominator as f64) * 100.0).clamp(0.0, 100.0) as u8
}

/// Memory-growth score: linear from 0 at rest to 100 at twice the configured
/// runaway threshold.
pub fn memory_growth_score(rate_bytes_per_second: f64, threshold_bytes_per_second: f64) -> u8 {
    if threshold_bytes_per_second <= 0.0 {
        return 100;
    }
    ((rate_bytes_per_second / (threshold_bytes_per_second * 2.0)) * 100.0).clamp(0.0, 100.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_bands_are_contiguous_and_documented() {
        assert_eq!(score_bands(100), ("critical", "high"));
        assert_eq!(score_bands(90), ("critical", "high"));
        assert_eq!(score_bands(89), ("high", "high"));
        assert_eq!(score_bands(70), ("high", "high"));
        assert_eq!(score_bands(69), ("medium", "medium"));
        assert_eq!(score_bands(50), ("medium", "medium"));
        assert_eq!(score_bands(49), ("low", "low"));
    }

    #[test]
    fn storage_score_is_monotonic_in_pressure() {
        assert_eq!(storage_score(0.5), 100);
        assert_eq!(storage_score(1.0), 100);
        assert_eq!(storage_score(2.2), 95);
        assert_eq!(storage_score(7.0), 80);
        assert_eq!(storage_score(15.0), 60);
        assert_eq!(storage_score(25.0), 0);
    }

    #[test]
    fn memory_pressure_score_ramps_from_threshold_to_100() {
        assert_eq!(memory_pressure_score(85.0, 85.0), 0);
        assert_eq!(memory_pressure_score(92.5, 85.0), 50);
        assert_eq!(memory_pressure_score(100.0, 85.0), 100);
    }

    #[test]
    fn app_memory_score_uses_quarter_of_ram() {
        assert_eq!(
            app_memory_score(
                4 * 1024 * 1024 * 1024,
                Some(16 * 1024 * 1024 * 1024),
                2 * 1024 * 1024 * 1024
            ),
            100
        );
        assert_eq!(
            app_memory_score(
                2 * 1024 * 1024 * 1024,
                Some(16 * 1024 * 1024 * 1024),
                2 * 1024 * 1024 * 1024
            ),
            50
        );
    }

    #[test]
    fn memory_growth_score_doubles_the_threshold_for_full() {
        let threshold = 50 * 1024 * 1024;
        assert_eq!(
            memory_growth_score(50.0 * 1024.0 * 1024.0, threshold as f64),
            50
        );
        assert_eq!(
            memory_growth_score(100.0 * 1024.0 * 1024.0, threshold as f64),
            100
        );
        assert_eq!(memory_growth_score(0.0, threshold as f64), 0);
    }
}
