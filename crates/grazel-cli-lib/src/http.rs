//! GR4a: the one-shot HTTP edge (GrazelHttpEdge.md).
//!
//! `POST /rpc`, body = EXACTLY the taut request map UDS carries (HTTP frames via
//! Content-Length, so no length prefix), response body = EXACTLY the response
//! envelope bytes — one protocol, two transports, byte-identical. Hand-rolled
//! minimal HTTP/1.1 (no keep-alive, no chunking, no TLS, `Connection: close`):
//! zero new deps for ~100 lines of pipe. LOCALHOST ONLY by construction — any
//! non-loopback bind is refused at startup; real binds wait for iroh-era auth.
//! WS streams are GR4b, gated on GR3's stream semantics.

use crate::daemon::State;
use razel_wire::{decode, encode};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;

/// Parse `--http-bind`; refuse anything that isn't loopback.
pub fn parse_bind(s: &str) -> Result<SocketAddr, String> {
    let addr: SocketAddr = s
        .parse()
        .map_err(|e| format!("--http-bind={s}: {e}"))?;
    if !addr.ip().is_loopback() {
        return Err(format!(
            "--http-bind={s}: the HTTP edge is localhost-only (127.0.0.1) until the iroh-era auth story exists (GR4)"
        ));
    }
    Ok(addr)
}

/// Bind the listener (default loopback, ephemeral port). Returns it with the
/// chosen port so daemon.json can record it BEFORE serving starts.
pub fn bind(flag: Option<&str>) -> Result<(TcpListener, u16), String> {
    let addr = parse_bind(flag.unwrap_or("127.0.0.1:0"))?;
    let listener = TcpListener::bind(addr).map_err(|e| format!("http bind {addr}: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    Ok((listener, port))
}

fn respond_http(stream: &mut TcpStream, status: &str, body: &[u8]) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/cbor\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

/// One request per connection: parse the head, check `POST /rpc`, read
/// Content-Length bytes, dispatch through the SAME `State::respond` UDS uses.
fn handle(state: &State, stream: &mut TcpStream) -> std::io::Result<()> {
    // Read until the header/body split; tolerate the body arriving with it.
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let split = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        if buf.len() > 64 * 1024 {
            return respond_http(stream, "431 Request Header Fields Too Large", b"");
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(()); // client gave up mid-head
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..split]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    if path != "/rpc" {
        return respond_http(stream, "404 Not Found", b"");
    }
    if method != "POST" {
        return respond_http(stream, "405 Method Not Allowed", b"");
    }
    let content_length: usize = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0);
    let mut body = buf[split + 4..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(()); // truncated body; nothing sane to answer
        }
        body.extend_from_slice(&chunk[..n]);
    }
    let (resp, shutdown) = state.respond(&decode(&body));
    respond_http(stream, "200 OK", &encode(&resp))?;
    if shutdown {
        state.exit_now()
    }
    Ok(())
}

/// The accept loop — same thread-per-connection shape as the UDS side, same
/// idle accounting (a daemon serving HTTP traffic is not idle).
pub(crate) fn serve(listener: TcpListener, state: Arc<State>) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let state = state.clone();
        state.enter();
        std::thread::spawn(move || {
            let _ = handle(&state, &mut stream);
            state.leave();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_only() {
        parse_bind("127.0.0.1:0").unwrap();
        parse_bind("127.0.0.1:8080").unwrap();
        for bad in ["0.0.0.0:0", "192.168.1.10:80", "[::]:0"] {
            let err = parse_bind(bad).unwrap_err();
            assert!(err.contains("localhost-only"), "{err}");
        }
    }
}
