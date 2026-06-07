//! Daemon transport — the OS adapter seam for IPC.
//!
//! The daemon's framing and dispatch are byte-stream agnostic (they work over any
//! `Read + Write`); only *binding/accepting/connecting* is OS-specific. This module
//! is that seam: a [`Listener`] yields [`Conn`]s, addressed by a filesystem
//! **rendezvous path**.
//!
//! - **unix** — a Unix domain socket bound at the path (the native choice).
//! - **windows** — `std` has no UDS/named-pipe surface, so the portable fallback
//!   is a **TCP listener on `127.0.0.1:0`** whose chosen port is written to the
//!   rendezvous path; the client reads the port and connects. (A native named-pipe
//!   impl is a later, nicer Windows option.)
//!
//! The path-addressed API is identical on both, so callers (`rpc`, the CLI) and
//! the tests are unchanged across platforms.

use std::io::{self, Read, Write};
use std::path::Path;

/// A bidirectional connection. Blanket-implemented for any `Read + Write + Send`
/// stream (e.g. `UnixStream`, `TcpStream`), so it can move onto a handler thread.
pub trait Conn: Read + Write + Send {}
impl<T: Read + Write + Send> Conn for T {}

/// A bound listener that accepts client connections.
pub trait Listener: Send {
    fn accept(&self) -> io::Result<Box<dyn Conn>>;
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::os::unix::net::{UnixListener, UnixStream};

    struct Uds(UnixListener);
    impl Listener for Uds {
        fn accept(&self) -> io::Result<Box<dyn Conn>> {
            self.0.accept().map(|(s, _)| Box::new(s) as Box<dyn Conn>)
        }
    }

    pub fn bind(endpoint: &Path) -> io::Result<Box<dyn Listener>> {
        let _ = std::fs::remove_file(endpoint); // clear a stale socket
        Ok(Box::new(Uds(UnixListener::bind(endpoint)?)))
    }

    pub fn connect(endpoint: &Path) -> io::Result<Box<dyn Conn>> {
        Ok(Box::new(UnixStream::connect(endpoint)?))
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    struct Tcp(TcpListener);
    impl Listener for Tcp {
        fn accept(&self) -> io::Result<Box<dyn Conn>> {
            self.0.accept().map(|(s, _)| Box::new(s) as Box<dyn Conn>)
        }
    }

    pub fn bind(endpoint: &Path) -> io::Result<Box<dyn Listener>> {
        // Loopback, ephemeral port → write the port to the rendezvous path.
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        std::fs::write(endpoint, port.to_string())?;
        Ok(Box::new(Tcp(listener)))
    }

    pub fn connect(endpoint: &Path) -> io::Result<Box<dyn Conn>> {
        let port: u16 = std::fs::read_to_string(endpoint)?
            .trim()
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad daemon port file"))?;
        Ok(Box::new(TcpStream::connect(("127.0.0.1", port))?))
    }
}

/// Bind a daemon listener at the rendezvous `endpoint` (UDS on unix, loopback TCP
/// on Windows). Removes a stale rendezvous first.
pub fn bind(endpoint: &Path) -> io::Result<Box<dyn Listener>> {
    imp::bind(endpoint)
}

/// Connect to a daemon listening at `endpoint`.
pub fn connect(endpoint: &Path) -> io::Result<Box<dyn Conn>> {
    imp::connect(endpoint)
}
