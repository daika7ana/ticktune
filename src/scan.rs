//! Two-pass sweep: coarse grid, then refine, then a stability re-check.

use std::time::Duration;

use crate::measure;
use crate::nt::{self, Range};
use crate::stats::{self, ms, Prefer, Stats};

/// `THREAD_PRIORITY_TIME_CRITICAL`.
const TIME_CRITICAL: i32 = 15;

pub struct Options {
    pub samples_coarse: usize,
    pub samples_fine: usize,
    pub prefer: Prefer,
}

pub struct Report {
    pub range: Range,
    /// Every measured point, in scan order.
    pub points: Vec<Stats>,
    pub chosen: Option<Stats>,
    /// Whether the fine winner reproduced within 40 us on a second pass.
    pub stable: bool,
}

/// Progress goes to stderr so `timres bench > results.txt` stays clean.
pub fn run(opts: &Options) -> Result<Report, String> {
    let range = nt::query()?;
    if range.current < stats::MAX_USEFUL_100NS {
        eprintln!(
            "warning: another process already holds a {:.3} ms request; this scan will look better than it will once that process exits.",
            ms(range.current)
        );
    }
    nt::ignore_timer_resolution_throttling();
    // Remember the caller's priority rather than assuming it was normal.
    let previous_priority = nt::thread_priority();
    nt::set_thread_priority(TIME_CRITICAL);

    let result = scan(opts, range);

    nt::set_thread_priority(previous_priority);
    let released = nt::release();
    let report = result?;
    released?;
    Ok(report)
}

/// Request `period`, let it settle, then sample. `Stats` is given the requested
/// period, not the granted one: the request is the candidate being evaluated.
fn measure_point(period: u32, samples: usize) -> Result<Stats, String> {
    // Release first: a coarser request does not necessarily move a tick that
    // this process's previous, finer request already holds. That is why the
    // original tool killed and respawned its process for every candidate.
    let _ = nt::release();
    nt::set(period)?;
    std::thread::sleep(Duration::from_millis(20));
    let mut durations = measure::sample_sleep(samples);
    Ok(Stats::from_samples(period, &mut durations))
}

fn scan(opts: &Options, range: Range) -> Result<Report, String> {
    let mut points = Vec::new();

    for &period in &stats::coarse_grid(range.finest, range.coarsest) {
        let s = measure_point(period, opts.samples_coarse)?;
        eprintln!(
            "  {:>7.4} ms -> p50 {:>6.3} p95 {:>6.3} interference {:.1}%",
            ms(period),
            s.p50_ms,
            s.p95_ms,
            s.interference * 100.0
        );
        points.push(s);
    }

    let coarse_len = points.len();
    let coarse_best = stats::choose(&points[..coarse_len], opts.prefer)
        .ok_or("no usable point in coarse pass")?;
    let coarse_period = points[coarse_best].period_100ns;

    for &period in &stats::fine_grid(coarse_period, range.finest, range.coarsest) {
        let s = measure_point(period, opts.samples_fine)?;
        eprintln!(
            "  {:>7.4} ms -> p50 {:>6.3} p95 {:>6.3} interference {:.1}%",
            ms(period),
            s.p50_ms,
            s.p95_ms,
            s.interference * 100.0
        );
        points.push(s);
    }

    let fine = &points[coarse_len..];
    // A noisy fine pass must not throw away a good coarse measurement.
    let chosen = stats::choose(fine, opts.prefer)
        .map(|i| fine[i].clone())
        .or_else(|| Some(points[coarse_best].clone()));

    let stable = match &chosen {
        Some(c) => {
            let again = measure_point(c.period_100ns, opts.samples_fine)?;
            eprintln!(
                "  {:>7.4} ms -> p50 {:>6.3} p95 {:>6.3} interference {:.1}% (recheck)",
                ms(c.period_100ns),
                again.p50_ms,
                again.p95_ms,
                again.interference * 100.0
            );
            (again.p50_ms - c.p50_ms).abs() <= 0.04
        }
        None => false,
    };

    Ok(Report {
        range,
        points,
        chosen,
        stable,
    })
}
