//! `Sleep(1)` sampler. Windows only.

use std::sync::OnceLock;

/// Samples discarded before recording, to skip first-touch page faults.
pub const WARMUP: usize = 10;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn QueryPerformanceFrequency(out: *mut i64) -> i32;
    fn QueryPerformanceCounter(out: *mut i64) -> i32;
    fn Sleep(ms: u32);
}

#[link(name = "user32")]
unsafe extern "system" {
    fn GetAsyncKeyState(key: i32) -> i16;
}

/// True when ESC was pressed since the previous call. Checked between scan
/// points, so a long bench can be stopped without killing the process (which
/// would leave a partial, unexplained report).
pub fn escape_pressed() -> bool {
    const VK_ESCAPE: i32 = 0x1B;
    unsafe { GetAsyncKeyState(VK_ESCAPE) & 1 != 0 }
}

/// QPC frequency never changes during a boot, so read it once.
static QPC_FREQ_HZ: OnceLock<i64> = OnceLock::new();

fn qpc_freq_hz() -> i64 {
    *QPC_FREQ_HZ.get_or_init(|| {
        let mut freq = 0i64;
        unsafe { QueryPerformanceFrequency(&mut freq) };
        // If QPC is unavailable, fall back to the documented 10 MHz tick so we
        // still produce numbers instead of dividing by zero.
        if freq > 0 { freq } else { 10_000_000 }
    })
}

/// Time one `Sleep(1)`, in milliseconds, using `QueryPerformanceCounter`.
pub fn qpc_now_ms() -> f64 {
    let mut counter = 0i64;
    unsafe { QueryPerformanceCounter(&mut counter) };
    counter as f64 * 1000.0 / qpc_freq_hz() as f64
}

/// Run `samples + WARMUP` sleeps and return the recorded durations in ms.
///
/// The caller is responsible for priority (see `nt`). Interference
/// is left in the data on purpose; `Stats` classifies it.
pub fn sample_sleep(samples: usize) -> Vec<f64> {
    if samples == 0 {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(samples);
    for i in 0..samples.saturating_add(WARMUP) {
        let start = qpc_now_ms();
        unsafe { Sleep(1) };
        let elapsed = qpc_now_ms() - start;
        if i >= WARMUP {
            out.push(elapsed);
        }
    }
    out
}
