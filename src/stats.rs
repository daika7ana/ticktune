//! Pure statistics and scan-grid maths. No platform calls, so this compiles and
//! is unit-tested on Linux.

/// Which point on the measured curve to recommend.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prefer {
    /// Smallest `Sleep(1)` overshoot.
    Latency,
    /// Coarsest period that still lands inside the latency budget.
    Power,
}

/// Result of sampling `Sleep(1)` a number of times at one requested period.
#[derive(Clone, Debug)]
pub struct Stats {
    /// The requested period, in 100 ns units.
    pub period_100ns: u32,
    /// Number of samples kept (warmup already discarded by the sampler).
    pub n: usize,
    pub min_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    /// Worst sample; exposes preemption that percentiles hide.
    pub max_ms: f64,
    /// Fraction of samples that look preempted rather than timer-quantised.
    pub interference: f64,
    /// Most likely whole number of ticks per `Sleep(1)`, i.e. `round(p50 / period)`.
    pub ticks: u32,
}

impl Stats {
    /// `samples` are slept durations in milliseconds; the slice is sorted in place.
    pub fn from_samples(period_100ns: u32, samples: &mut [f64]) -> Stats {
        samples.sort_by(|a, b| a.total_cmp(b));
        let n = samples.len();
        if n == 0 {
            return Stats {
                period_100ns,
                n: 0,
                min_ms: 0.0,
                p50_ms: 0.0,
                p95_ms: 0.0,
                max_ms: 0.0,
                interference: 0.0,
                ticks: 0,
            };
        }

        let pct = |p: f64| {
            let idx = (p * (n - 1) as f64).round() as usize;
            samples[idx.min(n - 1)]
        };

        let p50_ms = pct(0.50);
        let interference = if p50_ms <= 0.0 {
            0.0
        } else {
            let limit = 4.0 * p50_ms;
            samples.iter().filter(|&&s| s > limit).count() as f64 / n as f64
        };

        let ticks = if period_100ns == 0 {
            0
        } else {
            ((p50_ms / ms(period_100ns)).round() as u32).max(1)
        };

        Stats {
            period_100ns,
            n,
            min_ms: samples[0],
            p50_ms,
            p95_ms: pct(0.95),
            max_ms: samples[n - 1],
            interference,
            ticks,
        }
    }

    /// Lower is better. Rewards closeness to 1.0 ms, penalises tail spread.
    pub fn score(&self) -> f64 {
        0.5 * self.p50_ms + 0.5 * self.p95_ms
    }

    /// Safe to recommend at all: low interference, no early wake-ups.
    pub fn usable(&self) -> bool {
        self.n > 0 && self.interference < 0.01 && self.p50_ms >= 1.0 && self.p95_ms <= 2.5
    }

    /// Inside the latency budget, i.e. a candidate for `Prefer::Power`.
    pub fn within_budget(&self) -> bool {
        self.usable() && self.p50_ms <= 1.35 && self.p95_ms <= 2.0
    }
}

/// Index into `stats` of the recommended point, if any.
pub fn choose(stats: &[Stats], prefer: Prefer) -> Option<usize> {
    let latency = || {
        stats
            .iter()
            .enumerate()
            .filter(|(_, s)| s.usable())
            .fold(None, |best: Option<(usize, &Stats)>, (i, s)| match best {
                None => Some((i, s)),
                Some((_, b)) => {
                    let better = s.score() < b.score()
                        || (s.score() == b.score() && s.period_100ns < b.period_100ns);
                    if better {
                        Some((i, s))
                    } else {
                        best
                    }
                }
            })
            .map(|(i, _)| i)
    };

    match prefer {
        Prefer::Latency => latency(),
        Prefer::Power => stats
            .iter()
            .enumerate()
            .filter(|(_, s)| s.within_budget())
            .fold(None, |best: Option<(usize, &Stats)>, (i, s)| match best {
                None => Some((i, s)),
                Some((_, b)) => {
                    let better = s.period_100ns > b.period_100ns
                        || (s.period_100ns == b.period_100ns && s.score() < b.score());
                    if better {
                        Some((i, s))
                    } else {
                        best
                    }
                }
            })
            .map(|(i, _)| i)
            .or_else(latency),
    }
}

/// Convert a period in 100 ns units to milliseconds.
pub fn ms(period_100ns: u32) -> f64 {
    period_100ns as f64 / 10_000.0
}

/// Coarse scan step: 10 us.
pub const COARSE_STEP_100NS: u32 = 100;
/// Fine scan step: 2 us.
pub const FINE_STEP_100NS: u32 = 20;
/// Fine scan half-width around the coarse winner: 15 us.
pub const FINE_SPAN_100NS: u32 = 150;
/// Nothing above 1 ms is worth scanning: `Sleep(1)` already takes 2 ticks there.
pub const MAX_USEFUL_100NS: u32 = 10_000;

/// Coarse sweep from `finest` up to `min(1 ms, coarsest)`, aligned to the step.
pub fn coarse_grid(finest: u32, coarsest: u32) -> Vec<u32> {
    let lo = finest;
    let hi = coarsest.min(MAX_USEFUL_100NS);
    if hi <= lo {
        return vec![lo];
    }
    let mut v = Vec::new();
    let mut p = lo;
    while p <= hi {
        v.push(p);
        match p.checked_add(COARSE_STEP_100NS) {
            Some(next) => p = next,
            None => break,
        }
    }
    v
}

/// Fine sweep around `best` (+-15 us), clamped to the platform range.
pub fn fine_grid(best: u32, finest: u32, coarsest: u32) -> Vec<u32> {
    let lo = best.saturating_sub(FINE_SPAN_100NS).max(finest);
    let hi = best
        .saturating_add(FINE_SPAN_100NS)
        .min(coarsest)
        .min(MAX_USEFUL_100NS);
    if lo > hi {
        return vec![best];
    }
    let mut v = Vec::new();
    let mut p = lo;
    while p <= hi {
        v.push(p);
        match p.checked_add(FINE_STEP_100NS) {
            Some(next) => p = next,
            None => break,
        }
    }
    v.push(best);
    v.sort_unstable();
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() <= eps
    }

    /// Synthetic point with only the fields the rules read.
    fn stat(period_100ns: u32, p50: f64, p95: f64, interference: f64) -> Stats {
        Stats {
            period_100ns,
            n: 100,
            min_ms: p50,
            p50_ms: p50,
            p95_ms: p95,
            max_ms: p95,
            interference,
            ticks: 1,
        }
    }

    #[test]
    fn percentiles_nearest_rank() {
        let mut s: Vec<f64> = (1..=11).map(|v| v as f64).collect();
        let st = Stats::from_samples(5000, &mut s);
        assert_eq!(st.n, 11);
        assert_eq!(st.min_ms, 1.0);
        assert_eq!(st.max_ms, 11.0);
        assert_eq!(st.p50_ms, 6.0);
        assert_eq!(st.p95_ms, 11.0);
        // Sorted in place, ascending.
        assert!(s.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn empty_and_tick_counting() {
        let mut empty: [f64; 0] = [];
        let e = Stats::from_samples(5120, &mut empty);
        assert_eq!(e.n, 0);
        assert_eq!(e.ticks, 0);
        assert_eq!(e.p50_ms, 0.0);

        let mut a = [1.5];
        assert_eq!(Stats::from_samples(5000, &mut a).ticks, 3);
        let mut b = [1.02];
        assert_eq!(Stats::from_samples(5120, &mut b).ticks, 2);
        // period 0 must not divide by zero.
        let mut c = [1.5];
        assert_eq!(Stats::from_samples(0, &mut c).ticks, 0);
    }

    #[test]
    fn interference_counts_outliers_only() {
        let mut samples = vec![1.0; 20];
        samples.push(50.0);
        let st = Stats::from_samples(5000, &mut samples);
        assert_eq!(st.p50_ms, 1.0);
        assert!(approx(st.interference, 1.0 / 21.0, 1e-12));
        assert!(!st.usable());

        // Non-outlier spread contributes nothing.
        let mut clean = vec![1.0, 1.1, 1.2, 1.3, 1.4];
        assert_eq!(Stats::from_samples(5000, &mut clean).interference, 0.0);
    }

    #[test]
    fn latency_prefers_lowest_score_among_usable() {
        let stats = vec![
            stat(4000, 1.05, 1.6, 0.0),  // score 1.325
            stat(5120, 1.10, 1.2, 0.0),  // score 1.15  <- best
            stat(6000, 1.20, 1.3, 0.0),  // score 1.25
            stat(3000, 0.90, 0.95, 0.0), // score 0.925 but not usable
            stat(7000, 1.10, 1.2, 0.5),  // same score, interference
        ];
        assert_eq!(choose(&stats, Prefer::Latency), Some(1));

        // Tie on score -> smaller period wins.
        let tie = vec![stat(6000, 1.10, 1.2, 0.0), stat(5000, 1.10, 1.2, 0.0)];
        assert_eq!(choose(&tie, Prefer::Latency), Some(1));

        let none = vec![stat(5000, 0.5, 0.6, 0.0)];
        assert_eq!(choose(&none, Prefer::Latency), None);
    }

    #[test]
    fn power_prefers_coarsest_within_budget() {
        let stats = vec![
            stat(5000, 1.10, 1.2, 0.0),
            stat(6000, 1.20, 1.5, 0.0), // within budget
            stat(8000, 1.30, 1.9, 0.0), // coarsest within budget <- best
            stat(9000, 1.40, 1.5, 0.0), // usable, p50 too high for budget
        ];
        assert_eq!(choose(&stats, Prefer::Power), Some(2));

        // Tie on period -> lower score wins.
        let tie = vec![stat(7000, 1.20, 1.5, 0.0), stat(7000, 1.10, 1.3, 0.0)];
        assert_eq!(choose(&tie, Prefer::Power), Some(1));

        // Nothing within budget -> latency fallback.
        let fallback = vec![
            stat(5000, 1.50, 1.6, 0.0), // score 1.55
            stat(6000, 1.45, 1.5, 0.0), // score 1.475 <- best
        ];
        assert_eq!(choose(&fallback, Prefer::Power), Some(1));
    }

    #[test]
    fn grids_clamp_and_keep_promised_endpoints() {
        let c = coarse_grid(5000, 20_000);
        assert_eq!(c.first(), Some(&5000));
        assert_eq!(c.last(), Some(&10_000)); // 10_000 lands on the step
        assert!(c.iter().all(|&p| (5000..=10_000).contains(&p)));
        assert!(c.windows(2).all(|w| w[1] - w[0] == COARSE_STEP_100NS));

        let off_step = coarse_grid(5000, 7250);
        assert_eq!(off_step.first(), Some(&5000));
        assert_eq!(off_step.last(), Some(&7200)); // hi not included
        assert!(!off_step.contains(&7250));

        // hi <= lo collapses to a single point.
        assert_eq!(coarse_grid(20_000, 10_000), vec![20_000]);

        let f = fine_grid(5100, 5000, 20_000);
        assert!(f.contains(&5100));
        assert!(f.iter().all(|&p| (5000..=10_000).contains(&p)));
        assert!(f.windows(2).all(|w| w[1] - w[0] == FINE_STEP_100NS));
        assert_eq!(f.first(), Some(&5000)); // best - span clamped to finest

        let clamped = fine_grid(9900, 5000, 20_000);
        assert!(clamped.contains(&9900));
        assert!(clamped.iter().all(|&p| p <= MAX_USEFUL_100NS));
        assert_eq!(clamped.last(), Some(&9990));

        // lo > hi -> only best.
        assert_eq!(fine_grid(100, 500, 600), vec![100]);
    }
}
