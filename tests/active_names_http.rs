//! `--count-names`, through the real binary: names resolved over HTTP are stored by the
//! flush task as files in `seen/`, a store that cannot be written costs a client nothing,
//! and the counts themselves stay off the public surface.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const VOCAB: &str = "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
                     <https://iriref.org/resmud/core#Weapon> a rdfs:Class .\n";

const REGISTRY: &str = r#"{"namespaces":[
  {"prefix":"resmud","owner":"urn:cap:name:admin:resmud","strategy":"hosted","source":"urn:file:vocab/core.ttl"},
  {"prefix":"acme/vocab","owner":"urn:cap:name:admin:acme","strategy":"hosted","source":"urn:file:vocab/core.ttl"},
  {"prefix":"gone","owner":"urn:cap:name:admin:gone","strategy":"retired","reason":"ended"}
]}"#;

fn scratch_root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "ikigai-name-count-{}-{nanos}-{n}",
        std::process::id()
    ))
}

fn fill(dir: &Path) {
    std::fs::create_dir_all(dir.join("vocab")).expect("scratch directory");
    std::fs::write(dir.join("vocab/core.ttl"), VOCAB).expect("vocabulary");
    std::fs::write(dir.join("registry.json"), REGISTRY).expect("registry");
}

struct Server {
    child: Child,
    addr: SocketAddr,
    root: PathBuf,
    log: PathBuf,
}

impl Server {
    fn start(root: PathBuf, args: &[&str]) -> Server {
        let addr = TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("an ephemeral port");
        let log = root.with_extension("log");
        let child = Command::new(env!("CARGO_BIN_EXE_ikigai-name-server"))
            .arg("--listen")
            .arg(addr.to_string())
            .arg("--root")
            .arg(&root)
            .args(args)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).expect("a log file"))
            .spawn()
            .expect("spawn ikigai-name-server");
        let mut server = Server {
            child,
            addr,
            root,
            log,
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        while TcpStream::connect(server.addr).is_err() {
            if let Some(status) = server.child.try_wait().expect("poll the server") {
                panic!("ikigai-name-server exited before listening: {status}");
            }
            assert!(Instant::now() < deadline, "server never listened");
            std::thread::sleep(Duration::from_millis(25));
        }
        server
    }

    fn counting(root: PathBuf) -> Server {
        Server::start(root, &["--count-names", "--flush-every", "1"])
    }

    fn get(&self, target: &str, host: Option<&str>) -> u16 {
        String::from_utf8_lossy(&self.raw(target, host))
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .expect("a status line")
    }

    fn raw(&self, target: &str, host: Option<&str>) -> Vec<u8> {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .expect("read timeout");
        let host = host.map_or_else(|| self.addr.to_string(), str::to_string);
        let request = format!("GET {target} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).expect("send");
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).expect("receive");
        raw
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
        let _ = std::fs::remove_file(&self.log);
    }
}

/// The one set for `prefix` under `dir`'s `seen/`, once it holds `want` names. The month
/// is whatever the server's clock says, so the file is found by its prefix.
fn wait_for_set(dir: &Path, prefix: &str, want: usize) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(set) = read_set(dir, prefix) {
            if set["names"].as_array().map(Vec::len) == Some(want) {
                return set;
            }
        }
        assert!(
            Instant::now() < deadline,
            "no set of {want} names for {prefix} under {}",
            dir.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn read_set(dir: &Path, prefix: &str) -> Option<serde_json::Value> {
    let entries = std::fs::read_dir(dir.join("seen")).ok()?;
    for entry in entries {
        let name = entry.ok()?.file_name().to_string_lossy().into_owned();
        if name.starts_with(&format!("{prefix}.")) && !name.ends_with(".periods.json") {
            let text = std::fs::read_to_string(dir.join("seen").join(name)).ok()?;
            return serde_json::from_str(&text).ok();
        }
    }
    None
}

#[test]
fn names_resolved_over_http_are_stored_in_files() {
    let root = scratch_root();
    fill(&root);
    let server = Server::counting(root.clone());

    for _ in 0..3 {
        assert_eq!(server.get("/resmud/core", None), 200);
        assert_eq!(server.get("/resmud/other", None), 200);
        assert_eq!(server.get("/acme/vocab", None), 200);
    }
    assert_eq!(server.get("/nobody/here", None), 404, "unclaimed");
    assert_eq!(server.get("/gone/thing", None), 404, "retired");

    let set = wait_for_set(&root, "resmud", 2);
    let period = set["period"].as_str().expect("a period").to_string();
    assert_eq!(
        set,
        serde_json::json!({"period": period, "names": ["resmud/core", "resmud/other"]}),
        "names and the period, and nothing else"
    );
    wait_for_set(&root, "acme~2Fvocab", 1);
    assert!(
        read_set(&root, "gone").is_none(),
        "a refused name is not counted"
    );
    assert!(
        read_set(&root, "nobody").is_none(),
        "an unknown name is not counted"
    );

    let list =
        std::fs::read_to_string(root.join("seen/resmud.periods.json")).expect("the periods held");
    let list: serde_json::Value = serde_json::from_str(&list).unwrap();
    assert_eq!(list, serde_json::json!({"periods": [period]}));

    // More names join what is stored.
    assert_eq!(server.get("/resmud/third", None), 200);
    wait_for_set(&root, "resmud", 3);
}

/// The store's failure reaches the log and never a client, and nothing is lost: the
/// names wait for the store to come back.
#[test]
fn a_store_that_cannot_be_written_costs_a_client_nothing() {
    let root = scratch_root();
    fill(&root);
    let server = Server::counting(root.clone());
    // A file where the directory would go, so every write fails.
    std::fs::write(root.join("seen"), "in the way").expect("block the store");

    assert_eq!(server.get("/resmud/core", None), 200);
    let deadline = Instant::now() + Duration::from_secs(20);
    while !server.log().contains("stay buffered") {
        assert!(
            Instant::now() < deadline,
            "the failure was never logged: {}",
            server.log()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(server.get("/resmud/other", None), 200, "still resolving");

    std::fs::remove_file(root.join("seen")).expect("restore the store");
    wait_for_set(&root, "resmud", 2);
}

#[test]
fn each_tenant_counts_in_its_own_directory() {
    let root = scratch_root();
    for tenant in ["iriref.org", "urnid.org"] {
        fill(&root.join(tenant));
    }
    let server = Server::start(
        root.clone(),
        &[
            "--count-names",
            "--flush-every",
            "1",
            "--tenant",
            "iriref.org",
            "--tenant",
            "urnid.org",
        ],
    );
    assert_eq!(server.get("/resmud/core", Some("urnid.org")), 200);
    assert_eq!(server.get("/resmud/other", Some("urnid.org")), 200);
    assert_eq!(server.get("/resmud/core", Some("iriref.org")), 200);
    wait_for_set(&root.join("urnid.org"), "resmud", 2);
    wait_for_set(&root.join("iriref.org"), "resmud", 1);
}

/// The counts are an administrator's, and this server has no authenticated transport, so
/// no path reaches them. Requests still hold no write scope: claiming stays impossible.
#[test]
fn the_counts_are_not_on_the_public_surface() {
    let root = scratch_root();
    fill(&root);
    let server = Server::counting(root);
    for target in [
        "/urn:name:stat:namespace:resmud",
        "/urn:name:stat:namespace:resmud:period:2026-11",
        "/urn:name:seen-flush",
        "/seen/resmud.periods.json",
        "/-/stat",
    ] {
        let status = server.get(target, None);
        assert!(status == 400 || status == 404, "{target}: {status}");
    }
}

/// A counted answer carries a deadline, the end of the month. The door must not hand that
/// to a remote cache as a `max-age`, or an edit would go unseen there for weeks, and a
/// name's hits with it. It hangs from the registry's thread, so the door asks every time.
#[test]
fn counting_does_not_make_an_answer_fresh_for_a_month() {
    let root = scratch_root();
    fill(&root);
    let server = Server::start(
        root.clone(),
        &["--cache-files", "--count-names", "--flush-every", "1"],
    );
    let raw = server.raw("/resmud/core", None);
    let raw = String::from_utf8_lossy(&raw);
    let head = raw.split("\r\n\r\n").next().unwrap().to_ascii_lowercase();
    assert!(head.contains("cache-control: public, no-cache"), "{head}");
    assert!(!head.contains("max-age"), "{head}");
    wait_for_set(&root, "resmud", 1);
}

/// Not asked to count, the server is what it was: no clock, no directory, no writes.
#[test]
fn a_server_not_asked_to_count_writes_nothing() {
    let root = scratch_root();
    fill(&root);
    let server = Server::start(root.clone(), &[]);
    assert_eq!(server.get("/resmud/core", None), 200);
    assert!(!root.join("seen").exists());
    assert!(!server.log().contains("counting"), "{}", server.log());
}

#[test]
fn the_flush_interval_needs_counting_and_a_real_number() {
    let root = scratch_root();
    fill(&root);
    for args in [
        vec!["--flush-every", "1"],
        vec!["--count-names", "--flush-every", "0"],
        vec!["--count-names", "--flush-every", "soon"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_ikigai-name-server"))
            .arg("--root")
            .arg(&root)
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .expect("run ikigai-name-server");
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let said = String::from_utf8_lossy(&output.stderr);
        assert!(said.contains("--flush-every"), "{args:?}: {said}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
