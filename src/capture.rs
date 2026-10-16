use crate::ownership::Owners;
use crate::packet::{self, Flow};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub type Counts = HashMap<String, (u64, u64)>;
const GRACE_SECONDS: f64 = 5.0;
const MAX_PENDING: usize = 4096;
const MAX_PROGRAMS: usize = 512;

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

#[derive(Default)]
pub struct Accounting {
    session: String,
    sequence: u64,
    timestamp: f64,
    received: f64,
    totals: Counts,
    pub stats: Value,
}

impl Accounting {
    pub fn active(&self, time: f64) -> bool {
        self.received > 0.0 && time - self.received < GRACE_SECONDS
    }

    /// Rebase across startup, helper restarts and fallback, never replay a gap
    /// already covered by TCP estimates. Cumulative frames tolerate IPC loss.
    pub fn ingest(&mut self, bytes: &[u8], interface: &str, time: f64) -> Option<Counts> {
        let frame: Value = serde_json::from_slice(bytes).ok()?;
        if frame.get("version")?.as_u64()? != 2 || frame.get("interface")?.as_str()? != interface {
            return None;
        }
        let timestamp = frame.get("ts")?.as_f64()?;
        if !timestamp.is_finite()
            || (timestamp - time).abs() > GRACE_SECONDS
            || timestamp <= self.timestamp
        {
            return None;
        }
        let session = frame.get("session")?.as_str()?;
        if session.is_empty() || session.len() > 64 {
            return None;
        }
        let sequence = frame.get("sequence")?.as_u64()?;
        if session == self.session && sequence <= self.sequence {
            return None;
        }
        let entries = frame.get("apps")?.as_object()?;
        if entries.len() > MAX_PROGRAMS + 1 {
            return None;
        }
        let mut totals = Counts::new();
        for (name, value) in entries {
            if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
                return None;
            }
            let pair = value.as_array()?;
            if pair.len() != 2 {
                return None;
            }
            totals.insert(name.clone(), (pair[0].as_u64()?, pair[1].as_u64()?));
        }
        let mut delta = Counts::new();
        if self.active(time) && self.session == session {
            for (name, (rx, tx)) in &totals {
                let (old_rx, old_tx) = self.totals.get(name).copied().unwrap_or_default();
                delta.insert(
                    name.clone(),
                    (rx.saturating_sub(old_rx), tx.saturating_sub(old_tx)),
                );
            }
        }
        self.session = session.to_string();
        self.sequence = sequence;
        self.timestamp = timestamp;
        self.received = time;
        self.totals = totals;
        self.stats = frame.get("stats").cloned().unwrap_or_else(|| json!({}));
        Some(delta)
    }
}

#[derive(Default)]
struct InterfaceUsage {
    apps: Counts,
    unmatched: [u64; 2],
    protocol_bytes: [u64; 4],
}

struct Pending {
    interface: i32,
    flow: Flow,
    outgoing: bool,
    bytes: u64,
    since: Instant,
}

#[derive(Default)]
struct Tracker {
    interfaces: HashMap<i32, InterfaceUsage>,
    pending: HashMap<(i32, Flow, bool), Pending>,
}

impl Tracker {
    fn add(&mut self, index: i32, name: Option<&str>, outgoing: bool, bytes: u64) {
        let usage = self.interfaces.entry(index).or_default();
        if let Some(name) = name {
            let name = if usage.apps.len() >= MAX_PROGRAMS && !usage.apps.contains_key(name) {
                "Other programs"
            } else {
                name
            };
            let pair = usage.apps.entry(name.to_string()).or_default();
            if outgoing {
                pair.1 = pair.1.saturating_add(bytes);
            } else {
                pair.0 = pair.0.saturating_add(bytes);
            }
        } else {
            let direction = usize::from(outgoing);
            usage.unmatched[direction] = usage.unmatched[direction].saturating_add(bytes);
        }
    }

    fn packet(
        &mut self,
        index: i32,
        flow: Option<Flow>,
        outgoing: bool,
        bytes: u64,
        owners: &Owners,
    ) {
        if let Some(flow) = flow {
            let direction = if flow.protocol == 6 { 0 } else { 2 } + usize::from(outgoing);
            let usage = self.interfaces.entry(index).or_default();
            usage.protocol_bytes[direction] = usage.protocol_bytes[direction].saturating_add(bytes);
            if let Some(name) = owners.lookup(&flow) {
                self.add(index, Some(name), outgoing, bytes);
                return;
            }
            let key = (index, flow.clone(), outgoing);
            if let Some(pending) = self.pending.get_mut(&key) {
                pending.bytes = pending.bytes.saturating_add(bytes);
                return;
            }
            if self.pending.len() < MAX_PENDING {
                self.pending.insert(
                    key,
                    Pending {
                        interface: index,
                        flow,
                        outgoing,
                        bytes,
                        since: Instant::now(),
                    },
                );
                return;
            }
        }
        self.add(index, None, outgoing, bytes);
    }

    fn resolve(&mut self, owners: &Owners) {
        let ready: Vec<_> = self
            .pending
            .iter()
            .filter_map(|(key, pending)| {
                let owner = owners.lookup(&pending.flow);
                (owner.is_some() || pending.since.elapsed() >= Duration::from_secs(3))
                    .then(|| (key.clone(), owner.map(str::to_owned)))
            })
            .collect();
        for (key, owner) in ready {
            if let Some(p) = self.pending.remove(&key) {
                self.add(p.interface, owner.as_deref(), p.outgoing, p.bytes);
            }
        }
    }
}

fn interfaces(requested: Option<&str>) -> HashMap<i32, String> {
    let Ok(entries) = fs::read_dir("/sys/class/net") else {
        return HashMap::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if requested.map_or(!entry.path().join("device").exists(), |r| r != name) {
                return None;
            }
            let index = fs::read_to_string(entry.path().join("ifindex"))
                .ok()?
                .trim()
                .parse()
                .ok()?;
            Some((index, name))
        })
        .collect()
}

fn packet_socket() -> Result<OwnedFd, String> {
    // SOCK_DGRAM removes link headers. MSG_TRUNC later obtains the full network
    // packet length while copying only a small header prefix into userspace.
    let raw = unsafe {
        libc::socket(
            libc::AF_PACKET,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            i32::from((libc::ETH_P_ALL as u16).to_be()),
        )
    };
    if raw < 0 {
        return Err(format!(
            "Open packet capture: {}. Run data-watch full to install the capture service.",
            io::Error::last_os_error()
        ));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let buffer_bytes: libc::c_int = 1_048_576;
    unsafe {
        libc::setsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            (&buffer_bytes as *const libc::c_int).cast(),
            std::mem::size_of_val(&buffer_bytes) as libc::socklen_t,
        );
    }
    Ok(fd)
}

fn dropped(fd: &OwnedFd) -> u64 {
    let mut stats = [0u32; 2];
    let mut length = std::mem::size_of_val(&stats) as libc::socklen_t;
    let ok = unsafe {
        libc::getsockopt(
            fd.as_raw_fd(),
            libc::SOL_PACKET,
            libc::PACKET_STATISTICS,
            stats.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if ok == 0 {
        u64::from(stats[1])
    } else {
        0
    }
}

pub fn run(socket: &Path, requested: Option<&str>) -> Result<(), String> {
    if !socket.is_absolute() {
        return Err("Capture socket must be an absolute path.".into());
    }
    if let Some(name) = requested {
        if name.is_empty()
            || name.len() > 15
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_-.:".contains(c))
        {
            return Err("Invalid interface.".into());
        }
    }
    let mut identity = std::collections::hash_map::DefaultHasher::new();
    socket.hash(&mut identity);
    let lock_name = format!("data-watch.capture.{:x}", identity.finish());
    let address = std::os::unix::net::SocketAddr::from_abstract_name(lock_name.as_bytes())
        .map_err(|e| e.to_string())?;
    let output = UnixDatagram::bind_addr(&address).map_err(|e| {
        format!("Capture helper already running for this socket, or unavailable: {e}")
    })?;
    let fd = packet_socket()?;
    output.set_nonblocking(true).map_err(|e| e.to_string())?;
    let session = fs::read_to_string("/proc/sys/kernel/random/uuid")
        .map_err(|e| e.to_string())?
        .trim()
        .to_string();
    let mut devices = interfaces(requested);
    let mut owners = Owners::default();
    owners.refresh()?;
    let mut tracker = Tracker::default();
    let mut refreshed = Instant::now();
    let mut device_scan = Instant::now();
    let mut emitted = Instant::now();
    let mut sequence = 0u64;
    let mut losses = 0u64;
    let mut header = [0u8; 128];
    eprintln!("Continuous TCP/UDP capture started; aggregate counts only.");
    loop {
        let mut poll = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut poll, 1, 50) };
        if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(io::Error::last_os_error().to_string());
        }
        if device_scan.elapsed() >= Duration::from_secs(5) {
            devices = interfaces(requested);
            let indexes: HashSet<_> = devices.keys().copied().collect();
            tracker.interfaces.retain(|i, _| indexes.contains(i));
            tracker.pending.retain(|(i, _, _), _| indexes.contains(i));
            device_scan = Instant::now();
        }
        for _ in 0..4096 {
            let mut address: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
            let mut length = std::mem::size_of_val(&address) as libc::socklen_t;
            let size = unsafe {
                libc::recvfrom(
                    fd.as_raw_fd(),
                    header.as_mut_ptr().cast(),
                    header.len(),
                    libc::MSG_TRUNC | libc::MSG_DONTWAIT,
                    (&mut address as *mut libc::sockaddr_ll).cast(),
                    &mut length,
                )
            };
            if size < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::WouldBlock {
                    break;
                }
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(format!("Read capture: {error}"));
            }
            if !devices.contains_key(&address.sll_ifindex)
                || !matches!(address.sll_pkttype, 0 | 1 | 2 | 4)
            {
                continue;
            }
            let protocol = u16::from_be(address.sll_protocol);
            if protocol != libc::ETH_P_IP as u16 && protocol != libc::ETH_P_IPV6 as u16 {
                continue;
            }
            let outgoing = address.sll_pkttype == libc::PACKET_OUTGOING;
            let size = size as usize;
            let flow = packet::flow(&header[..size.min(header.len())], outgoing);
            tracker.packet(address.sll_ifindex, flow, outgoing, size as u64, &owners);
        }
        let interval = if tracker.pending.is_empty() {
            Duration::from_secs(1)
        } else {
            Duration::from_millis(100)
        };
        if refreshed.elapsed() >= interval {
            owners.refresh()?;
            tracker.resolve(&owners);
            refreshed = Instant::now();
        }
        if emitted.elapsed() >= Duration::from_secs(1) {
            sequence = sequence.saturating_add(1);
            losses = losses.saturating_add(dropped(&fd));
            for (index, name) in &devices {
                let usage = tracker.interfaces.entry(*index).or_default();
                let frame = json!({"version":2,"session":format!("{session}-{index}"),"sequence":sequence,"ts":now(),"interface":name,"apps":usage.apps,"stats":{"dropped_packets":losses,"unmatched_rx":usage.unmatched[0],"unmatched_tx":usage.unmatched[1],"pending_flows":tracker.pending.len(),"tcp_rx":usage.protocol_bytes[0],"tcp_tx":usage.protocol_bytes[1],"udp_rx":usage.protocol_bytes[2],"udp_tx":usage.protocol_bytes[3]}});
                if let Ok(bytes) = serde_json::to_vec(&frame) {
                    let _ = output.send_to(&bytes, socket);
                }
            }
            emitted = Instant::now();
        }
    }
}

pub fn full_tracking(data: PathBuf) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let uid = unsafe { libc::getuid() };
    if uid == 0 {
        return Err("Run data-watch full as your normal desktop user; sudo will request authentication for installation.".into());
    }
    let socket = data.join("capture.sock");
    let socket = if socket.is_absolute() {
        socket
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(socket)
    };
    println!("Installing the persistent TCP/UDP capture helper. Enter your Ubuntu password locally if requested.");
    let result = std::process::Command::new("sudo")
        .arg(&executable)
        .arg("install-capture")
        .arg(uid.to_string())
        .arg(socket)
        .status()
        .map_err(|e| e.to_string())?;
    if !result.success() {
        return Err(
            "Capture service installation was not completed. Basic TCP tracking continues.".into(),
        );
    }
    println!("TCP/UDP packet tracking is enabled at boot. You can close this terminal.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(session: &str, seq: u64, time: f64, rx: u64) -> Vec<u8> {
        serde_json::to_vec(&json!({"version":2,"session":session,"sequence":seq,"ts":time,"interface":"wlo1","apps":{"chrome":[rx,20]}})).unwrap()
    }
    #[test]
    fn cumulative_frames_handle_loss_replay_restart_and_fallback() {
        let mut a = Accounting::default();
        assert!(a
            .ingest(&frame("a", 1, 10., 100), "wlo1", 10.)
            .unwrap()
            .is_empty());
        assert_eq!(
            a.ingest(&frame("a", 3, 12., 500), "wlo1", 12.).unwrap()["chrome"],
            (400, 0)
        );
        assert!(a.ingest(&frame("a", 3, 12., 500), "wlo1", 12.).is_none());
        assert!(a.ingest(&frame("a", 2, 11., 400), "wlo1", 12.).is_none());
        assert!(a
            .ingest(&frame("b", 1, 13., 300), "wlo1", 13.)
            .unwrap()
            .is_empty());
        assert_eq!(
            a.ingest(&frame("b", 2, 14., 350), "wlo1", 14.).unwrap()["chrome"].0,
            50
        );
        assert!(!a.active(20.));
        assert!(a
            .ingest(&frame("b", 3, 20., 900), "wlo1", 20.)
            .unwrap()
            .is_empty());
        assert!(a.ingest(&frame("b", 4, 21., 1000), "other", 21.).is_none());
        assert!(a.ingest(&frame("b", 4, 1., 1000), "wlo1", 21.).is_none());
        assert!(a.ingest(b"{}", "wlo1", 21.).is_none());
    }
    #[test]
    fn unresolved_packets_expire_without_becoming_fake_programs() {
        let mut tracker = Tracker::default();
        let owners = Owners::default();
        let flow = Flow {
            protocol: 17,
            local: "127.0.0.1:1234".parse().unwrap(),
            remote: "127.0.0.1:53".parse().unwrap(),
        };
        tracker.packet(1, Some(flow), true, 128, &owners);
        tracker.pending.values_mut().next().unwrap().since =
            Instant::now() - Duration::from_secs(4);
        tracker.resolve(&owners);
        assert!(tracker.pending.is_empty());
        assert!(tracker.interfaces[&1].apps.is_empty());
        assert_eq!(tracker.interfaces[&1].unmatched, [0, 128]);
    }
}
