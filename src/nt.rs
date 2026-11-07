//! Hand-rolled timer-resolution bindings. Windows only, zero external crates.
//!
//! Both `Nt*TimerResolution` calls are undocumented but stable. Declare them via
//! `#[link(name = "ntdll")] extern "system"`; mingw ships `libntdll.a`.

use core::ffi::c_void;

/// What the platform reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    /// Smallest period the platform can provide, in 100 ns units (e.g. 5000 = 0.5 ms).
    pub finest: u32,
    /// Largest period, in 100 ns units (e.g. 156250 = 15.625 ms).
    pub coarsest: u32,
    /// Current system-wide tick in 100 ns units. **Not** this process's effective value.
    pub current: u32,
}

type Handle = *mut c_void;
type Hkey = *mut c_void;

const STATUS_SUCCESS: i32 = 0;
const STATUS_TIMER_RESOLUTION_NOT_SET: u32 = 0xC000_0245;

const TOKEN_QUERY: u32 = 0x0008;
const TOKEN_ELEVATION_CLASS: u32 = 20;

const PROCESS_POWER_THROTTLING: u32 = 4;
/// `PROCESS_POWER_THROTTLING_EXECUTION_SPEED`.
const EXECUTION_SPEED: u32 = 0x1;
/// `PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION` — 0x4, not 0x1.
const IGNORE_TIMER_RESOLUTION: u32 = 0x4;

const HKEY_LOCAL_MACHINE: Hkey = 0x8000_0002_isize as Hkey;
const KEY_SET_VALUE: u32 = 0x0002;
const REG_OPTION_NON_VOLATILE: u32 = 0x0000_0000;
const REG_DWORD: u32 = 4;
const RRF_RT_REG_DWORD: u32 = 0x10;

const KERNEL_KEY: &str = "SYSTEM\\CurrentControlSet\\Control\\Session Manager\\kernel";
const GLOBAL_VALUE: &str = "GlobalTimerResolutionRequests";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(core::iter::once(0)).collect()
}

#[link(name = "ntdll")]
extern "system" {
    fn NtSetTimerResolution(desired: u32, set: u8, actual: *mut u32) -> i32;
    fn NtQueryTimerResolution(maximum: *mut u32, minimum: *mut u32, current: *mut u32) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> Handle;
    fn GetCurrentThread() -> Handle;
    fn SetThreadPriority(thread: Handle, priority: i32) -> i32;
    fn GetThreadPriority(thread: Handle) -> i32;
    fn SetProcessInformation(process: Handle, class: u32, info: *mut c_void, size: u32) -> i32;
    fn CloseHandle(handle: Handle) -> i32;
}

#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    fn GetTokenInformation(
        token: Handle,
        class: u32,
        info: *mut c_void,
        len: u32,
        needed: *mut u32,
    ) -> i32;
    fn RegGetValueW(
        key: Hkey,
        subkey: *const u16,
        value: *const u16,
        flags: u32,
        kind: *mut u32,
        data: *mut c_void,
        len: *mut u32,
    ) -> i32;
    fn RegCreateKeyExW(
        key: Hkey,
        subkey: *const u16,
        reserved: u32,
        class: *const u16,
        options: u32,
        access: u32,
        security: *mut c_void,
        result: *mut Hkey,
        disposition: *mut u32,
    ) -> i32;
    fn RegSetValueExW(
        key: Hkey,
        value: *const u16,
        reserved: u32,
        kind: u32,
        data: *const u8,
        len: u32,
    ) -> i32;
    fn RegCloseKey(key: Hkey) -> i32;
}

/// Read the available range. `current` is the global tick, never evidence of
/// what a given process actually sleeps.
pub fn query() -> Result<Range, String> {
    let (mut finest, mut coarsest, mut current) = (0u32, 0u32, 0u32);
    // NAMING TRAP: Maximum is the coarsest period, Minimum the finest.
    let status = unsafe { NtQueryTimerResolution(&mut coarsest, &mut finest, &mut current) };
    if status != STATUS_SUCCESS {
        return Err(format!(
            "NtQueryTimerResolution failed: 0x{:08X}",
            status as u32
        ));
    }
    Ok(Range {
        finest,
        coarsest,
        current,
    })
}

/// Request `period_100ns` for this process, replacing any previous request.
///
/// The kernel's `ActualTime` out-param is deliberately discarded: on current
/// builds it reports the finest resolution (0.5000 ms) no matter what was
/// accepted, so only a timing sample shows whether the request took effect.
pub fn set(period_100ns: u32) -> Result<(), String> {
    let mut reported = 0u32;
    let status = unsafe { NtSetTimerResolution(period_100ns, 1, &mut reported) };
    if status != STATUS_SUCCESS {
        return Err(format!(
            "NtSetTimerResolution failed: 0x{:08X}",
            status as u32
        ));
    }
    Ok(())
}

/// Release this process's request. Harmless to call twice.
pub fn release() -> Result<(), String> {
    let mut ignored = 0u32;
    let status = unsafe { NtSetTimerResolution(0, 0, &mut ignored) };
    if status == STATUS_SUCCESS || status as u32 == STATUS_TIMER_RESOLUTION_NOT_SET {
        Ok(())
    } else {
        Err(format!(
            "NtSetTimerResolution(release) failed: 0x{:08X}",
            status as u32
        ))
    }
}

/// Whether the token is elevated. Only needed for registry / boot-config writes.
pub fn is_elevated() -> bool {
    unsafe {
        let mut token: Handle = core::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevated = 0u32;
        let mut needed = 0u32;
        let ok = GetTokenInformation(
            token,
            TOKEN_ELEVATION_CLASS,
            &mut elevated as *mut u32 as *mut c_void,
            core::mem::size_of::<u32>() as u32,
            &mut needed,
        );
        CloseHandle(token);
        ok != 0 && elevated != 0
    }
}

/// Keep the process out of EcoQoS so an occluded window cannot silently drop
/// our timer-resolution guarantee. Best effort, ignore failure.
pub fn ignore_timer_resolution_throttling() {
    // PROCESS_POWER_THROTTLING_STATE { Version, ControlMask, StateMask }.
    // A set StateMask bit turns a control ON; zero means "do not throttle".
    let mut state = [1u32, EXECUTION_SPEED | IGNORE_TIMER_RESOLUTION, 0];
    let ok = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            PROCESS_POWER_THROTTLING,
            state.as_mut_ptr() as *mut c_void,
            core::mem::size_of_val(&state) as u32,
        )
    };
    if ok == 0 {
        eprintln!("warning: could not opt out of power throttling");
    }
}

/// Current thread priority, so the sampler can restore the caller's value.
pub fn thread_priority() -> i32 {
    unsafe { GetThreadPriority(GetCurrentThread()) }
}

/// Set the current thread priority. Best effort.
pub fn set_thread_priority(priority: i32) {
    unsafe {
        SetThreadPriority(GetCurrentThread(), priority);
    }
}

/// `GlobalTimerResolutionRequests` DWORD, if the key exists.
pub fn global_requests_enabled() -> Option<u32> {
    let subkey = wide(KERNEL_KEY);
    let value = wide(GLOBAL_VALUE);
    let mut data = 0u32;
    let mut len = core::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            core::ptr::null_mut(),
            &mut data as *mut u32 as *mut c_void,
            &mut len,
        )
    };
    (status == 0).then_some(data)
}

/// Write `GlobalTimerResolutionRequests`. Requires elevation; takes effect after
/// a reboot. Never called without an explicit user flag.
pub fn set_global_requests(on: bool) -> Result<(), String> {
    if !is_elevated() {
        return Err("requires elevation".to_string());
    }
    unsafe {
        let subkey = wide(KERNEL_KEY);
        let mut key: Hkey = core::ptr::null_mut();
        let status = RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            0,
            core::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            core::ptr::null_mut(),
            &mut key,
            core::ptr::null_mut(),
        );
        if status != 0 {
            return Err(format!("RegCreateKeyExW failed: 0x{:08X}", status as u32));
        }
        let value = wide(GLOBAL_VALUE);
        let data: u32 = u32::from(on);
        let status = RegSetValueExW(
            key,
            value.as_ptr(),
            0,
            REG_DWORD,
            &data as *const u32 as *const u8,
            core::mem::size_of::<u32>() as u32,
        );
        RegCloseKey(key);
        if status != 0 {
            return Err(format!("RegSetValueExW failed: 0x{:08X}", status as u32));
        }
    }
    Ok(())
}

/// Read `disabledynamictick` from BCD. `None` if it cannot be read (needs admin).
pub fn dynamic_tick_disabled() -> Option<bool> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // Resolve through SystemRoot so an attacker-writable PATH entry cannot
    // substitute its own bcdedit.
    let bcdedit = match std::env::var("SystemRoot") {
        Ok(root) => format!("{root}\\System32\\bcdedit.exe"),
        Err(_) => "bcdedit".to_string(),
    };
    let out = std::process::Command::new(bcdedit)
        .args(["/enum", "{current}"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some("disabledynamictick") {
            continue;
        }
        return match parts.next() {
            Some(v) if v.eq_ignore_ascii_case("yes") => Some(true),
            Some(v) if v.eq_ignore_ascii_case("no") => Some(false),
            _ => None,
        };
    }
    None
}
