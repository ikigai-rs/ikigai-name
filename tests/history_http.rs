//! The history contract ([`history_contract::run`]) through the real binary, with
//! `--history git` and two tenants: a source is read as its namespace's document as of
//! a date, and the registry as whether a namespace answered then.

mod history_contract;

use history_contract::{git, Binding};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const TENANTS: [&str; 2] = ["one.example", "other.example"];

struct Server {
    child: Child,
    addr: SocketAddr,
    root: PathBuf,
}

/// A status and a body.
fn get(addr: SocketAddr, host: &str, target: &str) -> Option<(u16, String)> {
    let mut stream = TcpStream::connect(addr).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .ok()?;
    let request = format!(
        "GET {target} HTTP/1.1\r\nHost: {host}\r\nAccept: text/turtle\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).ok()?;
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).ok()?;
    let answer = String::from_utf8_lossy(&answer).into_owned();
    let status = answer.get(9..12)?.parse().ok()?;
    let body = answer.split_once("\r\n\r\n").map(|(_, b)| b.to_string())?;
    Some((status, body))
}

impl Server {
    fn start(root: PathBuf, args: &[&str]) -> std::result::Result<Server, String> {
        let addr = TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("an ephemeral port");
        let mut command = Command::new(env!("CARGO_BIN_EXE_ikigai-name-server"));
        command
            .arg("--listen")
            .arg(addr.to_string())
            .arg("--root")
            .arg(&root)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let child = command.spawn().expect("spawn ikigai-name-server");
        let mut server = Server { child, addr, root };
        let deadline = Instant::now() + Duration::from_secs(20);
        while get(server.addr, TENANTS[0], "/-/style").is_none() {
            if let Some(status) = server.child.try_wait().expect("poll the server") {
                let mut stderr = String::new();
                let _ = server
                    .child
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_string(&mut stderr);
                return Err(format!("{status}: {stderr}"));
            }
            assert!(Instant::now() < deadline, "the server never answered");
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(server)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Binding for Server {
    fn record(&mut self, tenant: Option<&str>, at: &str, files: &[(&str, &str)]) {
        git::commit(&self.root.join(tenant.unwrap()), at, files);
    }

    fn read(&self, tenant: Option<&str>, as_of: &str) -> Option<String> {
        let (status, body) = get(
            self.addr,
            tenant.unwrap(),
            &format!("/resmud?as-of={as_of}"),
        )
        .expect("an answer");
        match status {
            200 => Some(body),
            404 => None,
            other => panic!("as of {as_of}: {other} {body}"),
        }
    }

    fn claims(&self, tenant: Option<&str>, prefix: &str, as_of: &str) -> bool {
        let (status, body) = get(
            self.addr,
            tenant.unwrap(),
            &format!("/{prefix}?as-of={as_of}"),
        )
        .expect("an answer");
        match status {
            404 => false,
            200..=399 => true,
            other => panic!("{prefix} as of {as_of}: {other} {body}"),
        }
    }
}

fn tenanted_root() -> PathBuf {
    let root = git::scratch("history-http");
    for tenant in TENANTS {
        git::init(&root.join(tenant));
    }
    root
}

fn history_args() -> Vec<&'static str> {
    vec![
        "--tenant",
        TENANTS[0],
        "--tenant",
        TENANTS[1],
        "--history",
        "git",
    ]
}

#[test]
fn the_server_binds_git_history_by_configuration_and_keeps_the_contract() {
    let mut server = Server::start(tenanted_root(), &history_args()).expect("the server starts");
    history_contract::run(&mut server, Some(TENANTS[0]), Some(TENANTS[1]));
}

/// A kind the server does not know stops it, naming the kinds it does.
#[test]
fn an_unknown_history_kind_is_refused_with_the_kinds_named() {
    let refused = Server::start(tenanted_root(), &["--history", "svn"])
        .err()
        .expect("the server refuses to start");
    assert!(refused.contains("exit status: 2"), "{refused}");
    assert!(
        refused.contains("--history `svn` is not a kind of history source; the kinds are: git"),
        "{refused}"
    );
}
