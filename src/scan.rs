//! Dense sweep of the low band, coarse sweep of the rest, then a stability re-check.

use std::io::IsTerminal;
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
    /// Period range of the plateau the recommendation came from.
    pub band: Option<(u32, u32)>,
    /// Whether the dense winner reproduced within 40 us on a second pass.
    pub stable: bool,
    /// The user stopped the scan with ESC before it finished.
    pub partial: bool,
}

/// Progress goes to stderr so `timres bench > results.txt` stays clean.
pub fn run(opts: &Options) -> Result<Report, String> {
    let range = nt::query()?;
    // Requesters share the finest period anyone asks for, so a candidate above
    // another process's request cannot be measured. Near the 1 ms ceiling this
    // only affects the last row, where the warning would be noise.
    if range.current.saturating_add(stats::COARSE_STEP_100NS) <= stats::MAX_USEFUL_100NS {
        eprintln!(
            "warning: another process holds a {:.3} ms request. Candidates above that measure \
             the same value; close it for an accurate upper range.",
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

/// One line, rewritten in place: 546 lines of numbers is not progress.
fn progress(done: usize, total: usize, period: u32) {
    use std::io::Write;
    if std::io::stderr().is_terminal() {
        let _ = write!(
            std::io::stderr(),
            "\r  scanning {done}/{total}   {:.4} ms   ",
            ms(period)
        );
    }
}

fn end_progress() {
    if std::io::stderr().is_terminal() {
        eprintln!();
    }
}

fn scan(opts: &Options, range: Range) -> Result<Report, String> {
    let dense = stats::dense_grid(range.finest, range.coarsest);
    let coarse = stats::coarse_grid(range.finest, range.coarsest);
    let total = dense.len() + coarse.len();
    let mut points = Vec::new();
    let mut partial = false;
    // Drain any ESC press from before the scan so it does not stop it at once.
    let _ = measure::escape_pressed();

    // Fixed dense window: outside the knee every coarse row measures the same,
    // so a coarse winner there is noise and must not decide where this runs.
    for &period in &dense {
        if measure::escape_pressed() {
            partial = true;
            break;
        }
        points.push(measure_point(period, opts.samples_fine)?);
        progress(points.len(), total, period);
    }

    if !partial {
        for &period in &coarse {
            if measure::escape_pressed() {
                partial = true;
                break;
            }
            points.push(measure_point(period, opts.samples_coarse)?);
            progress(points.len(), total, period);
        }
    }
    end_progress();

    let (chosen, band) = match opts.prefer {
        Prefer::Power => match stats::choose(&points, Prefer::Power) {
            // A value inside the budget is a real choice, not a plateau tie.
            Some(i) if points[i].within_budget() => (Some(points[i].clone()), None),
            _ => match stats::choose_band(&points) {
                Some((i, b)) => (Some(points[i].clone()), Some(b)),
                None => (None, None),
            },
        },
        Prefer::Latency => match stats::choose_band(&points) {
            Some((i, b)) => (Some(points[i].clone()), Some(b)),
            None => (None, None),
        },
    };
    if partial {
        eprintln!("  stopped (ESC) after {} points", points.len());
    }
    let stable = match &chosen {
        Some(c) if !partial => {
            eprintln!("  re-checking {:.4} ms", ms(c.period_100ns));
            let again = measure_point(c.period_100ns, opts.samples_fine)?;
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
