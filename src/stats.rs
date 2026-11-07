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
    pub min_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    /// Worst sample; exposes preemption that percentiles hide.
    pub max_ms: f64,
    /// Fraction of samples that look preempted rather than timer-quantised.
    pub interference: f64,
}

/// A sample counts as interference when it overshoots p50 by this much. A
/// relative `4 x p50` bar would be 4 ms on a 1 ms row, hiding exactly the
/// 2-3 ms stalls worth reporting, and 2 ms on a 0.5 ms row.
const INTERFERENCE_MARGIN_MS: f64 = 1.0;

impl Stats {
    /// `samples` are slept durations in milliseconds; the slice is sorted in place.
    pub fn from_samples(period_100ns: u32, samples: &mut [f64]) -> Stats {
        samples.sort_by(|a, b| a.total_cmp(b));
        let n = samples.len();
        if n == 0 {
            return Stats {
                period_100ns,
                min_ms: 0.0,
                p50_ms: 0.0,
                p95_ms: 0.0,
                max_ms: 0.0,
                interference: 0.0,
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
            let limit = p50_ms + INTERFERENCE_MARGIN_MS;
            samples.iter().filter(|&&s| s > limit).count() as f64 / n as f64
        };

        Stats {
            period_100ns,
            min_ms: samples[0],
            p50_ms,
            p95_ms: pct(0.95),
            max_ms: samples[n - 1],
            interference,
        }
    }

    /// Safe to recommend at all: low interference, no early wake-ups.
    ///
    /// The coarse pass only takes 200 samples, so 1 % would mean two hiccups
    /// reject the point; 2 % still catches a genuinely noisy run.
    pub fn usable(&self) -> bool {
        self.interference <= 0.02 && self.p50_ms >= 0.99 && self.p95_ms <= 2.5
    }

    /// Inside the latency budget, i.e. a candidate for `Prefer::Power`.
    pub fn within_budget(&self) -> bool {
        self.usable() && self.p50_ms <= 1.35 && self.p95_ms <= 2.0
    }
}

/// Index of the coarsest point that still lands inside the latency budget.
///
/// `Prefer::Power` asks for the cheapest setting that keeps the latency; the
/// winner can sit on the edge of the plateau, which `scan` warns about.
pub fn coarsest_within_budget(points: &[Stats]) -> Option<usize> {
    points
        .iter()
        .enumerate()
        .filter(|(_, s)| s.within_budget())
        .max_by_key(|(_, s)| s.period_100ns)
        .map(|(i, _)| i)
}

/// How close to the best median a point must be to count as the same setting.
/// The quantisation plateaus are ~0.5 ms apart, so this separates them easily.
pub const BAND_MARGIN_MS: f64 = 0.05;

/// Index of the recommended point and the plateau it belongs to.
///
/// Inside the fast band every period measures the same, so the argmin is noise.
/// Recommend the middle of the band instead: the edges are marginal by
/// definition, and the middle reproduces across runs. The band is the
/// contiguous plateau containing the best point, so it never claims a gap is
/// equivalent.
pub fn choose_band(points: &[Stats]) -> Option<(usize, Plateau)> {
    let best_index = points
        .iter()
        .enumerate()
        .filter(|(_, s)| s.usable())
        .min_by(|a, b| a.1.p50_ms.total_cmp(&b.1.p50_ms))?
        .0;
    let group = plateaus(points)
        .into_iter()
        .find(|p| best_index >= p.start && best_index < p.start + p.points)?;
    let mid = group.lo_100ns + (group.hi_100ns - group.lo_100ns) / 2;
    let (index, _) = points[group.start..group.start + group.points]
        .iter()
        .enumerate()
        .filter(|(_, s)| s.usable())
        .map(|(k, s)| (group.start + k, s))
        .min_by_key(|(_, s)| s.period_100ns.abs_diff(mid))?;
    Some((index, group))
}

/// A run of consecutive measured points that behave the same.
#[derive(Clone, Copy, Debug)]
pub struct Plateau {
    /// Scan-order index of the first point in the run.
    pub start: usize,
    pub lo_100ns: u32,
    pub hi_100ns: u32,
    /// Median p50 of the run, in milliseconds.
    pub p50_ms: f64,
    pub points: usize,
}

/// Collapse the measured curve into plateaus for display.
///
/// Points arrive in scan order, which is period order, so a plateau is a
/// contiguous run whose p50 span stays within `BAND_MARGIN_MS`. Tracking the
/// running min and max (not the first point) keeps a monotonic drift from
/// hiding inside one plateau.
pub fn plateaus(points: &[Stats]) -> Vec<Plateau> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < points.len() {
        let mut lo = points[i].p50_ms;
        let mut hi = lo;
        let mut j = i + 1;
        while j < points.len() {
            let v = points[j].p50_ms;
            let next_lo = lo.min(v);
            let next_hi = hi.max(v);
            if next_hi - next_lo > BAND_MARGIN_MS {
                break;
            }
            lo = next_lo;
            hi = next_hi;
            j += 1;
        }
        let mut p50s: Vec<f64> = points[i..j].iter().map(|s| s.p50_ms).collect();
        p50s.sort_by(f64::total_cmp);
        out.push(Plateau {
            start: i,
            lo_100ns: points[i].period_100ns,
            hi_100ns: points[j - 1].period_100ns,
            p50_ms: p50s[p50s.len() / 2],
            points: j - i,
        });
        i = j;
    }
    out
}

/// What a two-process measurement says about `GlobalTimerResolutionRequests`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// A process that requested nothing saw the finer tick.
    Global,
    /// A process that requested nothing kept the coarse tick.
    PerProcess,
    /// The baseline was already fine, so nothing can be concluded.
    Inconclusive,
}

/// Baseline at or below this is already as fast as the request we made, so the
/// child cannot tell the modes apart. A 1 ms tick still sleeps ~2 ms, so even
/// the slowest "already fine" baseline stays under this.
const BASELINE_ALREADY_FAST_MS: f64 = 1.5;

/// Compare a no-request baseline against a child process measured while this
/// process held the finest request.
///
/// The ratio is the discriminator, not an absolute cutoff: a machine whose
/// ambient tick is 1 ms measures ~2 ms either way, so only a child that is
/// clearly faster than the baseline proves the request crossed into it.
pub fn classify_global(baseline_ms: f64, child_ms: f64) -> Verdict {
    if baseline_ms <= BASELINE_ALREADY_FAST_MS {
        Verdict::Inconclusive
    } else if child_ms <= baseline_ms * 0.75 {
        Verdict::Global
    } else {
        Verdict::PerProcess
    }
}

/// Convert a period in 100 ns units to milliseconds.
pub fn ms(period_100ns: u32) -> f64 {
    period_100ns as f64 / 10_000.0
}

/// Coarse scan step: 10 us.
pub const COARSE_STEP_100NS: u32 = 100;
/// Dense scan step: one representable timer unit (100 ns). The knee where
/// `Sleep(1)` drops from three ticks to two is under 1 us wide on recent
/// builds, so a coarser step can walk straight over it.
pub const FINE_STEP_100NS: u32 = 1;
/// How far above `finest` the dense sweep reaches: 50 us. Every knee measured
/// so far sits 2-11 us above `finest`, but there is no API for it, so this is
/// a fixed window rather than one anchored on a coarse winner: outside the
/// knee every coarse row measures the same, so that winner is noise.
pub const DENSE_SPAN_100NS: u32 = 500;
/// Nothing above 1 ms is worth scanning: `Sleep(1)` already takes 2 ticks there.
pub const MAX_USEFUL_100NS: u32 = 10_000;

/// Coarse sweep from after the dense window up to `min(1 ms, coarsest)`.
pub fn coarse_grid(finest: u32, coarsest: u32) -> Vec<u32> {
    let lo = finest.saturating_add(DENSE_SPAN_100NS + COARSE_STEP_100NS);
    let hi = coarsest.min(MAX_USEFUL_100NS);
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

/// Dense sweep where the tick knee lives, one unit at a time, starting at
/// `finest`. Exhaustive over the window, so no value the kernel accepts is
/// skipped and the result does not depend on a noisy coarse ranking.
pub fn dense_grid(finest: u32, coarsest: u32) -> Vec<u32> {
    let hi = finest
        .saturating_add(DENSE_SPAN_100NS)
        .min(coarsest)
        .min(MAX_USEFUL_100NS);
    let mut v = Vec::new();
    let mut p = finest;
    while p <= hi {
        v.push(p);
        match p.checked_add(FINE_STEP_100NS) {
            Some(next) => p = next,
            None => break,
        }
    }
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
            min_ms: p50,
            p50_ms: p50,
            p95_ms: p95,
            max_ms: p95,
            interference,
        }
    }

    #[test]
    fn percentiles_nearest_rank() {
        let mut s: Vec<f64> = (1..=11).map(|v| v as f64).collect();
        let st = Stats::from_samples(5000, &mut s);
        assert_eq!(st.min_ms, 1.0);
        assert_eq!(st.max_ms, 11.0);
        assert_eq!(st.p50_ms, 6.0);
        assert_eq!(st.p95_ms, 11.0);
        // Sorted in place, ascending.
        assert!(s.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn empty_input_is_zero() {
        let mut empty: [f64; 0] = [];
        let e = Stats::from_samples(5120, &mut empty);
        assert_eq!(e.p50_ms, 0.0);
        assert!(!e.usable());

        // A period of 0 must not be used as a divisor anywhere.
        let mut c = [1.5];
        let s = Stats::from_samples(0, &mut c);
        assert_eq!(s.p50_ms, 1.5);
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

        // An absolute margin, so a one-tick tail on a 1 ms row is not
        // interference while a 2.5 ms stall is.
        let mut tail = vec![1.017; 19];
        tail.push(1.5);
        assert_eq!(Stats::from_samples(5114, &mut tail).interference, 0.0);
        tail[19] = 2.5;
        let st = Stats::from_samples(5114, &mut tail);
        assert!(approx(st.interference, 1.0 / 20.0, 1e-12));
    }

    #[test]
    fn band_is_the_plateau_and_the_middle_wins() {
        let stats = vec![
            stat(5000, 1.505, 1.51, 0.0),
            stat(5100, 1.510, 1.52, 0.0),
            stat(5106, 1.026, 1.52, 0.0),
            stat(5110, 1.017, 1.52, 0.0), // best p50
            stat(5120, 1.022, 1.52, 0.0),
            stat(5132, 1.028, 1.52, 0.0),
            stat(5133, 1.513, 1.52, 0.0),
            stat(5200, 1.518, 1.52, 0.0),
        ];
        let (index, band) = choose_band(&stats).unwrap();
        assert_eq!((band.lo_100ns, band.hi_100ns, band.points), (5106, 5132, 4));
        assert_eq!(stats[index].period_100ns, 5120); // middle 5119, nearest measured
        assert!(stats[index].p50_ms < 1.1);

        // A single usable point is its own band.
        let lone = vec![stat(5000, 1.4, 1.6, 0.0), stat(6000, 1.2, 1.5, 0.0)];
        let (index, band) = choose_band(&lone).unwrap();
        assert_eq!((band.lo_100ns, band.hi_100ns, band.points), (6000, 6000, 1));
        assert_eq!(index, 1);

        let none = vec![stat(5000, 0.5, 0.6, 0.0)];
        assert!(choose_band(&none).is_none());
    }

    #[test]
    fn band_never_spans_a_gap() {
        // A stray fast-looking point far above the plateau must not widen the
        // band and claim every period in between is equivalent.
        let stats = vec![
            stat(5000, 1.505, 1.51, 0.0),
            stat(5106, 1.017, 1.52, 0.0),
            stat(5110, 1.020, 1.52, 0.0),
            stat(5200, 1.519, 1.52, 0.0),
            stat(5400, 1.030, 1.40, 0.0), // isolated, low p50
        ];
        let (_, band) = choose_band(&stats).unwrap();
        assert_eq!((band.lo_100ns, band.hi_100ns), (5106, 5110));
    }

    #[test]
    fn plateaus_collapse_the_curve() {
        let mut points = Vec::new();
        for p in (5000..5106).step_by(10) {
            points.push(stat(p, 1.505, 1.51, 0.0));
        }
        for p in 5106..=5132 {
            points.push(stat(p, 1.017, 1.52, 0.0));
        }
        for p in (5133..=5200).step_by(10) {
            points.push(stat(p, 1.518, 1.52, 0.0));
        }
        let g = plateaus(&points);
        assert_eq!(g.len(), 3);
        assert_eq!(
            (g[1].start, g[1].lo_100ns, g[1].hi_100ns, g[1].points),
            (11, 5106, 5132, 27)
        );
        assert_eq!(g[2].lo_100ns, 5133);
        assert_eq!(g.iter().map(|p| p.points).sum::<usize>(), points.len());
    }

    #[test]
    fn plateau_tracks_running_span_not_the_first_point() {
        // A 0.06 ms drift is a change of setting, not one plateau.
        let drift: Vec<Stats> = (0..11)
            .map(|k| stat(5000 + k, 1.00 + 0.006 * k as f64, 1.2, 0.0))
            .collect();
        assert!(plateaus(&drift).len() > 1);

        // A V shape that ends where it started is still a 0.10 ms spread.
        let v = vec![
            stat(5000, 1.00, 1.2, 0.0),
            stat(5001, 1.10, 1.2, 0.0),
            stat(5002, 1.00, 1.2, 0.0),
        ];
        assert!(plateaus(&v).len() > 1);
    }

    #[test]
    fn power_prefers_coarsest_within_budget() {
        let stats = vec![
            stat(5000, 1.10, 1.2, 0.0),
            stat(6000, 1.20, 1.5, 0.0), // within budget
            stat(8000, 1.30, 1.9, 0.0), // coarsest within budget <- best
            stat(9000, 1.40, 1.5, 0.0), // usable, p50 too high for budget
        ];
        assert_eq!(coarsest_within_budget(&stats), Some(2));

        // Nothing within budget.
        let none = vec![stat(5000, 1.50, 1.6, 0.0), stat(6000, 1.45, 1.5, 0.0)];
        assert_eq!(coarsest_within_budget(&none), None);
    }

    #[test]
    fn global_verdict_is_relative_to_the_baseline() {
        assert_eq!(classify_global(15.6, 1.0), Verdict::Global);
        assert_eq!(classify_global(15.6, 15.6), Verdict::PerProcess);
        // A 1 ms ambient tick sleeps ~2 ms; a child that stays there did not
        // see the request, so this is a per-process answer, not ambiguity.
        assert_eq!(classify_global(2.0, 2.0), Verdict::PerProcess);
        assert_eq!(classify_global(2.0, 1.0), Verdict::Global);
        // Already as fast as the request: the child cannot discriminate.
        assert_eq!(classify_global(1.0, 1.0), Verdict::Inconclusive);
        assert_eq!(classify_global(1.5, 1.5), Verdict::Inconclusive);
    }

    #[test]
    fn dense_window_covers_the_lower_band() {
        let d = dense_grid(5000, 20_000);
        assert_eq!(d.len(), 501);
        assert_eq!(d.first(), Some(&5000));
        assert_eq!(d.last(), Some(&5500));
        assert!(d.windows(2).all(|w| w[1] - w[0] == FINE_STEP_100NS));

        // Clamped at the useful maximum on a machine with a high floor.
        let top = dense_grid(9800, 20_000);
        assert_eq!(top.last(), Some(&MAX_USEFUL_100NS));
        assert_eq!(top.len(), 201);

        // Floor above the useful maximum: nothing to sweep.
        assert!(dense_grid(20_000, 156_250).is_empty());
    }

    #[test]
    fn coarse_grid_starts_after_the_dense_window() {
        let c = coarse_grid(5000, 20_000);
        assert_eq!(c.first(), Some(&5600));
        assert_eq!(c.last(), Some(&10_000)); // 10_000 lands on the step
        assert!(c.windows(2).all(|w| w[1] - w[0] == COARSE_STEP_100NS));

        let off_step = coarse_grid(5000, 7250);
        assert_eq!(off_step.last(), Some(&7200)); // hi not included
        assert!(!off_step.contains(&7250));

        // No room left between the dense window and the ceiling.
        assert!(coarse_grid(5000, 5500).is_empty());
    }

    #[test]
    fn dense_grid_covers_the_tick_knee() {
        // Builds 26200/26300 put the knee 18-25 units above the floor; the
        // machine measured here put it at 5106-5132.
        let d = dense_grid(5000, 20_000);
        assert!(d.contains(&5018) && d.contains(&5023));
        assert!(d.contains(&5106) && d.contains(&5132));
    }

    #[test]
    fn global_verdict_has_three_outcomes() {
        assert_eq!(classify_global(15.6, 1.02), Verdict::Global);
        assert_eq!(classify_global(15.6, 1.43), Verdict::Global);
        assert_eq!(classify_global(15.6, 15.5), Verdict::PerProcess);
        // Something already holds a fine request: no conclusion possible.
        assert_eq!(classify_global(1.02, 1.02), Verdict::Inconclusive);
    }
}
