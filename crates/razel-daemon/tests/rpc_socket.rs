//! Over-the-socket integration: spawn a real UDS daemon in a background thread,
//! then drive it from a client connection.

use razel_daemon::rpc::{self, Server};
use razel_wire::{BuildResult, BuildState, BuildStatus, VersionInfo};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const BUILD: &str = r#"
def _impl(ctx):
    out = ctx.attr.name + ".o"
    ctx.actions.run(executable="/usr/bin/cc", outputs=[out], inputs=[ctx.attr.src],
                    arguments=["-c", ctx.attr.src, "-o", out])
    return [DefaultInfo(files=[out])]
cc_obj = rule(implementation=_impl, attrs={"src":1})
cc_obj(name="widget", src="widget.c")
"#;

/// A rendezvous path for the daemon transport, portable across OSes. On unix it's
/// a short `/tmp` path (macOS `sun_path` is ~104 bytes, so the long system tempdir
/// won't fit a UDS); on Windows it's a temp-dir file holding the loopback-TCP port.
fn sock(tag: &str) -> PathBuf {
    let name = format!("razel-rpc-{}-{}.sock", tag, std::process::id());
    #[cfg(unix)]
    let p = PathBuf::from(format!("/tmp/{name}"));
    #[cfg(windows)]
    let p = std::env::temp_dir().join(name);
    p
}

fn spawn(server: Server, socket: PathBuf) {
    std::thread::spawn(move || {
        let _ = server.serve(&socket);
    });
}

fn wait_for(socket: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() {
        assert!(Instant::now() < deadline, "daemon socket never appeared");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn version_over_the_socket() {
    let socket = sock("ver");
    let cache = tempfile::tempdir().unwrap();
    spawn(
        Server::new(PathBuf::from("."), cache.path().to_path_buf()),
        socket.clone(),
    );
    wait_for(&socket);

    let resp = rpc::call(&socket, &rpc::req_version()).unwrap();
    let v = VersionInfo::from_cbor(&rpc::payload(&resp).unwrap());
    assert_eq!(v.protocol, rpc::PROTOCOL);
    assert!(!v.version.is_empty());
    let _ = std::fs::remove_file(&socket);
}

#[test]
fn build_over_the_socket_produces_a_real_object() {
    if !Path::new("/usr/bin/cc").exists() {
        return; // skip where no cc
    }
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("BUILD"), BUILD).unwrap();
    std::fs::write(ws.path().join("widget.c"), "int answer(void){return 42;}").unwrap();
    let cache = tempfile::tempdir().unwrap();

    let socket = sock("build");
    spawn(
        Server::new(ws.path().to_path_buf(), cache.path().to_path_buf()),
        socket.clone(),
    );
    wait_for(&socket);

    let resp = rpc::call(&socket, &rpc::req_build("//x:widget")).unwrap();
    let r = BuildResult::from_cbor(&rpc::payload(&resp).unwrap());
    assert_eq!(r.status, BuildStatus::Built, "msg: {:?}", r.message);
    assert_eq!(r.outputs.len(), 1);
    assert_eq!(r.outputs[0].path, "widget.o");
    assert!(!r.outputs[0].digest.is_empty());
    assert!(
        ws.path().join("widget.o").exists(),
        "daemon did not build the object"
    );
    let _ = std::fs::remove_file(&socket);
}

const NOOP_BUILD: &str = r#"
def _impl(ctx):
    ctx.actions.run(executable = "/usr/bin/true", outputs = [], inputs = [], arguments = [])
    return [DefaultInfo(files = [])]
noop = rule(implementation = _impl, attrs = {})
noop(name = "widget")
"#;

#[test]
fn build_subscribe_streams_state_when_a_build_lands() {
    // cc-independent: the rule's action is /usr/bin/true.
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("BUILD"), NOOP_BUILD).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let socket = sock("sub");
    spawn(
        Server::new(ws.path().to_path_buf(), cache.path().to_path_buf()),
        socket.clone(),
    );
    wait_for(&socket);

    // Subscribe: the first frame is the current (empty) state at revision 0.
    let mut sub = rpc::subscribe(&socket).unwrap();
    let s0 = BuildState::from_cbor(&rpc::payload(&rpc::next_frame(&mut sub).unwrap()).unwrap());
    assert_eq!(s0.revision, 0);
    assert!(s0.targets.is_empty());

    // Trigger a build on a separate connection → publishes a new state.
    let r = rpc::call(&socket, &rpc::req_build("widget")).unwrap();
    rpc::payload(&r).unwrap();

    // The subscriber receives the updated snapshot (atom: whole state, revision++).
    let s1 = BuildState::from_cbor(&rpc::payload(&rpc::next_frame(&mut sub).unwrap()).unwrap());
    assert_eq!(s1.revision, 1);
    assert_eq!(s1.targets.len(), 1);
    assert_eq!(s1.targets[0].label, "widget");
    assert_eq!(s1.targets[0].status, BuildStatus::Built);

    let _ = std::fs::remove_file(&socket);
}
