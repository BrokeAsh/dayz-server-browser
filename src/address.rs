//! Finding a server from an address someone typed, for servers the list doesn't have. Players
//! usually know the game port (the one in `-connect`), but servers answer queries on a separate
//! port: most often the game port + 1, or 27016. So this asks the likely ones at once and keeps
//! the server that says it plays on the typed port.

use anyhow::{Context, Result, anyhow, bail};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::time::Duration;

use crate::a2s::{self, Info};
use crate::server::Server;

const TIMEOUT: Duration = Duration::from_secs(2);
/// DayZ's own default game port, assumed when an address leaves the port out.
pub const DEFAULT_PORT: u16 = 2302;

/// A server that answered, with its answer and ping (ms).
pub struct Found {
    pub server: Server,
    pub info: Info,
    pub ping: u32,
}

/// Splits `host:port` (or a bare host, meaning port 2302) and resolves the host to IPv4.
pub fn parse(text: &str) -> Result<(IpAddr, u16)> {
    let text = text.trim();
    if text.is_empty() {
        bail!("type the server's address");
    }
    let (host, port) = match text.rsplit_once(':') {
        Some((host, port)) => (
            host,
            port.parse::<u16>()
                .ok()
                .filter(|p| *p > 0)
                .with_context(|| format!("{port} isn't a port number"))?,
        ),
        None => (text, DEFAULT_PORT),
    };
    // The system would read "1.2.3" as 1.2.0.3, which is never what was meant.
    if host.chars().all(|c| c.is_ascii_digit() || c == '.') && host.parse::<Ipv4Addr>().is_err() {
        bail!("{host} isn't a whole IP address (it has four numbers, such as 203.0.113.5)");
    }
    let ip = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("couldn't find {host}"))?
        .map(|a| a.ip())
        .find(IpAddr::is_ipv4)
        .with_context(|| format!("couldn't find an IPv4 address for {host}"))?;
    Ok((ip, port))
}

/// Finds the server at a typed address: `port` may be its game port or its query port.
pub fn find(ip: IpAddr, port: u16) -> Result<Found> {
    let mut ports: Vec<u16> = Vec::new();
    for offset in [0, 1, 24714, 3, 2, 4, 10] {
        if let Some(p) = port.checked_add(offset)
            && !ports.contains(&p)
        {
            ports.push(p);
        }
    }
    if !ports.contains(&27016) {
        // Third, after the port itself and the port + 1 (the two commonest).
        ports.insert(ports.len().min(2), 27016);
    }
    let answers: Vec<(u16, Info, Duration)> = std::thread::scope(|scope| {
        let asks: Vec<_> = ports
            .iter()
            .map(|&p| {
                scope.spawn(move || {
                    a2s::info(SocketAddr::new(ip, p), TIMEOUT)
                        .ok()
                        .map(|(info, took)| (p, info, took))
                })
            })
            .collect();
        asks.into_iter()
            .filter_map(|ask| ask.join().ok().flatten())
            .collect()
    });
    // One that plays on the typed port, in the order asked (the likeliest first); else the typed
    // port was the query port itself.
    let pick = answers
        .iter()
        .find(|(_, info, _)| info.game_port == Some(port))
        .or_else(|| answers.iter().find(|(p, _, _)| *p == port));
    let Some((query_port, info, took)) = pick else {
        bail!(
            "no DayZ server answered at {ip}:{port}. Check the address; use the game port from \
             the server's ad, or its query port."
        );
    };
    Ok(found(ip, *query_port, info.clone(), *took))
}

/// Asks a server whose query address (`ip:query_port`) is known.
pub fn query(addr: &str) -> Result<Found> {
    let target = a2s::resolve(addr)?;
    let (info, took) = a2s::info(target, TIMEOUT).map_err(|e| {
        if a2s::no_answer(&e) {
            anyhow!("it didn't answer within {} seconds", TIMEOUT.as_secs())
        } else {
            e.context("couldn't ask it")
        }
    })?;
    Ok(found(target.ip(), target.port(), info, took))
}

/// The server a query address answered for, as the list shows servers.
pub fn found(ip: IpAddr, query_port: u16, info: Info, took: Duration) -> Found {
    let tags = a2s::parse_tags(&info.keywords);
    let server = Server {
        name: info.name.clone(),
        map: info.map.clone(),
        ip: ip.to_string(),
        game_port: info.game_port.unwrap_or(query_port),
        query_port,
        players: info.players,
        max_players: info.max_players,
        time: tags.time.clone(),
        version: info.version.clone(),
        password: info.password,
        first_person: tags.first_person,
        reported_first_person: tags.first_person,
        battleye: tags.battleye,
        queue: tags.queue,
        mods: Vec::new(),
        modded: tags.modded,
        official: tags.official(),
        day_speed: tags.day_speed,
        night_speed: tags.night_speed(),
        country: None,
    };
    Found {
        server,
        info,
        ping: took.as_millis() as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_addresses() {
        let ip: IpAddr = "203.0.113.5".parse().unwrap();
        assert_eq!(parse("203.0.113.5:2402").unwrap(), (ip, 2402));
        assert_eq!(parse(" 203.0.113.5 ").unwrap(), (ip, 2302));
        assert!(parse("203.0.113.5:port").is_err());
        assert!(parse("203.0.113.5:0").is_err());
        assert!(parse("").is_err());
        // Not read as 1.2.0.3.
        assert!(parse("1.2.3:2302").is_err());
        assert!(parse("1.2.3.4.5").is_err());
    }

    #[test]
    fn says_when_a_server_doesnt_answer() {
        // Nothing answers queries on a fresh socket's port.
        let quiet = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let e = query(&quiet.local_addr().unwrap().to_string())
            .err()
            .unwrap();
        assert_eq!(e.to_string(), "it didn't answer within 2 seconds");
    }
}
