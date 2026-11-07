//! Logon persistence. A `.vbs` in the user's Startup folder launches the holder
//! with no console window: no COM, no Task Scheduler, no elevation, and the user
//! can read and delete it.

use std::path::PathBuf;

/// Path of the script we manage:
/// `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\timres.vbs`.
pub fn startup_script_path() -> Result<PathBuf, String> {
    let appdata = std::env::var("APPDATA").map_err(|_| "APPDATA is not set".to_string())?;
    Ok(PathBuf::from(appdata)
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("Startup")
        .join("timres.vbs"))
}

/// Write the script that runs this exe with `apply --resolution <period>
/// --no-console` at logon. Window style `0` keeps it hidden. Returns the path.
pub fn install_startup(period_100ns: u32) -> Result<PathBuf, String> {
    let path = startup_script_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    // Doubling quotes is how a VBScript string escapes them.
    let exe = exe.display().to_string().replace('"', "\"\"");
    let script = format!(
        "CreateObject(\"WScript.Shell\").Run \"\"\"{exe}\"\" apply --resolution {period_100ns}\", 0, False\r\n"
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
    parse_period(&std::fs::read_to_string(startup_script_path().ok()?).ok()?)
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
    use super::parse_period;

    #[test]
    fn reads_the_period_out_of_the_launcher() {
        let script = "CreateObject(\"WScript.Shell\").Run \"\"\"C:\\timres.exe\"\" \
                      apply --resolution 5114\", 0, False\r\n";
        assert_eq!(parse_period(script), Some(5114));
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
}
