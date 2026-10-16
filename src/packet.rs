use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Flow {
    pub protocol: u8,
    pub local: SocketAddr,
    pub remote: SocketAddr,
}

pub fn normalize(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(address),
        _ => address,
    }
}

/// Only reads network and transport headers from a truncated packet prefix.
pub fn flow(header: &[u8], outgoing: bool) -> Option<Flow> {
    let (source, destination, mut protocol, mut offset, limit) = match header.first()? >> 4 {
        4 => {
            if header.len() < 20 {
                return None;
            }
            let offset = usize::from(header[0] & 15) * 4;
            let total = usize::from(u16::from_be_bytes([header[2], header[3]]));
            if offset < 20
                || total < offset + 4
                || header.len() < offset + 4
                || u16::from_be_bytes([header[6], header[7]]) & 0x1fff != 0
            {
                return None;
            }
            (
                IpAddr::V4(Ipv4Addr::new(
                    header[12], header[13], header[14], header[15],
                )),
                IpAddr::V4(Ipv4Addr::new(
                    header[16], header[17], header[18], header[19],
                )),
                header[9],
                offset,
                total,
            )
        }
        6 => {
            if header.len() < 40 {
                return None;
            }
            let source = IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(&header[8..24]).ok()?));
            let destination =
                IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(&header[24..40]).ok()?));
            let payload = usize::from(u16::from_be_bytes([header[4], header[5]]));
            // GSO packets can use a zero IPv6 payload length before segmentation.
            (
                source,
                destination,
                header[6],
                40,
                if payload == 0 {
                    usize::MAX
                } else {
                    40 + payload
                },
            )
        }
        _ => return None,
    };
    for _ in 0..8 {
        if protocol == 6 || protocol == 17 {
            if offset + 4 > limit {
                return None;
            }
            let ports = header.get(offset..offset + 4)?;
            let source =
                SocketAddr::new(normalize(source), u16::from_be_bytes([ports[0], ports[1]]));
            let destination = SocketAddr::new(
                normalize(destination),
                u16::from_be_bytes([ports[2], ports[3]]),
            );
            let (local, remote) = if outgoing {
                (source, destination)
            } else {
                (destination, source)
            };
            return Some(Flow {
                protocol,
                local,
                remote,
            });
        }
        let extension = header.get(offset..offset + 2)?;
        let length = match protocol {
            0 | 43 | 60 => (usize::from(extension[1]) + 1) * 8,
            51 => (usize::from(extension[1]) + 2) * 4,
            44 => {
                let fragment = header.get(offset + 2..offset + 4)?;
                if u16::from_be_bytes([fragment[0], fragment[1]]) & 0xfff8 != 0 {
                    return None;
                }
                8
            }
            _ => return None,
        };
        protocol = extension[0];
        offset += length;
        if offset > limit {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_udp_and_tcp_directions_and_fragment_safety() {
        let mut header = vec![0u8; 40];
        header[0] = 0x45;
        header[3] = 40;
        header[9] = 17;
        header[12..16].copy_from_slice(&[192, 168, 1, 2]);
        header[16..20].copy_from_slice(&[8, 8, 8, 8]);
        header[20..24].copy_from_slice(&[0xc0, 0x00, 0x01, 0xbb]);
        let udp = flow(&header, true).unwrap();
        assert_eq!(udp.local, "192.168.1.2:49152".parse().unwrap());
        assert_eq!(udp.remote, "8.8.8.8:443".parse().unwrap());
        assert_eq!(udp.protocol, 17);
        assert_eq!(flow(&header, false).unwrap().local, udp.remote);
        header[9] = 6;
        assert_eq!(flow(&header, true).unwrap().protocol, 6);
        header[7] = 1;
        assert!(flow(&header, true).is_none());
        for length in 0..24 {
            assert!(flow(&header[..length], true).is_none());
        }
    }

    #[test]
    fn ipv6_extension_headers_and_mapped_addresses() {
        let mut header = vec![0u8; 64];
        header[0] = 0x60;
        header[5] = 24;
        header[6] = 0;
        header[8..24].copy_from_slice(&Ipv6Addr::LOCALHOST.octets());
        header[24..40]
            .copy_from_slice(&"2001:4860:4860::8888".parse::<Ipv6Addr>().unwrap().octets());
        header[40] = 17;
        header[48..52].copy_from_slice(&[0x12, 0x34, 0x00, 0x35]);
        let packet = flow(&header, true).unwrap();
        assert_eq!(packet.local.port(), 0x1234);
        assert_eq!(packet.remote.port(), 53);
        header[6] = 44;
        header[42] = 0;
        header[43] = 8;
        assert!(flow(&header, true).is_none());
        assert_eq!(
            normalize("::ffff:192.0.2.1".parse().unwrap()),
            "192.0.2.1".parse::<IpAddr>().unwrap()
        );
    }
}
