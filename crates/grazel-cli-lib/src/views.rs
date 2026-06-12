//! The View seam (GrazelViewSeam.md): mock→real subscription machinery.
//!
//! The "mock" is a REAL stream — `build.subscribe` (atom) and
//! `invocation.events` (log) are live and taut; the View-era producer registers
//! over the same [`ViewProducer`] trait when its IR lands, and nothing
//! downstream changes (the house mock→real pattern).
//!
//! [`KeyedFanout`] shares ONE upstream per key across N subscribers (the gryth
//! WS edge — many browsers, one daemon), with §4b bounded buffers: a slow
//! consumer's buffer is cleared and an in-band RESYNC marker (the existing
//! error envelope, no new wire type) delivered; on an atom stream the next
//! upstream frame is a fresh snapshot, so drop-with-resync self-heals.

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use razel_daemon::transport;
use razel_wire::{Cbor, encode};

/// Stream shape decides what a LATE subscriber must be given (the upstream
/// already spent its on-connect behavior on the first subscriber): an ATOM's
/// latest frame supersedes all others; a LOG replays from the start.
#[derive(Clone, Copy, PartialEq)]
pub enum Shape {
    Atom,
    Log,
}

/// A view key names an upstream subscription. Today the keys ARE the stream
/// method names; View-era keys arrive with the View IR (same trait, new producer).
pub trait ViewProducer: Send + Sync {
    fn open(&self, key: &str) -> Result<Box<dyn transport::Conn>, String>;
    fn shape(&self, key: &str) -> Shape;
}

/// Day-1 producer: dial OUR OWN scope socket and request the named stream —
/// real wire, real taut, zero invented protocol.
pub struct SelfSocketProducer {
    pub socket: PathBuf,
}

impl ViewProducer for SelfSocketProducer {
    fn open(&self, key: &str) -> Result<Box<dyn transport::Conn>, String> {
        let req = match key {
            "build.subscribe" => razel_daemon::rpc::req_subscribe(),
            "invocation.events" => razel_daemon::rpc::req_invocation_events(),
            other => return Err(format!("unknown view key {other:?}")),
        };
        let mut conn = transport::connect(&self.socket).map_err(|e| e.to_string())?;
        let payload = encode(&req);
        let mut framed = (payload.len() as u32).to_be_bytes().to_vec();
        framed.extend_from_slice(&payload);
        use std::io::Write;
        conn.write_all(&framed).map_err(|e| e.to_string())?;
        Ok(conn)
    }

    fn shape(&self, key: &str) -> Shape {
        match key {
            "invocation.events" => Shape::Log,
            _ => Shape::Atom,
        }
    }
}

/// The in-band resync marker: the wire's error envelope, so every client
/// already parses it. `dropped` = frames discarded since the last delivery.
fn resync_marker(dropped: usize) -> Vec<u8> {
    encode(&Cbor::Map(vec![
        (1, Cbor::Bool(false)),
        (3, Cbor::Text(format!("resync: dropped={dropped} (slow consumer; §4b)"))),
    ]))
}

struct SubBuf {
    q: Mutex<VecDeque<Vec<u8>>>,
    cv: Condvar,
    closed: AtomicBool,
}

impl SubBuf {
    fn push(&self, frame: Vec<u8>, bound: usize) {
        let mut q = self.q.lock().unwrap();
        if q.len() >= bound {
            let dropped = q.len();
            q.clear();
            q.push_back(resync_marker(dropped));
        }
        q.push_back(frame);
        self.cv.notify_all();
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.cv.notify_all();
    }
}

struct Entry {
    generation: u64,
    subs: Vec<(u64, Arc<SubBuf>)>,
    next_id: u64,
    shape: Shape,
    /// What a late subscriber is owed: an Atom's latest frame, a Log's full
    /// replay (unbounded v1 — mirrors razel's unbounded events log; their
    /// bounded-buffer work and ours land together, GrazelViewSeam.md).
    history: Vec<Vec<u8>>,
}

/// `key → one upstream + N subscriber buffers`. Refcounted: the upstream is
/// opened by the first subscriber and torn down when the last leaves (the map
/// entry goes immediately; the parked reader thread reaps on its next frame).
pub struct KeyedFanout {
    producer: Box<dyn ViewProducer>,
    bound: usize,
    /// Observability file (`<state>/fanout`, "key n" lines) — daemon status
    /// stays filesystem-only and still sees subscription counts.
    state_file: PathBuf,
    map: Arc<Mutex<HashMap<String, Entry>>>,
    next_gen: Mutex<u64>,
}

pub struct Subscription {
    key: String,
    id: u64,
    buf: Arc<SubBuf>,
    fanout_map: Arc<Mutex<HashMap<String, Entry>>>,
    state_file: PathBuf,
}

impl Subscription {
    /// A detached close handle — lets a transport's reader thread end the
    /// subscription (client went away) while the serve loop is parked in
    /// `recv` on a quiet stream.
    pub fn closer(&self) -> impl Fn() + Send + 'static {
        let buf = self.buf.clone();
        move || buf.close()
    }

    /// Next frame (raw envelope bytes); None = stream over (upstream closed or
    /// this subscription dropped).
    pub fn recv(&self) -> Option<Vec<u8>> {
        let mut q = self.buf.q.lock().unwrap();
        loop {
            if let Some(f) = q.pop_front() {
                return Some(f);
            }
            if self.buf.closed.load(Ordering::SeqCst) {
                return None;
            }
            q = self.buf.cv.wait(q).unwrap();
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.buf.close();
        let mut map = self.fanout_map.lock().unwrap();
        if let Some(e) = map.get_mut(&self.key) {
            e.subs.retain(|(id, _)| *id != self.id);
            if e.subs.is_empty() {
                map.remove(&self.key); // parked reader reaps on its next frame
            }
        }
        write_state_file(&self.state_file, &map);
    }
}

fn write_state_file(path: &Path, map: &HashMap<String, Entry>) {
    let mut lines: Vec<String> =
        map.iter().map(|(k, e)| format!("{k} {}", e.subs.len())).collect();
    lines.sort();
    let _ = std::fs::write(path, lines.join("\n") + "\n");
}

impl KeyedFanout {
    pub fn new(producer: Box<dyn ViewProducer>, bound: usize, state_file: PathBuf) -> Self {
        Self {
            producer,
            bound,
            state_file,
            map: Arc::new(Mutex::new(HashMap::new())),
            next_gen: Mutex::new(0),
        }
    }

    pub fn subscribe(&self, key: &str) -> Result<Subscription, String> {
        let mut map = self.map.lock().unwrap();
        if !map.contains_key(key) {
            let conn = self.producer.open(key)?; // before insert: open failure leaves no entry
            let generation = {
                let mut g = self.next_gen.lock().unwrap();
                *g += 1;
                *g
            };
            map.insert(
                key.to_string(),
                Entry {
                    generation,
                    subs: vec![],
                    next_id: 0,
                    shape: self.producer.shape(key),
                    history: vec![],
                },
            );
            self.spawn_reader(key.to_string(), generation, conn);
        }
        let entry = map.get_mut(key).expect("just ensured");
        let id = entry.next_id;
        entry.next_id += 1;
        let buf = Arc::new(SubBuf {
            q: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
            closed: AtomicBool::new(false),
        });
        // The upstream spent its on-connect behavior on the FIRST subscriber;
        // late joiners are owed the shape's catch-up (snapshot / replay). The
        // bound applies — an over-long replay resyncs, by design.
        for f in &entry.history {
            buf.push(f.clone(), self.bound);
        }
        entry.subs.push((id, buf.clone()));
        write_state_file(&self.state_file, &map);
        Ok(Subscription {
            key: key.to_string(),
            id,
            buf,
            fanout_map: self.map.clone(),
            state_file: self.state_file.clone(),
        })
    }

    /// One reader per upstream: frames go to every subscriber's bounded buffer.
    /// Exits when the key's entry is gone or superseded (generation check —
    /// a re-subscribed key gets a FRESH upstream; the old reader must not
    /// double-deliver into it).
    fn spawn_reader(&self, key: String, generation: u64, mut conn: Box<dyn transport::Conn>) {
        let map = self.map.clone();
        let bound = self.bound;
        std::thread::spawn(move || {
            loop {
                let mut len = [0u8; 4];
                if conn.read_exact(&mut len).is_err() {
                    break; // upstream gone
                }
                let mut frame = vec![0u8; u32::from_be_bytes(len) as usize];
                if conn.read_exact(&mut frame).is_err() {
                    break;
                }
                let mut guard = map.lock().unwrap();
                match guard.get_mut(&key) {
                    Some(e) if e.generation == generation => {
                        match e.shape {
                            Shape::Atom => e.history = vec![frame.clone()],
                            Shape::Log => e.history.push(frame.clone()),
                        }
                        for (_, sub) in &e.subs {
                            sub.push(frame.clone(), bound);
                        }
                    }
                    _ => break, // unsubscribed-to-zero or superseded: reap
                }
            }
            // Upstream died with subscribers still attached: end their streams.
            let mut guard = map.lock().unwrap();
            if let Some(e) = guard.get(&key)
                && e.generation == generation
            {
                for (_, sub) in &e.subs {
                    sub.close();
                }
                guard.remove(&key);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A producer over an in-memory pipe the test writes frames into.
    /// `block_after`: a live-but-quiet upstream (parks instead of EOF) — EOF
    /// would correctly close all subscriptions and reap the key.
    struct PipeProducer {
        frames: Vec<Vec<u8>>,
        block_after: bool,
    }
    struct PipeConn {
        data: std::io::Cursor<Vec<u8>>,
        block_after: bool,
    }
    impl Read for PipeConn {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.data.read(buf)?;
            if n == 0 && self.block_after {
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(3600));
                }
            }
            Ok(n)
        }
    }
    impl std::io::Write for PipeConn {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl ViewProducer for PipeProducer {
        fn shape(&self, _key: &str) -> Shape {
            Shape::Log
        }
        fn open(&self, _key: &str) -> Result<Box<dyn transport::Conn>, String> {
            let mut bytes = Vec::new();
            for f in &self.frames {
                bytes.extend_from_slice(&(f.len() as u32).to_be_bytes());
                bytes.extend_from_slice(f);
            }
            Ok(Box::new(PipeConn {
                data: std::io::Cursor::new(bytes),
                block_after: self.block_after,
            }))
        }
    }

    #[test]
    fn fanout_delivers_and_overflow_resyncs() {
        let tmp = tempfile::tempdir().unwrap();
        let frames: Vec<Vec<u8>> = (0..10u8).map(|i| vec![i; 3]).collect();
        let fanout = KeyedFanout::new(
            Box::new(PipeProducer { frames: frames.clone(), block_after: false }),
            4,
            tmp.path().join("fanout"),
        );
        let sub = fanout.subscribe("k").unwrap();
        // Let the reader drain the whole (EOF-bounded) upstream BEFORE we
        // consume — otherwise a fast consumer keeps the queue under the bound
        // and no overflow occurs (that's correct behavior, not this test).
        std::thread::sleep(std::time::Duration::from_millis(200));
        // 10 frames into a bound of 4 ⇒ at least one resync marker, and the
        // LAST frame always survives (cleared-then-pushed).
        let mut got = Vec::new();
        while let Some(f) = sub.recv() {
            got.push(f);
        }
        assert!(got.iter().any(|f| {
            let c = razel_wire::decode(f);
            matches!(c.get(3), Cbor::Text(t) if t.starts_with("resync"))
        }));
        assert_eq!(*got.last().unwrap(), frames[9]);
    }

    #[test]
    fn state_file_tracks_subscriber_counts() {
        let tmp = tempfile::tempdir().unwrap();
        let sf = tmp.path().join("fanout");
        let fanout = KeyedFanout::new(
            Box::new(PipeProducer { frames: vec![], block_after: true }),
            8,
            sf.clone(),
        );
        let a = fanout.subscribe("k").unwrap();
        let b = fanout.subscribe("k").unwrap();
        assert_eq!(std::fs::read_to_string(&sf).unwrap().trim(), "k 2");
        drop(a);
        assert_eq!(std::fs::read_to_string(&sf).unwrap().trim(), "k 1");
        drop(b);
        assert_eq!(std::fs::read_to_string(&sf).unwrap().trim(), "");
    }
}
