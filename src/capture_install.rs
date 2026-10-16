use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

fn systemd_quote(value: &str) -> Result<String, String> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err("Invalid capture socket path.".into());
    }
    Ok(format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
    ))
}

pub fn service_name(uid: u32) -> String {
    format!("data-watch-capture-{uid}.service")
}
fn executable(uid: u32) -> PathBuf {
    PathBuf::from(format!("/usr/local/libexec/data-watch-capture-{uid}"))
}

pub fn unit(uid: u32, socket: &Path) -> Result<String, String> {
    if uid == 0 || !socket.is_absolute() {
        return Err("Capture requires a desktop user and an absolute socket path.".into());
    }
    let socket = systemd_quote(socket.to_str().ok_or("Socket path is not UTF-8")?)?;
    Ok(format!("[Unit]\nDescription=Data Watch continuous TCP and UDP capture for user {uid}\nAfter=network.target\n\n[Service]\nType=simple\nUser={uid}\nExecStart={} packet-capture {socket}\nRestart=always\nRestartSec=3\nUMask=0077\nNoNewPrivileges=true\nCapabilityBoundingSet=CAP_NET_RAW CAP_DAC_READ_SEARCH CAP_SYS_PTRACE\nAmbientCapabilities=CAP_NET_RAW CAP_DAC_READ_SEARCH CAP_SYS_PTRACE\nRestrictAddressFamilies=AF_PACKET AF_UNIX\nPrivateTmp=true\nProtectSystem=strict\nProtectHome=read-only\nProtectKernelTunables=true\nProtectKernelModules=true\nProtectControlGroups=true\nRestrictSUIDSGID=true\nLockPersonality=true\n\n[Install]\nWantedBy=multi-user.target\n", executable(uid).display()))
}

fn protected_directory(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if parent != path {
            protected_directory(parent)?;
        }
    }
    if !path.exists() {
        fs::create_dir(path).map_err(|e| e.to_string())?;
    }
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(format!(
            "Refusing non-root-owned, writable, or symlinked system directory: {}",
            path.display()
        ));
    }
    Ok(())
}

fn atomic_install(path: &Path, content: &[u8], mode: u32) -> Result<(), String> {
    let parent = path.parent().ok_or("Missing installation directory")?;
    protected_directory(parent)?;
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(format!("Refusing unsafe system file: {}", path.display()));
        }
    }
    let staging = parent.join(format!(".data-watch-install-{}", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staging)
            .map_err(|e| e.to_string())?;
        file.write_all(content).map_err(|e| e.to_string())?;
        file.set_permissions(fs::Permissions::from_mode(mode))
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&staging, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staging);
    }
    result
}

pub fn install(uid: u32, socket: &Path) -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(
            "Capture installation needs sudo. Run data-watch full as your normal user.".into(),
        );
    }
    if let Ok(sudo_uid) = std::env::var("SUDO_UID") {
        if sudo_uid.parse::<u32>().ok() != Some(uid) {
            return Err("Capture user does not match the authenticated sudo user.".into());
        }
    }
    let unit = unit(uid, socket)?;
    let source = std::env::current_exe().map_err(|e| e.to_string())?;
    atomic_install(
        &executable(uid),
        &fs::read(source).map_err(|e| e.to_string())?,
        0o755,
    )?;
    let service = service_name(uid);
    atomic_install(
        &PathBuf::from("/etc/systemd/system").join(&service),
        unit.as_bytes(),
        0o644,
    )?;
    for args in [
        vec!["daemon-reload"],
        vec!["enable", "--now", &service],
        vec!["restart", &service],
    ] {
        let status = Command::new("/usr/bin/systemctl")
            .args(args)
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("Could not start {service}: {status}"));
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    let active = Command::new("/usr/bin/systemctl")
        .args(["is-active", "--quiet", &service])
        .status()
        .map_err(|e| e.to_string())?;
    if !active.success() {
        return Err(format!(
            "Capture helper failed to start. Check: journalctl -u {service} -n 20"
        ));
    }
    println!("Enabled {service}. Aggregate-only capture continues after logout and reboot.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn service_keeps_privileged_code_root_owned_and_rejects_injected_arguments() {
        let text = unit(1000, Path::new("/home/user space/100%/capture.sock")).unwrap();
        assert!(text.contains("ExecStart=/usr/local/libexec/data-watch-capture-1000 packet-capture \"/home/user space/100%%/capture.sock\""));
        assert!(text.contains("NoNewPrivileges=true"));
        assert!(!text.contains("serve\n"));
        assert!(unit(0, Path::new("/tmp/test.sock")).is_err());
        assert!(unit(1000, Path::new("relative.sock")).is_err());
        assert!(unit(1000, Path::new("/tmp/a\nExecStart=bad")).is_err());
    }
}
