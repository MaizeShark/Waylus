// SPDX-License-Identifier: GPL-3.0-or-later
//! Wi-Fi (LAN) transport. Design and rationale: docs/wifi.md.
//!
//! The tablet connects over TCP. A paired tablet authenticates with its pairing key and
//! an ephemeral ECDH exchange; an unpaired one can only pair (PIN + SPAKE2), and only
//! while the host runs with `--pair`. Nothing from an unauthenticated peer reaches the
//! session: every TCP record and UDP datagram after the handshake is AES-GCM sealed.
//!
//! Reliable messages (hello, config, layout, codec config) go over TCP records, the
//! latency-sensitive ones (input, frame reports, ping/pong) over UDP.

mod crypto;
pub mod spake2;
mod store;
mod video;

use std::{
    io::{self, Read, Write},
    net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use p256::{PublicKey, ecdh::EphemeralSecret, elliptic_curve::sec1::ToEncodedPoint};
use rand_core::{OsRng, RngCore};
use subtle::ConstantTimeEq;
use tracing::{debug, info, warn};

use crate::{
    link::{IDLE_TIMEOUT, Link},
    proto::{self, ty},
};
use crypto::{Aead, ReplayWindow, TAG_LEN, stream};
pub use store::Store;

/// How long pairing stays possible with `--pair`.
const PAIR_WINDOW: Duration = Duration::from_secs(300);
const MAX_PAIR_FAILURES: u32 = 3;
const PAIR_AAD: &[u8] = b"waylus pairing v1";
const HANDSHAKE_MAX: u32 = 4096;
const MAX_RECORD: u32 = 16 * 1024 * 1024 + 64;

/// AUTH_FAIL reasons (shown on the tablet).
pub mod fail {
    pub const UNKNOWN_DEVICE: u8 = 1;
    pub const BAD_MAC: u8 = 2;
    pub const PAIRING_OFF: u8 = 3;
    pub const WRONG_PIN: u8 = 4;
    pub const PROTOCOL: u8 = 5;
}

/// UDP datagram kinds (first byte).
mod kind {
    pub const CONTROL: u8 = 1;
}
const UDP_HDR: usize = 12;

/// DSCP AF41 (34): WMM video access category (RFC 8325), for video and pings. The
/// tablet tags its input EF (voice).
const TOS_AF41: u32 = 34 << 2;

pub struct Opts {
    pub port: u16,
    /// Video FEC parity, percent of data shards (at least 2 parity shards per block).
    pub fec_percent: usize,
    /// Addresses to listen on; empty = every private IPv4 address of this machine.
    pub listen: Vec<IpAddr>,
    pub pair: bool,
}

struct Pairing {
    until: Option<Instant>,
    failures: u32,
}

impl Pairing {
    fn open(&self) -> bool {
        self.until.is_some_and(|t| Instant::now() < t) && self.failures < MAX_PAIR_FAILURES
    }
}

/// Shared state of one listening address.
struct Hub {
    udp: UdpSocket,
    /// The newest authenticated session; only it receives UDP.
    current: Mutex<Option<Arc<UdpSession>>>,
}

struct UdpSession {
    rx: Aead,
    replay: Mutex<ReplayWindow>,
    chunks: Sender<Chunk>,
    peer: Mutex<Option<SocketAddr>>,
    /// Newest pen / touch sample time forwarded (drops the redundant copies).
    last_pen: AtomicU64,
    last_touch: AtomicU64,
    rejected: AtomicU64,
    /// Frame numbers of recent video frames, shared with the video sender.
    frames: video::FrameMap,
}

type Chunk = Result<Vec<u8>, io::ErrorKind>;

pub struct Server {
    links: Receiver<Link>,
    pub addrs: Vec<SocketAddr>,
    /// mDNS advertisement (`_waylus._tcp`), kept alive with the server.
    _mdns: Option<mdns_sd::ServiceDaemon>,
}

pub const MDNS_TYPE: &str = "_waylus._tcp.local.";

/// Advertises the listener so tablets find the host after DHCP changes. A separate
/// host name avoids clashing with the records Avahi publishes for this machine.
fn advertise(host_id: &str, addrs: &[SocketAddr]) -> Result<mdns_sd::ServiceDaemon> {
    let d = mdns_sd::ServiceDaemon::new().context("start mDNS responder")?;
    let ips: Vec<IpAddr> = addrs.iter().map(|a| a.ip()).collect();
    // Only the interfaces we listen on, never public or VPN ones.
    d.disable_interface(mdns_sd::IfKind::All).context("mDNS interfaces")?;
    for ip in &ips {
        d.enable_interface(mdns_sd::IfKind::Addr(*ip)).context("mDNS interface")?;
    }
    let name = hostname();
    let props = [("id", host_id.to_owned()), ("v", proto::PROTO_VERSION.to_string()), ("name", name.clone())];
    let info = mdns_sd::ServiceInfo::new(MDNS_TYPE, &name, &format!("waylus-{name}.local."), ips.as_slice(), addrs[0].port(), &props[..])
        .context("mDNS service info")?;
    d.register(info).context("register mDNS service")?;
    Ok(d)
}

impl Server {
    pub fn start(opts: Opts) -> Result<Self> {
        let store = Arc::new(Mutex::new(Store::load()?));
        let pairing = Arc::new(Mutex::new(Pairing { until: opts.pair.then(|| Instant::now() + PAIR_WINDOW), failures: 0 }));
        let ips = if opts.listen.is_empty() { lan_addresses() } else { opts.listen.clone() };
        if ips.is_empty() {
            bail!("no LAN address to listen on (no private IPv4 address found; use --listen)");
        }
        let (tx, rx) = crossbeam_channel::unbounded();
        let fec = opts.fec_percent;
        let mut addrs = Vec::new();
        for ip in ips {
            let addr = SocketAddr::new(ip, opts.port);
            let listener = TcpListener::bind(addr).with_context(|| format!("bind TCP {addr}"))?;
            let udp = UdpSocket::bind(addr).with_context(|| format!("bind UDP {addr}"))?;
            udp.set_read_timeout(Some(Duration::from_millis(200)))?;
            set_tos(&udp, TOS_AF41);
            set_sndbuf(&udp, 4 << 20);
            let hub = Arc::new(Hub { udp, current: Mutex::new(None) });
            {
                let hub = hub.clone();
                thread::Builder::new().name("wifi-udp".into()).spawn(move || udp_loop(&hub))?;
            }
            let (tx, store, pairing) = (tx.clone(), store.clone(), pairing.clone());
            thread::Builder::new().name("wifi-accept".into()).spawn(move || {
                for conn in listener.incoming() {
                    let s = match conn {
                        Ok(s) => s,
                        Err(e) => {
                            warn!("accept: {e}");
                            continue;
                        }
                    };
                    let (tx, store, pairing, hub) = (tx.clone(), store.clone(), pairing.clone(), hub.clone());
                    let _ = thread::Builder::new().name("wifi-handshake".into()).spawn(move || {
                        let peer = s.peer_addr().map(|a| a.to_string()).unwrap_or_default();
                        match handshake(s, &store, &pairing, &hub, fec) {
                            Ok(Some(link)) => {
                                let _ = tx.send(link);
                            }
                            Ok(None) => {}
                            Err(e) => warn!("Wi-Fi connection from {peer} rejected: {e:#}"),
                        }
                    });
                }
            })?;
            addrs.push(addr);
        }
        let mdns = match advertise(&store.lock().unwrap().host_id, &addrs) {
            Ok(d) => Some(d),
            Err(e) => {
                warn!("Wi-Fi: no mDNS advertisement ({e:#}); enter the host address on the tablet");
                None
            }
        };
        let paired = store.lock().unwrap().devices.len();
        info!(
            "Wi-Fi: listening on {} ({paired} paired device{})",
            addrs.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(", "),
            if paired == 1 { "" } else { "s" }
        );
        if opts.pair {
            info!("Wi-Fi: pairing is open for {} minutes; connect from the tablet's Waylus app", PAIR_WINDOW.as_secs() / 60);
        } else if paired == 0 {
            info!("Wi-Fi: no tablet paired yet; run with --pair to pair one");
        }
        Ok(Self { links: rx, addrs, _mdns: mdns })
    }

    /// Blocks until a paired tablet has authenticated.
    pub fn next_link(&self) -> Result<Link> {
        self.links.recv().context("Wi-Fi listener stopped")
    }
}

/// `waylus --unpair NAME|all`.
pub fn unpair(which: &str) -> Result<()> {
    let mut s = Store::load()?;
    let n = s.remove(which)?;
    info!("removed {n} paired device{}", if n == 1 { "" } else { "s" });
    Ok(())
}

/// Private IPv4 addresses of up interfaces (no loopback, no CGNAT/VPN ranges like tailscale).
pub fn lan_addresses() -> Vec<IpAddr> {
    let mut out = Vec::new();
    for i in if_addrs::get_if_addrs().unwrap_or_default() {
        if i.is_loopback() || !i.is_oper_up() {
            continue;
        }
        if let IpAddr::V4(ip) = i.ip()
            && ip.is_private()
            && !out.contains(&IpAddr::V4(ip))
        {
            out.push(IpAddr::V4(ip));
        }
    }
    out
}

/// IP type-of-service / DSCP byte for outgoing datagrams. (Windows ignores it unless a
/// QoS policy allows the app to set DSCP; harmless there.)
fn set_tos(s: &UdpSocket, tos: u32) {
    if let Err(e) = socket2::SockRef::from(s).set_tos(tos) {
        debug!("IP_TOS: {e}");
    }
}

fn set_sndbuf(s: &UdpSocket, bytes: usize) {
    if let Err(e) = socket2::SockRef::from(s).set_send_buffer_size(bytes) {
        debug!("SO_SNDBUF: {e}");
    }
}

fn hostname() -> String {
    gethostname::gethostname().to_str().filter(|n| !n.is_empty()).unwrap_or("waylus host").to_owned()
}

fn send_fail(s: &mut TcpStream, code: u8, msg: &str) {
    let _ = proto::message(s, ty::AUTH_FAIL, &[&[code], msg.as_bytes()]);
}

/// Runs pairing or authentication on a new TCP connection. Returns a link for an
/// authenticated session, None after pairing (the tablet then reconnects).
fn handshake(mut s: TcpStream, store: &Mutex<Store>, pairing: &Mutex<Pairing>, hub: &Arc<Hub>, fec: usize) -> Result<Option<Link>> {
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let (t, payload) = proto::read_frame(&mut s, HANDSHAKE_MAX)?;
    match t {
        ty::PAIR_REQUEST => pair(s, &payload, store, pairing).map(|()| None),
        ty::AUTH_C => auth(s, &payload, store, hub, fec).map(Some),
        other => {
            send_fail(&mut s, fail::PROTOCOL, "expected pairing or authentication");
            bail!("unexpected first message {other}")
        }
    }
}

fn pair(mut s: TcpStream, req: &[u8], store: &Mutex<Store>, pairing: &Mutex<Pairing>) -> Result<()> {
    if req.len() < 17 {
        bail!("short pairing request");
    }
    let device_id = &req[..16];
    let n = req[16] as usize;
    let name = String::from_utf8_lossy(req.get(17..17 + n).unwrap_or_default()).into_owned();
    if !pairing.lock().unwrap().open() {
        if store.lock().unwrap().key_for(device_id).is_some() {
            send_fail(&mut s, fail::PAIRING_OFF, "already paired; it connects by itself. To pair again, run waylus with --wifi --pair");
            bail!("'{name}' asked to pair again, but it is already paired and connects without it (use --pair to re-pair)");
        }
        send_fail(&mut s, fail::PAIRING_OFF, "pairing is off; run waylus with --wifi --pair");
        bail!("pairing request from '{name}', but pairing is off (start waylus with --pair to allow it)");
    }
    let host_id = store.lock().unwrap().host_id();
    // Uniform 6-digit PIN.
    let pin = loop {
        let v = OsRng.next_u32();
        if v < u32::MAX - u32::MAX % 1_000_000 {
            break format!("{:06}", v % 1_000_000);
        }
    };
    let shown = format!("{} {}", &pin[..3], &pin[3..]);
    eprintln!("\n  ┌──────────────────────────────────────────────┐");
    eprintln!("  │  Pairing request from '{name}'");
    eprintln!("  │  Enter this PIN on the tablet:   {shown}");
    eprintln!("  └──────────────────────────────────────────────┘\n");
    let _ = std::process::Command::new("notify-send")
        .args(["-a", "Waylus", "-u", "critical", &format!("Pair '{name}'"), &format!("PIN: {shown}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let host_name = hostname();
    let hn = &host_name.as_bytes()[..host_name.len().min(255)];
    proto::message(&mut s, ty::PAIR_READY, &[&host_id, &[hn.len() as u8], hn])?;

    // The user is typing the PIN.
    s.set_read_timeout(Some(Duration::from_secs(180)))?;
    let (t, pa) = proto::read_frame(&mut s, HANDSHAKE_MAX)?;
    if t != ty::PAIR_A {
        bail!("pairing: expected PAIR_A, got {t}");
    }
    let party = spake2::Party::new_b(spake2::w_from_pin(&pin, device_id, &host_id), device_id, &host_id)?;
    let keys = party.finish(&pa, PAIR_AAD)?;
    proto::message(&mut s, ty::PAIR_B, &[&party.msg, &keys.conf_b])?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let ok = match proto::read_frame(&mut s, HANDSHAKE_MAX) {
        Ok((ty::PAIR_CONFIRM, c)) => bool::from(c.ct_eq(&keys.conf_a)),
        _ => false,
    };
    if !ok {
        let mut p = pairing.lock().unwrap();
        p.failures += 1;
        let left = MAX_PAIR_FAILURES.saturating_sub(p.failures);
        send_fail(&mut s, fail::WRONG_PIN, "wrong PIN");
        if left == 0 {
            warn!("pairing with '{name}' failed (wrong PIN); too many failures, pairing is now off");
        } else {
            warn!("pairing with '{name}' failed (wrong PIN); {left} attempt(s) left, each with a new PIN");
        }
        bail!("wrong PIN");
    }
    let mut pair_key = [0u8; 32];
    crypto::hkdf(None, &keys.ke, b"waylus pair key v1", &mut pair_key);
    store.lock().unwrap().add(device_id, &name, &pair_key)?;
    pairing.lock().unwrap().until = None;
    proto::message(&mut s, ty::PAIR_DONE, &[])?;
    info!("paired with '{name}'; pairing is closed again");
    Ok(())
}

fn auth(mut s: TcpStream, hello: &[u8], store: &Mutex<Store>, hub: &Arc<Hub>, fec: usize) -> Result<Link> {
    if hello.len() != 16 + 32 + 65 {
        send_fail(&mut s, fail::PROTOCOL, "bad AUTH_C");
        bail!("bad AUTH_C length {}", hello.len());
    }
    let device_id = &hello[..16];
    let eph_c = &hello[48..];
    let (host_id, known) = {
        let st = store.lock().unwrap();
        (st.host_id(), st.key_for(device_id))
    };
    let Some((name, pair_key)) = known else {
        send_fail(&mut s, fail::UNKNOWN_DEVICE, "this tablet is not paired with this host");
        bail!("unpaired device {} (pair it with --pair)", store::to_hex(device_id));
    };
    let peer = PublicKey::from_sec1_bytes(eph_c).context("tablet's ephemeral key")?;
    let eph = EphemeralSecret::random(&mut OsRng);
    let eph_pub = eph.public_key().to_encoded_point(false);
    let shared = eph.diffie_hellman(&peer);
    let mut nonce_h = [0u8; 32];
    OsRng.fill_bytes(&mut nonce_h);
    let th = crypto::sha256(&[b"waylus auth v1", hello, &host_id, &nonce_h, eph_pub.as_bytes()]);
    let mac_h = crypto::hmac(&pair_key, &[b"host", &th]);
    proto::message(&mut s, ty::AUTH_H, &[&host_id, &nonce_h, eph_pub.as_bytes(), &mac_h])?;
    let (t, fin) = proto::read_frame(&mut s, HANDSHAKE_MAX)?;
    let expect = crypto::hmac(&pair_key, &[b"client", &th]);
    if t != ty::AUTH_FIN || !bool::from(fin.ct_eq(&expect)) {
        send_fail(&mut s, fail::BAD_MAC, "authentication failed");
        bail!("'{name}' failed authentication (stale pairing? re-pair with --pair)");
    }
    let keys = crypto::session_keys(shared.raw_secret_bytes(), &pair_key, &th);
    info!("Wi-Fi: '{name}' authenticated from {}", s.peer_addr()?);
    Ok(make_link(s, keys, hub, fec)?)
}

fn make_link(s: TcpStream, keys: crypto::SessionKeys, hub: &Arc<Hub>, fec: usize) -> io::Result<Link> {
    // Most traffic is UDP, so TCP may stay quiet for long; idleness is judged on both
    // together by WifiReader. The read ends when the link is closed (shutdown).
    s.set_read_timeout(None)?;
    let (tx, rx) = crossbeam_channel::unbounded::<Chunk>();
    let rx_aead = Aead::new(&keys.c2h);
    let udp = Arc::new(UdpSession {
        rx: rx_aead.clone(),
        replay: Default::default(),
        chunks: tx.clone(),
        peer: Mutex::new(None),
        last_pen: AtomicU64::new(0),
        last_touch: AtomicU64::new(0),
        rejected: AtomicU64::new(0),
        frames: Default::default(),
    });
    // The newest session takes over UDP; an older one is about to be closed.
    *hub.current.lock().unwrap() = Some(udp.clone());

    let mut tcp_in = s.try_clone()?;
    thread::Builder::new().name("wifi-tcp-rx".into()).spawn(move || {
        let mut ctr = 0u64;
        loop {
            let r = read_record(&mut tcp_in, &rx_aead, ctr);
            ctr += 1;
            match r {
                Ok(v) => {
                    if tx.send(Ok(v)).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e.kind()));
                    break;
                }
            }
        }
    })?;

    let closer = s.try_clone()?;
    let closed = Arc::new(AtomicBool::new(false));
    let (hub_c, udp_c, closed_c) = (hub.clone(), udp.clone(), closed.clone());
    let writer = WifiWriter {
        video: video::Sender::new(fec, udp.frames.clone()),
        tcp: s,
        tx: Aead::new(&keys.h2c),
        tcp_ctr: 0,
        udp_ctr: 0,
        hub: hub.clone(),
        session: udp,
        pending: Vec::new(),
        dgram: Vec::with_capacity(2048),
    };
    Ok(Link {
        reader: Box::new(WifiReader { rx, cur: Vec::new(), pos: 0 }),
        writer: Box::new(writer),
        close: Arc::new(move || {
            if closed_c.swap(true, Ordering::SeqCst) {
                return;
            }
            let _ = closer.shutdown(Shutdown::Both);
            let mut cur = hub_c.current.lock().unwrap();
            if cur.as_ref().is_some_and(|c| Arc::ptr_eq(c, &udp_c)) {
                *cur = None;
            }
            let rej = udp_c.rejected.load(Ordering::Relaxed);
            if rej > 0 {
                debug!("Wi-Fi: {rej} UDP datagrams failed authentication or replay checks");
            }
        }),
        sync: None,
    })
}

fn read_record(r: &mut TcpStream, aead: &Aead, ctr: u64) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let n = u32::from_le_bytes(len);
    if n > MAX_RECORD || (n as usize) < TAG_LEN {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "bad record length"));
    }
    let mut buf = vec![0u8; n as usize];
    r.read_exact(&mut buf)?;
    aead.open(stream::TCP, ctr, &len, &mut buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(buf)
}

fn udp_loop(hub: &Hub) {
    let mut buf = vec![0u8; 65536];
    loop {
        let (n, from) = match hub.udp.recv_from(&mut buf) {
            Ok(x) => x,
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => continue,
            Err(e) => {
                warn!("Wi-Fi UDP: {e}");
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let Some(sess) = hub.current.lock().unwrap().clone() else { continue };
        if n < UDP_HDR + TAG_LEN || buf[0] != kind::CONTROL {
            sess.rejected.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let seq = u64::from_le_bytes(buf[4..12].try_into().unwrap());
        let hdr: [u8; UDP_HDR] = buf[..UDP_HDR].try_into().unwrap();
        let mut body = buf[UDP_HDR..n].to_vec();
        if sess.rx.open(stream::UDP, seq, &hdr, &mut body).is_err() || !sess.replay.lock().unwrap().accept(seq) {
            sess.rejected.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        // Authenticated: this is where the tablet is now (roaming, DHCP changes).
        {
            let mut p = sess.peer.lock().unwrap();
            if *p != Some(from) {
                debug!("Wi-Fi: tablet UDP address {from}");
                *p = Some(from);
            }
        }
        let filtered = dedup_input(&sess, &body);
        if !filtered.is_empty() {
            let _ = sess.chunks.send(Ok(filtered));
        }
    }
}

/// Drops pen/touch messages that are not newer than the last one forwarded (the tablet
/// repeats recent input in every datagram) and UDP_HELLO.
fn dedup_input(sess: &UdpSession, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i + 5 <= body.len() {
        let t = body[i];
        let len = u32::from_le_bytes(body[i + 1..i + 5].try_into().unwrap()) as usize;
        let end = i + 5 + len;
        if end > body.len() {
            break;
        }
        let msg = &body[i..end];
        let keep = match t {
            ty::UDP_HELLO => false,
            ty::WIFI_FRAME_LOST if len >= 8 => {
                let first = u32::from_le_bytes(msg[5..9].try_into().unwrap());
                let last = u32::from_le_bytes(msg[9..13].try_into().unwrap());
                let frames = sess.frames.lock().unwrap();
                // The newest of the lost frames we still know about.
                if let Some(&(_, cap)) = frames.iter().rev().find(|(f, _)| *f >= first && *f <= last) {
                    let n = last.wrapping_sub(first).wrapping_add(1);
                    out.extend_from_slice(&[ty::FRAME_LOST, 12, 0, 0, 0]);
                    out.extend_from_slice(&cap.to_le_bytes());
                    out.extend_from_slice(&n.to_le_bytes());
                }
                false
            }
            ty::PEN | ty::TOUCH if len >= 8 => {
                let ts = u64::from_le_bytes(msg[5..13].try_into().unwrap());
                let last = if t == ty::PEN { &sess.last_pen } else { &sess.last_touch };
                if ts > last.load(Ordering::Relaxed) {
                    last.store(ts, Ordering::Relaxed);
                    true
                } else {
                    false
                }
            }
            _ => true,
        };
        if keep {
            out.extend_from_slice(msg);
        }
        i = end;
    }
    out
}

struct WifiReader {
    rx: Receiver<Chunk>,
    cur: Vec<u8>,
    pos: usize,
}

impl Read for WifiReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        while self.pos >= self.cur.len() {
            match self.rx.recv_timeout(IDLE_TIMEOUT) {
                Ok(Ok(v)) => {
                    self.cur = v;
                    self.pos = 0;
                }
                Ok(Err(kind)) => return Err(kind.into()),
                Err(RecvTimeoutError::Timeout) => return Err(io::ErrorKind::TimedOut.into()),
                Err(RecvTimeoutError::Disconnected) => return Err(io::ErrorKind::UnexpectedEof.into()),
            }
        }
        let n = buf.len().min(self.cur.len() - self.pos);
        buf[..n].copy_from_slice(&self.cur[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

struct WifiWriter {
    video: video::Sender,
    tcp: TcpStream,
    tx: Aead,
    tcp_ctr: u64,
    udp_ctr: u64,
    hub: Arc<Hub>,
    session: Arc<UdpSession>,
    /// Bytes of a message not yet complete.
    pending: Vec<u8>,
    dgram: Vec<u8>,
}

impl WifiWriter {
    fn send_tcp(&mut self, msg: &[u8]) -> io::Result<()> {
        let len = ((msg.len() + TAG_LEN) as u32).to_le_bytes();
        let mut rec = Vec::with_capacity(4 + msg.len() + TAG_LEN);
        rec.extend_from_slice(&len);
        rec.extend_from_slice(msg);
        self.tx.seal(stream::TCP, self.tcp_ctr, &len, &mut rec, 4);
        self.tcp_ctr += 1;
        // One write per record (see proto::message on split writes).
        self.tcp.write_all(&rec)
    }

    fn send_udp(&mut self, msg: &[u8]) -> io::Result<()> {
        let Some(peer) = *self.session.peer.lock().unwrap() else {
            // The tablet's UDP hello hasn't arrived yet.
            return Ok(());
        };
        let d = &mut self.dgram;
        d.clear();
        d.extend_from_slice(&[kind::CONTROL, 0, 0, 0]);
        d.extend_from_slice(&self.udp_ctr.to_le_bytes());
        let hdr: [u8; UDP_HDR] = d[..UDP_HDR].try_into().unwrap();
        d.extend_from_slice(msg);
        self.tx.seal(stream::UDP, self.udp_ctr, &hdr, d, UDP_HDR);
        self.udp_ctr += 1;
        match self.hub.udp.send_to(d, peer) {
            Ok(_) => Ok(()),
            // Transient (e.g. ENOBUFS, unreachable while roaming): UDP traffic is lossy anyway.
            Err(e) => {
                debug!("Wi-Fi UDP send: {e}");
                Ok(())
            }
        }
    }

    fn route(&mut self, msg: &[u8]) -> io::Result<()> {
        match msg[0] {
            ty::PING => self.send_udp(msg),
            // Codec config must not get lost: TCP. Frames: UDP with FEC.
            ty::VIDEO if msg.len() >= 14 && msg[13] & proto::video_flags::CONFIG == 0 => {
                // Before the tablet's UDP hello the frame can't be sent; the tablet asks for
                // a keyframe once it is ready.
                if let Some(peer) = *self.session.peer.lock().unwrap() {
                    self.video.send(&self.hub.udp, peer, &self.tx, &msg[5..]);
                }
                Ok(())
            }
            _ => self.send_tcp(msg),
        }
    }
}

impl Write for WifiWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Fast path: callers write whole messages (proto::message).
        if self.pending.is_empty() && buf.len() >= 5 && u32::from_le_bytes(buf[1..5].try_into().unwrap()) as usize + 5 == buf.len() {
            self.route(buf)?;
            return Ok(buf.len());
        }
        self.pending.extend_from_slice(buf);
        while self.pending.len() >= 5 {
            let len = u32::from_le_bytes(self.pending[1..5].try_into().unwrap()) as usize + 5;
            if self.pending.len() < len {
                break;
            }
            let msg: Vec<u8> = self.pending.drain(..len).collect();
            self.route(&msg)?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(t: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![t];
        v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn dedup_drops_repeated_input() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let s = UdpSession {
            rx: Aead::new(&[0; 32]),
            replay: Default::default(),
            chunks: tx,
            peer: Mutex::new(None),
            last_pen: AtomicU64::new(0),
            last_touch: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            frames: Default::default(),
        };
        let pen = |t: u64| frame(ty::PEN, &[&t.to_le_bytes()[..], &[0u8; 12]].concat());
        let d1 = [pen(1), pen(2), frame(ty::PONG, &[0; 16])].concat();
        assert_eq!(dedup_input(&s, &d1), d1);
        // Next datagram repeats sample 2 and adds 3.
        let d2 = [pen(2), pen(3), frame(ty::UDP_HELLO, &[])].concat();
        assert_eq!(dedup_input(&s, &d2), pen(3));
    }
}
