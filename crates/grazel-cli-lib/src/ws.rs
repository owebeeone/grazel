//! GR4b: WS streams on the HTTP edge (GrazelHttpEdge.md — the deferred half).
//!
//! `GET /events` + websocket upgrade; the FIRST binary message is the taut
//! request envelope (e.g. `invocation.events`), every subsequent server message
//! is one response envelope — the SAME bytes a UDS subscriber reads after the
//! length prefix (transport equivalence, §1b/GR4: WS frames replace the 4-byte
//! prefix as framing; payloads are byte-identical). Hand-rolled RFC6455 server
//! half (handshake SHA-1/base64, binary frames, close/ping) — zero new deps,
//! same posture as the HTTP/1.1 module.

use crate::daemon::State;
use std::io::{Read, Write};
use std::net::TcpStream;

// --- RFC6455 handshake primitives (hand-rolled, test-vector-pinned) -----------

pub(crate) fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0];
    let ml = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&ml.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes(word.try_into().unwrap());
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, x) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

pub(crate) fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ABC[(n >> 18) as usize & 63] as char);
        out.push(ABC[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ABC[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ABC[n as usize & 63] as char } else { '=' });
    }
    out
}

/// `Sec-WebSocket-Accept` for a client key (RFC6455 §1.3).
pub(crate) fn accept_key(client_key: &str) -> String {
    const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    base64(&sha1(format!("{}{GUID}", client_key.trim()).as_bytes()))
}

// --- frames -------------------------------------------------------------------

/// Write one server→client binary message (FIN, unmasked).
pub(crate) fn write_message(stream: &mut dyn Write, payload: &[u8]) -> std::io::Result<()> {
    let mut head = vec![0x82u8];
    match payload.len() {
        n if n < 126 => head.push(n as u8),
        n if n < 65536 => {
            head.push(126);
            head.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            head.push(127);
            head.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    stream.write_all(&head)?;
    stream.write_all(payload)?;
    stream.flush()
}

/// Read one client→server message (masked, per RFC). Close → Ok(None);
/// ping answered inline. Continuation frames unsupported (fine for envelopes).
pub(crate) fn read_message<S: Read + Write>(stream: &mut S) -> std::io::Result<Option<Vec<u8>>> {
    loop {
        let mut h = [0u8; 2];
        stream.read_exact(&mut h)?;
        let opcode = h[0] & 0x0F;
        let masked = h[1] & 0x80 != 0;
        let mut len = u64::from(h[1] & 0x7F);
        if len == 126 {
            let mut x = [0u8; 2];
            stream.read_exact(&mut x)?;
            len = u64::from(u16::from_be_bytes(x));
        } else if len == 127 {
            let mut x = [0u8; 8];
            stream.read_exact(&mut x)?;
            len = u64::from_be_bytes(x);
        }
        let mut mask = [0u8; 4];
        if masked {
            stream.read_exact(&mut mask)?;
        }
        let mut payload = vec![0u8; len as usize];
        stream.read_exact(&mut payload)?;
        if masked {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= mask[i % 4];
            }
        }
        match opcode {
            0x2 | 0x1 => return Ok(Some(payload)), // binary (or text) message
            0x8 => return Ok(None),                // close
            0x9 => {
                // ping → pong, same payload
                let mut head = vec![0x8Au8, payload.len() as u8];
                head.extend_from_slice(&payload);
                stream.write_all(&head)?;
                stream.flush()?;
            }
            _ => continue, // pong / continuation: ignore
        }
    }
}

// --- the serve path -------------------------------------------------------------

/// A `Read + Write` view of a WS connection that speaks the UDS envelope shape:
/// reads hand `serve_conn` length-prefixed request frames (from WS messages),
/// writes collect length-prefixed response frames and emit each payload as one
/// WS binary message — byte-identical payloads across transports.
struct WsConn<'a> {
    stream: &'a mut TcpStream,
    inbuf: std::io::Cursor<Vec<u8>>,
    outbuf: Vec<u8>,
}

impl WsConn<'_> {
    fn queue_in(&mut self, payload: &[u8]) {
        let mut framed = (payload.len() as u32).to_be_bytes().to_vec();
        framed.extend_from_slice(payload);
        self.inbuf = std::io::Cursor::new(framed);
    }
}

impl Read for WsConn<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inbuf.read(buf)?;
        if n > 0 {
            return Ok(n);
        }
        match read_message(self.stream)? {
            Some(msg) => {
                self.queue_in(&msg);
                self.inbuf.read(buf)
            }
            None => Ok(0), // client closed
        }
    }
}

impl Write for WsConn<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.outbuf.extend_from_slice(buf);
        // Drain every complete length-prefixed frame into a WS message.
        while self.outbuf.len() >= 4 {
            let len = u32::from_be_bytes(self.outbuf[..4].try_into().unwrap()) as usize;
            if self.outbuf.len() < 4 + len {
                break;
            }
            let payload: Vec<u8> = self.outbuf.drain(..4 + len).skip(4).collect();
            write_message(self.stream, &payload)?;
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

fn err_message(stream: &mut TcpStream, e: String) -> std::io::Result<()> {
    // The error envelope a UDS client would get, as one WS message.
    let env = razel_wire::Cbor::Map(vec![
        (1, razel_wire::Cbor::Bool(false)),
        (3, razel_wire::Cbor::Text(e)),
    ]);
    write_message(stream, &razel_wire::encode(&env))
}

/// Complete the upgrade (101) and serve: first WS message = request envelope.
/// STREAM methods ride the keyed fan-out (one upstream per key shared by all WS
/// subscribers — GrazelViewSeam.md); everything else routes to the sole member
/// exactly like the UDS path.
pub(crate) fn serve_upgraded(
    state: &State,
    stream: &mut TcpStream,
    client_key: &str,
) -> std::io::Result<()> {
    let resp = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        accept_key(client_key)
    );
    stream.write_all(resp.as_bytes())?;
    stream.flush()?;
    let Some(first) = read_message(stream)? else {
        return Ok(());
    };
    let req = razel_wire::decode(&first);
    if let razel_wire::Cbor::Text(method) = req.get(1)
        && matches!(method.as_str(), "build.subscribe" | "invocation.events")
    {
        let sub = match state.fanout().subscribe(method) {
            Ok(s) => s,
            Err(e) => return err_message(stream, e),
        };
        // Close detection: the serve loop below is WRITE-driven and a quiet
        // stream would never notice the client leaving — a reader thread eats
        // pings, and on close/error ends the SUBSCRIPTION directly (the closer
        // unblocks the parked recv) and shuts the socket for good measure.
        let close_sub = sub.closer();
        let mut reader = stream.try_clone()?;
        std::thread::spawn(move || {
            while let Ok(Some(_)) = read_message(&mut reader) {}
            close_sub();
            let _ = reader.shutdown(std::net::Shutdown::Both);
        });
        while let Some(frame) = sub.recv() {
            if write_message(stream, &frame).is_err() {
                break; // client gone → sub drops → unsubscribe → maybe last out
            }
        }
        return Ok(());
    }
    match state.sole_member_server() {
        Ok(server) => {
            let mut conn = WsConn { stream, inbuf: std::io::Cursor::new(vec![]), outbuf: vec![] };
            conn.queue_in(&first);
            server.serve_conn(&mut conn)
        }
        Err(e) => err_message(stream, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc6455_handshake_vector() {
        // The key/accept pair from RFC 6455 §1.3.
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn sha1_and_base64_vectors() {
        assert_eq!(
            sha1(b"abc"),
            [
                0xa9, 0x99, 0x3e, 0x36, 0x47, 0x06, 0x81, 0x6a, 0xba, 0x3e, 0x25, 0x71, 0x78,
                0x50, 0xc2, 0x6c, 0x9c, 0xd0, 0xd8, 0x9d
            ]
        );
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
