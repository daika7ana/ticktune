//! Logon persistence via Task Scheduler: one per-user ONLOGON task that starts
//! the holder. No COM — `schtasks.exe` is driven with an XML definition, so the
//! task is visible and editable in Task Scheduler like any other.

use std::path::Path;
use std::process::Command;

pub const TASK_NAME: &str = "ticktune";

/// Written into the task's Description and checked before overwriting or
/// deleting it, so a same-named task we did not create is left alone.
const MARKER: &str = "Managed by ticktune.";

/// What the logon task currently runs.
pub struct Logon {
    pub period_100ns: u32,
    /// The task no longer matches this build: it points at another exe, it was
    /// registered with a principal that still flashes a console (pre-S4U), or
    /// it predates the `--from-task` argument. Re-running `--persist` fixes all.
    pub stale: bool,
    /// The task's XML carries our marker. A task without it may belong to
    /// someone else and must not be overwritten or deleted.
    pub owned: bool,
}

#[cfg(windows)]
fn schtasks() -> Command {
    use std::os::windows::process::CommandExt;
    let mut cmd = Command::new(crate::nt::system32("schtasks.exe"));
    cmd.creation_flags(crate::nt::CREATE_NO_WINDOW);
    cmd
}

#[cfg(not(windows))]
fn schtasks() -> Command {
    Command::new("schtasks")
}

fn task_xml(exe: &str, period_100ns: u32, user: &str) -> String {
    // A logon task with no execution time limit (the holder blocks forever),
    // allowed on battery, and run as the invoking user without elevation.
    // StopExisting: re-running `apply --persist` must replace a live holder,
    // not have its `/Run` dropped while the old period keeps winning.
    //
    // S4U runs the console-subsystem holder on a non-interactive desktop, so
    // Windows has no visible console to flash at logon; `InteractiveToken`
    // would show one before `main` runs. It needs no stored password and is
    // safe because `--persist` already requires global mode.
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\r\n\
<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\r\n\
  <RegistrationInfo>\r\n\
    <Description>Holds a Windows timer resolution request. {}</Description>\r\n\
  </RegistrationInfo>\r\n\
  <Triggers>\r\n\
    <LogonTrigger>\r\n\
      <Enabled>true</Enabled>\r\n\
      <Delay>PT15S</Delay>\r\n\
    </LogonTrigger>\r\n\
  </Triggers>\r\n\
  <Principals>\r\n\
    <Principal id=\"Author\">\r\n\
      <UserId>{}</UserId>\r\n\
      <LogonType>S4U</LogonType>\r\n\
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
      <Arguments>apply --resolution {} --no-console --no-esc --from-task</Arguments>\r\n\
    </Exec>\r\n\
  </Actions>\r\n\
</Task>\r\n",
        MARKER,
        xml_escape(user),
        xml_escape(exe),
        period_100ns
    )
}

/// The account the task runs as. Unlike `InteractiveToken`, S4U does not infer
/// the principal from the registering user, so it must be named.
fn current_user() -> Result<String, String> {
    let user = std::env::var("USERNAME").map_err(|_| "USERNAME is not set".to_string())?;
    match std::env::var("USERDOMAIN") {
        Ok(domain) if !domain.is_empty() => Ok(format!("{domain}\\{user}")),
        _ => Ok(user),
    }
}

/// Refuse to overwrite or delete a task with our name that we cannot prove we
/// created, and hand back the parsed task when there is one. An unreadable XML
/// is not evidence of ownership.
fn ensure_ours() -> Result<Option<Logon>, String> {
    if !task_exists() {
        return Ok(None);
    }
    match logon_task() {
        Some(task) if task.owned => Ok(Some(task)),
        // Unreadable is not "absent": the name is still taken.
        _ => Err(not_ours()),
    }
}

fn not_ours() -> String {
    format!(
        "a task named {TASK_NAME} already exists and does not carry this build's marker; \
         inspect it with `schtasks /Query /TN {TASK_NAME} /XML` and delete it by hand, \
         then re-run"
    )
}

/// Register the logon task. Replaces an existing one.
pub fn install_startup(period_100ns: u32) -> Result<(), String> {
    let _ = ensure_ours()?;
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let user = current_user()?;
    let path = std::env::temp_dir().join(format!("ticktune-task-{}.xml", std::process::id()));
    write_utf16(
        &path,
        &task_xml(&exe.display().to_string(), period_100ns, &user),
    )?;

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

/// Whether a task with our name exists, even if its arguments cannot be read.
pub fn task_exists() -> bool {
    schtasks()
        .args(["/Query", "/TN", TASK_NAME])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Delete the task and report what was there. The caller checks afterwards
/// whether the request is really gone: deleting a task does not stop a holder
/// that already runs as S4U.
pub fn remove_startup() -> Result<Option<Logon>, String> {
    let Some(task) = ensure_ours()? else {
        return Ok(None);
    };
    // S4U holders are not tied to the interactive session, so deleting the task
    // alone would leave the request raised until reboot.
    let _ = schtasks().args(["/End", "/TN", TASK_NAME]).output();
    let out = schtasks()
        .args(["/Delete", "/TN", TASK_NAME, "/F"])
        .output()
        .map_err(|e| format!("run schtasks: {e}"))?;
    if !out.status.success() {
        return Err(command_error("/Delete", &out));
    }
    Ok(Some(task))
}

/// Compare a path `schtasks` echoed back against ours. The export may quote it
/// or pad it, and a mismatch marks the task stale forever if we do not allow
/// for that.
fn normalise(path: &str) -> String {
    path.trim().trim_matches('"').trim().to_lowercase()
}

/// The logon task's period and whether it still matches this build.
pub fn logon_task() -> Option<Logon> {
    let out = schtasks()
        .args(["/Query", "/TN", TASK_NAME, "/XML"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let exe = std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string());
    parse_logon(&decode(&out.stdout), exe.as_deref())
}

/// Parse one task definition. Split from `logon_task` so the ownership and
/// staleness rules can be tested without `schtasks`. `exe` is our own path, or
/// `None` when it cannot be read.
fn parse_logon(xml: &str, exe: Option<&str>) -> Option<Logon> {
    let args = extract_tag(xml, "Arguments")?;
    let period_100ns = parse_period(&args)?;
    let stale = match (extract_tag(xml, "Command"), exe) {
        (Some(command), Some(exe)) => normalise(&command) != normalise(exe),
        _ => false,
    } || !xml.contains("<LogonType>S4U</LogonType>")
        || !args.contains("--from-task");
    Some(Logon {
        period_100ns,
        stale,
        owned: xml.contains(MARKER),
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

#[cfg(test)]
mod tests {
    use super::{MARKER, decode, extract_tag, normalise, parse_logon, parse_period, task_xml};

    #[test]
    fn task_xml_round_trips_command_and_period() {
        let xml = task_xml(
            "C:\\Program Files\\tim & tune\\ticktune.exe",
            5119,
            "DESKTOP\\me",
        );
        assert_eq!(
            extract_tag(&xml, "Command").as_deref(),
            Some("C:\\Program Files\\tim & tune\\ticktune.exe")
        );
        let args = extract_tag(&xml, "Arguments").unwrap();
        assert_eq!(parse_period(&args), Some(5119));
    }

    #[test]
    fn logon_task_does_not_poll_esc() {
        // ESC is read desktop-wide; without --no-esc any window's ESC would
        // end the holder.
        let xml = task_xml("C:\\ticktune.exe", 5000, "DESKTOP\\me");
        let args = extract_tag(&xml, "Arguments").unwrap();
        assert!(args.contains("--no-esc"), "arguments were: {args}");
        // The marker is what makes --no-esc valid; `apply` warns without it.
        assert!(args.contains("--from-task"), "arguments were: {args}");
    }

    #[test]
    fn parse_logon_reads_ownership_and_staleness() {
        let xml = task_xml("C:\\ticktune.exe", 5000, "DESKTOP\\me");
        let task = parse_logon(&xml, Some("C:\\ticktune.exe")).unwrap();
        assert_eq!(task.period_100ns, 5000);
        assert!(task.owned);
        assert!(!task.stale);

        // No marker: someone else's task under our name. Install and remove
        // both refuse it, so this flag is what prevents the clobber.
        let foreign = xml.replace(MARKER, "someone else");
        assert!(!parse_logon(&foreign, None).unwrap().owned);

        // InteractiveToken would flash a console before `main` runs.
        let interactive = xml.replace(
            "<LogonType>S4U</LogonType>",
            "<LogonType>InteractiveToken</LogonType>",
        );
        assert!(
            parse_logon(&interactive, Some("C:\\ticktune.exe"))
                .unwrap()
                .stale
        );

        // Written before --from-task existed: re-running --persist upgrades it.
        let old = xml.replace(" --from-task", "");
        assert!(parse_logon(&old, Some("C:\\ticktune.exe")).unwrap().stale);

        // Unreadable is not ours, and not a task we can report on.
        assert!(parse_logon("<Task/>", None).is_none());
    }

    #[test]
    fn task_carries_the_ownership_marker() {
        // `install_startup` and `remove_startup` refuse to touch a same-named
        // task without this, so it must survive edits to the XML.
        let xml = task_xml("C:\\ticktune.exe", 5000, "DESKTOP\\me");
        assert!(xml.contains(MARKER));
    }

    #[test]
    fn stale_check_tolerates_quotes_and_case() {
        assert_eq!(
            normalise(" \"C:\\Tools\\TickTune.exe\" "),
            "c:\\tools\\ticktune.exe"
        );
        assert_eq!(
            normalise("C:\\Tools\\TickTune.exe"),
            normalise("\"c:\\tools\\ticktune.exe\"")
        );
    }

    #[test]
    fn task_does_not_get_a_default_time_limit() {
        // The holder blocks forever; the default 72 h limit would kill it.
        assert!(
            task_xml("C:\\ticktune.exe", 5000, "DESKTOP\\me")
                .contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>")
        );
        assert!(
            task_xml("C:\\ticktune.exe", 5000, "DESKTOP\\me")
                .contains("<DisallowStartIfOnBatteries>false")
        );
    }

    #[test]
    fn task_xml_pins_holder_replacement_and_privilege() {
        let xml = task_xml("C:\\ticktune.exe", 5000, "DESKTOP\\me");
        assert!(xml.contains("<MultipleInstancesPolicy>StopExisting</MultipleInstancesPolicy>"));
        assert!(xml.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(xml.contains("<UserId>DESKTOP\\me</UserId>"));
    }

    #[test]
    fn task_runs_on_a_non_interactive_desktop() {
        // InteractiveToken makes Windows show the console of a
        // console-subsystem image before `main` runs; S4U runs it on a
        // non-interactive desktop, so there is no window to flash.
        let xml = task_xml("C:\\ticktune.exe", 5000, "DESKTOP\\me");
        assert!(xml.contains("<LogonType>S4U</LogonType>"));
        assert!(!xml.contains("InteractiveToken"));
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
