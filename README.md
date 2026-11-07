# timres

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Platform: Windows x64](https://img.shields.io/badge/platform-Windows%20x64-0078D6)](https://github.com/daika7ana/win-timer-resolution)
[![Dependencies: none](https://img.shields.io/badge/dependencies-none-brightgreen)](Cargo.toml)
[![Rust 1.85+](https://img.shields.io/badge/rust-1.85%2B-orange)](https://www.rust-lang.org)
[![CI](https://github.com/daika7ana/win-timer-resolution/actions/workflows/ci.yml/badge.svg)](https://github.com/daika7ana/win-timer-resolution/actions/workflows/ci.yml)

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
- **Finds the real knee.** A fixed 100 ns sweep over the first 50 µs above the
  floor catches the narrow fast band that a coarse step can walk straight past.
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

This prints the finest and coarsest periods, the current global tick, the request
mode, the dynamic-tick state, the installed logon task and whether you are
elevated. It changes nothing; without elevation the dynamic-tick line reads
`unknown`, because reading it uses `bcdedit`.

### 2. Decide what you want to affect

- **Per-process (default).** A timer request belongs to the process that makes it
  and disappears when that process exits. It is **not inherited**: `timres apply`
  therefore only affects `timres.exe` itself — not a child process, and not an
  already-running game or browser.
- **Machine-wide.** After `timres global on` (elevated) and a reboot, a request
  affects every process — but only while some process holds one. `global on` alone
  changes nothing, so keep a holder running: the logon task from step 6, or a
  foreground `timres apply`. Confirm it with `timres verify-global` before relying
  on it.

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

The scan takes two to three minutes and prints a report like this example (the
plateau boundaries and point counts vary by machine):

```
timres bench

  range       0.5000 - 15.6250 ms
  global tick 1.0000 ms

curve  (median p50 per plateau)
  0.5000 - 0.5105 ms   p50 1.505 ms   106 points
  0.5106 - 0.5132 ms   p50 1.019 ms    27 points  <- recommended
  0.5133 - 1.0000 ms   p50 1.518 ms   413 points

result
  recommend   0.5119 ms  (5119 x 100ns)
  band        0.5106 - 0.5132 ms  (27 values, all equivalent)
  latency     min 1.008   p50 1.017   p95 1.517   max 1.525 ms
  vs floor    p50 1.505 ms at 0.5000 ms  (32% lower)
  stability   stable (re-check agreed)
  next step   timres apply --resolution 5119
```

The curve is collapsed into plateaus because 500+ identical rows are noise. Add
`--rows` to see every measured period. When several neighbouring periods measure
the same, the tool reports that band and recommends its middle: the edges of a
plateau are marginal by definition, so repeated runs agree on the middle and only
the edge rows wobble. `stable` means a re-check of the winner agreed with the first
pass; `unstable` means the machine was noisy — rerun when it is quieter.
`--samples 100` gives a quicker, noisier answer; the default 200 is two to three
minutes, and the cap of 1000 is about thirteen. Press `ESC` to stop early — the
recommendation then covers the points measured so far. Use `--prefer power` if you
would rather have the coarsest period inside the latency budget (p50 ≤ 1.35 ms and
p95 ≤ 2.0 ms); it notes when the pick sits on the edge of the budget, and falls
back to the latency pick if nothing qualifies. No point is ever recommended unless
interference ≤ 2 %, p50 ≥ 0.99 ms and p95 ≤ 2.5 ms.

`interference` is the share of samples that overshoot the median by more than 1 ms.
The plateau view omits it; `--rows` prints it as a percentage. The scan runs at
time-critical priority, so a normal application may see somewhat more interference
than this figure.

### 5. Try the value

```sh
timres apply --resolution 5120
```

Keep the window open. The request belongs to this process alone, so `timres measure`
in another terminal does not see it; to observe the effect system-wide, enable
global mode (§2) and use `timres verify-global`. Press ESC or Ctrl+C (or close the
window) to release it — no cleanup, no leftover state. With `--no-console` there is
no Ctrl+C, so use ESC.

### 6. Keep it across reboots (optional, machine-wide only)

```sh
timres apply --resolution 5120 --persist
```

This registers a Task Scheduler task named `timres` that runs the holder 15 s
after logon, starts it immediately, and returns — the task, not your shell, owns
the request from then on. The task runs with least privilege, is allowed on
battery, and has no execution time limit (a default task would stop it after 72
hours). It is started with `--no-esc`, because ESC is read desktop-wide and
would otherwise end the holder from any window; stop it from Task Scheduler
instead. Open Task Scheduler to inspect or edit it, or delete it with
`timres startup remove`.
It is refused unless `GlobalTimerResolutionRequests` is `1`, because in per-process
mode the holder would only accelerate itself. That check reads the registry value,
not the running boot: reboot and run `timres verify-global` (step 2) before
`--persist`. Check the task after the next logon with `timres status`. The task
stores the exe path, so rebuild or move the binary and you must run `--persist`
again — `query` marks the old one as `stale`.

### 7. Undo everything

```sh
timres startup remove    # delete the logon holder
timres global off        # elevated; takes effect after a reboot
```

Close any holder that is still running. No other state is left behind.

## 📋 Commands

| Command | What it does |
|---|---|
| `timres query` | Print the finest/coarsest periods, global tick, mode, dynamic-tick state and logon task |
| `timres bench [--samples N] [--prefer latency\|power] [--rows]` | Scan and recommend |
| `timres apply --resolution <100ns>` | Hold a period for this process; add `--persist` for the logon holder |
| `timres apply --ms <float>` | Same, in milliseconds |
| `timres status` | Sample this process, then print the `query` block |
| `timres measure [--samples N]` | Sample only; sets nothing (default 1000, max 100000) |
| `timres verify-global` | Check whether a request affects other processes |
| `timres global on\|off` | Write the registry key (elevated, reboot required) |
| `timres startup remove` | Delete the logon holder |
| `timres help` | Print the built-in help (`-h`/`--help` works after any command) |

Values are in 100 ns units; the output always shows milliseconds too.

Exit codes: `0` success, `1` runtime failure (a failed Windows call, no usable
range or measurement, or a second `bench` already running), `2` bad command line or
argument.

## 🤔 Why this is not "just set 0.5 ms"

`timres` targets Windows 10 2004 (build 19041) and later, including Windows 11:
earlier builds do not have the per-process request model this tool assumes.

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
  reboot to undo. `query`/`status` report it when run elevated, with the revert
  command if it is disabled; nothing writes it.
- **No automatic registry writes.** `GlobalTimerResolutionRequests` is reported by
  `query`/`status` and only changed by an explicit `timres global on|off` (which
  refuses to run unelevated).
- **No hardcoded "best" value.** The result is per machine, BIOS, power plan and
  driver set. Run `bench` again after a platform change.

## 🔨 Build

On Windows, a normal release build is enough (MSVC or GNU toolchain):

```sh
cargo build --release
# target\release\timres.exe
```

Put that directory on `PATH`, or replace `timres` in the examples with
`.\target\release\timres.exe`.

From Linux, install the mingw-w64 linker, then cross-compile; no MSVC, no runtime
DLLs:

```sh
rustup target add x86_64-pc-windows-gnu
sudo apt install gcc-mingw-w64-x86-64   # Debian/Ubuntu; the CI build uses this package
cargo build --release --target x86_64-pc-windows-gnu
# target/x86_64-pc-windows-gnu/release/timres.exe
```

## 📁 Layout

| file | role |
|---|---|
| `src/stats.rs` | pure statistics, grids, recommendation — unit-tested on any OS |
| `src/measure.rs` | `Sleep(1)` sampler on QPC |
| `src/scan.rs` | dense low band, coarse sweep, stability re-check |
| `src/nt.rs` | `NtSetTimerResolution` / `NtQueryTimerResolution`, registry, BCD probe |
| `src/persist.rs` | logon task in Task Scheduler (`schtasks`) |
| `src/main.rs` | CLI |

## License

MIT — see [LICENSE](LICENSE).
