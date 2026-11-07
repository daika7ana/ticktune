# timres

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Platform: Windows x64](https://img.shields.io/badge/platform-Windows%20x64-0078D6)](https://github.com/daika7ana/win-timer-resolution)
[![Dependencies: none](https://img.shields.io/badge/dependencies-none-brightgreen)](Cargo.toml)
[![Rust 1.75+](https://img.shields.io/badge/rust-1.75%2B-orange)](https://www.rust-lang.org)

Measure the timer resolution Windows actually delivers, then request the best one.

Windows reports a nominal range, but the latency a process really sees is quantised
and not monotonic: the nominal minimum is often slower than a slightly coarser
request. `timres` sweeps the usable range, measures real `Sleep(1)` latency at each
step, and recommends the point with the lowest consistent latency — or the coarsest
point that stays within a latency budget.

Single static executable. No dependencies, no driver, no service.

## ⚡ Features

- **Measures, does not assume.** Every candidate is timed with `Sleep(1)` on
  `QueryPerformanceCounter`, and the scan reports the distribution, not one number.
- **Finds the real knee.** A 100 ns sweep around the coarse winner catches the
  narrow fast band that a coarser step can walk straight past.
- **Latency or power.** `--prefer latency` minimises p50/p95; `--prefer power` takes
  the coarsest period that still meets the latency budget.
- **Two modes.** Per-process by default (no admin needed); machine-wide only after
  an explicit `timres global on` and a reboot.
- **Nothing hidden.** The registry key and the dynamic-tick setting are reported,
  never toggled behind your back.

## 🧭 Step by step: getting the best result

### 1. Look at what Windows reports

```sh
timres query
```

This prints the nominal range, the current global tick, the request mode and the
dynamic-tick state. It changes nothing and needs no admin rights.

### 2. Decide what you want to affect

- **Per-process (default).** A timer request belongs to the process that makes it
  and disappears when that process exits. `timres apply` therefore only helps
  `timres.exe` and anything it launches — not an already-running game or browser.
- **Machine-wide.** After `timres global on` (elevated) and a reboot, every process
  benefits, and a holder kept running in the background is useful. Confirm it with
  `timres verify-global` before relying on it.

```sh
# elevated terminal
timres global on
# reboot, then
timres verify-global
```

### 3. Quiet the machine

Close browsers, launchers, overlays, monitoring and RGB utilities. Some of them hold
their own timer requests, which can prop up the timer for every other process. Plug
in a laptop, keep the power plan fixed between runs, and avoid benchmarking under
load. For unattended runs, Windows can also demote the app to "Medium" QoS after a
period without input — see the **Before you benchmark** note below.

### 4. Run the scan

```sh
timres bench
```

The scan takes roughly 40 seconds. It prints one row per tested period
(p50, p95, max, interference) and a final recommendation. `[stable]` means a re-check
of the winner agreed with the first pass; `[unstable]` means the machine was noisy —
rerun when it is quieter. For a tighter estimate use `--samples 1000` (slower), and
`--prefer power` if you would rather have the coarsest period inside the latency
budget.

### 5. Try the value

```sh
timres apply --resolution 5120
```

Keep the window open. In another terminal, `timres measure --samples 1000` shows what
the system delivers while the request is held. Close the window to release it — no
cleanup, no leftover state.

### 6. Keep it across reboots (optional, machine-wide only)

```sh
timres apply --resolution 5120 --persist --no-console
```

This writes a small `.cmd` to the Startup folder that starts the holder at logon.
It is refused unless `GlobalTimerResolutionRequests` is `1`, because in per-process
mode the holder would only accelerate itself. Check it after the next logon with
`timres status`.

### 7. Undo everything

```sh
timres startup remove    # delete the logon holder
timres global off        # elevated; takes effect after a reboot
```

Close any holder that is still running. No other state is left behind.

## 📋 Commands

| Command | What it does |
|---|---|
| `timres query` | Print the nominal range, global tick, mode and dynamic-tick state |
| `timres bench [--samples N] [--prefer latency\|power]` | Scan and recommend |
| `timres apply --resolution <100ns>` | Hold a period for this process |
| `timres apply --ms <float>` | Same, in milliseconds |
| `timres status` | What this process really gets, next to what Windows reports |
| `timres measure [--samples N]` | Sample only; sets nothing (cross-process probe) |
| `timres verify-global` | Check whether global mode actually took effect |
| `timres global on\|off` | Write the registry key (elevated, reboot required) |
| `timres startup remove` | Delete the logon holder |

Values are in 100 ns units; the output always shows milliseconds too.

## 🤔 Why this is not "just set 0.5 ms"

Since Windows 10 2004 a timer-resolution request is **per-process**: it applies to
the process that made it and dies with it, unless
`HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\kernel\GlobalTimerResolutionRequests`
is `1` (read at boot, needs a reboot).

`Sleep(1)` granularity is also **quantised and non-monotonic**. A 0.500 ms request
typically sleeps ~1.50 ms, while a 0.512 ms request sleeps ~1.02 ms. Finer is not
better; the only way to know is to measure.

The knee moved between builds: 5023 (100 ns units) on 26100, 5018 on 26200/26300 —
where `0.5000 ms` is actually on the slow three-tick side, and the fast window is
under 1 µs wide. The scan therefore walks that band one 100 ns unit at a time
instead of trusting a hardcoded value.

On some systems only a narrow band is honoured: a request a few microseconds above
it silently falls back to the 0.5 ms tick, i.e. the slow three-tick case again. That
is why `bench` prints the whole curve rather than a single number.

## ⚠️ Before you benchmark

Windows 11 (2025+) can quietly drop a foreground app to "Medium" QoS after a period
with no input, which skews an unattended run — Microsoft's own docs warn about it.
Set `DisableUserPresenceQos` (REG_DWORD `1`) under
`HKLM\SYSTEM\CurrentControlSet\Control\Power\PowerThrottling` before benchmarking.
`timres` never writes it.

## 🛡️ What it deliberately does not do

- **No `bcdedit`.** `disabledynamictick` is documented as a debugging switch and is
  widely reported to make machines unusable (minutes per UI action), and it needs a
  reboot to undo. `status` reports it, with the revert command; nothing writes it.
- **No automatic registry writes.** `GlobalTimerResolutionRequests` is reported by
  `query`/`status` and only changed by an explicit `timres global on|off` (which
  refuses to run unelevated).
- **No hardcoded "best" value.** The result is per machine, BIOS, power plan and
  driver set. Run `bench` again after a platform change.

## 🔨 Build

Cross-compiles from Linux; no MSVC, no runtime DLLs.

```sh
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
# target/x86_64-pc-windows-gnu/release/timres.exe
```

## 📁 Layout

| file | role |
|---|---|
| `src/stats.rs` | pure statistics, grids, recommendation — unit-tested on any OS |
| `src/measure.rs` | `Sleep(1)` sampler on QPC |
| `src/scan.rs` | coarse → fine → stability re-check |
| `src/nt.rs` | `NtSetTimerResolution` / `NtQueryTimerResolution`, registry, BCD probe |
| `src/persist.rs` | logon `.cmd` in the Startup folder |
| `src/main.rs` | CLI |

See `PLAN.md` for the measurement model and the evidence behind it.

## License

MIT — see [LICENSE](LICENSE).
