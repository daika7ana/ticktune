//! Dense sweep of the low band, coarse sweep of the rest, then a stability re-check.

use std::time::Duration;

use crate::measure;
use crate::nt::{self, Range};
use crate::stats::{self, ms, Prefer, Stats};

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
    /// Whether the dense winner reproduced within 40 us on a second pass.
    pub stable: bool,
}

/// Progress goes to stderr so `timres bench > results.txt` stays clean.
pub fn run(opts: &Options) -> Result<Report, String> {
    let range = nt::query()?;
    if range.current < range.coarsest {
        eprintln!(
            "warning: another process already holds a {:.3} ms request; this scan will look better than it will once that process exits.",
            ms(range.current)
        );
    }
    // Remember the caller's priority rather than assuming it was normal.
    let previous_priority = nt::thread_priority();
    nt::prepare_sampling();

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

fn progress(period: u32, s: &Stats, note: &str) {
    eprintln!(
        "  {:>7.4} ms -> p50 {:>6.3} p95 {:>6.3} interference {:.1}%{note}",
        ms(period),
        s.p50_ms,
        s.p95_ms,
        s.interference * 100.0
    );
}

fn scan(opts: &Options, range: Range) -> Result<Report, String> {
    let mut points = Vec::new();

    // Fixed dense window: outside the knee every coarse row measures the same,
    // so a coarse winner there is noise and must not decide where this runs.
    for &period in &stats::dense_grid(range.finest, range.coarsest) {
        let s = measure_point(period, opts.samples_fine)?;
        progress(period, &s, "");
        points.push(s);
    }

    for &period in &stats::coarse_grid(range.finest, range.coarsest) {
        let s = measure_point(period, opts.samples_coarse)?;
        progress(period, &s, "");
        points.push(s);
    }

    let chosen = stats::choose(&points, opts.prefer).map(|i| points[i].clone());
    let stable = match &chosen {
        Some(c) => {
            let again = measure_point(c.period_100ns, opts.samples_fine)?;
            progress(c.period_100ns, &again, " (recheck)");
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
