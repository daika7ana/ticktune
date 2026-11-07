//! Two-pass sweep: coarse grid, then refine, then a stability re-check.

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
    /// Whether the fine winner reproduced within 4 us on a second pass.
    pub stable: bool,
}

/// Progress goes to stderr so `timres bench > results.csv` stays clean.
pub fn run(opts: &Options) -> Result<Report, String> {
    let range = nt::query()?;
    nt::ignore_timer_resolution_throttling();
    nt::set_thread_time_critical(true);

    let result = scan(opts, range);

    nt::set_thread_time_critical(false);
    let released = nt::release();
    let report = result?;
    released?;
    Ok(report)
}

/// Request `period`, let it settle, then sample. `Stats` is given the requested
/// period, not the granted one: the request is the candidate being evaluated.
fn measure_point(period: u32, samples: usize) -> Result<Stats, String> {
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
            "  {:>7.4} ms -> p50 {:>6.3} p95 {:>6.3} ticks {} interference {:.1}%",
            ms(period),
            s.p50_ms,
            s.p95_ms,
            s.ticks,
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
            "  {:>7.4} ms -> p50 {:>6.3} p95 {:>6.3} ticks {} interference {:.1}%",
            ms(period),
            s.p50_ms,
            s.p95_ms,
            s.ticks,
            s.interference * 100.0
        );
        points.push(s);
    }

    let fine = &points[coarse_len..];
    let chosen = stats::choose(fine, opts.prefer).map(|i| fine[i].clone());

    let stable = match &chosen {
        Some(c) => {
            let again = measure_point(c.period_100ns, opts.samples_fine)?;
            eprintln!(
                "  {:>7.4} ms -> p50 {:>6.3} p95 {:>6.3} ticks {} interference {:.1}% (recheck)",
                ms(c.period_100ns),
                again.p50_ms,
                again.p95_ms,
                again.ticks,
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
