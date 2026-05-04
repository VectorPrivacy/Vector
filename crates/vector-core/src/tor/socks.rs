//! Minimal SOCKS5 CONNECT-only listener bridging incoming streams into Arti.
//!
//! Why our own and not arti's: arti's bundled SOCKS server (in the `arti`
//! binary crate) is heavily entangled with their RPC subsystem, isolation
//! tags, HTTP-vs-SOCKS detection, etc. — way more surface than we need. For
//! a privacy-toggle on a chat app we just want:
//!
//!   client(reqwest/nostr-sdk) → SOCKS5 CONNECT → TorClient::connect → Tor
//!
//! No auth, no UDP ASSOCIATE, no BIND, no SOCKS4. ~150 lines of straight
//! protocol handling.

use std::sync::Arc;

use arti_client::{TorClient, IntoTorAddr};
use tor_rtcompat::PreferredRuntime;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio_util::compat::FuturesAsyncReadCompatExt;

const SOCKS_VERSION: u8 = 0x05;
const METHOD_NO_AUTH: u8 = 0x00;
const METHOD_NO_ACCEPTABLE: u8 = 0xFF;
const CMD_CONNECT: u8 = 0x01;
const ATYP_IPV4: u8 = 0x01;
const ATYP_DOMAIN: u8 = 0x03;
const ATYP_IPV6: u8 = 0x04;
const REP_SUCCEEDED: u8 = 0x00;
const REP_GENERAL_FAILURE: u8 = 0x01;
const REP_HOST_UNREACHABLE: u8 = 0x04;
const REP_COMMAND_NOT_SUPPORTED: u8 = 0x07;
const REP_ATYP_NOT_SUPPORTED: u8 = 0x08;

pub(super) async fn run(
    listener: TcpListener,
    tor: TorClient<PreferredRuntime>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let tor = Arc::new(tor);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            accept = listener.accept() => match accept {
                Ok((stream, _peer)) => {
                    let tor = Arc::clone(&tor);
                    tokio::spawn(async move {
                        if let Err(e) = handle(stream, tor).await {
                            log_debug!("[Tor SOCKS] connection failed: {}", e);
                        }
                    });
                }
                Err(e) => {
                    log_warn!("[Tor SOCKS] accept error: {}", e);
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        }
    }
}

async fn handle(
    mut conn: TcpStream,
    tor: Arc<TorClient<PreferredRuntime>>,
) -> Result<(), String> {
    // ---- Greeting: VER, NMETHODS, METHODS[NMETHODS] ----
    let mut hdr = [0u8; 2];
    conn.read_exact(&mut hdr).await.map_err(|e| format!("greeting read: {e}"))?;
    if hdr[0] != SOCKS_VERSION {
        return Err(format!("unsupported SOCKS version: {}", hdr[0]));
    }
    let nmethods = hdr[1] as usize;
    let mut methods = vec![0u8; nmethods];
    conn.read_exact(&mut methods).await.map_err(|e| format!("methods read: {e}"))?;
    let chosen = if methods.contains(&METHOD_NO_AUTH) {
        METHOD_NO_AUTH
    } else {
        // No mutually acceptable method.
        let _ = conn.write_all(&[SOCKS_VERSION, METHOD_NO_ACCEPTABLE]).await;
        return Err("client offered no NO-AUTH method".into());
    };
    conn.write_all(&[SOCKS_VERSION, chosen]).await.map_err(|e| format!("method ack: {e}"))?;

    // ---- Request: VER, CMD, RSV, ATYP, DST.ADDR, DST.PORT ----
    let mut req_hdr = [0u8; 4];
    conn.read_exact(&mut req_hdr).await.map_err(|e| format!("request read: {e}"))?;
    if req_hdr[0] != SOCKS_VERSION {
        return Err("bad request version".into());
    }
    if req_hdr[1] != CMD_CONNECT {
        write_reply(&mut conn, REP_COMMAND_NOT_SUPPORTED).await;
        return Err(format!("unsupported CMD: {}", req_hdr[1]));
    }
    let atyp = req_hdr[3];

    let host: String = match atyp {
        ATYP_IPV4 => {
            let mut b = [0u8; 4];
            conn.read_exact(&mut b).await.map_err(|e| format!("ipv4 read: {e}"))?;
            format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3])
        }
        ATYP_DOMAIN => {
            let mut len = [0u8; 1];
            conn.read_exact(&mut len).await.map_err(|e| format!("dom len: {e}"))?;
            let mut name = vec![0u8; len[0] as usize];
            conn.read_exact(&mut name).await.map_err(|e| format!("dom name: {e}"))?;
            String::from_utf8(name).map_err(|e| format!("dom utf8: {e}"))?
        }
        ATYP_IPV6 => {
            let mut b = [0u8; 16];
            conn.read_exact(&mut b).await.map_err(|e| format!("ipv6 read: {e}"))?;
            std::net::Ipv6Addr::from(b).to_string()
        }
        _ => {
            write_reply(&mut conn, REP_ATYP_NOT_SUPPORTED).await;
            return Err(format!("unknown ATYP: {atyp}"));
        }
    };
    let mut port_buf = [0u8; 2];
    conn.read_exact(&mut port_buf).await.map_err(|e| format!("port read: {e}"))?;
    let port = u16::from_be_bytes(port_buf);

    // ---- Connect via Arti ----
    let addr = (host.as_str(), port)
        .into_tor_addr()
        .map_err(|e| format!("addr parse: {e}"))?;
    let stream = match tor.connect(addr).await {
        Ok(s) => s,
        Err(e) => {
            log_debug!("[Tor SOCKS] tor.connect({}:{}) failed: {}", host, port, e);
            write_reply(&mut conn, REP_HOST_UNREACHABLE).await;
            return Err(format!("tor connect: {e}"));
        }
    };
    write_reply(&mut conn, REP_SUCCEEDED).await;

    // ---- Splice ----
    // Arti's DataStream is futures::io-based; tokio_util::compat bridges it
    // to tokio::io so copy_bidirectional can drive both sides.
    let mut tor_stream = stream.compat();
    let _ = tokio::io::copy_bidirectional(&mut conn, &mut tor_stream).await;
    Ok(())
}

/// Write a SOCKS5 reply with a zeroed BND.ADDR/BND.PORT — clients ignore those
/// for CONNECT replies, and we have nothing meaningful to put there.
async fn write_reply(conn: &mut TcpStream, rep: u8) {
    // VER, REP, RSV, ATYP=IPv4, BND.ADDR(4)=0, BND.PORT(2)=0
    let _ = conn
        .write_all(&[SOCKS_VERSION, rep, 0x00, ATYP_IPV4, 0, 0, 0, 0, 0, 0])
        .await;
}

// Suppress unused-import warning when not all REP_* codes are emitted yet.
#[allow(dead_code)]
const _UNUSED: &[u8] = &[REP_GENERAL_FAILURE];
