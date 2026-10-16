use serde_json::{json, Value};
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::Path;
use std::ptr;

#[repr(C)]
struct Sqlite3 {
    _private: [u8; 0],
}

#[repr(C)]
struct Sqlite3Stmt {
    _private: [u8; 0],
}

extern "C" {
    fn sqlite3_open(filename: *const c_char, db: *mut *mut Sqlite3) -> c_int;
    fn sqlite3_close(db: *mut Sqlite3) -> c_int;
    fn sqlite3_errmsg(db: *mut Sqlite3) -> *const c_char;
    fn sqlite3_prepare_v2(
        db: *mut Sqlite3,
        sql: *const c_char,
        bytes: c_int,
        stmt: *mut *mut Sqlite3Stmt,
        tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_finalize(stmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_step(stmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_reset(stmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_clear_bindings(stmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_bind_int64(stmt: *mut Sqlite3Stmt, index: c_int, value: i64) -> c_int;
    fn sqlite3_bind_double(stmt: *mut Sqlite3Stmt, index: c_int, value: f64) -> c_int;
    fn sqlite3_bind_text(
        stmt: *mut Sqlite3Stmt,
        index: c_int,
        value: *const c_char,
        bytes: c_int,
        destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    ) -> c_int;
    fn sqlite3_column_int64(stmt: *mut Sqlite3Stmt, column: c_int) -> i64;
    fn sqlite3_column_double(stmt: *mut Sqlite3Stmt, column: c_int) -> f64;
    fn sqlite3_column_text(stmt: *mut Sqlite3Stmt, column: c_int) -> *const u8;
    fn sqlite3_column_type(stmt: *mut Sqlite3Stmt, column: c_int) -> c_int;
    fn sqlite3_exec(
        db: *mut Sqlite3,
        sql: *const c_char,
        callback: Option<
            unsafe extern "C" fn(*mut c_void, c_int, *mut *mut c_char, *mut *mut c_char) -> c_int,
        >,
        argument: *mut c_void,
        error: *mut *mut c_char,
    ) -> c_int;
    fn sqlite3_free(value: *mut c_void);
}

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_NULL: c_int = 5;
const RETENTION_SECONDS: f64 = 30.0 * 86400.0;
type SeriesParts = HashMap<String, (u64, u64)>;
type SeriesBuckets = HashMap<i64, (u64, u64, SeriesParts)>;

pub struct Store {
    db: *mut Sqlite3,
}

// The handle is accessed only through an Arc<Mutex<Store>> once it leaves the
// creating thread. SQLite is built in serialized mode on supported Ubuntu.
unsafe impl Send for Store {}

impl Store {
    pub fn open(path: &Path) -> Result<Self, String> {
        let _bundled_sqlite_version = unsafe { libsqlite3_sys::sqlite3_libversion_number() };
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create database directory: {e}"))?;
        }
        let path = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| "database path contains a NUL byte".to_string())?;
        let mut db = ptr::null_mut();
        let result = unsafe { sqlite3_open(path.as_ptr(), &mut db) };
        if result != SQLITE_OK {
            let message = db_error(db, "open database");
            if !db.is_null() {
                unsafe { sqlite3_close(db) };
            }
            return Err(message);
        }
        let store = Self { db };
        store.exec(
            "PRAGMA busy_timeout=5000; PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;",
        )?;
        store.exec(
            "CREATE TABLE IF NOT EXISTS traffic (
                ts INTEGER, interface TEXT, rx INTEGER, tx INTEGER,
                PRIMARY KEY(ts, interface));
             CREATE TABLE IF NOT EXISTS apps (
                ts INTEGER, interface TEXT, name TEXT, rx INTEGER, tx INTEGER,
                last_seen REAL, PRIMARY KEY(ts, interface, name));
             CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT);",
        )?;
        Ok(store)
    }

    pub fn setting(&self, key: &str, default: &str) -> Result<String, String> {
        let mut stmt = self.prepare("SELECT value FROM settings WHERE key=?1")?;
        stmt.bind_text(1, key)?;
        match stmt.step()? {
            SQLITE_ROW => Ok(stmt.text(0).unwrap_or_default()),
            SQLITE_DONE => Ok(default.to_string()),
            _ => unreachable!(),
        }
    }

    pub fn set_setting(&mut self, key: &str, value: &str) -> Result<(), String> {
        let mut stmt = self.prepare("INSERT OR REPLACE INTO settings VALUES (?1,?2)")?;
        stmt.bind_text(1, key)?;
        stmt.bind_text(2, value)?;
        stmt.expect_done()
    }

    pub fn today_total(&self, now: f64, interface: &str) -> Result<u64, String> {
        let (start, _) = range_start("today", now)?;
        let mut stmt = self.prepare(
            "SELECT COALESCE(SUM(rx),0) + COALESCE(SUM(tx),0) FROM traffic WHERE ts>=?1 AND interface=?2",
        )?;
        stmt.bind_i64(1, start)?;
        stmt.bind_text(2, interface)?;
        if stmt.step()? == SQLITE_ROW {
            Ok(stmt.i64(0).max(0) as u64)
        } else {
            Err("Could not read today's traffic total.".into())
        }
    }

    pub fn record(
        &mut self,
        now: f64,
        interface: &str,
        rx: u64,
        tx: u64,
        apps: &HashMap<String, (u64, u64)>,
    ) -> Result<(), String> {
        let minute = (now as i64).div_euclid(60) * 60;
        self.exec("BEGIN IMMEDIATE")?;
        let result = (|| {
            let mut traffic = self.prepare(
                "INSERT INTO traffic VALUES (?1,?2,?3,?4)
                 ON CONFLICT(ts,interface) DO UPDATE SET rx=rx+excluded.rx, tx=tx+excluded.tx",
            )?;
            traffic.bind_i64(1, minute)?;
            traffic.bind_text(2, interface)?;
            traffic.bind_u64(3, rx)?;
            traffic.bind_u64(4, tx)?;
            traffic.expect_done()?;

            let mut app_stmt = self.prepare(
                "INSERT INTO apps VALUES (?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(ts,interface,name) DO UPDATE SET
                   rx=rx+excluded.rx, tx=tx+excluded.tx, last_seen=excluded.last_seen",
            )?;
            for (name, (app_rx, app_tx)) in apps {
                if app_rx.saturating_add(*app_tx) == 0 {
                    continue;
                }
                app_stmt.bind_i64(1, minute)?;
                app_stmt.bind_text(2, interface)?;
                app_stmt.bind_text(3, name)?;
                app_stmt.bind_u64(4, *app_rx)?;
                app_stmt.bind_u64(5, *app_tx)?;
                app_stmt.bind_double(6, now)?;
                app_stmt.expect_done()?;
                app_stmt.reset()?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => self.exec("COMMIT"),
            Err(error) => {
                let _ = self.exec("ROLLBACK");
                Err(error)
            }
        }
    }

    pub fn prune(&mut self, now: f64) -> Result<(), String> {
        let cutoff = (now - RETENTION_SECONDS) as i64;
        self.exec("BEGIN IMMEDIATE")?;
        let result = (|| {
            for table in ["traffic", "apps"] {
                let sql = format!("DELETE FROM {table} WHERE ts < ?1");
                let mut stmt = self.prepare(&sql)?;
                stmt.bind_i64(1, cutoff)?;
                stmt.expect_done()?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => self.exec("COMMIT"),
            Err(error) => {
                let _ = self.exec("ROLLBACK");
                Err(error)
            }
        }
    }

    pub fn history(&self, period: &str, interface: &str, now: f64) -> Result<Value, String> {
        let (mut start, bucket) = range_start(period, now)?;
        start = start.div_euclid(60) * 60;
        let mut traffic_stmt =
            self.prepare("SELECT ts,rx,tx FROM traffic WHERE ts>=?1 AND interface=?2 ORDER BY ts")?;
        traffic_stmt.bind_i64(1, start)?;
        traffic_stmt.bind_text(2, interface)?;
        let mut traffic = Vec::new();
        while traffic_stmt.step()? == SQLITE_ROW {
            traffic.push((
                traffic_stmt.i64(0),
                traffic_stmt.i64(1).max(0) as u64,
                traffic_stmt.i64(2).max(0) as u64,
            ));
        }

        let mut apps_stmt = self.prepare(
            "SELECT ts,name,rx,tx,last_seen FROM apps WHERE ts>=?1 AND interface=?2 ORDER BY ts",
        )?;
        apps_stmt.bind_i64(1, start)?;
        apps_stmt.bind_text(2, interface)?;
        let mut rows = Vec::new();
        while apps_stmt.step()? == SQLITE_ROW {
            rows.push((
                apps_stmt.i64(0),
                apps_stmt.text(1).unwrap_or_default(),
                apps_stmt.i64(2).max(0) as u64,
                apps_stmt.i64(3).max(0) as u64,
                apps_stmt.double(4),
            ));
        }

        let mut first_stmt = self.prepare("SELECT MIN(ts) FROM traffic WHERE interface=?1")?;
        first_stmt.bind_text(1, interface)?;
        let first = if first_stmt.step()? == SQLITE_ROW {
            if first_stmt.is_null(0) {
                Value::Null
            } else {
                json!(first_stmt.i64(0))
            }
        } else {
            Value::Null
        };

        let mut series: SeriesBuckets = HashMap::new();
        let mut totals = (0u64, 0u64);
        for (ts, rx, tx) in traffic {
            let key = ts.div_euclid(bucket) * bucket;
            let slot = series.entry(key).or_default();
            slot.0 = slot.0.saturating_add(rx);
            slot.1 = slot.1.saturating_add(tx);
            totals.0 = totals.0.saturating_add(rx);
            totals.1 = totals.1.saturating_add(tx);
        }

        let mut app_totals: HashMap<String, (u64, u64, f64)> = HashMap::new();
        for (ts, name, rx, tx, seen) in rows {
            let total = app_totals.entry(name.clone()).or_insert((0, 0, seen));
            total.0 = total.0.saturating_add(rx);
            total.1 = total.1.saturating_add(tx);
            total.2 = total.2.max(seen);
            let key = ts.div_euclid(bucket) * bucket;
            let slot = series.entry(key).or_default();
            let part = slot.2.entry(name).or_default();
            part.0 = part.0.saturating_add(rx);
            part.1 = part.1.saturating_add(tx);
        }

        let mut apps: Vec<_> = app_totals
            .into_iter()
            .map(|(name, (rx, tx, last_seen))| (name, rx, tx, last_seen))
            .collect();
        apps.sort_by(|a, b| b.1.saturating_add(b.2).cmp(&a.1.saturating_add(a.2)));

        let mut series: Vec<_> = series.into_iter().collect();
        series.sort_by_key(|(ts, _)| *ts);
        let series: Vec<Value> = series
            .into_iter()
            .map(|(ts, (rx, tx, app_parts))| {
                let mut parts: Vec<_> = app_parts.into_iter().collect();
                parts.sort_by(|a, b| {
                    b.1 .0
                        .saturating_add(b.1 .1)
                        .cmp(&a.1 .0.saturating_add(a.1 .1))
                });
                json!({
                    "ts": ts,
                    "rx": rx,
                    "tx": tx,
                    "apps": parts.into_iter().map(|(name, (rx, tx))| json!({"name": name, "rx": rx, "tx": tx})).collect::<Vec<_>>()
                })
            })
            .collect();
        let apps: Vec<Value> = apps
            .into_iter()
            .map(|(name, rx, tx, last_seen)| {
                json!({"name": name, "rx": rx, "tx": tx, "last_seen": last_seen})
            })
            .collect();
        Ok(json!({
            "totals": {"rx": totals.0, "tx": totals.1},
            "apps": apps,
            "series": series,
            "first_recorded_at": first
        }))
    }

    fn prepare(&self, sql: &str) -> Result<Statement, String> {
        let sql = CString::new(sql).map_err(|_| "SQL contains a NUL byte".to_string())?;
        let mut stmt = ptr::null_mut();
        let code =
            unsafe { sqlite3_prepare_v2(self.db, sql.as_ptr(), -1, &mut stmt, ptr::null_mut()) };
        if code != SQLITE_OK {
            return Err(db_error(self.db, "prepare SQL"));
        }
        Ok(Statement { db: self.db, stmt })
    }

    fn exec(&self, sql: &str) -> Result<(), String> {
        let sql = CString::new(sql).map_err(|_| "SQL contains a NUL byte".to_string())?;
        let mut error = ptr::null_mut();
        let code =
            unsafe { sqlite3_exec(self.db, sql.as_ptr(), None, ptr::null_mut(), &mut error) };
        if code == SQLITE_OK {
            return Ok(());
        }
        let message = if error.is_null() {
            db_error(self.db, "execute SQL")
        } else {
            let text = unsafe { CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned();
            unsafe { sqlite3_free(error.cast()) };
            format!("execute SQL: {text}")
        };
        Err(message)
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        if !self.db.is_null() {
            unsafe { sqlite3_close(self.db) };
        }
    }
}

struct Statement {
    db: *mut Sqlite3,
    stmt: *mut Sqlite3Stmt,
}

impl Statement {
    fn bind_i64(&mut self, index: c_int, value: i64) -> Result<(), String> {
        let code = unsafe { sqlite3_bind_int64(self.stmt, index, value) };
        self.check(code, "bind integer")
    }

    fn bind_u64(&mut self, index: c_int, value: u64) -> Result<(), String> {
        let value = i64::try_from(value)
            .map_err(|_| "byte counter exceeds SQLite integer range".to_string())?;
        self.bind_i64(index, value)
    }

    fn bind_double(&mut self, index: c_int, value: f64) -> Result<(), String> {
        let code = unsafe { sqlite3_bind_double(self.stmt, index, value) };
        self.check(code, "bind number")
    }

    fn bind_text(&mut self, index: c_int, value: &str) -> Result<(), String> {
        let length =
            c_int::try_from(value.len()).map_err(|_| "text value is too long".to_string())?;
        let transient: unsafe extern "C" fn(*mut c_void) = unsafe { std::mem::transmute(-1_isize) };
        let code = unsafe {
            sqlite3_bind_text(
                self.stmt,
                index,
                value.as_ptr().cast(),
                length,
                Some(transient),
            )
        };
        self.check(code, "bind text")
    }

    fn step(&mut self) -> Result<c_int, String> {
        let code = unsafe { sqlite3_step(self.stmt) };
        if code == SQLITE_ROW || code == SQLITE_DONE {
            Ok(code)
        } else {
            Err(db_error(self.db, "step SQL"))
        }
    }

    fn expect_done(&mut self) -> Result<(), String> {
        match self.step()? {
            SQLITE_DONE => Ok(()),
            _ => Err("SQL statement unexpectedly returned a row".to_string()),
        }
    }

    fn reset(&mut self) -> Result<(), String> {
        let reset_code = unsafe { sqlite3_reset(self.stmt) };
        self.check(reset_code, "reset SQL")?;
        let clear_code = unsafe { sqlite3_clear_bindings(self.stmt) };
        self.check(clear_code, "clear SQL bindings")
    }

    fn i64(&self, column: c_int) -> i64 {
        unsafe { sqlite3_column_int64(self.stmt, column) }
    }

    fn double(&self, column: c_int) -> f64 {
        unsafe { sqlite3_column_double(self.stmt, column) }
    }

    fn is_null(&self, column: c_int) -> bool {
        unsafe { sqlite3_column_type(self.stmt, column) == SQLITE_NULL }
    }

    fn text(&self, column: c_int) -> Option<String> {
        let value = unsafe { sqlite3_column_text(self.stmt, column) };
        if value.is_null() {
            None
        } else {
            Some(
                unsafe { CStr::from_ptr(value.cast()) }
                    .to_string_lossy()
                    .into_owned(),
            )
        }
    }

    fn check(&self, code: c_int, operation: &str) -> Result<(), String> {
        if code == SQLITE_OK {
            Ok(())
        } else {
            Err(db_error(self.db, operation))
        }
    }
}

impl Drop for Statement {
    fn drop(&mut self) {
        if !self.stmt.is_null() {
            unsafe { sqlite3_finalize(self.stmt) };
        }
    }
}

fn db_error(db: *mut Sqlite3, operation: &str) -> String {
    let message = if db.is_null() {
        "SQLite returned no connection".to_string()
    } else {
        let error = unsafe { sqlite3_errmsg(db) };
        if error.is_null() {
            "unknown SQLite error".to_string()
        } else {
            unsafe { CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned()
        }
    };
    format!("{operation}: {message}")
}

fn range_start(period: &str, now: f64) -> Result<(i64, i64), String> {
    match period {
        "hour" => Ok(((now - 3600.0) as i64, 60)),
        "week" => Ok(((now - 7.0 * 86400.0) as i64, 3600)),
        "today" => {
            let timestamp = now as libc::time_t;
            let mut local: libc::tm = unsafe { std::mem::zeroed() };
            let result = unsafe { libc::localtime_r(&timestamp, &mut local) };
            if result.is_null() {
                return Err("could not determine local date".to_string());
            }
            local.tm_hour = 0;
            local.tm_min = 0;
            local.tm_sec = 0;
            local.tm_isdst = -1;
            let midnight = unsafe { libc::mktime(&mut local) };
            Ok((midnight as i64, 900))
        }
        _ => Err("Choose today, hour, or week.".to_string()),
    }
}

pub fn local_day_key(now: f64) -> Result<String, String> {
    let timestamp = now as libc::time_t;
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&timestamp, &mut local) }.is_null() {
        return Err("could not determine local date".to_string());
    }
    Ok(format!("{}-{}", local.tm_year + 1900, local.tm_yday))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn store() -> (Store, std::path::PathBuf) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("data-watch-storage-{unique}.sqlite3"));
        (Store::open(&path).unwrap(), path)
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("sqlite3-wal"));
        let _ = std::fs::remove_file(path.with_extension("sqlite3-shm"));
    }

    #[test]
    fn record_accumulates_minute_and_app_usage() {
        let (mut db, path) = store();
        let mut apps = HashMap::new();
        apps.insert("firefox".to_string(), (20, 4));
        db.record(120.0, "wlan0", 30, 8, &apps).unwrap();
        db.record(130.0, "wlan0", 10, 2, &apps).unwrap();
        let value = db.history("hour", "wlan0", 130.0).unwrap();
        assert_eq!(value["totals"]["rx"], 40);
        assert_eq!(value["totals"]["tx"], 10);
        assert_eq!(value["apps"][0]["rx"], 40);
        assert_eq!(value["apps"][0]["last_seen"], 130.0);
        assert_eq!(value["series"].as_array().unwrap().len(), 1);
        drop(db);
        cleanup(&path);
    }

    #[test]
    fn today_total_counts_only_the_selected_interface() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let (mut db, path) = store();
        db.record(now, "wlan0", 10, 3, &HashMap::new()).unwrap();
        db.record(now, "eth0", 100, 50, &HashMap::new()).unwrap();
        assert_eq!(db.today_total(now, "wlan0").unwrap(), 13);
        assert_eq!(db.today_total(now, "eth0").unwrap(), 150);
        drop(db);
        cleanup(&path);
    }

    #[test]
    fn history_filters_interfaces_and_periods() {
        let (mut db, path) = store();
        db.record(60.0, "wlan0", 1, 2, &HashMap::new()).unwrap();
        db.record(7200.0, "eth0", 5, 6, &HashMap::new()).unwrap();
        db.record(7200.0, "wlan0", 3, 4, &HashMap::new()).unwrap();
        let value = db.history("hour", "wlan0", 7200.0).unwrap();
        assert_eq!(value["totals"]["rx"], 3);
        assert_eq!(value["series"].as_array().unwrap().len(), 1);
        assert_eq!(
            db.history("bad", "wlan0", 7200.0).unwrap_err(),
            "Choose today, hour, or week."
        );
        drop(db);
        cleanup(&path);
    }

    #[test]
    fn prune_removes_data_older_than_thirty_days() {
        let (mut db, path) = store();
        let mut apps = HashMap::new();
        apps.insert("browser".to_string(), (4, 2));
        db.record(1.0, "wlan0", 4, 2, &apps).unwrap();
        db.record(31.0 * 86400.0, "wlan0", 5, 3, &apps).unwrap();
        db.prune(31.0 * 86400.0).unwrap();
        let value = db.history("week", "wlan0", 31.0 * 86400.0).unwrap();
        assert_eq!(value["totals"]["rx"], 5);
        assert_eq!(value["apps"][0]["rx"], 4);
        drop(db);
        cleanup(&path);
    }

    #[test]
    fn today_range_uses_local_midnight_and_expected_bucket_width() {
        let now = 1_800_000_000.0;
        let (start, bucket) = range_start("today", now).unwrap();
        assert_eq!(bucket, 900);
        assert!(start <= now as i64);
        assert!(now as i64 - start < 86400);
        assert_eq!(
            range_start("hour", now).unwrap(),
            ((now - 3600.0) as i64, 60)
        );
        assert_eq!(
            range_start("week", now).unwrap(),
            ((now - 7.0 * 86400.0) as i64, 3600)
        );
    }
}
