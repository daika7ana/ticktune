#![cfg_attr(not(windows), allow(dead_code))]

mod stats;

#[cfg(windows)]
mod measure;
#[cfg(windows)]
mod nt;
#[cfg(windows)]
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

    const USAGE: &str = "usage: timres query | bench [--samples N] [--prefer latency|power] | \
apply (--resolution <100ns> | --ms <f64>) [--persist] [--no-console] | status | \
measure [--samples N] | verify-global | global on|off | startup remove";

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

    /// Short-lived measurement paths: no power throttling, highest priority.
    fn prepare_sampling() {
        nt::ignore_timer_resolution_throttling();
        nt::set_thread_priority(15);
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
        let cmd = match args.first() {
            Some(c) => c.as_str(),
            None => return Err(Fail::Usage("no subcommand".into())),
        };
        let rest = &args[1..];
        match cmd {
            "query" => cmd_query(),
            "bench" => cmd_bench(rest),
            "apply" => cmd_apply(rest),
            "status" => cmd_status(),
            "measure" => cmd_measure(rest),
            "verify-global" => cmd_verify_global(),
            "global" => cmd_global(rest),
            "startup" => cmd_startup(rest),
            "help" | "--help" | "-h" => {
                println!("{USAGE}");
                Ok(())
            }
            other => Err(Fail::Usage(format!("unknown subcommand: {other}"))),
        }
    }

    fn cmd_query() -> Result<(), Fail> {
        let range = nt::query().map_err(Fail::Op)?;
        print_query_block(&range)
    }

    fn print_query_block(range: &nt::Range) -> Result<(), Fail> {
        println!(
            "{:<20}{} ms  ({} x 100ns)",
            "finest",
            fmt_period(range.finest),
            range.finest
        );
        println!(
            "{:<20}{} ms  ({} x 100ns)",
            "coarsest",
            fmt_period(range.coarsest),
            range.coarsest
        );
        println!("{:<20}{} ms", "global tick", fmt_period(range.current));
        let mode = match nt::global_requests_enabled() {
            Some(1) => "global (GlobalTimerResolutionRequests=1)",
            Some(0) => "per-process (GlobalTimerResolutionRequests=0)",
            _ => "per-process",
        };
        println!("{:<20}{}", "mode", mode);
        match nt::dynamic_tick_disabled() {
            Some(true) => {
                println!("{:<20}disabled (! risk)", "dynamic tick");
                println!("{:<20}revert: bcdedit /deletevalue disabledynamictick", "");
            }
            Some(false) => println!("{:<20}enabled", "dynamic tick"),
            // bcdedit omits the line when it was never set, which is the default.
            None if nt::is_elevated() => {
                println!("{:<20}unset (default: enabled)", "dynamic tick")
            }
            None => println!("{:<20}unknown (run elevated to read)", "dynamic tick"),
        }
        let startup = match persist::startup_period() {
            Some(p) => {
                let path = persist::startup_script_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|e| e);
                format!("{} ms ({})", fmt_period(p), path)
            }
            None => "none".to_string(),
        };
        println!("{:<20}{}", "startup", startup);
        println!(
            "{:<20}{}",
            "elevated",
            if nt::is_elevated() { "yes" } else { "no" }
        );
        Ok(())
    }

    fn cmd_bench(args: &[String]) -> Result<(), Fail> {
        let mut samples: Option<usize> = None;
        let mut prefer = Prefer::Latency;
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--samples" => {
                    let v = take(args, &mut i, "--samples")?;
                    samples = Some(parse_samples(v)?);
                }
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

        let (coarse, fine) = match samples {
            Some(n) => (n.min(1000), n),
            None => (200, 200),
        };
        let report = scan::run(&scan::Options {
            samples_coarse: coarse,
            samples_fine: fine,
            prefer,
        })
        .map_err(Fail::Op)?;

        println!(
            "range: {} - {} ms, global tick {} ms",
            fmt_period(report.range.finest),
            fmt_period(report.range.coarsest),
            fmt_period(report.range.current),
        );
        println!("{:<8} {:>8} {:>8} {:>7}", "request", "p50", "p95", "interf");
        // The dense pass re-measures coarse points; print each request once,
        // keeping the later (more sampled) row.
        let mut seen = std::collections::HashSet::new();
        let rows: Vec<_> = report
            .points
            .iter()
            .rev()
            .filter(|p| seen.insert(p.period_100ns))
            .collect();
        for p in rows.into_iter().rev() {
            println!(
                "{:<8} {:>8.3} {:>8.3} {:>6.1}%",
                fmt_period(p.period_100ns),
                p.p50_ms,
                p.p95_ms,
                p.interference * 100.0,
            );
        }

        let chosen = match report.chosen {
            Some(c) => c,
            None => {
                return Err(Fail::Op(
                    "no candidate was usable (interference too high)".into(),
                ))
            }
        };
        let label = match (prefer, chosen.within_budget()) {
            (Prefer::Power, true) => "recommend (power):",
            (Prefer::Power, false) => "recommend (power, latency fallback):",
            (Prefer::Latency, _) => "recommend:",
        };
        let stability = if report.stable {
            "[stable]"
        } else {
            "[unstable: rerun]"
        };
        println!(
            "{} {} ms  ({} x 100ns)  min={:.3} p50={:.3} p95={:.3} max={:.3} {}",
            label,
            fmt_period(chosen.period_100ns),
            chosen.period_100ns,
            chosen.min_ms,
            chosen.p50_ms,
            chosen.p95_ms,
            chosen.max_ms,
            stability,
        );
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

        prepare_sampling();
        nt::set(period).map_err(Fail::Op)?;
        println!("requesting {} ms", fmt_period(period));
        if persist {
            let path = persist::install_startup(period).map_err(Fail::Op)?;
            println!("startup: {}", path.display());
        }
        println!("holding request; close this process to release.");
        // Detach only after all output: printing to a freed console panics.
        if no_console {
            unsafe {
                FreeConsole();
            }
        }
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }

    fn cmd_status() -> Result<(), Fail> {
        let range = nt::query().map_err(Fail::Op)?;
        prepare_sampling();
        let mut v = measure::sample_sleep(1000);
        let s = Stats::from_samples(range.current, &mut v);
        println!("effective (this process)");
        println!(
            "p50={:.3} p95={:.3} interference={:.1}%",
            s.p50_ms,
            s.p95_ms,
            s.interference * 100.0,
        );
        if nt::global_requests_enabled() == Some(1) {
            println!("note: global timer requests are enabled; this value also applies to other processes");
        }
        print_query_block(&range)
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

        let range = nt::query().map_err(Fail::Op)?;
        prepare_sampling();
        let mut v = measure::sample_sleep(samples);
        let s = Stats::from_samples(range.current, &mut v);
        println!(
            "p50={:.3} p95={:.3} interference={:.1}%",
            s.p50_ms,
            s.p95_ms,
            s.interference * 100.0,
        );
        Ok(())
    }

    fn cmd_verify_global() -> Result<(), Fail> {
        prepare_sampling();
        let mut v = measure::sample_sleep(500);
        let base = Stats::from_samples(0, &mut v);
        println!("baseline p50={:.3} ms", base.p50_ms);

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
            Verdict::Global => println!("global: yes - a child process saw the finer tick"),
            Verdict::PerProcess => {
                println!(
                    "global: NO - requests are per-process on this machine (child p50={:.3}, baseline={:.3})",
                    child, baseline,
                );
                println!("enabling requires GlobalTimerResolutionRequests=1 and a reboot.");
            }
            Verdict::Inconclusive => {
                println!(
                    "global: inconclusive - the baseline is already {:.3} ms, so something else holds a fine request.",
                    baseline,
                );
                println!("close it and rerun to get a real answer.");
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
        println!(
            "GlobalTimerResolutionRequests = {}. Reboot for it to take effect.",
            u8::from(on)
        );
        Ok(())
    }

    fn cmd_startup(args: &[String]) -> Result<(), Fail> {
        match args.first().map(String::as_str) {
            Some("remove") if args.len() == 1 => {
                let removed = persist::remove_startup().map_err(Fail::Op)?;
                println!(
                    "startup: {}",
                    if removed { "removed" } else { "not installed" }
                );
                if removed {
                    println!("note: a holder started at logon keeps running until you log out.");
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
}
