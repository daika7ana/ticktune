//! `Sleep(1)` sampler. Windows only.

use core::ffi::c_void;
use std::sync::OnceLock;

/// Samples discarded before recording, to skip first-touch page faults.
pub const WARMUP: usize = 10;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn QueryPerformanceFrequency(out: *mut i64) -> i32;
    fn QueryPerformanceCounter(out: *mut i64) -> i32;
    fn Sleep(ms: u32);
    fn GetStdHandle(std_handle: u32) -> *mut c_void;
    fn GetNumberOfConsoleInputEvents(handle: *mut c_void, pending: *mut u32) -> i32;
    fn ReadConsoleInputW(
        handle: *mut c_void,
        buffer: *mut InputRecord,
        len: u32,
        read: *mut u32,
    ) -> i32;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn GetAsyncKeyState(key: i32) -> i16;
}

/// True when ESC was pressed since the previous call. Checked between scan
/// points, so a long bench can be stopped without killing the process (which
/// would leave a partial, unexplained report).
///
/// Desktop-wide by nature: only used where no console exists to focus, i.e.
/// the `--no-console` holder.
pub fn escape_pressed() -> bool {
    const VK_ESCAPE: i32 = 0x1B;
    unsafe { GetAsyncKeyState(VK_ESCAPE) & 1 != 0 }
}

/// `STD_INPUT_HANDLE`: `(DWORD)-10`.
const STD_INPUT_HANDLE: u32 = u32::MAX - 9;

/// One console `INPUT_RECORD`. Only the key-event view is ever read, and it
/// is the largest member of the union, so this matches the Win32 layout.
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)] // unread fields exist to keep the record size and stride
struct InputRecord {
    event_type: u16,
    key: KeyEventRecord,
}

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)] // same: the layout must match KEY_EVENT_RECORD exactly
struct KeyEventRecord {
    down: i32,
    repeat_count: u16,
    virtual_key: u16,
    scan_code: u16,
    character: u16,
    control_state: u32,
}

/// True when ESC was typed into this console since the previous call. Windows
/// delivers key events only to the focused console, so a press in another
/// application never ends a holder or a bench. Returns false when stdin is
/// not a console (redirected), which simply disables ESC; Ctrl+C remains.
pub fn console_escape_pressed() -> bool {
    const KEY_EVENT: u16 = 0x1;
    const VK_ESCAPE: u16 = 0x1B;
    const CHUNK: u32 = 64;

    let stdin = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    let mut pending = 0u32;
    if unsafe { GetNumberOfConsoleInputEvents(stdin, &mut pending) } == 0 || pending == 0 {
        return false;
    }
    let mut records = [unsafe { core::mem::zeroed::<InputRecord>() }; CHUNK as usize];
    let mut saw_esc = false;
    let mut left = pending;
    while left > 0 {
        let mut read = 0u32;
        let ok =
            unsafe { ReadConsoleInputW(stdin, records.as_mut_ptr(), left.min(CHUNK), &mut read) };
        if ok == 0 || read == 0 {
            break;
        }
        left = left.saturating_sub(read);
        for r in &records[..read as usize] {
            if r.event_type == KEY_EVENT && r.key.down != 0 && r.key.virtual_key == VK_ESCAPE {
                saw_esc = true;
            }
        }
    }
    saw_esc
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
