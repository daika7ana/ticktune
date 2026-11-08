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

    /// One line for errors; `--help` prints the full table.
    const USAGE: &str = "usage: ticktune <command> [options] (run `ticktune --help`)";

    const HELP: &str = "\
usage: ticktune <command> [options]

commands:
  query                     show the timer range, mode, and current state
  bench                     measure the timer curve and recommend a period
  apply                     hold one timer request until ESC or Ctrl+C
                            (with --persist it installs the holder and returns)
  status                    sample this process, then print the query block
  measure                   print p50/p95/interference for this process
  verify-global             check whether requests affect other processes
  global on|off             set GlobalTimerResolutionRequests (admin, reboot)
  startup remove            delete the logon task
  help                      show this help

options:
  -h, --help                show this help; works after any command
  --resolution <100ns>      apply: period in 100 ns units
  --ms <f64>                apply: period in milliseconds
  --persist                 apply: install and start the logon holder
  --no-console              apply: detach the console (no Ctrl+C)
  --no-esc                  apply: internal, used by the generated logon task
  --from-task               apply: internal, marks the generated logon task
  --samples N               bench: per point (default 200, max 1000)
                            measure: total (default 1000, max 100000)
  --prefer latency|power    bench: optimize for latency (default) or power
  --rows                    bench: print every measured period, not plateaus

notes:
  apply needs exactly one of --resolution or --ms, within the platform range.
  apply --persist requires GlobalTimerResolutionRequests=1 (see verify-global).
  bench stops early on ESC; --samples 1000 takes about 14 minutes.
  startup remove ends a running holder, and warns if the tick stays raised.";

    /// Samples per point in a bench run. 546 grid points plus the re-check at
    /// 1000 samples is ~14 minutes; a larger number would look like a hang.
    const MAX_BENCH_SAMPLES: usize = 1000;

    /// stdout may be a closed pipe (`ticktune bench | head`); a failed write must
    /// not abort the scan.
    fn out(args: std::fmt::Arguments<'_>) {
        use std::io::Write;
        let _ = writeln!(std::io::stdout(), "{args}");
    }

    /// `println!` for stdout that may be a closed pipe.
    macro_rules! outln {
        ($($t:tt)*) => { out(format_args!($($t)*)) };
    }

    /// stderr may be closed by a wrapper; a failed write must not abort a run.
    pub(crate) fn err(args: std::fmt::Arguments<'_>) {
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "{args}");
    }

    /// `eprintln!` for stderr that may be closed.
    macro_rules! errln {
        ($($t:tt)*) => { err(format_args!($($t)*)) };
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
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
                errln!("error: {m}");
                errln!("{USAGE}");
                2
            }
            Err(Fail::Op(m)) => {
                errln!("error: {m}");
                1
            }
        }
    }

    fn dispatch() -> Result<(), Fail> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        // `--help` works in any position, including after a subcommand.
        if args.iter().any(|a| a == "--help" || a == "-h") {
            outln!("{HELP}");
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
                outln!("{HELP}");
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
            // No marker: not ours to manage. Both `--persist` and `startup
            // remove` refuse it, so pointing at them would be a dead end.
            Some(task) if !task.owned => format!(
                "{} task: present but not created by this build (delete it by hand)",
                persist::TASK_NAME
            ),
            Some(task) if task.stale => format!(
                "{} task: {} x 100ns (stale: re-run apply --persist)",
                persist::TASK_NAME,
                task.period_100ns
            ),
            Some(task) => format!("{} task: {} x 100ns", persist::TASK_NAME, task.period_100ns),
            None if persist::task_exists() => "present (arguments unreadable)".to_string(),
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
                            )));
                        }
                    };
                }
                a => return Err(Fail::Usage(format!("unknown bench argument: {a}"))),
            }
            i += 1;
        }

        let samples = match samples {
            Some(n) if n > MAX_BENCH_SAMPLES => {
                errln!(
                    "note: bench caps --samples at {MAX_BENCH_SAMPLES} so a run stays under ~14 minutes"
                );
                MAX_BENCH_SAMPLES
            }
            Some(n) => n,
            None => 200,
        };
        let report = scan::run(&scan::Options { samples, prefer }).map_err(Fail::Op)?;

        out(format_args!("ticktune bench"));
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
                ));
            }
            None if report.points.is_empty() => {
                return Err(Fail::Op(
                    "the platform reports no usable range (finest is above 1 ms)".into(),
                ));
            }
            None => {
                return Err(Fail::Op(
                    "no measured point met the usability thresholds (interference or latency)"
                        .into(),
                ));
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
        out(format_args!(
            "  {:<12}ticktune apply --resolution {}",
            "next step", chosen.period_100ns
        ));
        Ok(())
    }

    fn cmd_apply(args: &[String]) -> Result<(), Fail> {
        let mut resolution: Option<u32> = None;
        let mut ms_arg: Option<f64> = None;
        let mut persist = false;
        let mut no_console = false;
        let mut esc = true;
        let mut from_task = false;
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
                // Internal: the generated logon task passes both of these.
                "--no-esc" => esc = false,
                "--from-task" => from_task = true,
                a => return Err(Fail::Usage(format!("unknown apply argument: {a}"))),
            }
            i += 1;
        }

        // A hard error here would make holders installed by an older build exit
        // with code 2 at the next logon, silently.
        if !esc && !from_task {
            errln!("warning: --no-esc is internal and only meant for the generated logon task");
        }

        let period = match (resolution, ms_arg) {
            (Some(r), None) => r,
            (None, Some(m)) => {
                if !m.is_finite() || m <= 0.0 {
                    return Err(Fail::Usage(format!("--ms must be a positive number: {m}")));
                }
                stats::period_from_ms(m)
            }
            _ => {
                return Err(Fail::Usage(
                    "exactly one of --resolution or --ms is required".into(),
                ));
            }
        };
        if period == 0 {
            return Err(Fail::Usage("the period must be at least 1 x 100ns".into()));
        }

        let range = nt::query().map_err(Fail::Op)?;
        if period < range.finest || period > range.coarsest {
            return Err(Fail::Usage(format!(
                "{period} x 100ns is outside the platform range {}..{}",
                range.finest, range.coarsest
            )));
        }

        if nt::global_requests_enabled() != Some(1) {
            errln!("warning: requests are per-process here, so this affects ticktune.exe only.");
            errln!("         run `ticktune global on` (admin) and reboot for other applications.");
            if persist {
                return Err(Fail::Op(
                    "refusing --persist: the logon holder would only affect itself. Enable global mode and reboot first.".into(),
                ));
            }
        }

        nt::ignore_timer_resolution_throttling();
        nt::set(period).map_err(Fail::Op)?;
        if range.current < period {
            errln!(
                "note: another process holds a finer {:.3} ms request; the timer stays at that resolution, so this will not lower power use until it exits.",
                stats::ms(range.current)
            );
        }
        outln!("requesting {} ms", fmt_period(period));
        // `nt::set` reports success even when the platform silently falls back
        // to a coarser tick, so show what this process actually sleeps. Plain
        // samples only: no `prepare_sampling`, which raises this thread for the
        // whole process lifetime and would make the holder cost more.
        let observed = Stats::from_samples(period, &mut measure::sample_sleep(50));
        outln!("observed     p50 {:.3} ms", observed.p50_ms);
        if persist {
            persist::install_startup(period).map_err(Fail::Op)?;
            // `/Run` returning success does not prove the holder is alive, so
            // hand our own request over and wait for the tick to move.
            let _ = nt::release();
            match persist::start_startup() {
                Ok(()) => {}
                Err(e) => {
                    errln!("warning: task installed but could not start now: {e}");
                    errln!("         check `ticktune query` before relying on it.");
                    return Ok(());
                }
            }
            // Another process already holding a finer request looks the same as
            // our holder starting; a dead holder on a quiet machine does not.
            let mut held = false;
            for _ in 0..40 {
                if nt::query().is_ok_and(|r| r.current <= period) {
                    held = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            if held {
                outln!(
                    "logon task: {} installed and running (edit it in Task Scheduler)",
                    persist::TASK_NAME
                );
            } else {
                errln!(
                    "warning: task {} installed but no holder request appeared within 2 s.",
                    persist::TASK_NAME
                );
                errln!(
                    "         check `ticktune query`; a non-admin S4U task needs the \"Log on as a batch job\" right."
                );
            }
            return Ok(());
        }
        if esc {
            outln!("holding request; press ESC or Ctrl+C to release.");
        } else {
            outln!("holding request; close this process to release.");
        }
        // With a console, Ctrl+C terminates the process and the kernel drops the
        // request with it. --no-console keeps the holder windowless, which also
        // removes Ctrl+C, so ESC is polled. --no-esc is for the logon holder:
        // GetAsyncKeyState is desktop-global, so any ESC would end it.
        if no_console {
            unsafe {
                FreeConsole();
            }
        }
        if esc {
            let _ = measure::escape_pressed(); // drop a press from before we started
            loop {
                if measure::escape_pressed() {
                    outln!("released (ESC)");
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }

    /// Prepare, sample once, and print the effective distribution.
    fn sample_and_print(samples: usize) {
        nt::prepare_sampling();
        let mut v = measure::sample_sleep(samples);
        let s = Stats::from_samples(0, &mut v);
        outln!(
            "{}",
            stats::summary_line(s.p50_ms, s.p95_ms, s.interference)
        );
    }

    fn cmd_status() -> Result<(), Fail> {
        let range = nt::query().map_err(Fail::Op)?;
        outln!("effective (this process)");
        sample_and_print(1000);
        outln!(
            "note: this process made no request; the value above is the ambient tick shared by every process"
        );
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

        sample_and_print(samples);
        Ok(())
    }

    fn cmd_verify_global() -> Result<(), Fail> {
        // This command raises the finest request and spawns a sampler, so it
        // must not run beside a bench: either would corrupt the other's curve.
        let _guard = nt::bench_instance().map_err(Fail::Op)?;
        let range = nt::query().map_err(Fail::Op)?;
        if range.current < range.coarsest {
            // The test needs the no-request timer. Anything already holding a
            // finer request - including the logon holder - makes a child's
            // sleep unattributable, and a Sleep(1) baseline can land on either
            // side of a fixed cutoff.
            outln!(
                "global: inconclusive - the tick is already {} ms, so something holds a fine request.",
                fmt_period(range.current)
            );
            if persist::logon_task().is_some_and(|t| t.period_100ns == range.current) {
                outln!(
                    "the logon task holds it: run `ticktune startup remove`, verify, then `apply --persist` again."
                );
            } else {
                outln!(
                    "close it and rerun to get a real answer; `powercfg /energy` (elevated) names the process holding it."
                );
            }
            return Ok(());
        }

        nt::prepare_sampling();
        let mut v = measure::sample_sleep(500);
        let base = Stats::from_samples(0, &mut v);
        outln!("baseline p50={:.3} ms", base.p50_ms);

        nt::set(range.finest).map_err(Fail::Op)?;

        let outcome = child_check(base.p50_ms, stats::ms(range.coarsest));
        let release = nt::release().map_err(Fail::Op);
        release?;
        outcome
    }

    fn child_check(baseline: f64, coarsest: f64) -> Result<(), Fail> {
        use std::os::windows::process::CommandExt;

        let exe = std::env::current_exe().map_err(|e| Fail::Op(format!("current_exe: {e}")))?;
        let out = std::process::Command::new(exe)
            .args(["measure", "--samples", "200"])
            .creation_flags(nt::CREATE_NO_WINDOW)
            .output()
            .map_err(|e| Fail::Op(format!("spawn measure: {e}")))?;
        let text = String::from_utf8_lossy(&out.stdout);
        let child = stats::parse_p50(&text)
            .ok_or_else(|| Fail::Op(format!("could not parse child output: {}", text.trim())))?;

        match stats::classify_global(baseline, child, coarsest) {
            Verdict::Global => outln!("global: yes - a child process saw the finer tick"),
            Verdict::PerProcess => {
                outln!(
                    "global: NO - requests are per-process on this machine (child p50={:.3}, baseline={:.3})",
                    child,
                    baseline,
                );
                if nt::global_requests_enabled() == Some(1) {
                    outln!("the registry flag is set but not in effect; reboot to apply it.");
                } else {
                    outln!("enabling requires GlobalTimerResolutionRequests=1 and a reboot.");
                }
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
            _ => return Err(Fail::Usage("expected: ticktune global on|off".into())),
        };
        nt::set_global_requests(on).map_err(Fail::Op)?;
        outln!(
            "GlobalTimerResolutionRequests = {}. Reboot for it to take effect.",
            u8::from(on)
        );
        if on {
            outln!(
                "The flag is read at boot, so `apply --persist` is only safe after the reboot and a passing `ticktune verify-global`."
            );
        }
        Ok(())
    }

    fn cmd_startup(args: &[String]) -> Result<(), Fail> {
        match args.first().map(String::as_str) {
            Some("remove") if args.len() == 1 => {
                // Deleting the task is not proof the holder stopped: `/End` can
                // fail silently, and an S4U holder outlives the session.
                let removed = persist::remove_startup().map_err(Fail::Op)?;
                outln!(
                    "logon task: {}",
                    if removed.is_some() {
                        "removed"
                    } else {
                        "not installed"
                    }
                );
                if let (Some(task), Ok(range)) = (removed, nt::query()) {
                    if range.current <= task.period_100ns {
                        errln!(
                            "warning: the tick is still {:.3} ms; a holder that was running keeps its request until reboot",
                            stats::ms(range.current)
                        );
                    }
                }
                Ok(())
            }
            Some("remove") => Err(Fail::Usage("startup remove takes no arguments".into())),
            _ => Err(Fail::Usage("expected: ticktune startup remove".into())),
        }
    }
}

#[cfg(windows)]
fn main() {
    std::process::exit(cli::run());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("ticktune only runs on Windows. `cargo test` exercises the pure logic.");
    std::process::exit(1);
}
