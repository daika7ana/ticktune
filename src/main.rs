#![cfg_attr(not(windows), allow(dead_code))]

mod stats;

#[cfg(windows)]
mod measure;
#[cfg(windows)]
mod nt;
#[cfg(any(windows, test))]
mod persist;
#[cfg(windows)]
mod scan;

#[cfg(windows)]
mod cli {
    use crate::measure;
    use crate::nt;
    use crate::persist;
    use crate::scan;
    use crate::stats::{self, Prefer, Stats, Verdict};

    const USAGE: &str =
        "usage: timres query | bench [--samples N] [--prefer latency|power] [--rows] | \
apply (--resolution <100ns> | --ms <f64>) [--persist] [--no-console] | status | \
measure [--samples N] | verify-global | global on|off | startup remove";

    /// Samples per point in a bench run. 547 points at 1000 samples is ~11
    /// minutes; a larger number would look like a hang.
    const MAX_BENCH_SAMPLES: usize = 1000;

    /// stdout may be a closed pipe (`timres bench | head`); a failed write must
    /// not abort the scan.
    fn out(args: std::fmt::Arguments<'_>) {
        use std::io::Write;
        let _ = writeln!(std::io::stdout(), "{args}");
    }

    /// `println!` for stdout that may be a closed pipe.
    macro_rules! outln {
        ($($t:tt)*) => { out(format_args!($($t)*)) };
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn FreeConsole() -> i32;
    }

    enum Fail {
        Usage(String),
        Op(String),
    }

    fn fmt_period(p: u32) -> String {
        format!("{:.4}", stats::ms(p))
    }

    fn take<'a>(args: &'a [String], i: &mut usize, flag: &str) -> Result<&'a str, Fail> {
        *i += 1;
        args.get(*i)
            .map(String::as_str)
            .ok_or_else(|| Fail::Usage(format!("{flag} requires a value")))
    }

    /// Cap sample counts so a typo cannot allocate or run for days.
    fn parse_samples(v: &str) -> Result<usize, Fail> {
        let n: usize = v
            .parse()
            .map_err(|_| Fail::Usage(format!("invalid --samples: {v}")))?;
        if n == 0 {
            return Err(Fail::Usage("--samples must be at least 1".into()));
        }
        Ok(n.min(100_000))
    }

    pub fn run() -> i32 {
        match dispatch() {
            Ok(()) => 0,
            Err(Fail::Usage(m)) => {
                eprintln!("error: {m}");
                eprintln!("{USAGE}");
                2
            }
            Err(Fail::Op(m)) => {
                eprintln!("error: {m}");
                1
            }
        }
    }

    fn dispatch() -> Result<(), Fail> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        // `--help` works in any position, including after a subcommand.
        if args.iter().any(|a| a == "--help" || a == "-h") {
            outln!("{USAGE}");
            return Ok(());
        }
        let cmd = match args.first() {
            Some(c) => c.as_str(),
            None => return Err(Fail::Usage("no subcommand".into())),
        };
        let rest = &args[1..];
        match cmd {
            "query" => {
                no_args("query", rest)?;
                cmd_query()
            }
            "bench" => cmd_bench(rest),
            "apply" => cmd_apply(rest),
            "status" => {
                no_args("status", rest)?;
                cmd_status()
            }
            "measure" => cmd_measure(rest),
            "verify-global" => {
                no_args("verify-global", rest)?;
                cmd_verify_global()
            }
            "global" => cmd_global(rest),
            "startup" => cmd_startup(rest),
            "help" => {
                outln!("{USAGE}");
                Ok(())
            }
            other => Err(Fail::Usage(format!("unknown subcommand: {other}"))),
        }
    }

    fn no_args(name: &str, rest: &[String]) -> Result<(), Fail> {
        match rest.first() {
            Some(a) => Err(Fail::Usage(format!("{name} takes no arguments (got {a})"))),
            None => Ok(()),
        }
    }

    fn cmd_query() -> Result<(), Fail> {
        let range = nt::query().map_err(Fail::Op)?;
        print_query_block(&range);
        Ok(())
    }

    fn print_query_block(range: &nt::Range) {
        let elevated = nt::is_elevated();
        outln!(
            "{:<20}{} ms  ({} x 100ns)",
            "finest",
            fmt_period(range.finest),
            range.finest
        );
        outln!(
            "{:<20}{} ms  ({} x 100ns)",
            "coarsest",
            fmt_period(range.coarsest),
            range.coarsest
        );
        outln!("{:<20}{} ms", "global tick", fmt_period(range.current));
        let mode = match nt::global_requests_enabled() {
            Some(1) => "global (GlobalTimerResolutionRequests=1)",
            Some(0) => "per-process (GlobalTimerResolutionRequests=0)",
            _ => "per-process",
        };
        outln!("{:<20}{}", "mode", mode);
        match nt::dynamic_tick_disabled() {
            Some(true) => {
                outln!("{:<20}disabled (! risk)", "dynamic tick");
                outln!("{:<20}revert: bcdedit /deletevalue disabledynamictick", "");
            }
            Some(false) => outln!("{:<20}enabled", "dynamic tick"),
            // bcdedit omits the line when it was never set, which is the default.
            None if elevated => outln!("{:<20}unset (default: enabled)", "dynamic tick"),
            None => outln!("{:<20}unknown (run elevated to read)", "dynamic tick"),
        }
        let startup = match persist::logon_task() {
            Some(task) => {
                let stale = if task.stale {
                    " (stale: task points at another exe)"
                } else {
                    ""
                };
                format!(
                    "{} task: {} x 100ns{}",
                    persist::TASK_NAME,
                    task.period_100ns,
                    stale
                )
            }
            None => "none".to_string(),
        };
        outln!("{:<20}{}", "logon", startup);
        outln!("{:<20}{}", "elevated", if elevated { "yes" } else { "no" });
    }

    fn cmd_bench(args: &[String]) -> Result<(), Fail> {
        let mut samples: Option<usize> = None;
        let mut prefer = Prefer::Latency;
        let mut rows = false;
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--samples" => {
                    let v = take(args, &mut i, "--samples")?;
                    samples = Some(parse_samples(v)?);
                }
                "--rows" => rows = true,
                "--prefer" => {
                    let v = take(args, &mut i, "--prefer")?;
                    prefer = match v {
                        "latency" => Prefer::Latency,
                        "power" => Prefer::Power,
                        _ => {
                            return Err(Fail::Usage(format!(
                                "--prefer must be latency or power: {v}"
                            )))
                        }
                    };
                }
                a => return Err(Fail::Usage(format!("unknown bench argument: {a}"))),
            }
            i += 1;
        }

        let samples = match samples {
            Some(n) if n > MAX_BENCH_SAMPLES => {
                eprintln!(
                    "note: bench caps --samples at {MAX_BENCH_SAMPLES} so a run stays under ~13 minutes"
                );
                MAX_BENCH_SAMPLES
            }
            Some(n) => n,
            None => 200,
        };
        let report = scan::run(&scan::Options { samples, prefer }).map_err(Fail::Op)?;

        out(format_args!("timres bench"));
        out(format_args!(""));
        out(format_args!(
            "  {:<12}{} - {} ms",
            "range",
            fmt_period(report.range.finest),
            fmt_period(report.range.coarsest)
        ));
        out(format_args!(
            "  {:<12}{} ms",
            "global tick",
            fmt_period(report.range.current)
        ));
        out(format_args!(""));

        let chosen = match report.chosen {
            Some(c) => c,
            None if report.partial => {
                return Err(Fail::Op(
                    "stopped with ESC before any candidate was usable".into(),
                ))
            }
            None if report.points.is_empty() => {
                return Err(Fail::Op(
                    "the platform reports no usable range (finest is above 1 ms)".into(),
                ))
            }
            None => {
                return Err(Fail::Op(
                    "no measured point met the usability thresholds (interference or latency)"
                        .into(),
                ))
            }
        };

        if rows {
            out(format_args!("curve  (every measured period)"));
            out(format_args!(
                "  {:<8} {:>8} {:>8} {:>7}",
                "request", "p50", "p95", "interf"
            ));
            for p in &report.points {
                out(format_args!(
                    "  {:<8} {:>8.3} {:>8.3} {:>6.1}%",
                    fmt_period(p.period_100ns),
                    p.p50_ms,
                    p.p95_ms,
                    p.interference * 100.0,
                ));
            }
        } else {
            out(format_args!("curve  (median p50 per plateau)"));
            for g in stats::plateaus(&report.points) {
                let mark = if (g.lo_100ns..=g.hi_100ns).contains(&chosen.period_100ns) {
                    "  <- recommended"
                } else {
                    ""
                };
                out(format_args!(
                    "  {} - {} ms   p50 {:.3} ms  {:>4} points{}",
                    fmt_period(g.lo_100ns),
                    fmt_period(g.hi_100ns),
                    g.p50_ms,
                    g.points,
                    mark
                ));
            }
        }

        out(format_args!(""));
        out(format_args!("result"));
        let suffix = match (prefer, chosen.within_budget()) {
            (Prefer::Latency, _) => "",
            (Prefer::Power, true) => "  [power]",
            (Prefer::Power, false) => "  [power fallback]",
        };
        out(format_args!(
            "  {:<12}{} ms  ({} x 100ns){}",
            "recommend",
            fmt_period(chosen.period_100ns),
            chosen.period_100ns,
            suffix
        ));
        if let Some(band) = report.band {
            if band.lo_100ns != band.hi_100ns {
                out(format_args!(
                    "  {:<12}{} - {} ms  ({} values, all equivalent)",
                    "band",
                    fmt_period(band.lo_100ns),
                    fmt_period(band.hi_100ns),
                    band.points
                ));
            }
        }
        out(format_args!(
            "  {:<12}min {:.3}   p50 {:.3}   p95 {:.3}   max {:.3} ms",
            "latency", chosen.min_ms, chosen.p50_ms, chosen.p95_ms, chosen.max_ms
        ));
        if let Some(floor) = report
            .points
            .iter()
            .find(|p| p.period_100ns == report.range.finest)
        {
            if floor.period_100ns != chosen.period_100ns && floor.p50_ms > 0.0 {
                let gain = (floor.p50_ms - chosen.p50_ms) / floor.p50_ms * 100.0;
                if gain >= 1.0 {
                    out(format_args!(
                        "  {:<12}p50 {:.3} ms at {} ms  ({:.0}% lower)",
                        "vs floor",
                        floor.p50_ms,
                        fmt_period(floor.period_100ns),
                        gain
                    ));
                }
            }
        }
        let stability = if report.partial {
            "partial (stopped with ESC)"
        } else if report.stable {
            "stable (re-check agreed)"
        } else {
            "unstable (rerun when the machine is quieter)"
        };
        out(format_args!("  {:<12}{}", "stability", stability));
        if report.power_edge {
            out(format_args!(
                "  {:<12}the next tested value is outside the budget; the edge can move between runs",
                "note"
            ));
        }
        out(format_args!(
            "  {:<12}timres apply --resolution {}",
            "next step", chosen.period_100ns
        ));
        Ok(())
    }

    fn cmd_apply(args: &[String]) -> Result<(), Fail> {
        let mut resolution: Option<u32> = None;
        let mut ms_arg: Option<f64> = None;
        let mut persist = false;
        let mut no_console = false;
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--resolution" => {
                    let v = take(args, &mut i, "--resolution")?;
                    resolution = Some(
                        v.parse()
                            .map_err(|_| Fail::Usage(format!("invalid --resolution: {v}")))?,
                    );
                }
                "--ms" => {
                    let v = take(args, &mut i, "--ms")?;
                    ms_arg = Some(
                        v.parse()
                            .map_err(|_| Fail::Usage(format!("invalid --ms: {v}")))?,
                    );
                }
                "--persist" => persist = true,
                "--no-console" => no_console = true,
                a => return Err(Fail::Usage(format!("unknown apply argument: {a}"))),
            }
            i += 1;
        }

        let period = match (resolution, ms_arg) {
            (Some(r), None) => r,
            (None, Some(m)) => (m * 10_000.0).round() as u32,
            _ => {
                return Err(Fail::Usage(
                    "exactly one of --resolution or --ms is required".into(),
                ))
            }
        };

        let range = nt::query().map_err(Fail::Op)?;
        if period < range.finest || period > range.coarsest {
            return Err(Fail::Usage(format!(
                "{period} x 100ns is outside the platform range {}..{}",
                range.finest, range.coarsest
            )));
        }

        if nt::global_requests_enabled() != Some(1) {
            eprintln!("warning: requests are per-process here, so this affects timres.exe only.");
            eprintln!("         run `timres global on` (admin) and reboot for other applications.");
            if persist {
                return Err(Fail::Usage(
                    "refusing --persist: the logon holder would only affect itself. Enable global mode and reboot first.".into(),
                ));
            }
        }

        nt::prepare_sampling();
        nt::set(period).map_err(Fail::Op)?;
        if range.current < period {
            eprintln!(
                "note: another process holds a finer {:.3} ms request; the timer stays at that resolution, so this will not lower power use until it exits.",
                stats::ms(range.current)
            );
        }
        outln!("requesting {} ms", fmt_period(period));
        if persist {
            persist::install_startup(period).map_err(Fail::Op)?;
            // The task owns the request from here, so this process can exit
            // instead of blocking the shell that started it.
            match persist::start_startup() {
                Ok(()) => outln!(
                    "logon task: {} installed and running (edit it in Task Scheduler)",
                    persist::TASK_NAME
                ),
                Err(e) => {
                    eprintln!("warning: task installed but could not start now: {e}");
                    eprintln!("         it will start at the next logon.");
                }
            }
            return Ok(());
        }
        outln!("holding request; press ESC or Ctrl+C to release.");
        // With a console, Ctrl+C terminates the process and the kernel drops the
        // request with it. --no-console keeps the holder windowless, which also
        // removes Ctrl+C, so ESC is polled either way.
        if no_console {
            unsafe {
                FreeConsole();
            }
        }
        let _ = measure::escape_pressed(); // drop a press from before we started
        loop {
            if measure::escape_pressed() {
                outln!("released (ESC)");
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    fn cmd_status() -> Result<(), Fail> {
        let range = nt::query().map_err(Fail::Op)?;
        nt::prepare_sampling();
        let mut v = measure::sample_sleep(1000);
        let s = Stats::from_samples(0, &mut v);
        outln!("effective (this process)");
        outln!(
            "p50={:.3} p95={:.3} interference={:.1}%",
            s.p50_ms,
            s.p95_ms,
            s.interference * 100.0,
        );
        outln!("note: this process made no request; the value above is the ambient tick shared by every process");
        print_query_block(&range);
        Ok(())
    }

    fn cmd_measure(args: &[String]) -> Result<(), Fail> {
        let mut samples = 1000usize;
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--samples" => {
                    let v = take(args, &mut i, "--samples")?;
                    samples = parse_samples(v)?;
                }
                a => return Err(Fail::Usage(format!("unknown measure argument: {a}"))),
            }
            i += 1;
        }

        nt::prepare_sampling();
        let mut v = measure::sample_sleep(samples);
        let s = Stats::from_samples(0, &mut v);
        outln!(
            "p50={:.3} p95={:.3} interference={:.1}%",
            s.p50_ms,
            s.p95_ms,
            s.interference * 100.0,
        );
        Ok(())
    }

    fn cmd_verify_global() -> Result<(), Fail> {
        nt::prepare_sampling();
        let mut v = measure::sample_sleep(500);
        let base = Stats::from_samples(0, &mut v);
        outln!("baseline p50={:.3} ms", base.p50_ms);

        let range = nt::query().map_err(Fail::Op)?;
        nt::set(range.finest).map_err(Fail::Op)?;

        let outcome = child_check(base.p50_ms);
        let release = nt::release().map_err(Fail::Op);
        release?;
        outcome
    }

    fn child_check(baseline: f64) -> Result<(), Fail> {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        let exe = std::env::current_exe().map_err(|e| Fail::Op(format!("current_exe: {e}")))?;
        let out = std::process::Command::new(exe)
            .args(["measure", "--samples", "200"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| Fail::Op(format!("spawn measure: {e}")))?;
        let text = String::from_utf8_lossy(&out.stdout);
        let child = text
            .split_whitespace()
            .find_map(|w| w.strip_prefix("p50="))
            .and_then(|v| v.parse::<f64>().ok())
            .ok_or_else(|| Fail::Op(format!("could not parse child output: {}", text.trim())))?;

        match stats::classify_global(baseline, child) {
            Verdict::Global => outln!("global: yes - a child process saw the finer tick"),
            Verdict::PerProcess => {
                outln!(
                    "global: NO - requests are per-process on this machine (child p50={:.3}, baseline={:.3})",
                    child, baseline,
                );
                outln!("enabling requires GlobalTimerResolutionRequests=1 and a reboot.");
            }
            Verdict::Inconclusive => {
                outln!(
                    "global: inconclusive - the baseline is already {:.3} ms, so something else holds a fine request.",
                    baseline,
                );
                outln!("close it and rerun to get a real answer.");
            }
        }
        Ok(())
    }

    fn cmd_global(args: &[String]) -> Result<(), Fail> {
        let on = match args {
            [a] if a == "on" => true,
            [a] if a == "off" => false,
            _ => return Err(Fail::Usage("expected: timres global on|off".into())),
        };
        nt::set_global_requests(on).map_err(Fail::Op)?;
        outln!(
            "GlobalTimerResolutionRequests = {}. Reboot for it to take effect.",
            u8::from(on)
        );
        if on {
            outln!("The flag is read at boot, so `apply --persist` is only safe after the reboot and a passing `timres verify-global`.");
        }
        Ok(())
    }

    fn cmd_startup(args: &[String]) -> Result<(), Fail> {
        match args.first().map(String::as_str) {
            Some("remove") if args.len() == 1 => {
                let removed = persist::remove_startup().map_err(Fail::Op)?;
                outln!(
                    "logon task: {}",
                    if removed { "removed" } else { "not installed" }
                );
                if removed {
                    outln!("note: a holder started at logon keeps running until you log out.");
                }
                Ok(())
            }
            Some("remove") => Err(Fail::Usage("startup remove takes no arguments".into())),
            _ => Err(Fail::Usage("expected: timres startup remove".into())),
        }
    }
}

#[cfg(windows)]
fn main() {
    std::process::exit(cli::run());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("timres only runs on Windows. `cargo test` exercises the pure logic.");
    std::process::exit(1);
}
