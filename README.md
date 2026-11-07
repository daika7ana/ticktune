# timres

Measure the Windows timer resolution your machine actually delivers, then request
the best one. A rebuild of the manual procedure behind *Better Timer Resolution*,
with the measurement done properly and the risky parts left out.

## Why this is not "just set 0.5 ms"

Since Windows 10 2004 a timer-resolution request is **per-process**: it applies to
the process that made it and dies with it, unless
`HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\kernel\GlobalTimerResolutionRequests`
is `1` (read at boot, needs a reboot).

`Sleep(1)` granularity is also **quantised and non-monotonic**. A 0.500 ms request
typically sleeps ~1.50 ms, while a 0.512 ms request sleeps ~1.02 ms. Finer is not
better; the only way to know is to measure.

## Build

Cross-compiles from Linux; no MSVC, no runtime DLLs.

```sh
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
# target/x86_64-pc-windows-gnu/release/timres.exe
```

## Use

```sh
timres query                 # range, global tick, registry + dynamic-tick state
timres bench                 # ~1 minute scan, prints the curve and a recommendation
timres bench --prefer power  # coarsest period that still lands in the latency budget
timres apply --resolution 5120            # hold 0.512 ms for this process
timres apply --resolution 5120 --persist  # ... and again at every logon
timres apply --ms 0.512 --no-console      # no console window
timres status                # what this process really gets, vs what Windows reports
timres measure --samples 1000 # sample only, set nothing (cross-process probe)
timres verify-global         # does GlobalTimerResolutionRequests actually work here?
timres global on             # write GlobalTimerResolutionRequests=1 (admin, reboot)
timres startup remove        # undo --persist
```

`apply` blocks; close it (or kill it) and the request is gone. That is the whole
reversibility story: nothing is written outside your own process unless you pass
`--persist` or `timres` is asked to change the registry.

## What it deliberately does not do

- **No `bcdedit`.** `disabledynamictick yes` has made real machines unusable
  (minutes per UI action) and needs a reboot to undo. `status` reports it, with
  the revert command; nothing writes it.
- **No automatic registry writes.** `GlobalTimerResolutionRequests` is reported
  by `query`/`status` and only changed by an explicit `timres global on|off`
  (which refuses to run unelevated).
- **No hardcoded "best" value.** The result is per machine, BIOS, power plan and
  driver set. Run `bench` again after a platform change.

## Layout

| file | role |
|---|---|
| `src/stats.rs` | pure statistics, grids, recommendation — unit-tested on any OS |
| `src/measure.rs` | `Sleep(1)` sampler on QPC |
| `src/scan.rs` | coarse → fine → stability re-check |
| `src/nt.rs` | `NtSetTimerResolution` / `NtQueryTimerResolution`, registry, BCD probe |
| `src/persist.rs` | logon `.cmd` in the Startup folder |
| `src/main.rs` | CLI |

See `PLAN.md` for the measurement model and the evidence behind it.
