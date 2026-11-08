//! Dense sweep of the low band, coarse sweep of the rest, then a stability re-check.

use std::time::Duration;

use crate::cli::err;
use crate::measure;
use crate::nt::{self, Range};
use crate::stats::{self, Plateau, Prefer, Stats, ms};

pub struct Options {
    pub samples: usize,
    pub prefer: Prefer,
}

pub struct Report {
    pub range: Range,
    /// Every measured point, in scan order.
    pub points: Vec<Stats>,
    pub chosen: Option<Stats>,
    /// The plateau the recommendation came from.
    pub band: Option<Plateau>,
    /// Whether the winner reproduced within 40 us on a second pass.
    pub stable: bool,
    /// The user stopped the scan before it finished.
    pub partial: bool,
}

/// Progress goes to stderr so `ticktune bench > results.txt` stays clean.
pub fn run(opts: &Options) -> Result<Report, String> {
    // Two benches share one tick, so each would corrupt the other's curve.
    let _guard = nt::bench_instance()?;

    let range = nt::query()?;
    // A request below the scanned ceiling props up every candidate above it.
    // A holder above the ceiling never meets a scanned candidate, so stay quiet.
    if range.current <= stats::MAX_USEFUL_100NS {
        err(format_args!(
            "warning: another process holds a {:.3} ms request. Candidates at or above that measure the same value; close it for an accurate upper range.",
            ms(range.current)
        ));
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

fn progress(done: usize, total: usize, period: u32) {
    use std::io::{IsTerminal, Write};
    if std::io::stderr().is_terminal() {
        let _ = write!(
            std::io::stderr(),
            "\r  scanning {done}/{total}   {:.4} ms   ",
            ms(period)
        );
    }
}

fn end_progress() {
    use std::io::{IsTerminal, Write};
    if std::io::stderr().is_terminal() {
        let _ = writeln!(std::io::stderr());
    }
}

fn scan(opts: &Options, range: Range) -> Result<Report, String> {
    let list: Vec<u32> = stats::dense_grid(range.finest, range.coarsest)
        .into_iter()
        .chain(stats::coarse_grid(range.finest, range.coarsest))
        .collect();
    let total = list.len();
    let mut points = Vec::with_capacity(total);
    let mut partial = false;

    // Drain a press that happened before the scan started.
    let _ = measure::escape_pressed();

    for period in list {
        if measure::escape_pressed() {
            partial = true;
            break;
        }
        points.push(measure_point(period, opts.samples)?);
        progress(points.len(), total, period);
    }
    end_progress();
    if partial {
        err(format_args!(
            "  stopped (ESC) after {} points",
            points.len()
        ));
    }

    let pick = match opts.prefer {
        Prefer::Latency => stats::choose_band(&points),
        // Nothing inside the budget: fall back to the latency winner.
        Prefer::Power => stats::choose_power_band(&points).or_else(|| stats::choose_band(&points)),
    };
    let (chosen, band) = match pick {
        Some((i, band)) => (Some(points[i].clone()), Some(band)),
        None => (None, None),
    };

    let stable = match (&chosen, partial) {
        (Some(c), false) => {
            let again = measure_point(c.period_100ns, opts.samples)?;
            (again.p50_ms - c.p50_ms).abs() <= 0.04
        }
        _ => false,
    };

    Ok(Report {
        range,
        points,
        chosen,
        band,
        stable,
        partial,
    })
}
