use serde_json::{json, Value};
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
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
const DETAILED_RETENTION_SECONDS: f64 = 30.0 * 86400.0;
type SeriesParts = HashMap<String, (u64, u64)>;
type SeriesBuckets = HashMap<i64, (u64, u64, SeriesParts)>;

#[derive(Clone, Copy)]
enum BucketUnit {
    Minute,
    QuarterHour,
    Day,
    Week,
    Month,
    Year,
}

pub struct Store {
    db: *mut Sqlite3,
    path: PathBuf,
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
        let file_path = path.to_path_buf();
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
        let store = Self {
            db,
            path: file_path,
        };
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
             CREATE TABLE IF NOT EXISTS traffic_daily (
                day_ts INTEGER, interface TEXT, rx INTEGER, tx INTEGER,
                PRIMARY KEY(day_ts, interface));
             CREATE TABLE IF NOT EXISTS apps_daily (
                day_ts INTEGER, interface TEXT, name TEXT, rx INTEGER, tx INTEGER,
                last_seen REAL, PRIMARY KEY(day_ts, interface, name));
             CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT);",
        )?;
        Ok(store)
    }

    pub fn size_bytes(&self) -> Result<u64, String> {
        let mut size = std::fs::metadata(&self.path)
            .map_err(|e| format!("read database size: {e}"))?
            .len();
        let mut wal = self.path.as_os_str().to_os_string();
        wal.push("-wal");
        match std::fs::metadata(Path::new(&wal)) {
            Ok(metadata) => size = size.saturating_add(metadata.len()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("read database WAL size: {error}")),
        }
        Ok(size)
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
        let (start, _, _) = period_buckets("today", now)?;
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
        let cutoff = (now - DETAILED_RETENTION_SECONDS) as i64;
        let archive_cutoff = period_buckets("years", now)?.0;
        self.exec("BEGIN IMMEDIATE")?;
        let result = (|| {
            let mut traffic = self.prepare(
                "INSERT INTO traffic_daily(day_ts,interface,rx,tx)
                 SELECT CAST(strftime('%s',datetime(ts,'unixepoch','localtime','start of day')) AS INTEGER),interface,SUM(rx),SUM(tx)
                 FROM traffic WHERE ts < ?1 GROUP BY 1,2
                 ON CONFLICT(day_ts,interface) DO UPDATE SET rx=rx+excluded.rx,tx=tx+excluded.tx",
            )?;
            traffic.bind_i64(1, cutoff)?;
            traffic.expect_done()?;
            let mut apps = self.prepare(
                "INSERT INTO apps_daily(day_ts,interface,name,rx,tx,last_seen)
                 SELECT CAST(strftime('%s',datetime(ts,'unixepoch','localtime','start of day')) AS INTEGER),interface,name,SUM(rx),SUM(tx),MAX(last_seen)
                 FROM apps WHERE ts < ?1 GROUP BY 1,2,3
                 ON CONFLICT(day_ts,interface,name) DO UPDATE SET rx=rx+excluded.rx,tx=tx+excluded.tx,last_seen=MAX(last_seen,excluded.last_seen)",
            )?;
            apps.bind_i64(1, cutoff)?;
            apps.expect_done()?;
            for table in ["traffic", "apps"] {
                let mut stmt = self.prepare(&format!("DELETE FROM {table} WHERE ts < ?1"))?;
                stmt.bind_i64(1, cutoff)?;
                stmt.expect_done()?;
            }
            for table in ["traffic_daily", "apps_daily"] {
                let mut stmt = self.prepare(&format!("DELETE FROM {table} WHERE day_ts < ?1"))?;
                stmt.bind_i64(1, archive_cutoff)?;
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
        let (start, unit, bucket_starts) = period_buckets(period, now)?;
        let mut series: SeriesBuckets = bucket_starts
            .iter()
            .map(|ts| (*ts, (0, 0, HashMap::new())))
            .collect();
        let mut totals = (0u64, 0u64);

        for (table, time_column) in [("traffic", "ts"), ("traffic_daily", "day_ts")] {
            let mut stmt = self.prepare(&format!(
                "SELECT {time_column},rx,tx FROM {table} WHERE {time_column}>=?1 AND interface=?2 ORDER BY {time_column}"
            ))?;
            stmt.bind_i64(1, start)?;
            stmt.bind_text(2, interface)?;
            while stmt.step()? == SQLITE_ROW {
                let ts = stmt.i64(0);
                let rx = stmt.i64(1).max(0) as u64;
                let tx = stmt.i64(2).max(0) as u64;
                let key = bucket_start(ts, unit)?;
                let slot = series.entry(key).or_default();
                slot.0 = slot.0.saturating_add(rx);
                slot.1 = slot.1.saturating_add(tx);
                totals.0 = totals.0.saturating_add(rx);
                totals.1 = totals.1.saturating_add(tx);
            }
        }

        let mut app_totals: HashMap<String, (u64, u64, f64)> = HashMap::new();
        for (table, time_column) in [("apps", "ts"), ("apps_daily", "day_ts")] {
            let mut stmt = self.prepare(&format!(
                "SELECT {time_column},name,rx,tx,last_seen FROM {table} WHERE {time_column}>=?1 AND interface=?2 ORDER BY {time_column}"
            ))?;
            stmt.bind_i64(1, start)?;
            stmt.bind_text(2, interface)?;
            while stmt.step()? == SQLITE_ROW {
                let ts = stmt.i64(0);
                let name = stmt.text(1).unwrap_or_default();
                let rx = stmt.i64(2).max(0) as u64;
                let tx = stmt.i64(3).max(0) as u64;
                let seen = stmt.double(4);
                let total = app_totals.entry(name.clone()).or_insert((0, 0, seen));
                total.0 = total.0.saturating_add(rx);
                total.1 = total.1.saturating_add(tx);
                total.2 = total.2.max(seen);
                let key = bucket_start(ts, unit)?;
                let part = series.entry(key).or_default().2.entry(name).or_default();
                part.0 = part.0.saturating_add(rx);
                part.1 = part.1.saturating_add(tx);
            }
        }

        let mut first_stmt = self.prepare(
            "SELECT MIN(ts) FROM (
                SELECT ts FROM traffic WHERE interface=?1
                UNION ALL SELECT day_ts FROM traffic_daily WHERE interface=?1
             )",
        )?;
        first_stmt.bind_text(1, interface)?;
        let first = if first_stmt.step()? == SQLITE_ROW && !first_stmt.is_null(0) {
            json!(first_stmt.i64(0))
        } else {
            Value::Null
        };

        let mut apps: Vec<_> = app_totals
            .into_iter()
            .map(|(name, (rx, tx, last_seen))| (name, rx, tx, last_seen))
            .collect();
        apps.sort_by(|a, b| b.1.saturating_add(b.2).cmp(&a.1.saturating_add(a.2)));

        let series: Vec<Value> = bucket_starts
            .iter()
            .map(|ts| {
                let (rx, tx, app_parts) = series.remove(ts).unwrap_or_default();
                let mut parts: Vec<_> = app_parts.into_iter().collect();
                parts.sort_by(|a, b| {
                    b.1 .0
                        .saturating_add(b.1 .1)
                        .cmp(&a.1 .0.saturating_add(a.1 .1))
                });
                Ok(json!({
                    "ts": ts,
                    "end": next_bucket(*ts, unit)?,
                    "rx": rx,
                    "tx": tx,
                    "apps": parts.into_iter().map(|(name, (rx, tx))| json!({"name": name, "rx": rx, "tx": tx})).collect::<Vec<_>>()
                }))
            })
            .collect::<Result<Vec<_>, String>>()?;
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

fn local_tm(timestamp: i64) -> Result<libc::tm, String> {
    let raw: libc::time_t = timestamp as libc::time_t;
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&raw, &mut local) }.is_null() {
        return Err("could not determine local date".to_string());
    }
    Ok(local)
}

fn normalize_local(mut local: libc::tm) -> i64 {
    local.tm_isdst = -1;
    unsafe { libc::mktime(&mut local) as i64 }
}

fn shift_bucket(timestamp: i64, unit: BucketUnit, amount: i32) -> Result<i64, String> {
    match unit {
        BucketUnit::Minute => Ok(timestamp + i64::from(amount) * 60),
        BucketUnit::QuarterHour => Ok(timestamp + i64::from(amount) * 900),
        _ => {
            let mut local = local_tm(timestamp)?;
            match unit {
                BucketUnit::Day => local.tm_mday += amount,
                BucketUnit::Week => local.tm_mday += amount * 7,
                BucketUnit::Month => local.tm_mon += amount,
                BucketUnit::Year => local.tm_year += amount,
                BucketUnit::Minute | BucketUnit::QuarterHour => unreachable!(),
            }
            Ok(normalize_local(local))
        }
    }
}

fn bucket_start(timestamp: i64, unit: BucketUnit) -> Result<i64, String> {
    if matches!(unit, BucketUnit::Minute) {
        return Ok(timestamp.div_euclid(60) * 60);
    }
    if matches!(unit, BucketUnit::QuarterHour) {
        let day = bucket_start(timestamp, BucketUnit::Day)?;
        return Ok(day + (timestamp - day).div_euclid(900) * 900);
    }
    let mut local = local_tm(timestamp)?;
    local.tm_hour = 0;
    local.tm_min = 0;
    local.tm_sec = 0;
    match unit {
        BucketUnit::Day => {}
        BucketUnit::Week => local.tm_mday -= (local.tm_wday + 6) % 7,
        BucketUnit::Month => local.tm_mday = 1,
        BucketUnit::Year => {
            local.tm_mon = 0;
            local.tm_mday = 1;
        }
        BucketUnit::Minute | BucketUnit::QuarterHour => unreachable!(),
    }
    Ok(normalize_local(local))
}

fn next_bucket(timestamp: i64, unit: BucketUnit) -> Result<i64, String> {
    shift_bucket(timestamp, unit, 1)
}

fn period_buckets(period: &str, now: f64) -> Result<(i64, BucketUnit, Vec<i64>), String> {
    let now = now as i64;
    let (unit, current, count) = match period {
        "hour" => {
            let current = now.div_euclid(60) * 60;
            (BucketUnit::Minute, current, 60)
        }
        "today" => (
            BucketUnit::QuarterHour,
            bucket_start(now, BucketUnit::Day)?,
            0,
        ),
        "week" => {
            let current = bucket_start(now, BucketUnit::Day)?;
            (BucketUnit::Day, current, 7)
        }
        "weeks" => {
            let current = bucket_start(now, BucketUnit::Week)?;
            (BucketUnit::Week, current, 12)
        }
        "months" => {
            let current = bucket_start(now, BucketUnit::Month)?;
            (BucketUnit::Month, current, 12)
        }
        "years" => {
            let current = bucket_start(now, BucketUnit::Year)?;
            (BucketUnit::Year, current, 5)
        }
        _ => return Err("Choose today, hour, week, weeks, months, or years.".to_string()),
    };
    let mut starts = vec![current];
    if count == 0 {
        while next_bucket(*starts.last().unwrap(), unit)? <= now {
            starts.push(next_bucket(*starts.last().unwrap(), unit)?);
        }
    } else {
        while starts.len() < count {
            let previous = shift_bucket(*starts.last().unwrap(), unit, -1)?;
            starts.push(previous);
        }
        starts.reverse();
    }
    let start = starts[0];
    Ok((start, unit, starts))
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
        assert_eq!(value["series"].as_array().unwrap().len(), 60);
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
        assert_eq!(value["series"].as_array().unwrap().len(), 60);
        assert_eq!(
            db.history("bad", "wlan0", 7200.0).unwrap_err(),
            "Choose today, hour, week, weeks, months, or years."
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
        let value = db.history("months", "wlan0", 31.0 * 86400.0).unwrap();
        assert_eq!(value["totals"]["rx"], 9);
        assert_eq!(value["apps"][0]["rx"], 8);
        drop(db);
        cleanup(&path);
    }

    #[test]
    fn calendar_ranges_have_one_bucket_for_each_period() {
        let now = 1_800_000_000.0;
        let (today_start, today_unit, today) = period_buckets("today", now).unwrap();
        assert!(matches!(today_unit, BucketUnit::QuarterHour));
        assert!(today_start <= now as i64);
        assert!(now as i64 - today_start < 86400);
        assert!((1..=96).contains(&today.len()));
        assert_eq!(period_buckets("hour", now).unwrap().2.len(), 60);
        assert_eq!(period_buckets("week", now).unwrap().2.len(), 7);
        let weeks = period_buckets("weeks", now).unwrap().2;
        assert_eq!(weeks.len(), 12);
        assert!(weeks.iter().all(|ts| local_tm(*ts).unwrap().tm_wday == 1));
        let months = period_buckets("months", now).unwrap().2;
        assert_eq!(months.len(), 12);
        assert!(months.iter().all(|ts| local_tm(*ts).unwrap().tm_mday == 1));
        let years = period_buckets("years", now).unwrap().2;
        assert_eq!(years.len(), 5);
        assert!(years.iter().all(|ts| {
            let local = local_tm(*ts).unwrap();
            local.tm_mon == 0 && local.tm_mday == 1
        }));
    }

    #[test]
    fn old_minutes_are_compacted_and_remain_visible_in_long_ranges() {
        let (mut db, path) = store();
        let now = 1_800_000_000.0;
        let old = now - 40.0 * 86400.0;
        let mut apps = HashMap::new();
        apps.insert("browser".to_string(), (70, 10));
        db.record(old, "wlan0", 100, 20, &apps).unwrap();
        db.prune(now).unwrap();
        let history = db.history("months", "wlan0", now).unwrap();
        assert_eq!(history["totals"]["rx"], 100);
        assert_eq!(history["totals"]["tx"], 20);
        assert_eq!(history["apps"][0]["name"], "browser");
        assert_eq!(history["apps"][0]["rx"], 70);
        assert_eq!(history["series"].as_array().unwrap().len(), 12);
        let mut wal_path = path.as_os_str().to_os_string();
        wal_path.push("-wal");
        let main_bytes = std::fs::metadata(&path).unwrap().len();
        let wal_bytes = std::fs::metadata(Path::new(&wal_path))
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        assert_eq!(db.size_bytes().unwrap(), main_bytes + wal_bytes);
        drop(db);
        cleanup(&path);
    }
}
