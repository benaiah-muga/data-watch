mod capture;
mod capture_install;
mod collector;
mod ownership;
mod packet;
mod storage;

use serde_json::{json, Value};
use std::fs;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tiny_http::{Header, Method, Response, Server, StatusCode};

const PORT: u16 = 8765;
const URL: &str = "http://127.0.0.1:8765";
const APP: &str = "data-watch";
const INDEX_HTML: &[u8] = include_bytes!("../static/index.html");
const APP_JS: &[u8] = include_bytes!("../static/app.js");
const APP_CSS: &[u8] = include_bytes!("../static/app.css");
const FAVICON: &[u8] = include_bytes!("../static/favicon.svg");
const FONT: &[u8] = include_bytes!("../static/fonts/ibm-plex-sans-latin.woff2");

fn data_dir() -> PathBuf {
    std::env::var_os("DATA_WATCH_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                .join(".local/share/data-watch")
        })
}

fn local_response(
    status: u16,
    body: Vec<u8>,
    mime: &str,
    attachment: Option<&str>,
) -> Response<io::Cursor<Vec<u8>>> {
    let status = StatusCode(status);
    let mut headers = vec![
        Header::from_bytes("Content-Type", mime).unwrap(),
        Header::from_bytes("Cache-Control", "no-store").unwrap(),
        Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap(),
        Header::from_bytes("Content-Security-Policy", "default-src 'self'; style-src 'self'; script-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'").unwrap(),
    ];
    if let Some(name) = attachment {
        headers.push(
            Header::from_bytes(
                "Content-Disposition",
                format!("attachment; filename=\"{name}\""),
            )
            .unwrap(),
        );
    }
    Response::new(status, headers, io::Cursor::new(body), None, None)
}

fn json_response(status: u16, value: Value) -> Response<io::Cursor<Vec<u8>>> {
    local_response(
        status,
        serde_json::to_vec(&value).unwrap_or_default(),
        "application/json; charset=utf-8",
        None,
    )
}

fn quoted_csv(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn csv_response(value: &Value, period: &str) -> Response<io::Cursor<Vec<u8>>> {
    let mut csv = String::from("local_time,connection,program,download_bytes,upload_bytes\r\n");
    let interface = value["history_interface"]
        .as_str()
        .or_else(|| value["interface"].as_str())
        .unwrap_or("");
    if let Some(series) = value["series"].as_array() {
        for slot in series {
            let ts = slot["ts"].as_i64().unwrap_or_default();
            let local = libc_timestamp(ts);
            let rx = slot["rx"].as_u64().unwrap_or_default();
            let tx = slot["tx"].as_u64().unwrap_or_default();
            csv.push_str(&format!(
                "{},{},CONNECTION TOTAL,{rx},{tx}\r\n",
                quoted_csv(&local),
                quoted_csv(interface)
            ));
            if let Some(apps) = slot["apps"].as_array() {
                for app in apps {
                    let raw = app["name"].as_str().unwrap_or("Unknown");
                    let name = if raw.starts_with(['=', '+', '-', '@', '\t', '\r']) {
                        format!("'{raw}")
                    } else {
                        raw.to_owned()
                    };
                    csv.push_str(&format!(
                        "{},{},{},{},{}\r\n",
                        quoted_csv(&local),
                        quoted_csv(interface),
                        quoted_csv(&name),
                        app["rx"].as_u64().unwrap_or_default(),
                        app["tx"].as_u64().unwrap_or_default()
                    ));
                }
            }
        }
    }
    local_response(
        200,
        csv.into_bytes(),
        "text/csv; charset=utf-8",
        Some(&format!("data-watch-{period}.csv")),
    )
}

fn libc_timestamp(timestamp: i64) -> String {
    unsafe {
        let raw: libc::time_t = timestamp as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&raw, &mut tm).is_null() {
            return timestamp.to_string();
        }
        let mut output = [0 as libc::c_char; 64];
        let fmt = b"%Y-%m-%dT%H:%M:%S%:z\0";
        let size = libc::strftime(output.as_mut_ptr(), output.len(), fmt.as_ptr().cast(), &tm);
        std::ffi::CStr::from_ptr(output.as_ptr())
            .to_string_lossy()
            .into_owned()
            .chars()
            .take(size)
            .collect()
    }
}

fn header(request: &tiny_http::Request, key: &str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(key))
        .map(|h| h.value.as_str().to_owned())
}

fn serve_http(collector: Arc<collector::Collector>, port: u16) -> Result<(), String> {
    let server = Server::http(("127.0.0.1", port))
        .map_err(|e| format!("Could not listen on localhost: {e}"))?;
    let actual_port = server
        .server_addr()
        .to_ip()
        .map(|addr| addr.port())
        .unwrap_or(port);
    println!("Data Watch: http://127.0.0.1:{actual_port}");
    loop {
        let mut request = match server.recv_timeout(Duration::from_millis(500)) {
            Ok(Some(request)) => request,
            Ok(None) => continue,
            Err(error) => {
                eprintln!("HTTP server: {error}");
                continue;
            }
        };
        let host = header(&request, "Host").unwrap_or_default();
        let host_ok = host == format!("127.0.0.1:{actual_port}")
            || host == format!("localhost:{actual_port}");
        let remote_ok = request
            .remote_addr()
            .map(|a| a.ip().is_loopback())
            .unwrap_or(false);
        let method = request.method().clone();
        let url = request.url().to_owned();
        let response = if !host_ok || !remote_ok {
            json_response(403, json!({"error":"Local access only."}))
        } else if method == Method::Get {
            let (path, query) = url.split_once('?').unwrap_or((&url, ""));
            let period = query
                .split('&')
                .find_map(|pair| pair.strip_prefix("range="))
                .unwrap_or("today");
            match path {
                "/api/status" | "/api/export" => match collector.status(period) {
                    Ok(value) if path == "/api/export" => csv_response(&value, period),
                    Ok(value) => json_response(200, value),
                    Err(error) => json_response(400, json!({"error":error})),
                },
                "/"
                | "/index.html"
                | "/app.js"
                | "/app.css"
                | "/favicon.svg"
                | "/fonts/ibm-plex-sans-latin.woff2" => {
                    let (contents, content_type) = match path {
                        "/" | "/index.html" => (INDEX_HTML, "text/html; charset=utf-8"),
                        "/app.js" => (APP_JS, "text/javascript; charset=utf-8"),
                        "/favicon.svg" => (FAVICON, "image/svg+xml"),
                        "/fonts/ibm-plex-sans-latin.woff2" => (FONT, "font/woff2"),
                        _ => (APP_CSS, "text/css; charset=utf-8"),
                    };
                    local_response(200, contents.to_vec(), content_type, None)
                }
                _ => json_response(404, json!({"error":"Not found."})),
            }
        } else if method == Method::Post && url == "/api/interface" {
            let same_origin = header(&request, "Origin")
                .map(|o| o == format!("http://{host}"))
                .unwrap_or(true);
            let custom_header = header(&request, "X-Data-Watch").as_deref() == Some("1");
            if !same_origin || !custom_header {
                json_response(403, json!({"error":"Local dashboard requests only."}))
            } else {
                let mut body = String::new();
                match request.as_reader().take(1025).read_to_string(&mut body) {
                    Ok(count) if count > 0 && count <= 1024 => {
                        match serde_json::from_str::<Value>(&body) {
                            Ok(value) => match value.get("interface").and_then(Value::as_str) {
                                Some(name) => match collector.choose(name) {
                                    Ok(()) => json_response(200, json!({"ok":true})),
                                    Err(error) => json_response(400, json!({"error":error})),
                                },
                                None => json_response(
                                    400,
                                    json!({"error":"Choose a network connection."}),
                                ),
                            },
                            Err(_) => json_response(400, json!({"error":"Invalid JSON."})),
                        }
                    }
                    _ => json_response(400, json!({"error":"Invalid request size."})),
                }
            }
        } else if method == Method::Post && (url == "/api/alerts" || url == "/api/alerts/test") {
            let same_origin = header(&request, "Origin")
                .map(|o| o == format!("http://{host}"))
                .unwrap_or(true);
            let custom_header = header(&request, "X-Data-Watch").as_deref() == Some("1");
            if !same_origin || !custom_header {
                json_response(403, json!({"error":"Local dashboard requests only."}))
            } else if url == "/api/alerts/test" {
                match collector.test_notification() {
                    Ok(()) => json_response(200, json!({"ok":true})),
                    Err(error) => json_response(503, json!({"error":error})),
                }
            } else {
                let mut body = String::new();
                match request.as_reader().take(1025).read_to_string(&mut body) {
                    Ok(count) if count > 0 && count <= 1024 => {
                        match serde_json::from_str::<Value>(&body)
                            .ok()
                            .and_then(|value| value.get("threshold_bytes")?.as_u64())
                        {
                            Some(threshold) if threshold <= 1_099_511_627_776 => {
                                match collector.set_alert_threshold(threshold) {
                                    Ok(()) => json_response(200, json!({"ok":true})),
                                    Err(error) => json_response(500, json!({"error":error})),
                                }
                            }
                            _ => json_response(
                                400,
                                json!({"error":"Choose an alert limit from 0 to 1 TiB."}),
                            ),
                        }
                    }
                    _ => json_response(400, json!({"error":"Invalid request size."})),
                }
            }
        } else {
            json_response(404, json!({"error":"Not found."}))
        };
        if let Err(error) = request.respond(response) {
            eprintln!("HTTP response: {error}");
        }
    }
}

fn install() -> Result<(), String> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?);
    let current_binary = std::env::current_exe().map_err(|e| e.to_string())?;
    let bin = home.join(".local/bin");
    let apps = home.join(".local/share/applications");
    let icons = home.join(".local/share/icons/hicolor/scalable/apps");
    let units = home.join(".config/systemd/user");
    let data = data_dir();
    for dir in [&bin, &apps, &icons, &units, &data] {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    use std::os::unix::fs::PermissionsExt;
    let binary = bin.join(APP);
    if current_binary != binary {
        let staging = bin.join(format!("{APP}.new-{}", std::process::id()));
        fs::copy(&current_binary, &staging).map_err(|e| format!("Copy executable: {e}"))?;
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Set executable permissions: {e}"))?;
        fs::rename(&staging, &binary).map_err(|e| format!("Install executable: {e}"))?;
    }
    fs::set_permissions(&data, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    let service = format!("[Unit]\nDescription=Data Watch local traffic history\nAfter=network.target\n\n[Service]\nType=simple\nExecStart={} serve\nRestart=always\nRestartSec=5\nUMask=0077\nNoNewPrivileges=true\nPrivateTmp=true\nProtectSystem=strict\nProtectHome=read-only\nReadWritePaths={}\n\n[Install]\nWantedBy=default.target\n", binary.display(), data.display());
    fs::write(units.join("data-watch.service"), service).map_err(|e| e.to_string())?;
    fs::write(icons.join("data-watch.svg"), FAVICON).map_err(|e| e.to_string())?;
    let desktop = apps.join("data-watch.desktop");
    fs::write(&desktop, format!("[Desktop Entry]\nType=Application\nName=Data Watch\nComment=See which programs use your mobile data\nExec=\"{}\" open\nIcon=data-watch\nTerminal=false\nStartupNotify=true\nStartupWMClass=DataWatch\nCategories=Network;\nKeywords=bandwidth;traffic;hotspot;\nActions=FullTracking;\n\n[Desktop Action FullTracking]\nName=Enable persistent TCP + UDP tracking (Ubuntu password)\nExec=gnome-terminal -- \"{}\" full\n", binary.display(), binary.display())).map_err(|e| e.to_string())?;
    fs::set_permissions(&desktop, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    let _ = Command::new("update-desktop-database").arg(&apps).status();
    for args in [
        vec!["--user", "daemon-reload"],
        vec!["--user", "enable", "--now", "data-watch.service"],
        vec!["--user", "restart", "data-watch.service"],
    ] {
        let result = Command::new("systemctl")
            .args(args)
            .status()
            .map_err(|e| e.to_string())?;
        if !result.success() {
            return Err("Could not install the user service.".into());
        }
    }
    let username = home
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let linger = Command::new("loginctl")
        .args(["enable-linger", &username])
        .status()
        .map_err(|e| e.to_string())?;
    if !linger.success() {
        return Err("Could not enable start-at-boot. The service is installed and will still start at login.".into());
    }
    println!("Installed. Open {URL} or launch Data Watch from Applications.");
    println!("Monitoring starts at boot, continues after logout, and restarts automatically.");
    if ["DISPLAY", "WAYLAND_DISPLAY"]
        .iter()
        .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
    {
        if let Err(error) = open_dashboard() {
            eprintln!(
                "Monitoring is installed. {error} Open {URL} from Applications or your browser."
            );
        }
    } else {
        println!(
            "No graphical session detected. Open {URL} in a browser on this PC when available."
        );
    }
    Ok(())
}

fn dashboard_ready(address: SocketAddr) -> bool {
    let timeout = Duration::from_millis(250);
    let Ok(mut stream) = TcpStream::connect_timeout(&address, timeout) else {
        return false;
    };
    if stream.set_read_timeout(Some(timeout)).is_err()
        || stream.set_write_timeout(Some(timeout)).is_err()
        || write!(
            stream,
            "GET /favicon.svg HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
        )
        .is_err()
    {
        return false;
    }
    let mut status = [0; 12];
    stream.read_exact(&mut status).is_ok() && &status == b"HTTP/1.1 200"
}

fn open_dashboard() -> Result<(), String> {
    let result = Command::new("systemctl")
        .args(["--user", "start", "data-watch.service"])
        .status()
        .map_err(|e| e.to_string())?;
    if !result.success() {
        return Err("Could not start the Data Watch service.".into());
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    while !dashboard_ready(SocketAddr::from(([127, 0, 0, 1], PORT))) {
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "Dashboard did not become ready at {URL}. Check data-watch.service."
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    for browser in [
        "google-chrome-stable",
        "google-chrome",
        "chromium",
        "chromium-browser",
        "brave-browser",
        "microsoft-edge",
        "microsoft-edge-stable",
    ] {
        if Command::new(browser)
            .args(["--app=http://127.0.0.1:8765", "--class=DataWatch"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
        {
            return Ok(());
        }
    }
    Command::new("xdg-open")
        .arg(URL)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not open the Data Watch dashboard: {error}"))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str).unwrap_or("open") {
        "install" => install(),
        "serve" => {
            let data = data_dir();
            (|| -> Result<(), String> {
                fs::create_dir_all(&data).map_err(|e| e.to_string())?;
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&data, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
                use std::os::fd::AsRawFd;
                use std::os::unix::fs::OpenOptionsExt;
                let lock = std::fs::OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .write(true)
                    .mode(0o600)
                    .open(data.join("monitor.lock"))
                    .map_err(|e| format!("Cannot open monitor lock: {e}"))?;
                let locked = unsafe {
                    libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB)
                };
                if locked != 0 {
                    return Err("Data Watch is already running for this history database.".into());
                }
                let store = Arc::new(Mutex::new(storage::Store::open(&data.join("usage.sqlite3"))?));
                let collector = collector::Collector::start(store, data)?;
                let port = std::env::var("DATA_WATCH_PORT").ok().and_then(|p| p.parse::<u16>().ok()).unwrap_or(PORT);
                serve_http(collector, port)
            })()
        }
        "full" => capture::full_tracking(data_dir()),
        "packet-capture" => (|| {
            if args.len() < 2 || args.len() > 3 { return Err("Use packet-capture SOCKET [INTERFACE].".into()); }
            capture::run(std::path::Path::new(&args[1]), args.get(2).map(String::as_str))
        })(),
        "install-capture" => (|| {
            if args.len() != 3 { return Err("Use install-capture UID SOCKET.".into()); }
            let uid = args[1].parse::<u32>().map_err(|_| "Invalid capture UID")?;
            capture_install::install(uid, std::path::Path::new(&args[2]))
        })(),
        "capture-service" => (|| {
            if args.len() != 3 { return Err("Use capture-service UID SOCKET.".into()); }
            let uid = args[1].parse::<u32>().map_err(|_| "Invalid capture UID")?;
            println!("{}", capture_install::unit(uid, std::path::Path::new(&args[2]))?);
            Ok(())
        })(),
        "start" | "stop" | "restart" | "status" => {
            let action = args.first().unwrap();
            Command::new("systemctl").args(["--user", action, "data-watch.service"]).status()
                .map_err(|e| e.to_string())
                .and_then(|status| if status.success() { Ok(()) } else { Err(format!("systemctl {action} failed with {status}")) })
        }
        "open" => open_dashboard(),
        command => Err(format!("Unknown command '{command}'. Use install, open, serve, full, start, stop, restart or status.")),
    };
    if let Err(error) = result {
        eprintln!("Data Watch: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn dashboard_readiness_requires_a_successful_http_response() {
        for (response, expected) in [
            ("HTTP/1.1 200 OK\r\n\r\n", true),
            ("HTTP/1.1 503 Unavailable\r\n\r\n", false),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                    assert!(request.len() < 512);
                }
                assert!(String::from_utf8_lossy(&request).contains("GET /favicon.svg"));
                stream.write_all(response.as_bytes()).unwrap();
            });
            assert_eq!(dashboard_ready(address), expected);
            server.join().unwrap();
        }
    }

    #[test]
    fn offline_export_labels_the_saved_interface() {
        let response = csv_response(
            &json!({
                "interface": "", "history_interface": "saved-wifi",
                "series": [{"ts": 120, "rx": 100, "tx": 20, "apps": []}]
            }),
            "today",
        );
        let mut csv = String::new();
        response.into_reader().read_to_string(&mut csv).unwrap();
        assert!(csv.contains("\"saved-wifi\",CONNECTION TOTAL,100,20"));
    }
}
