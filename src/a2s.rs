//! Steam server queries (A2S) over UDP: `A2S_INFO` for players, tags, and ping, and
//! `A2S_RULES`, where DayZ servers put their mod list in a binary form.
//!
//! The mod list layout (as documented by the MIT-licensed `dayzquery` and `WoozyMasta/a2s`
//! projects; written here from that description): rules whose key is two bytes, `[chunk number,
//! chunk count]`, hold pieces of one escaped blob. Joined in order and unescaped (`01 01` → `01`,
//! `01 02` → `00`, `01 03` → `FF`), it reads little-endian: version `u8`, overflow flags `u8`, DLC
//! flags `u16`, a `u32` hash per DLC flag set, mod count `u8`, then per mod a `u32` hash, the
//! Workshop id (its length `u8`, then that many bytes), and the name (length `u8`, then bytes).

use anyhow::{Context, Result, bail};
use std::collections::{HashMap, VecDeque};
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::server::{ModRef, clean_name};

pub(crate) const INFO_REQUEST: &[u8] = b"\xff\xff\xff\xffTSource Engine Query\0";
const RULES_REQUEST: &[u8] = b"\xff\xff\xff\xffV\xff\xff\xff\xff";

#[derive(Debug, Clone, Default)]
pub struct Info {
    pub name: String,
    /// The world name, such as `chernarusplus`.
    pub map: String,
    pub players: u32,
    pub max_players: u32,
    pub version: String,
    pub password: bool,
    pub game_port: Option<u16>,
    /// DayZ's tags, such as `battleye,no3rd,lqs0,mod,17:30`.
    pub keywords: String,
}

/// What DayZ's tags say. They're the same in A2S_INFO and Steam's server list.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tags {
    pub battleye: bool,
    pub first_person: bool,
    pub modded: bool,
    pub queue: Option<u32>,
    pub time: Option<String>,
}

pub fn parse_tags(keywords: &str) -> Tags {
    let mut tags = Tags::default();
    for tag in keywords.split(',').map(str::trim) {
        match tag {
            "battleye" => tags.battleye = true,
            "no3rd" => tags.first_person = true,
            "mod" => tags.modded = true,
            _ => {
                if let Some(n) = tag.strip_prefix("lqs").and_then(|n| n.parse().ok()) {
                    tags.queue = Some(n);
                } else if tag.len() == 5
                    && tag.as_bytes()[2] == b':'
                    && tag
                        .bytes()
                        .enumerate()
                        .all(|(i, b)| i == 2 || b.is_ascii_digit())
                {
                    tags.time = Some(tag.to_string());
                }
            }
        }
    }
    tags
}

#[derive(Debug, Clone, Default)]
pub struct Rules {
    /// The mods in load order.
    pub mods: Vec<ModRef>,
    /// The server's list didn't fit in its reply, so `mods` may be incomplete.
    pub truncated: bool,
}

/// Resolves `ip:port`.
pub fn resolve(addr: &str) -> Result<SocketAddr> {
    addr.to_socket_addrs()
        .with_context(|| format!("bad address {addr}"))?
        .next()
        .with_context(|| format!("bad address {addr}"))
}

/// Asks a server for its details, and how long the answer took.
pub fn info(addr: SocketAddr, timeout: Duration) -> Result<(Info, Duration)> {
    let (payload, took) = request(addr, INFO_REQUEST, false, timeout)?;
    Ok((parse_info(&payload)?, took))
}

/// Asks a DayZ server for its mod list.
pub fn rules(addr: SocketAddr, timeout: Duration) -> Result<Rules> {
    let (payload, _) = request(addr, RULES_REQUEST, true, timeout)?;
    decode_dayz(&parse_rules(&payload)?)
}

/// Sends a query and returns the reply's payload (after the `FF FF FF FF` header), answering a
/// challenge if the server sends one: INFO appends it, RULES replaces its placeholder.
fn request(
    addr: SocketAddr,
    request: &[u8],
    replace_challenge: bool,
    timeout: Duration,
) -> Result<(Vec<u8>, Duration)> {
    let socket = UdpSocket::bind(if addr.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    })?;
    socket.connect(addr)?;
    // The whole exchange, challenges and split replies included, has to fit in `timeout`.
    let deadline = Instant::now() + timeout;
    let mut sent = Instant::now();
    socket.send(request)?;
    for _ in 0..3 {
        let payload = receive(&socket, deadline).context("no reply")?;
        if payload.first() == Some(&0x41) && payload.len() >= 5 {
            let mut again = request.to_vec();
            if replace_challenge {
                again.truncate(again.len() - 4);
            }
            again.extend_from_slice(&payload[1..5]);
            sent = Instant::now();
            socket.send(&again)?;
            continue;
        }
        return Ok((payload, sent.elapsed()));
    }
    bail!("the server kept sending challenges")
}

/// One reply, reassembled if the server split it over several packets, by `deadline`.
fn receive(socket: &UdpSocket, deadline: Instant) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; 65536];
    let mut parts: Vec<Option<Vec<u8>>> = Vec::new();
    let mut reply_id = None;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            bail!("timed out");
        }
        socket.set_read_timeout(Some(left))?;
        let len = socket.recv(&mut buf)?;
        let packet = &buf[..len];
        if packet.len() < 5 {
            bail!("short packet");
        }
        match packet[..4] {
            [0xff, 0xff, 0xff, 0xff] => return Ok(packet[4..].to_vec()),
            [0xfe, 0xff, 0xff, 0xff] => {}
            _ => bail!("not a query reply"),
        }
        // Split: id (u32, top bit set when bzip2-compressed), count, number, size (u16).
        if packet.len() < 12 {
            bail!("short packet");
        }
        let id = u32::from_le_bytes(packet[4..8].try_into().unwrap());
        if id & 0x8000_0000 != 0 {
            bail!("compressed replies aren't supported");
        }
        let (total, number) = (packet[8] as usize, packet[9] as usize);
        if total == 0 || number >= total {
            bail!("bad split packet");
        }
        if *reply_id.get_or_insert(id) != id {
            continue; // A piece of some other (earlier) reply.
        }
        if parts.is_empty() {
            parts = vec![None; total];
        }
        if parts.len() != total {
            bail!("mixed split replies");
        }
        parts[number] = Some(packet[12..].to_vec());
        if parts.iter().all(Option::is_some) {
            let joined: Vec<u8> = parts.into_iter().flatten().flatten().collect();
            return match joined.get(..4) {
                Some([0xff, 0xff, 0xff, 0xff]) => Ok(joined[4..].to_vec()),
                _ => bail!("not a query reply"),
            };
        }
    }
}

struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.data.len() < n {
            bail!("reply ended early");
        }
        let (head, rest) = self.data.split_at(n);
        self.data = rest;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    /// A NUL-terminated string as bytes (the rest, if there's no NUL).
    fn cbytes(&mut self) -> &'a [u8] {
        let end = self.data.iter().position(|&b| b == 0);
        let (s, rest) = match end {
            Some(i) => (&self.data[..i], &self.data[i + 1..]),
            None => (self.data, &[][..]),
        };
        self.data = rest;
        s
    }
    fn cstr(&mut self) -> String {
        String::from_utf8_lossy(self.cbytes()).into_owned()
    }
}

pub(crate) fn parse_info(payload: &[u8]) -> Result<Info> {
    let mut r = Reader { data: payload };
    if r.u8()? != 0x49 {
        bail!("not an info reply");
    }
    let _protocol = r.u8()?;
    let mut info = Info {
        name: clean_name(&r.cstr()),
        map: r.cstr().to_lowercase(),
        ..Default::default()
    };
    let _folder = r.cstr();
    let _game = r.cstr();
    let _id = r.u16()?;
    info.players = r.u8()?.into();
    info.max_players = r.u8()?.into();
    let _bots = r.u8()?;
    let _server_type = r.u8()?;
    let _environment = r.u8()?;
    info.password = r.u8()? != 0;
    let _vac = r.u8()?;
    info.version = r.cstr();
    // Extra data, flagged by bits. Optional, so a reply cut short here still counts.
    let _ = read_extra(&mut r, &mut info);
    Ok(info)
}

fn read_extra(r: &mut Reader, info: &mut Info) -> Result<()> {
    let edf = r.u8()?;
    if edf & 0x80 != 0 {
        info.game_port = Some(r.u16()?);
    }
    if edf & 0x10 != 0 {
        r.take(8)?;
    }
    if edf & 0x40 != 0 {
        r.u16()?;
        r.cbytes();
    }
    if edf & 0x20 != 0 {
        info.keywords = r.cstr();
    }
    Ok(())
}

/// The rules as raw key and value bytes. A reply cut short gives the pairs read so far.
fn parse_rules(payload: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let mut r = Reader { data: payload };
    if r.u8()? != 0x45 {
        bail!("not a rules reply");
    }
    let count = r.u16()?;
    let mut pairs = Vec::new();
    for _ in 0..count {
        if r.data.is_empty() {
            break;
        }
        let key = r.cbytes().to_vec();
        let value = r.cbytes().to_vec();
        pairs.push((key, value));
    }
    Ok(pairs)
}

fn unescape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == 0x01 && i + 1 < data.len() {
            match data[i + 1] {
                0x01 => out.push(0x01),
                0x02 => out.push(0x00),
                0x03 => out.push(0xff),
                other => out.extend_from_slice(&[0x01, other]),
            }
            i += 2;
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out
}

fn decode_dayz(pairs: &[(Vec<u8>, Vec<u8>)]) -> Result<Rules> {
    let mut chunks: Vec<(u8, u8, &[u8])> = pairs
        .iter()
        .filter(|(k, _)| k.len() == 2 && k[0] >= 1)
        .map(|(k, v)| (k[0], k[1], v.as_slice()))
        .collect();
    if chunks.is_empty() {
        bail!("the server didn't send a mod list");
    }
    chunks.sort_by_key(|c| c.0);
    let total = chunks[0].1;
    let mut joined = Vec::new();
    let mut truncated = false;
    for (expected, (index, _, value)) in (1u8..).zip(&chunks) {
        if *index != expected {
            truncated = true;
            break;
        }
        joined.extend_from_slice(value);
    }
    if chunks.len() < total as usize {
        truncated = true;
    }
    let data = unescape(&joined);
    let mut r = Reader { data: &data };
    let mut rules = Rules {
        truncated,
        ..Default::default()
    };
    let _version = r.u8()?;
    let overflow = r.u8()?;
    rules.truncated |= overflow != 0;
    // DLC flags (0x1 Livonia, 0x2 Frostline), each followed by a hash.
    let dlc = r.u16()?;
    for _ in 0..dlc.count_ones() {
        r.u32()?;
    }
    let count = r.u8()?;
    for _ in 0..count {
        let Some(m) = read_mod(&mut r) else {
            rules.truncated = true;
            break;
        };
        rules.mods.push(m);
    }
    Ok(rules)
}

fn read_mod(r: &mut Reader) -> Option<ModRef> {
    let _hash = r.u32().ok()?;
    let id_len = r.u8().ok()? as usize;
    if id_len > 8 {
        return None;
    }
    let id_bytes = r.take(id_len).ok()?;
    let id = id_bytes
        .iter()
        .rev()
        .fold(0u64, |acc, &b| (acc << 8) | u64::from(b));
    let name_len = r.u8().ok()? as usize;
    let name = r.take(name_len).ok()?;
    Some(ModRef {
        id,
        name: clean_name(&String::from_utf8_lossy(name)),
    })
}

/// A ping result for one server.
pub struct Pong {
    pub addr: String,
    /// Round trip in milliseconds, or `None` if it didn't answer in time.
    pub ping: Option<u32>,
    pub info: Option<Info>,
}

/// Pings many servers at once from one socket: keeps a few dozen queries in flight and reports
/// each answer (or timeout) through the callback.
pub struct Pinger {
    /// Addresses to ping, and whether each goes ahead of the queue.
    tx: mpsc::Sender<(String, bool)>,
}

const IN_FLIGHT: usize = 48;
const PING_TIMEOUT: Duration = Duration::from_millis(1200);

impl Pinger {
    pub fn spawn(report: impl Fn(Pong) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel::<(String, bool)>();
        std::thread::Builder::new()
            .name("pinger".into())
            .spawn(move || {
                if let Err(e) = run_pinger(&rx, &report) {
                    log::warn!("pinger stopped: {e:#}");
                }
            })
            .expect("can start a thread");
        Self { tx }
    }

    /// Pings `ip:query_port` next (a server on screen).
    pub fn request(&self, addr: String) {
        let _ = self.tx.send((addr, true));
    }

    /// Queues `ip:query_port` behind everything else (a sweep of the whole list).
    pub fn request_later(&self, addr: String) {
        let _ = self.tx.send((addr, false));
    }
}

struct Flight {
    addr: String,
    sent: Instant,
    challenges: u8,
}

fn run_pinger(rx: &mpsc::Receiver<(String, bool)>, report: &dyn Fn(Pong)) -> Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_read_timeout(Some(Duration::from_millis(15)))?;
    let mut queue: VecDeque<String> = VecDeque::new();
    let mut flights: HashMap<SocketAddr, Flight> = HashMap::new();
    let mut answered: HashMap<SocketAddr, Instant> = HashMap::new();
    let mut buf = vec![0u8; 4096];
    loop {
        if queue.is_empty() && flights.is_empty() {
            answered.retain(|_, at| at.elapsed() < Duration::from_secs(10));
            // Nothing to do: wait for work (and stop when the browser is gone).
            match rx.recv() {
                Ok((addr, _)) => queue.push_back(addr),
                Err(_) => return Ok(()),
            }
        }
        while let Ok((addr, urgent)) = rx.try_recv() {
            if urgent {
                queue.push_front(addr);
            } else {
                queue.push_back(addr);
            }
        }
        while flights.len() < IN_FLIGHT
            && let Some(addr) = queue.pop_front()
        {
            let Ok(target) = addr.parse::<SocketAddr>() else {
                report(Pong {
                    addr,
                    ping: None,
                    info: None,
                });
                continue;
            };
            // Already on its way, or just answered (a server can be queued twice: once by a
            // sweep, once more when it comes on screen).
            if flights.contains_key(&target)
                || answered
                    .get(&target)
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(10))
            {
                continue;
            }
            if socket.send_to(INFO_REQUEST, target).is_ok() {
                flights.insert(
                    target,
                    Flight {
                        addr,
                        sent: Instant::now(),
                        challenges: 0,
                    },
                );
            } else {
                // Unreachable (offline, or an IPv6 server): report it rather than drop it.
                report(Pong {
                    addr,
                    ping: None,
                    info: None,
                });
            }
        }
        match socket.recv_from(&mut buf) {
            Ok((len, from)) => {
                let packet = &buf[..len];
                if let Some(flight) = flights.get_mut(&from)
                    && packet.len() > 5
                    && packet[..4] == [0xff; 4]
                {
                    let payload = &packet[4..];
                    if payload[0] == 0x41 && payload.len() >= 5 && flight.challenges < 2 {
                        let mut again = INFO_REQUEST.to_vec();
                        again.extend_from_slice(&payload[1..5]);
                        flight.sent = Instant::now();
                        flight.challenges += 1;
                        let _ = socket.send_to(&again, from);
                    } else if payload[0] == 0x49 {
                        let ping = flight.sent.elapsed().as_millis() as u32;
                        let flight = flights.remove(&from).unwrap();
                        answered.insert(from, Instant::now());
                        report(Pong {
                            addr: flight.addr,
                            ping: Some(ping.max(1)),
                            info: parse_info(payload).ok(),
                        });
                    }
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            // Windows reports an earlier send's ICMP "port unreachable" here; ignore it.
            Err(_) => {}
        }
        let now = Instant::now();
        let expired: Vec<SocketAddr> = flights
            .iter()
            .filter(|(_, f)| now - f.sent > PING_TIMEOUT)
            .map(|(a, _)| *a)
            .collect();
        for target in expired {
            let flight = flights.remove(&target).unwrap();
            answered.insert(target, Instant::now());
            report(Pong {
                addr: flight.addr,
                ping: None,
                info: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Escapes like a DayZ server does.
    fn escape(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for &b in data {
            match b {
                0x01 => out.extend_from_slice(&[0x01, 0x01]),
                0x00 => out.extend_from_slice(&[0x01, 0x02]),
                0xff => out.extend_from_slice(&[0x01, 0x03]),
                b => out.push(b),
            }
        }
        out
    }

    fn blob(mods: &[(u32, &str)], dlc: u16) -> Vec<u8> {
        let mut b = vec![2u8, 0];
        b.extend_from_slice(&dlc.to_le_bytes());
        for _ in 0..dlc.count_ones() {
            b.extend_from_slice(&0xdead_beefu32.to_le_bytes());
        }
        b.push(mods.len() as u8);
        for (id, name) in mods {
            b.extend_from_slice(&0x0102_00ffu32.to_le_bytes());
            b.push(4);
            b.extend_from_slice(&id.to_le_bytes());
            b.push(name.len() as u8);
            b.extend_from_slice(name.as_bytes());
        }
        b.push(0); // no signatures
        b
    }

    fn rules_reply(chunks: &[Vec<u8>], total: u8, extra: &[(&str, &str)]) -> Vec<u8> {
        let mut p = vec![0x45];
        p.extend_from_slice(&((chunks.len() + extra.len()) as u16).to_le_bytes());
        for (k, v) in extra {
            p.extend_from_slice(k.as_bytes());
            p.push(0);
            p.extend_from_slice(v.as_bytes());
            p.push(0);
        }
        for (i, c) in chunks.iter().enumerate() {
            p.extend_from_slice(&[i as u8 + 1, total, 0]);
            p.extend_from_slice(c);
            p.push(0);
        }
        p
    }

    #[test]
    fn decodes_mod_list() {
        // Ids with bytes that need escaping (01, 00, FF).
        let mods = [(1559212036, "Community Framework"), (0x01ff_0001, "Odd Id")];
        let escaped = escape(&blob(&mods, 0x1));
        let (a, b) = escaped.split_at(escaped.len() / 2);
        let reply = rules_reply(&[a.to_vec(), b.to_vec()], 2, &[("island", "enoch")]);
        let rules = decode_dayz(&parse_rules(&reply).unwrap()).unwrap();
        assert!(!rules.truncated);
        assert_eq!(rules.mods.len(), 2);
        assert_eq!(rules.mods[0].id, 1559212036);
        assert_eq!(rules.mods[0].name, "Community Framework");
        assert_eq!(rules.mods[1].id, 0x01ff_0001);
    }

    #[test]
    fn unescapes_in_one_pass() {
        // `01 01` then `02` is 01 followed by 02, not 00.
        assert_eq!(unescape(&[0x01, 0x01, 0x02]), vec![0x01, 0x02]);
        assert_eq!(unescape(&[0x01, 0x02, 0x01, 0x03]), vec![0x00, 0xff]);
    }

    #[test]
    fn missing_chunk_is_truncated() {
        let escaped = escape(&blob(&[(1, "A"), (2, "B"), (3, "C")], 0));
        let first = escaped[..escaped.len() - 3].to_vec();
        let reply = rules_reply(&[first], 2, &[]);
        let rules = decode_dayz(&parse_rules(&reply).unwrap()).unwrap();
        assert!(rules.truncated);
        assert_eq!(rules.mods.len(), 2);
    }

    #[test]
    fn reads_info_and_tags() {
        let mut p = vec![0x49, 17];
        for s in ["My Server", "ChernarusPlus", "dayz", ""] {
            p.extend_from_slice(s.as_bytes());
            p.push(0);
        }
        p.extend_from_slice(&0u16.to_le_bytes());
        p.extend_from_slice(&[12, 60, 0, b'd', b'w', 1, 1]);
        p.extend_from_slice(b"1.28.160049\0");
        p.push(0x80 | 0x20);
        p.extend_from_slice(&2302u16.to_le_bytes());
        p.extend_from_slice(b"battleye,no3rd,lqs4,mod,17:30\0");
        let info = parse_info(&p).unwrap();
        assert_eq!((info.players, info.max_players), (12, 60));
        assert!(info.password);
        assert_eq!(info.game_port, Some(2302));
        let tags = parse_tags(&info.keywords);
        assert_eq!(
            tags,
            Tags {
                battleye: true,
                first_person: true,
                modded: true,
                queue: Some(4),
                time: Some("17:30".into()),
            }
        );
    }
}
