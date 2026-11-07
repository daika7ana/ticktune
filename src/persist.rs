//! Logon persistence via Task Scheduler: one per-user ONLOGON task that starts
//! the holder. No COM — `schtasks.exe` is driven with an XML definition, so the
//! task is visible and editable in Task Scheduler like any other.

use std::path::{Path, PathBuf};
use std::process::Command;

pub const TASK_NAME: &str = "timres";

/// Kept only so a launcher left by an older build is removed instead of
/// surviving `startup remove`.
const LEGACY_SCRIPTS: [&str; 2] = ["timres.vbs", "timres.cmd"];

/// What the logon task currently runs.
pub struct Logon {
    pub period_100ns: u32,
    /// The task points at a different exe than this one, e.g. after a rebuild
    /// elsewhere. It would start at logon and quietly do nothing useful.
    pub stale: bool,
}

#[cfg(windows)]
fn schtasks() -> Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut cmd = Command::new(schtasks_exe());
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

#[cfg(not(windows))]
fn schtasks() -> Command {
    Command::new(schtasks_exe())
}

fn schtasks_exe() -> String {
    match std::env::var("SystemRoot") {
        Ok(root) => format!("{root}\\System32\\schtasks.exe"),
        Err(_) => "schtasks".to_string(),
    }
}

fn task_xml(exe: &str, period_100ns: u32) -> String {
    // A logon task with no execution time limit (the holder blocks forever),
    // allowed on battery, and run as the invoking user without elevation.
    // StopExisting: re-running `apply --persist` must replace a live holder,
    // not have its `/Run` dropped while the old period keeps winning.
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\r\n\
<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\r\n\
  <RegistrationInfo>\r\n\
    <Description>Holds a Windows timer resolution request. Managed by timres.</Description>\r\n\
  </RegistrationInfo>\r\n\
  <Triggers>\r\n\
    <LogonTrigger>\r\n\
      <Enabled>true</Enabled>\r\n\
      <Delay>PT15S</Delay>\r\n\
    </LogonTrigger>\r\n\
  </Triggers>\r\n\
  <Principals>\r\n\
    <Principal id=\"Author\">\r\n\
      <LogonType>InteractiveToken</LogonType>\r\n\
      <RunLevel>LeastPrivilege</RunLevel>\r\n\
    </Principal>\r\n\
  </Principals>\r\n\
  <Settings>\r\n\
    <MultipleInstancesPolicy>StopExisting</MultipleInstancesPolicy>\r\n\
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\r\n\
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\r\n\
    <AllowHardTerminate>true</AllowHardTerminate>\r\n\
    <StartWhenAvailable>false</StartWhenAvailable>\r\n\
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>\r\n\
    <IdleSettings>\r\n\
      <StopOnIdleEnd>false</StopOnIdleEnd>\r\n\
      <RestartOnIdle>false</RestartOnIdle>\r\n\
    </IdleSettings>\r\n\
    <AllowStartOnDemand>true</AllowStartOnDemand>\r\n\
    <Enabled>true</Enabled>\r\n\
    <Hidden>false</Hidden>\r\n\
    <RunOnlyIfIdle>false</RunOnlyIfIdle>\r\n\
    <WakeToRun>false</WakeToRun>\r\n\
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\r\n\
  </Settings>\r\n\
  <Actions Context=\"Author\">\r\n\
    <Exec>\r\n\
      <Command>{}</Command>\r\n\
      <Arguments>apply --resolution {} --no-console --no-esc</Arguments>\r\n\
    </Exec>\r\n\
  </Actions>\r\n\
</Task>\r\n",
        xml_escape(exe),
        period_100ns
    )
}

/// Register the logon task. Replaces an existing one.
pub fn install_startup(period_100ns: u32) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let path = std::env::temp_dir().join(format!("timres-task-{}.xml", std::process::id()));
    write_utf16(&path, &task_xml(&exe.display().to_string(), period_100ns))?;

    let out = schtasks()
        .args(["/Create", "/TN", TASK_NAME, "/XML"])
        .arg(&path)
        .arg("/F")
        .output()
        .map_err(|e| format!("run schtasks: {e}"));
    let _ = std::fs::remove_file(&path);
    let out = out?;
    if !out.status.success() {
        return Err(command_error("/Create", &out));
    }
    remove_legacy_scripts();
    Ok(())
}

/// Start the registered task now, so `apply --persist` does not have to keep
/// its own process alive to deliver the request.
pub fn start_startup() -> Result<(), String> {
    let out = schtasks()
        .args(["/Run", "/TN", TASK_NAME])
        .output()
        .map_err(|e| format!("run schtasks: {e}"))?;
    if !out.status.success() {
        return Err(command_error("/Run", &out));
    }
    Ok(())
}

/// Delete the task and any launcher left by an older build.
pub fn remove_startup() -> Result<bool, String> {
    let existed = logon_task().is_some();
    if existed {
        let out = schtasks()
            .args(["/Delete", "/TN", TASK_NAME, "/F"])
            .output()
            .map_err(|e| format!("run schtasks: {e}"))?;
        if !out.status.success() {
            return Err(command_error("/Delete", &out));
        }
    }
    Ok(remove_legacy_scripts() || existed)
}

/// The logon task's period and whether it still points at this exe.
pub fn logon_task() -> Option<Logon> {
    let out = schtasks()
        .args(["/Query", "/TN", TASK_NAME, "/XML"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let xml = decode(&out.stdout);
    let args = extract_tag(&xml, "Arguments")?;
    let period_100ns = parse_period(&args)?;
    let stale = match (extract_tag(&xml, "Command"), std::env::current_exe()) {
        (Some(command), Ok(exe)) => !command
            .trim()
            .eq_ignore_ascii_case(&exe.display().to_string()),
        _ => false,
    };
    Some(Logon {
        period_100ns,
        stale,
    })
}

fn command_error(verb: &str, out: &std::process::Output) -> String {
    let stderr = decode(&out.stderr);
    let text = stderr.trim();
    if text.is_empty() {
        format!("schtasks {verb} failed with {}", out.status)
    } else {
        format!("schtasks {verb} failed: {text}")
    }
}

fn write_utf16(path: &Path, text: &str) -> Result<(), String> {
    // schtasks expects the encoding the XML declaration names.
    let mut bytes = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    std::fs::write(path, bytes).map_err(|e| format!("write {}: {e}", path.display()))
}

/// schtasks writes UTF-16; accept that and anything else gracefully.
fn decode(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn extract_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml_unescape(&xml[start..end]))
}

/// Pull the value after `--resolution` out of the task arguments. Kept free of
/// platform calls so it is testable anywhere.
fn parse_period(arguments: &str) -> Option<u32> {
    let mut parts = arguments.split_whitespace();
    while let Some(part) = parts.next() {
        if part == "--resolution" {
            return parts.next()?.parse().ok();
        }
    }
    None
}

fn remove_legacy_scripts() -> bool {
    let Some(dir) = std::env::var("APPDATA").ok().map(|a| {
        PathBuf::from(a)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Startup")
    }) else {
        return false;
    };
    let mut removed = false;
    for name in LEGACY_SCRIPTS {
        if std::fs::remove_file(dir.join(name)).is_ok() {
            removed = true;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::{decode, extract_tag, parse_period, task_xml};

    #[test]
    fn task_xml_round_trips_command_and_period() {
        let xml = task_xml("C:\\Program Files\\tim & res\\timres.exe", 5119);
        assert_eq!(
            extract_tag(&xml, "Command").as_deref(),
            Some("C:\\Program Files\\tim & res\\timres.exe")
        );
        let args = extract_tag(&xml, "Arguments").unwrap();
        assert_eq!(parse_period(&args), Some(5119));
    }

    #[test]
    fn logon_task_does_not_poll_esc() {
        // ESC is read desktop-wide; without --no-esc any window's ESC would
        // end the holder.
        let xml = task_xml("C:\\timres.exe", 5000);
        let args = extract_tag(&xml, "Arguments").unwrap();
        assert!(args.contains("--no-esc"), "arguments were: {args}");
    }

    #[test]
    fn task_does_not_get_a_default_time_limit() {
        // The holder blocks forever; the default 72 h limit would kill it.
        assert!(task_xml("C:\\timres.exe", 5000)
            .contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(task_xml("C:\\timres.exe", 5000).contains("<DisallowStartIfOnBatteries>false"));
    }

    #[test]
    fn reads_utf16_with_a_bom() {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "Command".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(decode(&bytes), "Command");
        assert_eq!(decode(b"plain"), "plain");
    }

    #[test]
    fn missing_or_malformed_period_is_none() {
        assert_eq!(parse_period("apply --resolution notanumber"), None);
        assert_eq!(parse_period("apply --resolution"), None);
        assert_eq!(parse_period("apply --no-console"), None);
    }
}
