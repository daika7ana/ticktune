//! Logon persistence. Deliberately a `.cmd` in the user's Startup folder: no COM,
//! no Task Scheduler, no elevation, and the user can read and delete it.

use std::path::PathBuf;

/// Path of the script we manage:
/// `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\timres.cmd`.
pub fn startup_script_path() -> Result<PathBuf, String> {
    let appdata = std::env::var("APPDATA").map_err(|_| "APPDATA is not set".to_string())?;
    Ok(PathBuf::from(appdata)
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("Startup")
        .join("timres.cmd"))
}

/// Write the script that runs this exe with `apply --resolution <period> --no-console`
/// at logon. Returns the script path.
pub fn install_startup(period_100ns: u32) -> Result<PathBuf, String> {
    let path = startup_script_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let script = format!(
        "@echo off\r\nstart \"\" \"{}\" apply --resolution {} --no-console\r\n",
        exe.display(),
        period_100ns
    );
    std::fs::write(&path, script).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

/// Delete the script. `Ok(true)` when something was removed.
pub fn remove_startup() -> Result<bool, String> {
    let path = startup_script_path()?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("remove {}: {e}", path.display())),
    }
}

/// Period currently configured at logon, if our script is present.
pub fn startup_period() -> Option<u32> {
    let text = std::fs::read_to_string(startup_script_path().ok()?).ok()?;
    let mut parts = text.split_whitespace();
    while let Some(part) = parts.next() {
        if part == "--resolution" {
            return parts.next()?.parse().ok();
        }
    }
    None
}
