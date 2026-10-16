use crate::packet::{normalize, Flow};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;

#[derive(Default)]
pub struct Owners {
    exact: HashMap<Flow, Option<String>>,
    wildcard: HashMap<(u8, SocketAddr), Option<String>>,
    inodes: HashMap<u64, String>,
}

fn address(value: &str) -> Option<SocketAddr> {
    let (ip, port) = value.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let ip = match ip.len() {
        8 => IpAddr::V4(Ipv4Addr::from(
            u32::from_str_radix(ip, 16).ok()?.to_ne_bytes(),
        )),
        32 => {
            let mut bytes = [0u8; 16];
            for (index, chunk) in ip.as_bytes().chunks_exact(8).enumerate() {
                let word = u32::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
                bytes[index * 4..index * 4 + 4].copy_from_slice(&word.to_ne_bytes());
            }
            IpAddr::V6(Ipv6Addr::from(bytes))
        }
        _ => return None,
    };
    Some(SocketAddr::new(normalize(ip), port))
}

fn sockets(text: &str, protocol: u8) -> Vec<(Flow, u64)> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<_> = line.split_whitespace().collect();
            let inode = f.get(9)?.parse::<u64>().ok()?;
            if inode == 0 {
                return None;
            }
            Some((
                Flow {
                    protocol,
                    local: address(f.get(1)?)?,
                    remote: address(f.get(2)?)?,
                },
                inode,
            ))
        })
        .collect()
}

fn insert_unique<K: std::hash::Hash + Eq>(
    map: &mut HashMap<K, Option<String>>,
    key: K,
    name: &str,
) {
    map.entry(key)
        .and_modify(|current| {
            if current.as_deref() != Some(name) {
                *current = None;
            }
        })
        .or_insert_with(|| Some(name.to_string()));
}

impl Owners {
    pub fn refresh(&mut self) -> Result<(), String> {
        let mut rows = Vec::new();
        for (file, protocol) in [("tcp", 6), ("tcp6", 6), ("udp", 17), ("udp6", 17)] {
            let path = format!("/proc/net/{file}");
            match fs::read_to_string(&path) {
                Ok(text) => rows.extend(sockets(&text, protocol)),
                Err(e) if file.ends_with('6') && e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(format!("Read {path}: {e}")),
            }
        }
        let wanted: HashSet<u64> = rows.iter().map(|(_, inode)| *inode).collect();
        self.inodes.retain(|inode, _| wanted.contains(inode));
        if wanted.iter().any(|inode| !self.inodes.contains_key(inode)) {
            let mut processes: Vec<_> = fs::read_dir("/proc")
                .map_err(|e| e.to_string())?
                .flatten()
                .filter_map(|p| {
                    p.file_name()
                        .to_str()?
                        .parse::<u32>()
                        .ok()
                        .map(|pid| (pid, p.path()))
                })
                .collect();
            processes.sort_by_key(|(pid, _)| *pid);
            for (_, path) in processes {
                let Ok(fds) = fs::read_dir(path.join("fd")) else {
                    continue;
                };
                let mut name = None;
                for fd in fds.flatten() {
                    let Ok(link) = fs::read_link(fd.path()) else {
                        continue;
                    };
                    let Some(inode) = link
                        .to_str()
                        .and_then(|l| l.strip_prefix("socket:["))
                        .and_then(|l| l.strip_suffix(']'))
                        .and_then(|l| l.parse::<u64>().ok())
                    else {
                        continue;
                    };
                    if !wanted.contains(&inode) || self.inodes.contains_key(&inode) {
                        continue;
                    }
                    let owner = name.get_or_insert_with(|| process_name(&path));
                    if let Some(owner) = owner {
                        self.inodes.insert(inode, owner.clone());
                    }
                }
            }
        }
        self.exact.clear();
        self.wildcard.clear();
        for (flow, inode) in rows {
            let Some(name) = self.inodes.get(&inode) else {
                continue;
            };
            if flow.remote.ip().is_unspecified() && flow.remote.port() == 0 {
                insert_unique(&mut self.wildcard, (flow.protocol, flow.local), name);
            } else {
                insert_unique(&mut self.exact, flow, name);
            }
        }
        Ok(())
    }

    pub fn lookup(&self, flow: &Flow) -> Option<&str> {
        // A connected socket takes precedence over a shared listening port.
        if let Some(owner) = self.exact.get(flow) {
            return owner.as_deref();
        }
        if let Some(owner) = self.wildcard.get(&(flow.protocol, flow.local)) {
            return owner.as_deref();
        }
        let any = if flow.local.is_ipv4() {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        } else {
            IpAddr::V6(Ipv6Addr::UNSPECIFIED)
        };
        self.wildcard
            .get(&(flow.protocol, SocketAddr::new(any, flow.local.port())))?
            .as_deref()
    }
}

fn process_name(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path.join("comm")).ok()?;
    let name: String = text
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(128)
        .collect();
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_addresses_and_inode_columns() {
        assert_eq!(
            address("0100007F:01BB"),
            Some("127.0.0.1:443".parse().unwrap())
        );
        assert_eq!(
            address("00000000000000000000000001000000:0035"),
            Some("[::1]:53".parse().unwrap())
        );
        let text="header\n0: 0100007F:C000 08080808:01BB 01 00000000:00000000 00:00000000 00000000 1000 0 123 1\n";
        let rows = sockets(text, 17);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, 123);
        assert_eq!(rows[0].0.remote, "8.8.8.8:443".parse().unwrap());
        assert!(address("bad:FFFF").is_none());
    }

    #[test]
    fn connected_udp_wildcards_and_conflicts_are_not_guessed() {
        let mut owners = Owners::default();
        let flow = Flow {
            protocol: 17,
            local: "192.168.1.2:4000".parse().unwrap(),
            remote: "8.8.8.8:443".parse().unwrap(),
        };
        insert_unique(
            &mut owners.wildcard,
            (17, "0.0.0.0:4000".parse().unwrap()),
            "server",
        );
        assert_eq!(owners.lookup(&flow), Some("server"));
        insert_unique(&mut owners.exact, flow.clone(), "chrome");
        assert_eq!(owners.lookup(&flow), Some("chrome"));
        insert_unique(&mut owners.exact, flow.clone(), "another");
        assert_eq!(owners.lookup(&flow), None);
        let mut tcp = flow.clone();
        tcp.protocol = 6;
        assert_eq!(owners.lookup(&tcp), None);
    }
}
