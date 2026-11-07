# win-timer-resolution — implementation plan

Goal: replace the manual amitxv procedure (scan resolutions by hand, eyeball a CSV,
paste a shortcut) with one tool that **measures what the machine actually does**,
picks a value, and applies it.

---

## 1. What the reference tool actually is

| Piece | What it does |
|---|---|
| `SetTimerResolution.exe` | `NtSetTimerResolution(desired_100ns, TRUE, &actual)`, `SetProcessInformation` (power throttling), single-instance mutex, `--resolution`, `--no-console` |
| `MeasureSleep.exe` | `NtQueryTimerResolution` + N × `QPC`-timed `Sleep(1)` → min/max/avg/stdev |
| `bench.ps1` | 0.5 → 0.7 ms in 0.002 ms steps, 20 samples, **restarts the setter process each step**, 1 s settle |

The README's manual steps (registry `GlobalTimerResolutionRequests=1`,
`bcdedit /set disabledynamictick yes`, reboot, Prime95) exist only to make the
benchmark meaningful and to make the result apply to *other* processes.

## 2. Two facts that dictate the design

**A. Since Win10 2004 timer resolution is per-process.**
`NtSetTimerResolution` creates a request owned by the calling process, released
when that process exits. Other processes only benefit if
`HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\kernel\GlobalTimerResolutionRequests`
(DWORD) is `1` — read at boot, reboot required, and **build-dependent on 24H2/25H2**.
This is why `bench.ps1` measures in a *separate* process and still works: the guide
sets that key first. It also means the tool has two distinct modes:

- **self** — the request is honoured by this process, and only by this process;
  it cannot speed up another application — no admin.
- **system** — one requester affects everything — admin + registry + reboot.
  `apply` is only useful for other applications in this mode.

**B. Sleep granularity is quantised and non-monotonic.**
Observed in the bundled `results.csv`: 0.500 ms → slept ≈ 1.50 ms (3 ticks),
0.512 ms → slept ≈ 1.17 ms, 0.51 ms → ≈ 1.20 ms. Finer is *not* better.
So "best" cannot be computed, only measured, and must be measured as a
distribution, not a mean.

Consequence: `NtQueryTimerResolution`'s `Current` value is the **global tick**,
not your effective resolution. Never display it as "your timer resolution".

## 3. Definition of "best" (must be explicit)

Two competing objectives. The tool reports both and recommends one point:

- **latency** — minimise `Sleep(1)` overshoot.
- **power** — maximise the requested period (coarser tick → fewer wakeups).

Recommendation rule, in two tiers:

- **usable** — `p50 ≥ 1.0 ms`, `p95 ≤ 2.5 ms`, `interference < 1 %`, `n > 0`.
- **within budget** — usable **and** `p50 ≤ 1.35 ms`, `p95 ≤ 2.0 ms`.

Default (`--prefer latency`) picks the usable point with the lowest
`score = 0.5·p50 + 0.5·p95`, i.e. the closest thing to a true 1 ms sleep —
the same objective the original tool used, just measured properly.
`--prefer power` picks the **coarsest** within-budget point instead: the
cheapest setting that still delivers ~1 ms latency. If nothing is usable the
tool errors rather than inventing a number.

`interference` = fraction of samples above `4 × p50` (preemption / unrelated
load), reported, never silently averaged away.

## 4. Milestones

**M0 — skeleton.** One static x86-64 exe, subcommands, builds in CI from Linux
via the already-installed `x86_64-w64-mingw32-gcc`. No MSVC dependency, no installer.

```
timres query                 # available range + global tick + mode detection
timres bench                 # scan + recommend (no permanent change)
timres apply --us 5120       # request for this process, stay alive
timres apply --us 5120 --persist   # + logon task / startup shortcut
timres status                # effective vs reported, who else holds a request
timres verify-global         # does GlobalTimerResolutionRequests actually work here?
```

**M1 — primitives** (`nt.c`): query, set, release-on-exit, power-throttling
ignore (`PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION`), single-instance
mutex, hypervisor detection (VMs lie about timers — warn, don't silently score).
Hand-declare `NtSetTimerResolution` and the two missing constants: the installed
mingw headers are from 2022 and lack them.

**M2 — measurement engine** (`measure.c`). Dedicated thread,
`THREAD_PRIORITY_TIME_CRITICAL`, `QPC` around `Sleep(1)`. Deliberately not pinned:
on hybrid CPUs a fixed affinity can land the sampler on an E-core.
Emit: `min / p50 / p90 / p95 / p99 / mean / stdev / tick-count histogram /
interference`. Also one pass with
`CreateWaitableTimerEx(CREATE_WAITABLE_TIMER_HIGH_RESOLUTION)` because that is
what a game or audio client actually uses — the two can disagree.

**M3 — scan** (`scan.rs`). Two-pass, in-place:
1. coarse: `[min_available, 1.0 ms]`, 10 µs step, 200 samples per point (~15 s)
2. dense: `[best − 10 µs, best]`, **100 ns step** (one representable unit),
   200 samples per point (~25 s). The knee where `Sleep(1)` drops from three
   ticks to two sits a few microseconds above `min_available` and is under
   1 µs wide on builds 26200/26300, so a 2 µs step can walk straight over it.
3. repeat the dense winner once; reject the result if p50 moves > 40 µs.

Changing the request is a **replace, not a restart** — no process spawning, no
1 s settle. This alone takes the run from ~5 min to ~40 s. Record power plan,
AC/battery and hypervisor state with the result; refuse to compare across a change.

**M4 — apply / persist** (`persist.c`). Self mode: the process must stay alive
(request dies with it) → `apply` blocks until Ctrl-C, or `--background` detaches.
System mode: write the registry DWORD only with explicit `--set-global`, then tell
the user a reboot is required. **Never write `bcdedit`** — read
`disabledynamictick` (`bcdedit /enum {current}`) and, if it is `yes`, print a
warning with the exact revert command, because on some machines it makes the
system unusable (observed: 10–15 s per UI action, only fixed by deleting the
entry). `timres status` reports it read-only alongside the registry DWORD.

The benchmark must not require it: the measuring thread wakes every ~1 ms, so the
system never reaches tickless idle during a scan anyway — dynamic tick cannot
affect the samples. It only ever mattered for the idle-state tests in the README.
Logon persistence via a Scheduled Task (only when system mode needs elevation);
a plain `shell:startup` shortcut otherwise.

**M5 — validation.** A/B against the bundled `MeasureSleep.exe` at fixed values
(run it with global mode on, since it is a separate process). Repeatability: 5
runs, chosen value within 40 µs. Cross-check `powercfg /energy` attribution.
Then: desktop, laptop on battery, hybrid P/E-core, and a VM (expected to fail —
that is a test too).

## 4a. Platform notes (checked 2026-10)

- Windows 11 **25H2 = build 26200**, **26H2 = build 26300**. Neither changes the
  timer API, the per-process model, or tick-rate semantics. **26H1 = build
  28000** is a device-scoped release, so build numbers do not order the versions —
  never gate behaviour on "build ≥ N".
- `GlobalTimerResolutionRequests` still works on 26200/26300: behaviourally
  verified — absent → ~15.6 ms sleeps, `1` → ~1.01 ms measured from a process
  that requests nothing. Still boot-only, still undocumented, still the only way
  to get machine-wide behaviour.
- The knee moved: 5023 (26100) → **5018** (26200/26300), and the fast window on
  26300 is only ~5018–5025. `0.5000 ms` is on the slow (3-tick) side there. That
  is why the dense pass exists and why no value is hardcoded.
- New since 2025: a "Medium" QoS level plus a user-inactivity demotion. Microsoft
  warns that benchmarks without user input can be silently lowered mid-run. Set
  `HKLM\SYSTEM\CurrentControlSet\Control\Power\PowerThrottling\DisableUserPresenceQos = 1`
  (REG_DWORD) before a benchmark run. The tool reports, never writes it.

## 5. Risks

- **Not portable.** Result is per machine + BIOS + power plan + driver set.
  The tool must re-scan, never ship a hardcoded 5120.
- **Power/heat.** A 0.5 ms tick keeps the CPU out of deep C-states. This is the
  entire reason for "coarsest valid" instead of "always 0.5".
- **The registry key may be ignored on 24H2+.** `verify-global` exists to make
  that a measurement, not an assumption.
- **Boot-level tweaks are not safe booleans.** `disabledynamictick yes` bricked
  usability on at least one real machine, and reverting needs an extra reboot.
  The tool therefore changes exactly one thing that reverts itself on exit —
  the process-owned timer request — and treats every other setting as
  read-only diagnostics. A `doctor`/`status` path must be able to tell the user
  what a previous guide told them to change and how to undo it.
- **Anti-cheat / vendor detection.** Holding a global fine timer is visible.
  System mode stays opt-in; self mode is indistinguishable from a normal app.
- **Licensing.** Reimplement from the behaviour, do not ship the bundled exes;
  the repo carries no license.

## 6. Deliberately out of scope (v1)

GUI/tray icon, per-process profiles, `timeBeginPeriod` fallback, auto-update,
telemetry, driver-level timer work, automatic `bcdedit` / power-plan changes.
