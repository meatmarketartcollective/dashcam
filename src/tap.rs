//! Optional network metadata tap: which providers each process calls, and
//! when. Passive and payload-free: it reads the destination hostname from
//! the TLS SNI field (visible without any decryption), counts bytes in and
//! out per connection, and writes network_hop events into the timeline. No
//! man-in-the-middle, no root certificates, no payloads, ever. Off by
//! default: live capture needs root or CAP_NET_RAW, and `--pcap` parses an
//! offline capture instead (useful in CI and on locked-down machines).
//!
//! Limits a first version accepts: the ClientHello must fit in the first
//! 16 KiB of a connection (virtually always true), and live mode runs until
//! the process is stopped rather than flushing on ctrl-c.

use crate::event::EventKind;
use crate::store::Store;
use std::collections::HashMap;
use std::sync::Arc;

pub struct TapConfig {
    /// Live packet capture (needs root or CAP_NET_RAW); false with a pcap
    /// path parses the offline file instead.
    pub live: bool,
    pub pcap: Option<std::path::PathBuf>,
    pub session: i64,
    pub retention: Option<crate::store::RetentionPolicy>,
    pub redactor: crate::redact::Redactor,
}

#[derive(Default)]
struct Flow {
    sni: Option<String>,
    reported: bool,
    out_bytes: u64,
    in_bytes: u64,
    buf: Vec<u8>,
    client: String,
    server: String,
}

pub fn run(store: Arc<Store>, cfg: TapConfig) -> Result<(), String> {
    if cfg.pcap.is_some() {
        let path = cfg.pcap.clone().expect("pcap path");
        return run_pcap(store, cfg, &path);
    }
    if cfg.live {
        run_live(store, cfg)
    } else {
        Err("tap needs --live or --pcap".into())
    }
}

/// Start (or join) the flow store, record one TCP payload, and emit hops
/// when the SNI appears and when the flow ends.
fn process_flow(
    store: &Arc<Store>,
    cfg: &TapConfig,
    flows: &mut HashMap<(String, u16, String, u16), Flow>,
    src: &str,
    sport: u16,
    dst: &str,
    dport: u16,
    payload: &[u8],
    fin_or_rst: bool,
) {
    let key = if (src, sport) <= (dst, dport) {
        (src.to_string(), sport, dst.to_string(), dport)
    } else {
        (dst.to_string(), dport, src.to_string(), sport)
    };
    let outbound = src == key.0 && sport == key.1;
    let flow = flows.entry(key.clone()).or_insert_with(|| Flow {
        client: if outbound { src.to_string() } else { dst.to_string() },
        server: if outbound { dst.to_string() } else { src.to_string() },
        ..Default::default()
    });
    if outbound {
        flow.out_bytes += payload.len() as u64;
        if flow.buf.len() < 16 * 1024 {
            flow.buf.extend_from_slice(payload);
            flow.buf.truncate(16 * 1024);
        }
        if !flow.reported {
            if let Some(host) = sni_from_client_hello(&flow.buf) {
                flow.reported = true;
                flow.sni = Some(host.clone());
                write_hop(
                    store,
                    cfg,
                    &format!("tls {host}"),
                    &serde_json::json!({
                        "event": "tls_client_hello",
                        "host": host,
                        "client": flow.client,
                        "server": flow.server,
                        "bytes_out": flow.out_bytes,
                    }),
                );
            }
        }
    } else {
        flow.in_bytes += payload.len() as u64;
    }
    if fin_or_rst {
        let who = flow
            .sni
            .clone()
            .unwrap_or_else(|| format!("{}:{}", flow.server, dport.max(sport)));
        write_hop(
            store,
            cfg,
            &format!("flow end {who}"),
            &serde_json::json!({
                "event": "flow_end",
                "host": flow.sni,
                "client": flow.client,
                "server": flow.server,
                "bytes_out": flow.out_bytes,
                "bytes_in": flow.in_bytes,
            }),
        );
        flows.remove(&key);
    }
}

fn write_hop(store: &Arc<Store>, cfg: &TapConfig, summary: &str, payload: &serde_json::Value) {
    let scrubbed = cfg.redactor.scrub(&payload.to_string());
    let _ = store.append(cfg.session, EventKind::NetworkHop, summary, &scrubbed);
    if let Some(policy) = &cfg.retention {
        let _ = store.prune(policy);
    }
}

/// IPv4/IPv6 over Ethernet (VLAN tags skipped): returns the TCP payload
/// offset with textual source and destination addresses.
fn ip_layer(frame: &[u8]) -> Option<(usize, String, String)> {
    if frame.len() < 14 {
        return None;
    }
    let mut etype = u16::from_be_bytes([frame[12], frame[13]]);
    let mut off = 14usize;
    if etype == 0x8100 {
        if frame.len() < 18 {
            return None;
        }
        etype = u16::from_be_bytes([frame[16], frame[17]]);
        off = 18;
    }
    match etype {
        0x0800 => {
            if frame.len() < off + 20 {
                return None;
            }
            let ihl = ((frame[off] & 0x0f) * 4) as usize;
            if ihl < 20 || frame.len() < off + ihl || frame[off + 9] != 6 {
                return None;
            }
            let src = format!(
                "{}.{}.{}.{}",
                frame[off + 12], frame[off + 13], frame[off + 14], frame[off + 15]
            );
            let dst = format!(
                "{}.{}.{}.{}",
                frame[off + 16], frame[off + 17], frame[off + 18], frame[off + 19]
            );
            Some((off + ihl, src, dst))
        }
        0x86dd => {
            if frame.len() < off + 40 || frame[off + 6] != 6 {
                return None;
            }
            let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
            Some((off + 40, hex(&frame[off + 8..off + 24]), hex(&frame[off + 24..off + 40])))
        }
        _ => None,
    }
}

/// TCP header: ports, payload slice, and whether the flow is ending.
fn tcp_layer(p: &[u8]) -> Option<(u16, u16, &[u8], bool)> {
    if p.len() < 20 {
        return None;
    }
    let sport = u16::from_be_bytes([p[0], p[1]]);
    let dport = u16::from_be_bytes([p[2], p[3]]);
    let doff = ((p[12] >> 4) * 4) as usize;
    if doff < 20 || p.len() < doff {
        return None;
    }
    let flags = p[13];
    Some((sport, dport, &p[doff..], flags & 0x05 != 0))
}

/// Pull the SNI hostname out of a TLS ClientHello, or None. Works on the
/// reassembled outbound stream prefix; never looks at anything encrypted.
fn sni_from_client_hello(buf: &[u8]) -> Option<String> {
    if buf.len() < 5 || buf[0] != 0x16 {
        return None;
    }
    let rec_len = u16::from_be_bytes([buf[3], buf[4]]) as usize;
    let end = buf.len().min(5 + rec_len);
    let hs = &buf[5..end];
    if hs.len() < 4 || hs[0] != 0x01 {
        return None;
    }
    let body_len = ((hs[1] as usize) << 16) | ((hs[2] as usize) << 8) | hs[3] as usize;
    if hs.len() < 4 + body_len {
        return None;
    }
    let mut p = 4 + 34; // handshake header, version, random
    if p >= hs.len() {
        return None;
    }
    let sid = hs[p] as usize;
    p += 1 + sid;
    if p + 2 > hs.len() {
        return None;
    }
    let cs = u16::from_be_bytes([hs[p], hs[p + 1]]) as usize;
    p += 2 + cs;
    if p >= hs.len() {
        return None;
    }
    let comp = hs[p] as usize;
    p += 1 + comp;
    if p + 2 > hs.len() {
        return None;
    }
    let ext_total = u16::from_be_bytes([hs[p], hs[p + 1]]) as usize;
    p += 2;
    let ext_end = hs.len().min(p + ext_total);
    while p + 4 <= ext_end {
        let etype = u16::from_be_bytes([hs[p], hs[p + 1]]);
        let elen = u16::from_be_bytes([hs[p + 2], hs[p + 3]]) as usize;
        let body_end = hs.len().min(p + 4 + elen);
        if etype == 0 {
            let body = &hs[p + 4..body_end];
            if body.len() >= 5 && body[2] == 0 {
                let nlen = u16::from_be_bytes([body[3], body[4]]) as usize;
                if body.len() >= 5 + nlen {
                    return String::from_utf8(body[5..5 + nlen].to_vec()).ok();
                }
            }
            return None;
        }
        p += 4 + elen;
    }
    None
}

/// Parse a classic pcap file and record what it shows.
fn run_pcap(store: Arc<Store>, cfg: TapConfig, path: &std::path::Path) -> Result<(), String> {
    let data = std::fs::read(path).map_err(|e| format!("pcap read: {e}"))?;
    if data.len() < 24 {
        return Err("pcap too short".into());
    }
    let le = match &data[0..4] {
        [0xd4, 0xc3, 0xb2, 0xa1] | [0x4d, 0x3c, 0xb2, 0xa1] => true,
        [0xa1, 0xb2, 0xc3, 0xd4] | [0xa1, 0xb2, 0x3c, 0x4d] => false,
        _ => return Err("not a pcap file (magic mismatch)".into()),
    };
    let u32_at = |o: usize| -> u32 {
        if le {
            u32::from_le_bytes(data[o..o + 4].try_into().expect("u32"))
        } else {
            u32::from_be_bytes(data[o..o + 4].try_into().expect("u32"))
        }
    };
    let mut flows = HashMap::new();
    let mut packets = 0u64;
    let mut off = 24usize;
    while off + 16 <= data.len() {
        let incl = u32_at(off + 8) as usize;
        if off + 16 + incl > data.len() {
            break;
        }
        let frame = &data[off + 16..off + 16 + incl];
        if let Some((l4, src, dst)) = ip_layer(frame) {
            if let Some((sport, dport, payload, fin)) = tcp_layer(&frame[l4..]) {
                packets += 1;
                process_flow(&store, &cfg, &mut flows, &src, sport, &dst, dport, payload, fin);
            }
        }
        off += 16 + incl;
    }
    let named = flows.values().filter(|f| f.reported).count();
    println!("tap: {packets} tcp packets, {named} flows with a hostname");
    Ok(())
}

/// Live capture on a raw packet socket. Runs until the process is stopped.
#[cfg(target_os = "linux")]
fn run_live(store: Arc<Store>, cfg: TapConfig) -> Result<(), String> {
    let sock = unsafe {
        libc::socket(
            libc::AF_PACKET,
            libc::SOCK_RAW,
            (libc::ETH_P_ALL as u16).to_be() as i32,
        )
    };
    if sock < 0 {
        return Err("cannot open the packet socket: live capture needs root or CAP_NET_RAW (use --pcap for offline)".into());
    }
    let tv = libc::timeval {
        tv_sec: 5,
        tv_usec: 0,
    };
    unsafe {
        libc::setsockopt(
            sock,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &tv as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::timeval>() as u32,
        );
    }
    let mut buf = [0u8; 65536];
    let mut flows = HashMap::new();
    loop {
        let n = unsafe { libc::recv(sock, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::WouldBlock
                || err.kind() == std::io::ErrorKind::TimedOut
                || err.raw_os_error() == Some(libc::EAGAIN)
                || err.raw_os_error() == Some(libc::EINTR)
            {
                continue;
            }
            return Err(format!("packet read: {err}"));
        }
        let n = n as usize;
        if let Some((l4, src, dst)) = ip_layer(&buf[..n]) {
            if let Some((sport, dport, payload, fin)) = tcp_layer(&buf[l4..]) {
                let payload = payload.to_vec();
                process_flow(&store, &cfg, &mut flows, &src, sport, &dst, dport, &payload, fin);
            }
        }
    }
}

#[cfg(test)]
mod tap_tests {
    use super::*;

    /// Build a minimal TLS ClientHello carrying one server_name extension.
    fn client_hello(host: &str) -> Vec<u8> {
        let name = host.as_bytes();
        let ext_body_len = 2 + 1 + 2 + name.len();
        let ext_total = 4 + ext_body_len;
        let body_len = 2 + 32 + 1 + 2 + 2 + 1 + 1 + 2 + ext_total;
        let mut v = Vec::new();
        v.extend_from_slice(&[0x16, 0x03, 0x01]);
        v.extend_from_slice(&((4 + body_len) as u16).to_be_bytes());
        v.push(0x01);
        v.extend_from_slice(&(body_len as u32).to_be_bytes()[1..]);
        v.extend_from_slice(&[0x03, 0x03]);
        v.extend_from_slice(&[0u8; 32]);
        v.push(0x00);
        v.extend_from_slice(&2u16.to_be_bytes());
        v.extend_from_slice(&[0x13, 0x01]);
        v.push(0x01);
        v.push(0x00);
        v.extend_from_slice(&(ext_total as u16).to_be_bytes());
        v.extend_from_slice(&0u16.to_be_bytes());
        v.extend_from_slice(&(ext_body_len as u16).to_be_bytes());
        v.extend_from_slice(&((1 + 2 + name.len()) as u16).to_be_bytes());
        v.push(0x00);
        v.extend_from_slice(&(name.len() as u16).to_be_bytes());
        v.extend_from_slice(name);
        v
    }

    #[test]
    fn sni_is_extracted_from_client_hello() {
        let hello = client_hello("api.example.test");
        assert_eq!(
            sni_from_client_hello(&hello).as_deref(),
            Some("api.example.test")
        );
    }

    #[test]
    fn non_tls_traffic_yields_no_host() {
        assert!(sni_from_client_hello(b"GET / HTTP/1.1\r\n\r\n").is_none());
        assert!(sni_from_client_hello(&[]).is_none());
    }

    #[test]
    fn ip_and_tcp_layers_parse() {
        let mut f = Vec::new();
        f.extend_from_slice(&[0u8; 12]);
        f.extend_from_slice(&[0x08, 0x00]);
        let ip_start = f.len();
        f.push(0x45);
        f.push(0x00);
        f.extend_from_slice(&(20u16 + 20u16 + 4u16).to_be_bytes());
        f.extend_from_slice(&[0, 0, 0, 0]);
        f.push(64);
        f.push(6);
        f.extend_from_slice(&[0, 0]);
        f.extend_from_slice(&[10, 0, 0, 1]);
        f.extend_from_slice(&[93, 184, 216, 34]);
        let l4 = f.len();
        f.extend_from_slice(&54321u16.to_be_bytes());
        f.extend_from_slice(&443u16.to_be_bytes());
        f.extend_from_slice(&0u32.to_be_bytes());
        f.extend_from_slice(&0u32.to_be_bytes());
        f.push(0x50);
        f.push(0x18);
        f.extend_from_slice(&[0xff, 0xff, 0, 0]);
        f.extend_from_slice(&[0, 0]);
        f.extend_from_slice(b"test");
        let (off, src, dst) = ip_layer(&f).expect("ip");
        assert_eq!(off, l4);
        assert_eq!(src, "10.0.0.1");
        assert_eq!(dst, "93.184.216.34");
        let (sport, dport, payload, fin) = tcp_layer(&f[off..]).expect("tcp");
        assert_eq!((sport, dport), (54321, 443));
        assert_eq!(payload, b"test");
        assert!(!fin);
        let _ = ip_start;
    }
}

#[cfg(not(target_os = "linux"))]
fn run_live(_store: Arc<Store>, _cfg: TapConfig) -> Result<(), String> {
    Err("live capture is Linux-only (raw packet sockets). Use --pcap for offline replay instead.".into())
}
