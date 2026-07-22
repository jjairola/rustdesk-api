//! A TCP shim that sits in front of an OSS hbbs and unblocks logged-in clients.
//!
//! # Why this exists
//!
//! RustDesk client 1.4.9, `src/client.rs:429`:
//!
//! ```ignore
//! if !key.is_empty() && !token.is_empty() {
//!     secure_tcp(&mut socket, &key).await
//!         .map_err(|e| anyhow!("Failed to secure tcp: {}", e))?;
//! }
//! ```
//!
//! `key` is never empty (it falls back to a built-in public key), so this
//! reduces to "is the user logged in to an API server?". When it is,
//! `secure_tcp` blocks waiting for the rendezvous server to speak first:
//!
//! ```ignore
//! match timeout(READ_TIMEOUT, conn.next()).await? {   // READ_TIMEOUT = 18_000 ms
//!     Some(Ok(bytes)) => {
//!         if let Ok(msg_in) = RendezvousMessage::parse_from_bytes(&bytes) {
//!             match msg_in.union {
//!                 Some(Union::KeyExchange(ex)) => { /* verify + derive key */ }
//!                 _ => {}          // anything else: proceed unencrypted
//!             }
//!         }                        // unparseable: proceed unencrypted
//!     }
//!     _ => {}
//! }
//! Ok(())
//! ```
//!
//! The open-source hbbs never sends `KeyExchange` — it is implemented only in
//! hbbs Pro, and it waits for the client to speak first. Both sides wait, the
//! client's 18-second deadline expires, and every connection fails with
//! "Failed to secure tcp: deadline has elapsed". Logging out clears the token
//! and the connection works again.
//!
//! Note the fallback arms: the client does not *require* encryption, only that
//! *something* arrives within 18 seconds. So this shim writes one frame and
//! then gets out of the way. The control channel stays plaintext, exactly as it
//! is today for logged-out clients; end-to-end encryption between peers is a
//! separate mechanism and is unaffected.
//!
//! # Deployment
//!
//! hbbs uses port 21116 for both TCP (punch requests — the broken path) and UDP
//! (registration — fine), and needs host networking to see real client IPs. So
//! this shim takes over only inbound *TCP* 21116 via a DNAT rule, and forwards
//! to hbbs over loopback, which is not redirected:
//!
//! ```text
//!   client --TCP 21116--> [DNAT] --> shim :21126 --> 127.0.0.1:21116 (hbbs)
//!   client --UDP 21116---------------------------->  hbbs, untouched
//! ```
//!
//! # Caveat
//!
//! This relies on the client tolerating a non-`KeyExchange` greeting. If
//! upstream ever tightens `secure_tcp` to require a real handshake, this stops
//! working and connections fail the same way they do now. The fallback is to
//! set `allow-websocket = 'Y'` on the clients, which skips `secure_tcp`
//! entirely at the cost of relaying all traffic.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::thread;

/// One length-prefixed frame the client will read, discard, and move on from.
///
/// Framing (`hbb_common/src/bytes_codec.rs`): the header length is
/// `(byte0 & 0x3) + 1` and the payload length is `byte0 >> 2`. So `0x0C` means
/// a 1-byte header describing a 3-byte payload.
///
/// Payload `A0 06 00` is protobuf field 100, varint, value 0. The highest field
/// number `RendezvousMessage` actually uses is 28, so this parses cleanly as an
/// unknown field, leaves `union` unset, and lands in the client's `_ => {}` arm.
const GREETING: [u8; 4] = [0x0C, 0xA0, 0x06, 0x00];

fn main() {
    let listen = env_or("SHIM_LISTEN", "0.0.0.0:21126");
    let upstream = env_or("SHIM_UPSTREAM", "127.0.0.1:21116");

    let listener = match TcpListener::bind(&listen) {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("fatal: cannot bind {listen}: {e}");
            std::process::exit(1);
        }
    };

    println!("rendezvous shim listening on {listen}, forwarding to {upstream}");

    for stream in listener.incoming() {
        match stream {
            Ok(client) => {
                let upstream = upstream.clone();
                thread::spawn(move || handle(client, &upstream));
            }
            Err(e) => eprintln!("accept failed: {e}"),
        }
    }
}

fn handle(mut client: TcpStream, upstream: &str) {
    let peer = client
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "?".into());

    let mut server = match TcpStream::connect(upstream) {
        Ok(server) => server,
        Err(e) => {
            eprintln!("{peer}: cannot reach hbbs at {upstream}: {e}");
            return;
        }
    };

    let _ = client.set_nodelay(true);
    let _ = server.set_nodelay(true);

    // The whole point: speak first, so the client's secure_tcp deadline is met.
    if let Err(e) = client.write_all(&GREETING) {
        eprintln!("{peer}: failed to send greeting: {e}");
        return;
    }

    let (mut client_rx, mut server_tx) = match (client.try_clone(), server.try_clone()) {
        (Ok(c), Ok(s)) => (c, s),
        _ => {
            eprintln!("{peer}: failed to duplicate sockets");
            return;
        }
    };

    let up = thread::spawn(move || pipe(&mut client_rx, &mut server_tx));
    pipe(&mut server, &mut client);
    let _ = up.join();
}

/// Copies until either end closes, then half-closes both directions so the
/// peers observe a clean EOF rather than a hang.
fn pipe(from: &mut TcpStream, to: &mut TcpStream) {
    let mut buf = [0u8; 8192];
    loop {
        match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if to.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    let _ = to.shutdown(Shutdown::Write);
    let _ = from.shutdown(Shutdown::Read);
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}
