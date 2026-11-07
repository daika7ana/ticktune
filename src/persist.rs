//! Logon persistence. A `.vbs` in the user's Startup folder launches the holder
//! with no console window: no COM, no Task Scheduler, no elevation, and the user
//! can read and delete it.

use std::path::PathBuf;

/// `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup`.
fn startup_dir() -> Result<PathBuf, String> {
    let appdata = std::env::var("APPDATA").map_err(|_| "APPDATA is not set".to_string())?;
    Ok(PathBuf::from(appdata)
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("Startup"))
}

/// The script we manage now.
pub fn startup_script_path() -> Result<PathBuf, String> {
    Ok(startup_dir()?.join("timres.vbs"))
}

/// The script builds before the hidden launcher used. Kept so an installed
/// holder from an older build can be found and removed instead of silently
/// surviving `startup remove`.
pub fn legacy_script_path() -> Result<PathBuf, String> {
    Ok(startup_dir()?.join("timres.cmd"))
}

fn launcher(exe: &str, period_100ns: u32) -> String {
    // Doubling quotes is how a VBScript string escapes them.
    let exe = exe.replace('"', "\"\"");
    format!(
        "CreateObject(\"WScript.Shell\").Run \"\"\"{exe}\"\" apply --resolution {period_100ns}\", 0, False\r\n"
    )
}

/// Write the script that runs this exe with `apply --resolution <period>
/// --no-console` at logon. Window style `0` keeps it hidden. Returns the path.
pub fn install_startup(period_100ns: u32) -> Result<PathBuf, String> {
    let path = startup_script_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let script = launcher(&exe.display().to_string(), period_100ns);
    std::fs::write(&path, script).map_err(|e| format!("write {}: {e}", path.display()))?;
    // The old launcher would start a second holder with a stale period.
    let _ = remove_file_if_present(&legacy_script_path()?);
    Ok(path)
}

/// Delete both launchers. `Ok(true)` when anything was removed.
pub fn remove_startup() -> Result<bool, String> {
    let current = remove_file_if_present(&startup_script_path()?)?;
    let legacy = remove_file_if_present(&legacy_script_path()?)?;
    Ok(current || legacy)
}

fn remove_file_if_present(path: &PathBuf) -> Result<bool, String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("remove {}: {e}", path.display())),
    }
}

fn read_script() -> Option<String> {
    std::fs::read_to_string(startup_script_path().ok()?)
        .ok()
        .or_else(|| std::fs::read_to_string(legacy_script_path().ok()?).ok())
}

/// Period currently configured at logon, if our script is present.
pub fn startup_period() -> Option<u32> {
    parse_period(&read_script()?)
}

/// Whether the installed launcher points at a different exe than this one,
/// e.g. after the binary was rebuilt elsewhere. It would start at logon and
/// quietly do nothing useful.
pub fn startup_stale() -> bool {
    let Some(script) = read_script() else {
        return false;
    };
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    !script
        .to_lowercase()
        .contains(&exe.display().to_string().to_lowercase())
}

/// Pull the value after `--resolution` out of the launcher script. Kept free of
/// platform calls so it is testable anywhere.
fn parse_period(script: &str) -> Option<u32> {
    let mut parts = script.split_whitespace();
    while let Some(part) = parts.next() {
        if part == "--resolution" {
            // The value is followed by the closing quote of the Run argument.
            let token = parts.next()?.trim_matches(|c: char| !c.is_ascii_digit());
            return token.parse().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{launcher, parse_period};

    #[test]
    fn reads_the_period_out_of_the_launcher() {
        let script = launcher("C:\\timres.exe", 5114);
        assert_eq!(parse_period(&script), Some(5114));
    }

    #[test]
    fn missing_or_malformed_period_is_none() {
        assert_eq!(
            parse_period("CreateObject(\"WScript.Shell\").Run \"x\"\r\n"),
            None
        );
        assert_eq!(parse_period("--resolution notanumber"), None);
        assert_eq!(parse_period("--resolution"), None);
    }

    #[test]
    fn launcher_quotes_the_path_for_vbscript() {
        let script = launcher("C:\\Program Files\\timres.exe", 5000);
        assert!(script.contains("\"\"\"C:\\Program Files\\timres.exe\"\" apply"));
    }
}
