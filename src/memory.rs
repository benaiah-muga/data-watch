use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Clone, Debug)]
pub struct Program {
    pub name: String,
    pub rss: u64,
    pub swap: u64,
    pub processes: u64,
}

#[derive(Clone, Debug)]
pub struct Sample {
    pub ts: f64,
    pub total: u64,
    pub available: u64,
    pub available_estimated: bool,
    pub cache: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub psi_some: Option<f64>,
    pub psi_full: Option<f64>,
    pub swap_in_total: Option<u64>,
    pub swap_out_total: Option<u64>,
    pub oom_total: Option<u64>,
    pub swap_in: u64,
    pub swap_out: u64,
    pub oom_kills: u64,
    pub elapsed: f64,
    pub programs: Vec<Program>,
    pub unreadable_processes: u64,
}

fn kb_values(text: &str) -> HashMap<&str, u64> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            let value = value.split_whitespace().next()?.parse::<u64>().ok()?;
            Some((key, value.saturating_mul(1024)))
        })
        .collect()
}

fn pressure(text: &str, category: &str) -> Option<f64> {
    let line = text
        .lines()
        .find(|line| line.split_whitespace().next() == Some(category))?;
    let value = line
        .split_whitespace()
        .find_map(|word| word.strip_prefix("avg10="))?
        .parse::<f64>()
        .ok()?;
    (value.is_finite() && (0.0..=100.0).contains(&value)).then_some(value)
}

fn counters(text: &str, page_size: u64) -> (Option<u64>, Option<u64>, Option<u64>) {
    let values: HashMap<&str, u64> = text
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            Some((words.next()?, words.next()?.parse().ok()?))
        })
        .collect();
    (
        values.get("pswpin").map(|n| n.saturating_mul(page_size)),
        values.get("pswpout").map(|n| n.saturating_mul(page_size)),
        values.get("oom_kill").copied(),
    )
}

fn program(text: &str) -> Option<Program> {
    let name = text
        .lines()
        .find_map(|line| line.strip_prefix("Name:"))?
        .trim();
    let values = kb_values(text);
    let rss = *values.get("VmRSS")?;
    Some(Program {
        name: name.to_owned(),
        rss,
        swap: values.get("VmSwap").copied().unwrap_or(0),
        processes: 1,
    })
}

impl Sample {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }

    pub fn read(root: &Path, now: f64, previous: Option<&Self>) -> Result<Self, String> {
        let text = fs::read_to_string(root.join("meminfo"))
            .map_err(|e| format!("Read memory totals: {e}"))?;
        let values = kb_values(&text);
        let total = values
            .get("MemTotal")
            .copied()
            .filter(|n| *n > 0)
            .ok_or("MemTotal is missing or zero.")?;
        let value = |key: &str| values.get(key).copied().unwrap_or(0);
        let cache = value("Cached")
            .saturating_add(value("SReclaimable"))
            .saturating_sub(value("Shmem"));
        let available = values
            .get("MemAvailable")
            .copied()
            .unwrap_or_else(|| {
                value("MemFree")
                    .saturating_add(value("Buffers"))
                    .saturating_add(cache)
            })
            .min(total);
        let psi = fs::read_to_string(root.join("pressure/memory")).unwrap_or_default();
        let vmstat = fs::read_to_string(root.join("vmstat")).unwrap_or_default();
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let (swap_in_total, swap_out_total, oom_total) =
            counters(&vmstat, u64::try_from(page_size).unwrap_or(4096));
        let elapsed = previous.map(|p| (now - p.ts).max(0.1)).unwrap_or(0.0);
        let delta = |current: Option<u64>, old: Option<u64>| {
            current
                .zip(old)
                .map(|(a, b)| a.saturating_sub(b))
                .unwrap_or(0)
        };
        let mut groups: HashMap<String, Program> = HashMap::new();
        let mut unreadable_processes = 0;
        for entry in fs::read_dir(root)
            .map_err(|e| format!("Read process list: {e}"))?
            .flatten()
        {
            if !entry
                .file_name()
                .to_string_lossy()
                .bytes()
                .all(|b| b.is_ascii_digit())
            {
                continue;
            }
            match fs::read_to_string(entry.path().join("status")) {
                Ok(text) => {
                    if let Some(item) = program(&text) {
                        let group = groups.entry(item.name.clone()).or_insert(Program {
                            name: item.name,
                            rss: 0,
                            swap: 0,
                            processes: 0,
                        });
                        group.rss = group.rss.saturating_add(item.rss);
                        group.swap = group.swap.saturating_add(item.swap);
                        group.processes += 1;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                    unreadable_processes += 1
                }
                Err(_) => {} // A process may exit between listing and reading it.
            }
        }
        let mut programs: Vec<_> = groups.into_values().collect();
        programs.sort_by(|a, b| b.rss.cmp(&a.rss).then_with(|| a.name.cmp(&b.name)));
        Ok(Self {
            ts: now,
            total,
            available,
            available_estimated: !values.contains_key("MemAvailable"),
            cache,
            swap_total: value("SwapTotal"),
            swap_used: value("SwapTotal").saturating_sub(value("SwapFree")),
            psi_some: pressure(&psi, "some"),
            psi_full: pressure(&psi, "full"),
            swap_in_total,
            swap_out_total,
            oom_total,
            swap_in: delta(swap_in_total, previous.and_then(|p| p.swap_in_total)),
            swap_out: delta(swap_out_total, previous.and_then(|p| p.swap_out_total)),
            oom_kills: delta(oom_total, previous.and_then(|p| p.oom_total)),
            elapsed,
            programs,
            unreadable_processes,
        })
    }

    pub fn json(&self) -> Value {
        let used_percent = self.used() as f64 / self.total as f64 * 100.0;
        let status = if self.oom_kills > 0
            || used_percent >= 95.0
            || self.psi_some.unwrap_or(0.0) >= 20.0
            || self.psi_full.unwrap_or(0.0) >= 5.0
        {
            "critical"
        } else if used_percent >= 90.0
            || self.psi_some.unwrap_or(0.0) >= 5.0
            || self.psi_full.unwrap_or(0.0) >= 1.0
        {
            "warning"
        } else {
            "normal"
        };
        json!({
            "ts": self.ts, "total": self.total, "used": self.used(), "available": self.available,
            "available_estimated": self.available_estimated, "cache": self.cache,
            "used_percent": used_percent, "swap_total": self.swap_total, "swap_used": self.swap_used,
            "psi_some": self.psi_some, "psi_full": self.psi_full,
            "swap_in_rate": if self.elapsed > 0.0 { self.swap_in as f64 / self.elapsed } else { 0.0 },
            "swap_out_rate": if self.elapsed > 0.0 { self.swap_out as f64 / self.elapsed } else { 0.0 },
            "paging_available": self.swap_in_total.is_some() && self.swap_out_total.is_some(),
            "oom_total": self.oom_total, "oom_kills": self.oom_kills, "status": status,
            "unreadable_processes": self.unreadable_processes,
            "programs": self.programs.iter().map(|p| json!({"name":p.name,"rss":p.rss,"swap":p.swap,"processes":p.processes})).collect::<Vec<_>>()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_pressure_and_rejects_invalid_values() {
        assert_eq!(
            pressure("some avg10=3.25 avg60=1.00\nfull avg10=0.50", "some"),
            Some(3.25)
        );
        assert_eq!(pressure("some avg10=NaN", "some"), None);
        assert_eq!(pressure("some avg10=101", "some"), None);
        assert_eq!(pressure("", "full"), None);
    }

    #[test]
    fn reads_available_groups_processes_and_baselines_kernel_counters() {
        let dir = std::env::temp_dir().join(format!(
            "dw-mem-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(dir.join("pressure")).unwrap();
        fs::write(dir.join("meminfo"), "MemTotal: 16000 kB\nMemAvailable: 10000 kB\nMemFree: 1000 kB\nCached: 8500 kB\nSReclaimable: 500 kB\nShmem: 1000 kB\nSwapTotal: 2000 kB\nSwapFree: 1500 kB\n").unwrap();
        fs::write(
            dir.join("pressure/memory"),
            "some avg10=0.00\nfull avg10=0.00",
        )
        .unwrap();
        fs::write(dir.join("vmstat"), "pswpin 100\npswpout 200\noom_kill 3").unwrap();
        for (pid, rss) in [(10, 100), (11, 200)] {
            fs::create_dir(dir.join(pid.to_string())).unwrap();
            fs::write(
                dir.join(pid.to_string()).join("status"),
                format!("Name:\teditor\nVmRSS: {rss} kB\nVmSwap: 10 kB\n"),
            )
            .unwrap();
        }
        let first = Sample::read(&dir, 100.0, None).unwrap();
        assert_eq!(first.used(), 6000 * 1024);
        assert_eq!(first.cache, 8000 * 1024);
        assert_eq!(first.oom_kills, 0);
        assert_eq!(first.swap_in, 0);
        assert_eq!(first.programs.len(), 1);
        assert_eq!(first.programs[0].rss, 300 * 1024);
        assert_eq!(first.programs[0].processes, 2);
        fs::write(dir.join("vmstat"), "pswpin 110\npswpout 220\noom_kill 4").unwrap();
        let next = Sample::read(&dir, 106.0, Some(&first)).unwrap();
        assert_eq!(next.oom_kills, 1);
        assert!(next.swap_in > 0);
        assert!(next.json()["swap_out_rate"].as_f64().unwrap() > 0.0);
        fs::remove_file(dir.join("pressure/memory")).unwrap();
        fs::write(dir.join("vmstat"), "pswpin 1\npswpout 2\noom_kill 0").unwrap();
        let reset = Sample::read(&dir, 112.0, Some(&next)).unwrap();
        assert_eq!(reset.swap_in, 0);
        assert_eq!(reset.oom_kills, 0);
        assert!(reset.json()["psi_some"].is_null());
        assert_eq!(reset.json()["status"], "normal");
        fs::write(
            dir.join("meminfo"),
            "MemTotal: 16000 kB\nMemFree: 3000 kB\nCached: 7000 kB\nBuffers: 1000 kB",
        )
        .unwrap();
        let fallback = Sample::read(&dir, 118.0, None).unwrap();
        assert_eq!(fallback.available, 11000 * 1024);
        assert!(fallback.available_estimated);
        fs::remove_dir_all(dir).unwrap();
    }
}
