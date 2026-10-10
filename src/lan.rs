//! Finding DayZ servers on the local network: asks every address in this computer's /24 for its
//! details (`A2S_INFO`) on the usual query ports, from one socket, and keeps what answers.

use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

use crate::a2s;
use crate::address::{self, Found};

/// DayZ's default query port, and the game port + 1 that many servers use instead.
pub const PORTS: [u16; 2] = [27016, 2303];
/// How long answers are collected after the last query goes out.
const LISTEN: Duration = Duration::from_secs(2);
/// Queries sent from each socket.
const PER_SOCKET: usize = 48;

/// What a scan found, and where it looked.
pub struct Scan {
    /// Such as `192.168.1.0/24`.
    pub subnet: String,
    pub ports: Vec<u16>,
    pub found: Vec<Found>,
}

/// This computer's address on its local network: the one it would send from to the internet, or
/// failing that (no internet) to a common home router address. No packet is sent.
pub fn local_ipv4() -> Result<Ipv4Addr> {
    for probe in [
        "1.1.1.1:9",
        "192.168.1.1:9",
        "192.168.0.1:9",
        "10.0.0.1:9",
        "172.16.0.1:9",
    ] {
        let ip = UdpSocket::bind("0.0.0.0:0")
            .and_then(|s| s.connect(probe).map(|()| s))
            .and_then(|s| s.local_addr());
        if let Ok(SocketAddr::V4(addr)) = ip
            && addr.ip().is_private()
        {
            return Ok(*addr.ip());
        }
    }
    bail!("this computer doesn't seem to be on a home or office network")
}

/// The other addresses in `ip`'s /24 (its own included, for a server on this computer).
pub fn subnet_hosts(ip: Ipv4Addr) -> Vec<Ipv4Addr> {
    let [a, b, c, _] = ip.octets();
    (1..=254).map(|d| Ipv4Addr::new(a, b, c, d)).collect()
}

/// The ports to ask: the usual ones, and `extra` if given.
pub fn ports(extra: Option<u16>) -> Vec<u16> {
    let mut ports = PORTS.to_vec();
    if let Some(port) = extra.filter(|p| *p > 0 && !ports.contains(p)) {
        ports.push(port);
    }
    ports
}

/// Asks every address in the local /24 on the usual query ports (and `extra`).
pub fn scan(extra: Option<u16>) -> Result<Scan> {
    let ip = local_ipv4()?;
    let ports = ports(extra);
    let [a, b, c, _] = ip.octets();
    let targets: Vec<SocketAddr> = subnet_hosts(ip)
        .into_iter()
        .flat_map(|host| {
            ports
                .iter()
                .map(move |&p| SocketAddr::V4(SocketAddrV4::new(host, p)))
        })
        .collect();
    let found = ask_all(&targets)?;
    Ok(Scan {
        subnet: format!("{a}.{b}.{c}.0/24"),
        ports,
        found,
    })
}

/// Sends `A2S_INFO` to each target and collects the answers (answering challenges) until
/// `LISTEN` after the last one went out.
fn ask_all(targets: &[SocketAddr]) -> Result<Vec<Found>> {
    // Queries to addresses nobody has wait in the system for an address lookup that fails after
    // a few seconds, and count against their socket's send buffer meanwhile: from one socket,
    // sending would stall. Spread over several, none fills up.
    let sockets: Vec<UdpSocket> = (0..targets.len().div_ceil(PER_SOCKET).max(1))
        .map(|_| {
            let socket = UdpSocket::bind("0.0.0.0:0")?;
            socket.set_nonblocking(true)?;
            Ok(socket)
        })
        .collect::<std::io::Result<_>>()
        .context("couldn't open a network socket")?;
    // When each was asked, and how many challenges it sent back.
    let mut asked: HashMap<SocketAddr, (Instant, u8)> = HashMap::new();
    for (chunk, socket) in targets.chunks(PER_SOCKET).zip(&sockets) {
        for &target in chunk {
            // Errors (an unreachable address, a full buffer) only mean no answer.
            if socket.send_to(a2s::INFO_REQUEST, target).is_ok() {
                asked.insert(target, (Instant::now(), 0));
            }
        }
    }
    let mut answers: Vec<Found> = Vec::new();
    let mut buf = vec![0u8; 4096];
    let deadline = Instant::now() + LISTEN;
    while Instant::now() < deadline && !asked.is_empty() {
        for socket in &sockets {
            // Everything waiting (bounded, in case errors keep coming).
            for _ in 0..256 {
                let (len, from) = match socket.recv_from(&mut buf) {
                    Ok(got) => got,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    // On Windows, an earlier query's "port unreachable".
                    Err(_) => continue,
                };
                let packet = &buf[..len];
                let Some((sent, challenges)) = asked.get_mut(&from) else {
                    continue;
                };
                if packet.len() <= 5 || packet[..4] != [0xff; 4] {
                    continue;
                }
                let payload = &packet[4..];
                if payload[0] == 0x41 && payload.len() >= 5 && *challenges < 2 {
                    let mut again = a2s::INFO_REQUEST.to_vec();
                    again.extend_from_slice(&payload[1..5]);
                    *sent = Instant::now();
                    *challenges += 1;
                    let _ = socket.send_to(&again, from);
                } else if payload[0] == 0x49
                    && let Ok(info) = a2s::parse_info(payload)
                {
                    let took = sent.elapsed().max(Duration::from_millis(1));
                    asked.remove(&from);
                    answers.push(address::found(from.ip(), from.port(), info, took));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(tidy(answers))
}

/// In address order, each server once (one answering on two ports counts once).
fn tidy(mut found: Vec<Found>) -> Vec<Found> {
    let key = |f: &Found| {
        let ip: IpAddr = f
            .server
            .ip
            .parse()
            .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        (ip, f.server.game_port, f.server.query_port)
    };
    found.sort_by_key(key);
    let mut seen = std::collections::HashSet::new();
    found.retain(|f| seen.insert((f.server.ip.clone(), f.server.game_port)));
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a2s::Info;

    #[test]
    fn covers_the_subnet() {
        let hosts = subnet_hosts(Ipv4Addr::new(192, 168, 1, 128));
        assert_eq!(hosts.len(), 254);
        assert_eq!(hosts[0], Ipv4Addr::new(192, 168, 1, 1));
        assert_eq!(hosts[253], Ipv4Addr::new(192, 168, 1, 254));
        assert!(hosts.contains(&Ipv4Addr::new(192, 168, 1, 128)), "itself");
        assert_eq!(ports(None), [27016, 2303]);
        assert_eq!(ports(Some(2303)), [27016, 2303]);
        assert_eq!(ports(Some(2403)), [27016, 2303, 2403]);
        assert_eq!(ports(Some(0)), [27016, 2303]);
    }

    #[test]
    fn lists_each_server_once() {
        let at = |ip: &str, query: u16, game: u16| {
            address::found(
                ip.parse().unwrap(),
                query,
                Info {
                    game_port: Some(game),
                    ..Default::default()
                },
                Duration::from_millis(3),
            )
        };
        let found = tidy(vec![
            at("192.168.1.20", 27016, 2302),
            at("192.168.1.3", 2303, 2302),
            at("192.168.1.20", 2303, 2302),
            at("192.168.1.20", 2403, 2402),
        ]);
        let list: Vec<String> = found.iter().map(|f| f.server.addr()).collect();
        assert_eq!(
            list,
            ["192.168.1.3:2303", "192.168.1.20:2303", "192.168.1.20:2403"]
        );
    }

    #[test]
    fn finds_a_server_answering_locally() {
        // A stand-in server on this computer that sends a challenge first, like DayZ does.
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        std::thread::spawn(move || {
            let mut buf = [0u8; 1500];
            let (_, from) = server.recv_from(&mut buf).unwrap();
            server
                .send_to(b"\xff\xff\xff\xffA\x01\x02\x03\x04", from)
                .unwrap();
            let (len, from) = server.recv_from(&mut buf).unwrap();
            assert!(buf[..len].ends_with(&[1, 2, 3, 4]), "challenge answered");
            let mut reply = b"\xff\xff\xff\xffI\x11".to_vec();
            for s in ["LAN Server", "enoch", "dayz", "DayZ"] {
                reply.extend_from_slice(s.as_bytes());
                reply.push(0);
            }
            reply.extend_from_slice(&[0, 0, 2, 10, 0, b'd', b'w', 0, 0]);
            reply.extend_from_slice(b"1.28\0");
            server.send_to(&reply, from).unwrap();
        });
        let quiet = SocketAddr::from(([127, 0, 0, 1], 9));
        let found = ask_all(&[quiet, addr]).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].server.name, "LAN Server");
        assert_eq!(found[0].server.map, "enoch");
        assert_eq!(found[0].server.query_port, addr.port());
        assert_eq!(
            (found[0].server.players, found[0].server.max_players),
            (2, 10)
        );
    }
}
