use crate::capture::{Accounting, Counts};
use crate::storage::Store;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::IpAddr;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const POLL_SECONDS: u64 = 2;
const INTERFACE_REFRESH_SECONDS: f64 = 5.0;
const ADDRESS_REFRESH_SECONDS: f64 = 30.0;
const RETENTION_DAYS: u64 = 30;
const CAPTURE_NAME: &str = "capture.sock";

#[derive(Clone, Debug, PartialEq, Eq)]
struct SocketSample {
    name: String,
    rx: u64,
    tx: u64,
}

struct State {
    interface: String,
    selection: String,
    cached_interface: String,
    last_interface_scan: f64,
    last_address_scan: f64,
    previous_counters: Option<(u64, u64)>,
    previous_sockets: Option<HashMap<String, SocketSample>>,
    local_addresses: HashSet<String>,
    live: (f64, f64),
    rates: HashMap<String, (f64, f64)>,
    mode: &'static str,
    error: String,
    alert_threshold_bytes: u64,
    last_alert_attempt: f64,
    notifications_available: bool,
    capture_accounting: Accounting,
    packet_tracking_since: f64,
    started_at: f64,
}

/// Collects per-interface byte totals and best-effort per-program estimates.
///
/// The collector uses kernel interface counters for connection totals. Program
/// attribution is an estimate from public TCP socket counters unless a local
/// native packet helper is feeding `capture.sock`.
pub struct Collector {
    store: Arc<Mutex<Store>>,
    capture: UnixDatagram,
    state: Mutex<State>,
}

impl Collector {
    pub fn start(store: Arc<Mutex<Store>>, data: PathBuf) -> Result<Arc<Self>, String> {
        fs::create_dir_all(&data).map_err(|e| format!("Cannot create data directory: {e}"))?;
        let socket_path = data.join(CAPTURE_NAME);
        let _ = fs::remove_file(&socket_path);
        let capture = UnixDatagram::bind(&socket_path)
            .map_err(|e| format!("Cannot create capture socket: {e}"))?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("Cannot secure capture socket: {e}"))?;
        capture
            .set_nonblocking(true)
            .map_err(|e| format!("Cannot configure capture socket: {e}"))?;

        let (selection, alert_threshold_bytes, packet_tracking_since) = {
            let store = store
                .lock()
                .map_err(|_| "Storage lock is unavailable.".to_string())?;
            let selection = store.setting("interface", "auto")?;
            let threshold = store
                .setting("alert_threshold_bytes", "0")?
                .parse::<u64>()
                .unwrap_or(0);
            let packet_since = store
                .setting("packet_tracking_since", "0")?
                .parse::<f64>()
                .unwrap_or(0.0);
            (selection, threshold, packet_since)
        };
        if selection != "auto" && !is_physical_interface(&selection) {
            let mut locked = store
                .lock()
                .map_err(|_| "Storage lock is unavailable.".to_string())?;
            locked.set_setting("interface", "auto")?;
        }
        let selection = if selection != "auto" && is_physical_interface(&selection) {
            selection
        } else {
            "auto".to_string()
        };
        let now = unix_time();
        let collector = Arc::new(Self {
            store,
            capture,
            state: Mutex::new(State {
                interface: String::new(),
                selection,
                cached_interface: String::new(),
                last_interface_scan: 0.0,
                last_address_scan: 0.0,
                previous_counters: None,
                previous_sockets: None,
                local_addresses: HashSet::new(),
                live: (0.0, 0.0),
                rates: HashMap::new(),
                mode: "tcp-estimate",
                error: String::new(),
                alert_threshold_bytes,
                last_alert_attempt: 0.0,
                notifications_available: notification_available(),
                capture_accounting: Accounting::default(),
                packet_tracking_since,
                started_at: now,
            }),
        });
        let running = Arc::clone(&collector);
        thread::Builder::new()
            .name("data-watch-collector".into())
            .spawn(move || running.poll_loop())
            .map_err(|e| format!("Cannot start collector thread: {e}"))?;
        Ok(collector)
    }

    pub fn status(&self, period: &str) -> Result<Value, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "Collector state is unavailable.".to_string())?;
        let now = unix_time();
        let mut result = self
            .store
            .lock()
            .map_err(|_| "Storage lock is unavailable.".to_string())?
            .history(period, &state.interface, now)?;
        let obj = result
            .as_object_mut()
            .ok_or_else(|| "Storage returned an invalid history response.".to_string())?;
        let devices = Self::interfaces();
        let totals = obj
            .get("totals")
            .cloned()
            .unwrap_or_else(|| json!({"rx": 0, "tx": 0}));
        if let Some(app_rows) = obj.get_mut("apps").and_then(Value::as_array_mut) {
            for app in app_rows {
                if let Some(name) = app.get("name").and_then(Value::as_str) {
                    let (rx, tx) = state.rates.get(name).copied().unwrap_or_default();
                    if let Some(app) = app.as_object_mut() {
                        app.insert("rate_rx".into(), json!(rx));
                        app.insert("rate_tx".into(), json!(tx));
                    }
                }
            }
        }
        let app_rows = obj
            .get("apps")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut attributed_rx = 0u64;
        let mut attributed_tx = 0u64;
        for app in &app_rows {
            attributed_rx =
                attributed_rx.saturating_add(app.get("rx").and_then(Value::as_u64).unwrap_or(0));
            attributed_tx =
                attributed_tx.saturating_add(app.get("tx").and_then(Value::as_u64).unwrap_or(0));
        }
        let total_rx = totals.get("rx").and_then(Value::as_u64).unwrap_or(0);
        let total_tx = totals.get("tx").and_then(Value::as_u64).unwrap_or(0);
        obj.insert("now".into(), json!(now));
        obj.insert("started_at".into(), json!(state.started_at));
        obj.insert("interface".into(), json!(state.interface));
        obj.insert("interface_selection".into(), json!(state.selection));
        obj.insert("mode".into(), json!(state.mode));
        obj.insert("capture".into(), json!({"active":state.capture_accounting.active(now), "stats":state.capture_accounting.stats}));
        obj.insert(
            "packet_tracking_since".into(),
            if state.packet_tracking_since > 0.0 {
                json!(state.packet_tracking_since)
            } else {
                Value::Null
            },
        );
        obj.insert("collector_error".into(), json!(state.error));
        obj.insert("live".into(), json!({"rx":state.live.0,"tx":state.live.1}));
        obj.insert("interfaces".into(), json!(devices));
        obj.insert("retention_days".into(), json!(RETENTION_DAYS));
        obj.insert(
            "alerts".into(),
            json!({
                "threshold_bytes": state.alert_threshold_bytes,
                "notifications_available": state.notifications_available
            }),
        );
        obj.insert("warning".into(), json!("Earlier program history may contain incomplete TCP estimates. Packet tracking counts IP bytes. Link headers, local traffic, VPNs and containers can affect attribution."));
        obj.insert(
            "unattributed".into(),
            json!({
                "rx": total_rx.saturating_sub(attributed_rx),
                "tx": total_tx.saturating_sub(attributed_tx)
            }),
        );
        Ok(result)
    }

    pub fn choose(&self, name: &str) -> Result<(), String> {
        if name != "auto" && !is_physical_interface(name) {
            return Err("Choose an available physical network connection.".into());
        }
        self.store
            .lock()
            .map_err(|_| "Storage lock is unavailable.".to_string())?
            .set_setting("interface", name)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Collector state is unavailable.".to_string())?;
        state.selection = name.to_string();
        state.last_interface_scan = 0.0;
        state.previous_counters = None;
        state.previous_sockets = None;
        state.capture_accounting = Accounting::default();
        state.live = (0.0, 0.0);
        state.rates.clear();
        Ok(())
    }

    pub fn set_alert_threshold(&self, bytes: u64) -> Result<(), String> {
        self.store
            .lock()
            .map_err(|_| "Storage lock is unavailable.".to_string())?
            .set_setting("alert_threshold_bytes", &bytes.to_string())?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Collector state is unavailable.".to_string())?;
        state.alert_threshold_bytes = bytes;
        state.last_alert_attempt = 0.0;
        self.store
            .lock()
            .map_err(|_| "Storage lock is unavailable.".to_string())?
            .set_setting("alert_last_notified_day", "")
    }

    pub fn test_notification(&self) -> Result<(), String> {
        send_desktop_notification(
            "Test notification",
            "Data Watch desktop notifications are working.",
        )
    }

    pub fn interfaces() -> Vec<Value> {
        let mut result = Vec::new();
        let Ok(entries) = fs::read_dir("/sys/class/net") else {
            return result;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "lo" || !entry.path().join("device").exists() {
                continue;
            }
            let active = fs::read_to_string(entry.path().join("operstate"))
                .map(|s| s.trim() == "up")
                .unwrap_or(false);
            result.push(json!({"name":name,"active":active}));
        }
        result.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        result
    }

    fn poll_loop(self: Arc<Self>) {
        let mut last_tick = std::time::Instant::now();
        let mut prune_at = 0.0;
        loop {
            let now = unix_time();
            let tick = std::time::Instant::now();
            let elapsed = tick.duration_since(last_tick).as_secs_f64().max(0.1);
            let result = self.sample(now, elapsed);
            if now >= prune_at {
                if let Ok(mut store) = self.store.lock() {
                    let _ = store.prune(now);
                }
                prune_at = now + 3600.0;
            }
            if let Err(error) = result {
                if let Ok(mut state) = self.state.lock() {
                    state.error = error;
                    state.previous_counters = None;
                    state.previous_sockets = None;
                    state.live = (0.0, 0.0);
                    state.rates.clear();
                }
            }
            last_tick = tick;
            thread::sleep(Duration::from_secs(POLL_SECONDS));
        }
    }

    fn sample(&self, now: f64, elapsed: f64) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Collector state is unavailable.".to_string())?;
        let chosen = if state.selection == "auto" {
            if now - state.last_interface_scan >= INTERFACE_REFRESH_SECONDS {
                state.cached_interface = default_interface();
                state.last_interface_scan = now;
            }
            state.cached_interface.clone()
        } else if is_physical_interface(&state.selection) {
            state.selection.clone()
        } else {
            String::new()
        };
        if chosen.is_empty() {
            state.interface.clear();
            state.local_addresses.clear();
            state.last_address_scan = now;
            state.previous_counters = None;
            state.previous_sockets = None;
            state.live = (0.0, 0.0);
            state.rates.clear();
            state.mode = "tcp-estimate";
            state.capture_accounting = Accounting::default();
            state.error =
                "No active physical connection. Monitoring resumes when you reconnect.".into();
            return Ok(());
        }
        if state.interface != chosen {
            state.interface = chosen.clone();
            state.local_addresses = interface_addresses(&chosen);
            state.last_address_scan = now;
            state.previous_counters = None;
            state.previous_sockets = None;
            state.capture_accounting = Accounting::default();
            state.rates.clear();
        }

        let counters = interface_counters(&chosen)?;
        if now - state.last_address_scan >= ADDRESS_REFRESH_SECONDS {
            state.local_addresses = interface_addresses(&chosen);
            state.last_address_scan = now;
        }
        let packet_apps = self.capture_packets(&chosen, now, &mut state)?;
        let packet_active = state.capture_accounting.active(now);
        state.mode = if packet_active {
            "packet-capture"
        } else {
            "tcp-estimate"
        };
        let mut ss_ok = true;
        let (apps, current_sockets) = if packet_active {
            if state.packet_tracking_since == 0.0 {
                self.store
                    .lock()
                    .map_err(|_| "Storage lock is unavailable.")?
                    .set_setting("packet_tracking_since", &now.to_string())?;
                state.packet_tracking_since = now;
            }
            // Seed TCP counters again on fallback; never count old TCP bytes
            // that have already been recorded by packet capture.
            (packet_apps, None)
        } else {
            match run_command("ss", &["-HtinpeO"]) {
                Ok(output) => {
                    let current = parse_ss(&output, &state.local_addresses);
                    (
                        socket_deltas(state.previous_sockets.as_ref(), &current),
                        Some(current),
                    )
                }
                Err(error) => {
                    ss_ok = false;
                    state.error = format!("App tracking temporarily unavailable: {error}. Connection totals still work.");
                    (HashMap::new(), None)
                }
            }
        };
        let (rx, tx) = match state.previous_counters {
            Some(old) => (
                counters.0.saturating_sub(old.0),
                counters.1.saturating_sub(old.1),
            ),
            None => (0, 0),
        };
        state.live = (rx as f64 / elapsed, tx as f64 / elapsed);
        state.rates = apps
            .iter()
            .map(|(name, (app_rx, app_tx))| {
                (
                    name.clone(),
                    (*app_rx as f64 / elapsed, *app_tx as f64 / elapsed),
                )
            })
            .collect();
        state.previous_counters = Some(counters);
        state.previous_sockets = current_sockets;
        if ss_ok {
            let drops = state
                .capture_accounting
                .stats
                .get("dropped_packets")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            state.error = if packet_active && drops > 0 {
                format!("The packet helper has dropped {drops} packets across monitored interfaces since startup; program totals may be incomplete.")
            } else {
                String::new()
            };
        }
        self.store
            .lock()
            .map_err(|_| "Storage lock is unavailable.".to_string())?
            .record(now, &chosen, rx, tx, &apps)?;
        if state.alert_threshold_bytes > 0
            && now - state.last_alert_attempt >= 60.0
            && state.notifications_available
        {
            let day = crate::storage::local_day_key(now)?;
            let alert = {
                let store = self
                    .store
                    .lock()
                    .map_err(|_| "Storage lock is unavailable.".to_string())?;
                let total = store.today_total(now, &chosen)?;
                let last_notified_day = store.setting("alert_last_notified_day", "")?;
                alert_due(
                    total,
                    state.alert_threshold_bytes,
                    &day,
                    &last_notified_day,
                    state.last_alert_attempt,
                    now,
                )
                .then_some(total)
            };
            if let Some(total) = alert {
                state.last_alert_attempt = now;
                let message = format!(
                    "{:.1} MiB used today on {}. Your alert limit is {:.1} MiB.",
                    total as f64 / 1_048_576.0,
                    chosen,
                    state.alert_threshold_bytes as f64 / 1_048_576.0
                );
                if send_desktop_notification("Daily data limit reached", &message).is_ok() {
                    self.store
                        .lock()
                        .map_err(|_| "Storage lock is unavailable.".to_string())?
                        .set_setting("alert_last_notified_day", &day)?;
                }
            }
        }
        Ok(())
    }

    fn capture_packets(
        &self,
        interface: &str,
        now: f64,
        state: &mut State,
    ) -> Result<HashMap<String, (u64, u64)>, String> {
        let mut apps: Counts = HashMap::new();
        let mut buffer = [0u8; 65_536];
        for _ in 0..64 {
            match self.capture.recv(&mut buffer) {
                Ok(size) => {
                    if let Some(delta) =
                        state
                            .capture_accounting
                            .ingest(&buffer[..size], interface, now)
                    {
                        for (name, (rx, tx)) in delta {
                            let item = apps.entry(name).or_default();
                            item.0 = item.0.saturating_add(rx);
                            item.1 = item.1.saturating_add(tx);
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(format!("Cannot read capture socket: {error}")),
            }
        }
        Ok(apps)
    }
}

fn alert_due(
    total: u64,
    threshold: u64,
    day: &str,
    last_notified_day: &str,
    last_attempt: f64,
    now: f64,
) -> bool {
    threshold > 0 && total >= threshold && day != last_notified_day && now - last_attempt >= 60.0
}

fn notification_available() -> bool {
    Command::new("notify-send")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn send_desktop_notification(title: &str, message: &str) -> Result<(), String> {
    let uid = unsafe { libc::geteuid() };
    let result = Command::new("notify-send")
        .args([
            "--app-name=Data Watch",
            "--urgency=critical",
            "--expire-time=15000",
            "--hint=string:sound-name:alarm-clock-elapsed",
            title,
            message,
        ])
        .env("XDG_RUNTIME_DIR", format!("/run/user/{uid}"))
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path=/run/user/{uid}/bus"),
        )
        .status()
        .map_err(|error| {
            format!("Could not send desktop notification: {error}. Install libnotify-bin.")
        })?;
    if result.success() {
        Ok(())
    } else {
        Err("Desktop notification service is unavailable.".into())
    }
}

fn unix_time() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn run_command(command: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(command)
        .args(args)
        .output()
        .map_err(|e| format!("{command}: {e}"))?;
    if !output.status.success() {
        return Err(format!("{command} exited with status {}", output.status));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("{command} returned invalid UTF-8: {e}"))
}

fn interface_counters(name: &str) -> Result<(u64, u64), String> {
    let base = Path::new("/sys/class/net").join(name).join("statistics");
    let read = |key: &str| -> Result<u64, String> {
        fs::read_to_string(base.join(key))
            .map_err(|e| format!("Cannot read {name} {key}: {e}"))?
            .trim()
            .parse::<u64>()
            .map_err(|e| format!("Invalid {name} {key}: {e}"))
    };
    Ok((read("rx_bytes")?, read("tx_bytes")?))
}

fn is_physical_interface(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && name != "lo"
        && Path::new("/sys/class/net")
            .join(name)
            .join("device")
            .exists()
}

fn default_interface() -> String {
    let interfaces = Collector::interfaces();
    let physical: HashSet<String> = interfaces
        .iter()
        .filter_map(|item| item["name"].as_str().map(str::to_owned))
        .collect();
    for family in ["-4", "-6"] {
        let Ok(output) = run_command("ip", &["-j", family, "route", "show", "default"]) else {
            continue;
        };
        let Ok(routes) = serde_json::from_str::<Value>(&output) else {
            continue;
        };
        let Some(routes) = routes.as_array() else {
            continue;
        };
        let mut matching: Vec<(u64, String)> = routes
            .iter()
            .filter_map(|route| {
                let dev = route.get("dev")?.as_str()?;
                if !physical.contains(dev) {
                    return None;
                }
                Some((
                    route.get("metric").and_then(Value::as_u64).unwrap_or(0),
                    dev.to_owned(),
                ))
            })
            .collect();
        matching.sort_by(|a, b| a.0.cmp(&b.0));
        if let Some((_, dev)) = matching.into_iter().next() {
            return dev;
        }
    }
    interfaces
        .iter()
        .find_map(|d| {
            if d["active"].as_bool().unwrap_or(false) {
                d["name"].as_str().map(str::to_owned)
            } else {
                None
            }
        })
        .unwrap_or_default()
}

fn interface_addresses(name: &str) -> HashSet<String> {
    let Ok(output) = run_command("ip", &["-j", "address", "show", "dev", name]) else {
        return HashSet::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&output) else {
        return HashSet::new();
    };
    let mut result = HashSet::new();
    if let Some(items) = value.as_array() {
        for item in items {
            if let Some(addresses) = item.get("addr_info").and_then(Value::as_array) {
                for address in addresses {
                    if let Some(local) = address.get("local").and_then(Value::as_str) {
                        if let Ok(ip) = local.split('%').next().unwrap_or(local).parse::<IpAddr>() {
                            result.insert(normalized_ip(ip));
                        }
                    }
                }
            }
        }
    }
    result
}

fn normalized_ip(ip: IpAddr) -> String {
    match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(|v4| v4.to_string())
            .unwrap_or_else(|| v6.to_string()),
        IpAddr::V4(v4) => v4.to_string(),
    }
}

fn endpoint_address(value: &str) -> Option<IpAddr> {
    let mut host = value.rsplit_once(':')?.0.trim();
    if host.starts_with('[') && host.ends_with(']') {
        host = &host[1..host.len() - 1];
    }
    host = host.split('%').next()?;
    host.parse::<IpAddr>().ok()
}

fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 168)
                || (a == 192 && b == 0 && (c == 0 || c == 2))
                || (a == 198 && (b == 18 || b == 19 || b == 51 && c == 100))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let s = ip.segments();
            (s[0] & 0xe000) == 0x2000
                && !ip.is_loopback()
                && !ip.is_unspecified()
                && !(s[0] == 0x2001 && (s[1] == 0x0db8 || s[1] == 0x0000))
                && s[0] != 0x2002
        }
    }
}

/// Parse `ss -HtinpeO`/`ss -HtineO` output into stable socket-keyed byte counters.
fn parse_ss(output: &str, local_addresses: &HashSet<String>) -> HashMap<String, SocketSample> {
    let mut sockets = HashMap::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 5 {
            continue;
        }
        let (Some(local), Some(remote)) =
            (endpoint_address(fields[3]), endpoint_address(fields[4]))
        else {
            continue;
        };
        let local = normalized_ip(local);
        if !local_addresses.contains(&local) || !is_public(remote) {
            continue;
        }
        let cookie = field_value(line, "sk:").or_else(|| field_value(line, "ino:"));
        let key = format!("{}|{}|{}", fields[3], fields[4], cookie.unwrap_or_default());
        let name = parse_process_name(line).unwrap_or_default();
        let received = field_number(line, "bytes_received:").unwrap_or(0);
        let sent = field_number(line, "bytes_sent:").unwrap_or(0);
        sockets.insert(
            key,
            SocketSample {
                name: name.chars().take(128).collect(),
                rx: received,
                tx: sent,
            },
        );
    }
    sockets
}

fn field_value<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let start = line.find(prefix)? + prefix.len();
    let value = line[start..]
        .split(|c: char| c.is_whitespace() || c == ',' || c == ')')
        .next()?;
    (!value.is_empty()).then_some(value)
}

fn field_number(line: &str, prefix: &str) -> Option<u64> {
    field_value(line, prefix)?.parse().ok()
}

fn parse_process_name(line: &str) -> Option<String> {
    let start = line.find("users:((")? + "users:((".len();
    let rest = &line[start..];
    let quote = rest.find('"')? + 1;
    let after = &rest[quote..];
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

fn socket_deltas(
    previous: Option<&HashMap<String, SocketSample>>,
    current: &HashMap<String, SocketSample>,
) -> HashMap<String, (u64, u64)> {
    let mut apps = HashMap::new();
    let Some(previous) = previous else {
        return apps;
    };
    for (key, sample) in current {
        if sample.name.is_empty() {
            continue;
        }
        let (old_rx, old_tx) = previous
            .get(key)
            .map(|old| (old.rx, old.tx))
            .unwrap_or_default();
        let rx = sample.rx.saturating_sub(old_rx);
        let tx = sample.tx.saturating_sub(old_tx);
        if rx + tx == 0 {
            continue;
        }
        let entry = apps.entry(sample.name.clone()).or_insert((0u64, 0u64));
        entry.0 = entry.0.saturating_add(rx);
        entry.1 = entry.1.saturating_add(tx);
    }
    apps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_alert_fires_once_after_crossing_and_retries_slowly() {
        assert!(alert_due(100, 100, "2026-289", "", 0.0, 1000.0));
        assert!(!alert_due(99, 100, "2026-289", "", 0.0, 1000.0));
        assert!(!alert_due(100, 100, "2026-289", "2026-289", 0.0, 1000.0));
        assert!(!alert_due(100, 100, "2026-289", "", 950.0, 1000.0));
    }

    #[test]
    fn parses_ss_with_ipv4_ipv6_and_cookie() {
        let output = "ESTAB 0 0 192.168.1.2:54321 8.8.8.8:443 users:((\"firefox\",pid=12,fd=4)) sk:abc123 bytes_received:100 bytes_sent:70\nESTAB 0 0 [2001:db8::1]:1234 [2001:db8::2]:443 users:((\"private\",pid=2,fd=3)) sk:bad bytes_received:5 bytes_sent:8\n";
        let addresses = HashSet::from(["192.168.1.2".to_string(), "2001:db8::1".to_string()]);
        let parsed = parse_ss(output, &addresses);
        assert_eq!(parsed.len(), 1);
        let sample = parsed.values().next().unwrap();
        assert_eq!(sample.name, "firefox");
        assert_eq!((sample.rx, sample.tx), (100, 70));
    }

    #[test]
    fn socket_deltas_count_new_connections_but_baseline_startup_and_resets() {
        let old = HashMap::from([(
            "a".into(),
            SocketSample {
                name: "browser".into(),
                rx: 30,
                tx: 8,
            },
        )]);
        let current = HashMap::from([
            (
                "a".into(),
                SocketSample {
                    name: "browser".into(),
                    rx: 50,
                    tx: 6,
                },
            ),
            (
                "b".into(),
                SocketSample {
                    name: "other".into(),
                    rx: 50,
                    tx: 50,
                },
            ),
        ]);
        let delta = socket_deltas(Some(&old), &current);
        assert_eq!(delta.get("browser"), Some(&(20, 0)));
        assert_eq!(delta.get("other"), Some(&(50, 50)));
        assert!(socket_deltas(None, &current).is_empty());
    }

    #[test]
    fn unnamed_startup_sockets_do_not_backfill_old_bytes_when_owner_appears() {
        let unnamed =
            "ESTAB 0 0 192.168.1.2:1234 8.8.8.8:443 sk:a bytes_received:1000 bytes_sent:500";
        let named = "ESTAB 0 0 192.168.1.2:1234 8.8.8.8:443 users:((\"chrome\",pid=1,fd=2)) sk:a bytes_received:1100 bytes_sent:550";
        let addresses = HashSet::from(["192.168.1.2".to_string()]);
        let old = parse_ss(unnamed, &addresses);
        let current = parse_ss(named, &addresses);
        assert_eq!(socket_deltas(Some(&old), &current)["chrome"], (100, 50));
    }
}
