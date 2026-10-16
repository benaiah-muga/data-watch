use std::fs;

/// Sends SIGTERM to one process, but only when it is still the expected
/// process, owned by the same user as the monitor.
pub fn terminate_own_process(pid: u32, expected_name: &str) -> Result<(), String> {
    if pid < 2 || pid > libc::pid_t::MAX as u32 {
        return Err("Choose a valid process to stop.".into());
    }
    if pid == std::process::id() {
        return Err("Data Watch does not stop its own process.".into());
    }
    let expected = expected_name.trim();
    if expected.is_empty() || expected.len() > 256 {
        return Err("The process name is invalid.".into());
    }
    let status = match fs::read_to_string(format!("/proc/{pid}/status")) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("Process {pid} is no longer running."));
        }
        Err(error) => return Err(format!("Cannot read process {pid}: {error}")),
    };
    let name = status
        .lines()
        .find_map(|line| line.strip_prefix("Name:"))
        .map(str::trim)
        .unwrap_or_default();
    if name != expected {
        return Err(format!(
            "Process {pid} changed since the last sample. Refresh and try again."
        ));
    }
    let owner = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|line| line.split_whitespace().next())
        .and_then(|value| value.parse::<u32>().ok());
    if owner != Some(unsafe { libc::geteuid() }) {
        return Err(format!("Process {pid} belongs to another user."));
    }
    if unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    Err(match error.raw_os_error() {
        Some(libc::ESRCH) => format!("Process {pid} is no longer running."),
        Some(libc::EPERM) => format!("Not permitted to stop process {pid}."),
        _ => format!("Could not stop process {pid}: {error}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::thread;
    use std::time::{Duration, Instant};

    fn wait_for_name(pid: u32, name: &str) {
        for _ in 0..200 {
            if let Ok(status) = fs::read_to_string(format!("/proc/{pid}/status")) {
                if status
                    .lines()
                    .any(|line| line.strip_prefix("Name:").map(str::trim) == Some(name))
                {
                    return;
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("spawned process did not reach the expected name");
    }

    #[test]
    fn rejects_invalid_targets() {
        assert!(terminate_own_process(0, "init").is_err());
        assert!(terminate_own_process(1, "init").is_err());
        assert!(terminate_own_process(std::process::id(), "cargo").is_err());
        assert!(terminate_own_process(999_999_999, "nothing").is_err());
        assert!(terminate_own_process(2, "  ").is_err());
    }

    #[test]
    fn stops_a_spawned_process_only_with_the_matching_name() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        wait_for_name(child.id(), "sleep");
        assert!(terminate_own_process(child.id(), "not-sleep").is_err());
        assert!(child.try_wait().unwrap().is_none());
        terminate_own_process(child.id(), "sleep").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "process ignored SIGTERM");
            thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(status.code(), None);
    }
}
