# AGENTS.md

`ticktune`: dependency-free Rust CLI that measures and requests Windows timer
resolution. Windows-only binary, cross-compiled from Linux.

## Verify (run all five)

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings          # host only
cargo test
cargo clippy --target x86_64-pc-windows-gnu --all-targets -- -D warnings
cargo build --release --target x86_64-pc-windows-gnu
```

Cross-target setup: `rustup target add x86_64-pc-windows-gnu` plus
`gcc-mingw-w64-x86-64` (Debian/Ubuntu package used by CI).

Host `cargo test`/`clippy` do **not** compile the real program: `main.rs`'s
`cli` module, `measure`, `nt`, and `scan` are `#[cfg(windows)]`. Host checks
only cover `stats` and `persist`'s XML/UTF-16 helpers. The cross steps compile
and lint the Windows code; only CI's `windows-latest` job runs it (`cargo test`,
`query`, `measure --samples 20`). Don't claim Windows behaviour from a local
build.

## Layout / compile gates

- `src/stats.rs` — pure math, grids, recommendation; the only module compiled on
  every OS outside test builds. Put testable logic here and add unit tests
  (`#[cfg(test)]` in-file).
- `src/measure.rs` — `Sleep(1)` sampling on QPC; Windows only.
- `src/scan.rs` — dense low band + coarse sweep + stability re-check; Windows only.
- `src/nt.rs` — `NtSetTimerResolution`/`NtQueryTimerResolution`, registry, BCD
  probe; Windows only. Shared `CREATE_NO_WINDOW` and `system32()` live here.
- `src/persist.rs` — Task Scheduler logon task via `schtasks.exe` XML; compiled
  on `any(windows, test)` so its pure parsing tests run on Linux. Task presence
  is `task_exists()` (`/Query /TN`); `logon_task()` returns `None` when an
  edited task's arguments don't parse, so it is not an existence check.
  `remove_startup()` ends a running instance before deleting it, because an S4U
  holder outlives the interactive session.
- `src/main.rs` — hand-rolled CLI dispatch (no clap); most of the file is the
  `cfg(windows)` `cli` module.

Crate/binary name is `ticktune`. The project was renamed from `timres`; don't
revive the old name.

## CLI conventions

- Units are 100 ns; `--ms` is a converted convenience.
- Exit codes are a contract: `0` success, `1` runtime/Windows failure,
  `2` bad command line. Use `Fail::Op` vs `Fail::Usage`; runtime state errors
  (e.g. `--persist` without global mode) are `Op`, not `Usage`.
- Write output through `outln!`/`out` and diagnostics through `errln!`/`err`,
  never `println!`/`eprintln!`: stdout or stderr may be closed
  (`ticktune bench | head`) and a failed write must not abort a running scan.
- Defaults/thresholds live in code and are mirrored in `README.md`; the sample
  defaults also appear in the `HELP` string. Keep them in sync when changing
  them: bench 200 samples (max 1000), measure 1000 (max 100000); usable =
  interference ≤ 2 %, p50 ≥ 0.99 ms, p95 ≤ 2.5 ms; power budget
  p50 ≤ 1.35 / p95 ≤ 2.0 ms.

## Invariants

- No dependencies. Keep `[dependencies]` empty and the lockfile trivial.
- Never write registry or BCD settings automatically. Only an explicit
  `ticktune global on|off` touches `GlobalTimerResolutionRequests`; `bcdedit`
  is read-only by design (see README "What it deliberately does not do").
- Per-process timer request is the default; global mode requires the registry
  flag plus a reboot.
- The logon task XML behaviours are pinned by tests — preserve `--no-esc`
  (ESC is desktop-wide), `StopExisting`, `PT0S` execution limit, battery
  allowed, least privilege, and UTF-16 BOM decoding when editing `persist.rs`.
  The arguments also carry `--from-task`, which is what silences `apply`'s
  `--no-esc` warning, and the Description carries `MARKER`; `install_startup`
  and `remove_startup` refuse to touch a same-named task that lacks the marker.
- The task principal is `S4U` with the current user's `UserId`. That runs the
  console-subsystem holder on a non-interactive desktop, so no console window
  flashes at logon; `InteractiveToken` would show one before `main` runs. S4U
  stores no password and needs the "Log on as a batch job" right (administrators
  have it); it is only safe because `--persist` already requires global mode.
- `apply` is a holder that also takes 50 plain samples for its `observed` line:
  it calls `ignore_timer_resolution_throttling()`, never `prepare_sampling()`
  (which also raises the thread to time-critical for the process lifetime).

## Build / release

- Edition 2024, MSRV 1.85 (`Cargo.toml`).
- Release profile is size-oriented: `opt-level = "z"`, LTO, `codegen-units = 1`,
  `panic = "abort"`, `strip`.
- Pushing a `v*` tag publishes `ticktune.exe` plus `.sha256` to GitHub Releases.
  The tag must equal `Cargo.toml`'s version; the release job runs fmt, clippy,
  and tests before packaging.
- `README.md` is the user-facing source of truth for measured behaviour and
  documented knees; change code and docs together.
