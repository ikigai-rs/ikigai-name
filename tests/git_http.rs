//! Repository-backed namespaces through the real binary, built with `git`.
//!
//! The forge is a small HTTP server in this process that answers what GitHub's API does
//! for one file, so the real client, `ikigai-http`, the real validator, the real secret
//! store and real files are all on the path, and nothing leaves the machine. A webhook is
//! a real signed `POST`, and a read is a real `GET`.

use hmac::{Hmac, Mac};
use ikigai_name::git::{hook_secret, token_secret};
use ikigai_secret::{Backend, FileBackend};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SHAPES: &str = r#"
@prefix sh:   <http://www.w3.org/ns/shacl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix ex:   <https://acme.example/ns#> .

ex:ClassShape a sh:NodeShape ;
    sh:targetClass rdfs:Class ;
    sh:property [ sh:path rdfs:comment ; sh:minCount 1 ] .
"#;

const KEY: &[u8] = b"hook-key";
const PUSH: &[u8] = br#"{"ref":"refs/heads/main"}"#;

fn vocabulary(comment: &str) -> String {
    format!(
        "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
         @prefix ex: <https://acme.example/ns#> .\n\
         ex:Thing a rdfs:Class ; rdfs:comment \"{comment}\" .\n"
    )
}

const NONCONFORMING: &str = "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
                             @prefix ex: <https://acme.example/ns#> .\n\
                             ex:Thing a rdfs:Class .\n";

fn scratch() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ikigai-name-git-http-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

/// What the forge has been asked for, and what it answers.
#[derive(Default)]
struct ForgeState {
    /// The file on `main`, or `None` for a 404.
    file: Option<String>,
    /// The token it demands, if the repository is private.
    token: Option<String>,
    requests: Vec<String>,
    authorizations: Vec<String>,
}

struct Forge {
    addr: SocketAddr,
    state: Arc<Mutex<ForgeState>>,
}

impl Forge {
    fn start(file: &str) -> Forge {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let state = Arc::new(Mutex::new(ForgeState {
            file: Some(file.to_string()),
            ..ForgeState::default()
        }));
        let shared = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                answer_as_forge(stream, &shared);
            }
        });
        Forge { addr, state }
    }

    fn push(&self, file: &str) {
        self.state.lock().unwrap().file = Some(file.to_string());
    }

    fn vanish(&self) {
        self.state.lock().unwrap().file = None;
    }

    fn reads(&self) -> usize {
        self.state.lock().unwrap().requests.len()
    }
}

fn answer_as_forge(mut stream: TcpStream, state: &Mutex<ForgeState>) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => return,
        }
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let line = head.lines().next().unwrap_or_default().to_string();
    let authorization = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("authorization")
                .then(|| value.trim().to_string())
        })
        .unwrap_or_default();
    let mut state = state.lock().unwrap();
    state.requests.push(line.clone());
    state.authorizations.push(authorization.clone());
    let wanted = line.contains("/repos/acme/vocab/contents/vocab/core.ttl?ref=main");
    let allowed = match &state.token {
        Some(token) => authorization == format!("Bearer {token}"),
        None => true,
    };
    let (status, body) = match (&state.file, wanted && allowed) {
        (Some(file), true) => ("200 OK", file.clone()),
        _ => ("404 Not Found", "{\"message\":\"Not Found\"}".to_string()),
    };
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/vnd.github.raw+json\r\n\
         Cache-Control: max-age=300\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(reply.as_bytes());
}

fn registry(forge_host: &str, strategy: &str, private: bool) -> String {
    let field = if strategy == "hosted" {
        "source"
    } else {
        "origin"
    };
    format!(
        r#"{{"namespaces":[{{"prefix":"acme","owner":"urn:cap:name:admin:acme",
          "strategy":"{strategy}","{field}":"urn:name:git:acme",
          "repository":{{"url":"https://{forge_host}/acme/vocab","branch":"main",
            "path":"vocab/core.ttl","shape":"urn:file:shapes.ttl","private":{private}}}}}]}}"#
    )
}

struct Server {
    child: Child,
    addr: SocketAddr,
    root: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.local_addr().expect("addr")
}

/// The server, with a timer too slow to act during a test but for its first tick.
fn serve(forge: &Forge, strategy: &str, private: bool) -> (Server, PathBuf) {
    let base = scratch();
    let root = base.join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("registry.json"),
        registry("forge.test", strategy, private),
    )
    .unwrap();
    std::fs::write(root.join("shapes.ttl"), SHAPES).unwrap();
    let secrets = base.join("secrets");
    let store = FileBackend::new(&secrets);
    store.set(&hook_secret(None, "acme"), KEY).unwrap();
    let server = spawn(forge, &root, &secrets, &[]);
    (server, secrets)
}

fn spawn(forge: &Forge, root: &Path, secrets: &Path, extra: &[&str]) -> Server {
    let addr = free_addr();
    let child = Command::new(env!("CARGO_BIN_EXE_ikigai-name-server"))
        .args(["--listen", &addr.to_string()])
        .arg("--root")
        .arg(root)
        .arg("--git")
        .args(["--git-every", "3600"])
        .arg("--secrets")
        .arg(secrets)
        .args([
            "--git-forge",
            &format!("forge.test=github@http://{}", forge.addr),
        ])
        .args(extra)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the server starts");
    Server {
        child,
        addr,
        root: root.to_path_buf(),
    }
}

/// A response's status, lowercased head and body.
fn http(addr: SocketAddr, request: &[u8]) -> Option<(u16, String, String)> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .ok()?;
    stream.write_all(request).ok()?;
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).ok()?;
    let answer = String::from_utf8_lossy(&answer).into_owned();
    let (head, body) = answer.split_once("\r\n\r\n")?;
    let status = head.split_whitespace().nth(1)?.parse().ok()?;
    Some((status, head.to_ascii_lowercase(), body.to_string()))
}

fn get(server: &Server, path: &str, accept: &str) -> (u16, String, String) {
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: t\r\nAccept: {accept}\r\nConnection: close\r\n\r\n");
    http(server.addr, request.as_bytes()).expect("an answer")
}

fn post(server: &Server, path: &str, body: &[u8], signature: Option<&str>) -> (u16, String) {
    let signed = signature
        .map(|s| format!("X-Hub-Signature-256: {s}\r\n"))
        .unwrap_or_default();
    let mut request = format!(
        "POST {path} HTTP/1.1\r\nHost: t\r\n{signed}Content-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    request.extend_from_slice(body);
    let (status, _, body) = http(server.addr, &request).expect("an answer");
    (status, body)
}

fn signature(key: &[u8], body: &[u8]) -> String {
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(key).unwrap();
    mac.update(body);
    let hex: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("sha256={hex}")
}

fn webhook(server: &Server) -> (u16, String) {
    post(server, "/-/git/acme", PUSH, Some(&signature(KEY, PUSH)))
}

/// The timer's first tick fills the store: wait for it to have served.
fn wait_until_served(server: &Server) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some((200, _, body)) = http(
            server.addr,
            b"GET /acme/core HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n",
        ) {
            return body;
        }
        assert!(Instant::now() < deadline, "never served");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn status_file(server: &Server) -> String {
    std::fs::read_to_string(server.root.join("git/status.acme.json")).expect("a status file")
}

#[test]
fn a_signed_push_serves_the_new_version_on_the_next_read() {
    let forge = Forge::start(&vocabulary("one"));
    let (server, _) = serve(&forge, "mirror", false);
    assert!(wait_until_served(&server).contains("one"));

    // Pushed, and no word yet: what is served is what was accepted.
    forge.push(&vocabulary("two"));
    let (status, head, body) = get(&server, "/acme/core", "text/turtle");
    assert_eq!(status, 200);
    assert!(head.contains("content-type: text/turtle"), "{head}");
    assert!(head.contains("x-content-type-options: nosniff"), "{head}");
    assert!(body.contains("one"), "{body}");

    let reads = forge.reads();
    let (status, body) = webhook(&server);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("now serving"), "{body}");
    assert_eq!(forge.reads(), reads + 1);

    let (_, _, body) = get(&server, "/acme/core", "text/turtle");
    assert!(body.contains("two"), "{body}");
    // The copy is on disk, where a restart will find it.
    let copy = std::fs::read_to_string(server.root.join("git/document.acme.ttl")).unwrap();
    assert!(copy.contains("two"), "{copy}");

    // And a request does not reach the forge.
    let reads = forge.reads();
    for _ in 0..3 {
        get(&server, "/acme/core", "text/turtle");
    }
    assert_eq!(forge.reads(), reads);
}

/// With file caching on, the answer is held under the golden thread, so this is the path
/// on which a webhook has something to cut. The write through the kernel is what cuts it.
#[test]
fn a_signed_push_cuts_a_cached_answer() {
    let forge = Forge::start(&vocabulary("one"));
    let base = scratch();
    let root = base.join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("registry.json"),
        registry("forge.test", "mirror", false),
    )
    .unwrap();
    std::fs::write(root.join("shapes.ttl"), SHAPES).unwrap();
    let secrets = base.join("secrets");
    FileBackend::new(&secrets)
        .set(&hook_secret(None, "acme"), KEY)
        .unwrap();
    let server = spawn(&forge, &root, &secrets, &["--cache-files"]);
    assert!(wait_until_served(&server).contains("one"));

    // Behind the kernel's back the file changes, as nothing but a bug would change it, so
    // that serving the old bytes shows the answer is cached rather than read again.
    let copy = root.join("git/document.acme.ttl");
    let held = std::fs::read_to_string(&copy).unwrap();
    std::fs::write(&copy, vocabulary("behind its back")).unwrap();
    let (_, _, body) = get(&server, "/acme/core", "text/turtle");
    assert_eq!(body, held, "the answer is held");
    std::fs::write(&copy, &held).unwrap();

    forge.push(&vocabulary("two"));
    let (status, answer) = webhook(&server);
    assert_eq!(status, 200, "{answer}");
    let (_, _, body) = get(&server, "/acme/core", "text/turtle");
    assert!(body.contains("two"), "the push cut it: {body}");
}

#[test]
fn a_push_nobody_signed_does_nothing() {
    let forge = Forge::start(&vocabulary("one"));
    let (server, _) = serve(&forge, "hosted", false);
    wait_until_served(&server);
    forge.push(&vocabulary("two"));
    let reads = forge.reads();
    for (path, body, signed) in [
        ("/-/git/acme", PUSH, None),
        ("/-/git/acme", PUSH, Some(signature(b"wrong", PUSH))),
        ("/-/git/acme", PUSH, Some("sha256=00".to_string())),
        // A signature over something else is not one over this.
        ("/-/git/acme", &b"{}"[..], Some(signature(KEY, PUSH))),
        // No key at all, and the sentence does not say so.
        ("/-/git/other", PUSH, Some(signature(KEY, PUSH))),
    ] {
        let (status, answer) = post(&server, path, body, signed.as_deref());
        assert_eq!(status, 403, "{path}: {answer}");
        assert!(answer.contains("signature"), "{answer}");
    }
    assert_eq!(forge.reads(), reads);
    let (_, _, body) = get(&server, "/acme/core", "text/turtle");
    assert!(body.contains("one"), "{body}");

    let (status, head, _) = get(&server, "/-/git/acme", "text/plain");
    assert_eq!(status, 403, "{head}");
}

/// A key is one namespace's, so it refreshes nothing else, even if it is known.
#[test]
fn a_key_refreshes_only_its_own_namespace() {
    let forge = Forge::start(&vocabulary("one"));
    let (server, secrets) = serve(&forge, "hosted", false);
    wait_until_served(&server);
    FileBackend::new(&secrets)
        .set(&hook_secret(None, "other"), b"other-key")
        .unwrap();
    let signed = signature(b"other-key", PUSH);
    assert_eq!(post(&server, "/-/git/acme", PUSH, Some(&signed)).0, 403);
}

#[test]
fn a_version_failing_its_shape_is_refused_and_the_owner_can_read_why() {
    let forge = Forge::start(&vocabulary("good"));
    let (server, _) = serve(&forge, "hosted", false);
    wait_until_served(&server);

    forge.push(NONCONFORMING);
    let (status, answer) = webhook(&server);
    // The forge shows this answer in the webhook's deliveries, to whoever owns it.
    assert_eq!(status, 200, "{answer}");
    assert!(answer.contains("refused"), "{answer}");
    assert!(answer.contains("rdf-schema#comment"), "{answer}");

    let (_, _, body) = get(&server, "/acme/core", "text/turtle");
    assert!(body.contains("good"), "{body}");
    assert!(status_file(&server).contains("rdf-schema#comment"));
}

#[test]
fn repository_html_is_refused_and_only_generated_html_is_served() {
    let forge = Forge::start(&vocabulary("<script>alert(1)</script>"));
    let (server, _) = serve(&forge, "hosted", false);
    wait_until_served(&server);

    // The page is this module's, and what the repository says in it is escaped.
    let (status, head, page) = get(&server, "/acme/core", "text/html");
    assert_eq!(status, 200, "{page}");
    assert!(head.contains("content-type: text/html"), "{head}");
    assert!(head.contains("content-security-policy"), "{head}");
    assert!(!page.contains("<script>alert(1)"), "{page}");
    assert!(page.contains("&lt;script&gt;alert(1)"), "{page}");

    forge.push("<!doctype html><html><script>alert(1)</script></html>");
    let (_, answer) = webhook(&server);
    assert!(answer.contains("refused"), "{answer}");
    let (_, head, body) = get(&server, "/acme/core", "text/turtle");
    assert!(head.contains("content-type: text/turtle"), "{head}");
    assert!(!body.contains("doctype"), "{body}");
}

/// The registry cannot name it, so the server has nothing to fetch and serve.
#[test]
fn a_registry_naming_repository_html_serves_nothing() {
    let forge = Forge::start("<html></html>");
    let base = scratch();
    let root = base.join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("registry.json"),
        registry("forge.test", "hosted", false).replace("vocab/core.ttl", "index.html"),
    )
    .unwrap();
    let server = spawn(&forge, &root, &base.join("secrets"), &[]);
    let deadline = Instant::now() + Duration::from_secs(20);
    let answer = loop {
        if let Some(answer) = http(
            server.addr,
            b"GET /acme/core HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n",
        ) {
            break answer;
        }
        assert!(Instant::now() < deadline, "never answered");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_ne!(answer.0, 200, "{answer:?}");
    assert!(!answer.2.contains("<html></html>"), "{answer:?}");
    assert_eq!(forge.reads(), 0);
}

#[test]
fn a_vanished_repository_still_serves_the_last_good_copy_from_a_mirror() {
    let forge = Forge::start(&vocabulary("kept"));
    let (server, _) = serve(&forge, "mirror", false);
    wait_until_served(&server);
    forge.vanish();
    let (status, answer) = webhook(&server);
    assert_eq!(status, 200, "{answer}");
    assert!(answer.contains("nothing accepted"), "{answer}");
    let (status, _, body) = get(&server, "/acme/core", "text/turtle");
    assert_eq!(status, 200);
    assert!(body.contains("kept"), "{body}");
    assert!(status_file(&server).contains("\"gone\": true"));
}

#[test]
fn a_vanished_repository_stops_a_hosted_namespace() {
    let forge = Forge::start(&vocabulary("one"));
    let (server, _) = serve(&forge, "hosted", false);
    wait_until_served(&server);
    forge.vanish();
    webhook(&server);
    let (status, _, _) = get(&server, "/acme/core", "text/turtle");
    assert_eq!(status, 404);
}

/// The copy survives a restart with the repository gone: that is the whole promise.
#[test]
fn a_mirror_s_copy_outlives_the_process_and_the_repository() {
    let forge = Forge::start(&vocabulary("kept"));
    let (server, secrets) = serve(&forge, "mirror", false);
    wait_until_served(&server);
    let root = server.root.clone();
    drop(server);

    forge.vanish();
    let server = spawn(&forge, &root, &secrets, &[]);
    assert!(wait_until_served(&server).contains("kept"));
}

#[test]
fn a_private_repository_is_read_with_the_token_from_the_secret_store() {
    let forge = Forge::start(&vocabulary("private"));
    forge.state.lock().unwrap().token = Some("tok-123".into());
    let base = scratch();
    let root = base.join("root");
    std::fs::create_dir_all(&root).unwrap();
    let written = registry("forge.test", "hosted", true);
    assert!(!written.contains("tok-123"));
    std::fs::write(root.join("registry.json"), &written).unwrap();
    std::fs::write(root.join("shapes.ttl"), SHAPES).unwrap();
    let secrets = base.join("secrets");
    let store = FileBackend::new(&secrets);
    store.set(&token_secret(None, "acme"), b"tok-123").unwrap();
    store.set(&hook_secret(None, "acme"), KEY).unwrap();

    let server = spawn(&forge, &root, &secrets, &[]);
    assert!(wait_until_served(&server).contains("private"));
    assert!(forge
        .state
        .lock()
        .unwrap()
        .authorizations
        .contains(&"Bearer tok-123".to_string()));

    // Nothing a client or a file holds it.
    for file in [
        "registry.json",
        "git/status.acme.json",
        "git/document.acme.ttl",
    ] {
        let text = std::fs::read_to_string(root.join(file)).unwrap_or_default();
        assert!(!text.contains("tok-123"), "{file}");
    }
}

/// One tenant's key, token and files are not another's.
#[test]
fn a_tenant_s_push_refreshes_that_tenant() {
    let forge = Forge::start(&vocabulary("one"));
    let base = scratch();
    let root = base.join("root");
    for host in ["a.test", "b.test"] {
        std::fs::create_dir_all(root.join(host)).unwrap();
        std::fs::write(
            root.join(host).join("registry.json"),
            registry("forge.test", "hosted", false),
        )
        .unwrap();
        std::fs::write(root.join(host).join("shapes.ttl"), SHAPES).unwrap();
    }
    let secrets = base.join("secrets");
    FileBackend::new(&secrets)
        .set(&hook_secret(Some("a.test"), "acme"), KEY)
        .unwrap();
    let server = spawn(
        &forge,
        &root,
        &secrets,
        &["--tenant", "a.test", "--tenant", "b.test"],
    );
    let read = |host: &str| {
        let request = format!(
            "GET /acme/core HTTP/1.1\r\nHost: {host}\r\nAccept: text/turtle\r\n\
             Connection: close\r\n\r\n"
        );
        http(server.addr, request.as_bytes())
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while !["a.test", "b.test"]
        .iter()
        .all(|host| matches!(read(host), Some((200, _, _))))
    {
        assert!(Instant::now() < deadline, "never served");
        std::thread::sleep(Duration::from_millis(50));
    }

    forge.push(&vocabulary("two"));
    let hook = |host: &str| {
        let mut request = format!(
            "POST /-/git/acme HTTP/1.1\r\nHost: {host}\r\nX-Hub-Signature-256: {}\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            signature(KEY, PUSH),
            PUSH.len()
        )
        .into_bytes();
        request.extend_from_slice(PUSH);
        http(server.addr, &request).expect("an answer")
    };
    // b.test has no key, and a.test's does not reach it.
    assert_eq!(hook("b.test").0, 403);
    assert_eq!(hook("a.test").0, 200);
    assert!(read("a.test").unwrap().2.contains("two"));
    assert!(read("b.test").unwrap().2.contains("one"));
    assert!(root.join("a.test/git/document.acme.ttl").exists());
}

/// A server not told to has no such path, whatever the build.
#[test]
fn without_git_the_webhook_path_does_not_exist() {
    let base = scratch();
    std::fs::write(base.join("registry.json"), r#"{"namespaces":[]}"#).unwrap();
    let addr = free_addr();
    let mut child = Command::new(env!("CARGO_BIN_EXE_ikigai-name-server"))
        .args(["--listen", &addr.to_string()])
        .arg("--root")
        .arg(&base)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("starts");
    let mut request = format!(
        "POST /-/git/acme HTTP/1.1\r\nHost: t\r\nX-Hub-Signature-256: {}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        signature(KEY, PUSH),
        PUSH.len()
    )
    .into_bytes();
    request.extend_from_slice(PUSH);
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some((status, _, _)) = http(addr, &request) {
            break status;
        }
        assert!(Instant::now() < deadline, "never answered");
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = child.kill();
    let _ = child.wait();
    assert_ne!(status, 200);
    assert!(!base.join("git").exists());
}

/// Secrets the server would serve as files are refused at the door.
#[test]
fn secrets_under_the_root_stop_the_server() {
    let forge = Forge::start("x");
    let base = scratch();
    let root = base.join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("registry.json"), r#"{"namespaces":[]}"#).unwrap();
    let mut server = spawn(&forge, &root, &root.join("secrets"), &[]);
    let status = server.child.wait().expect("it stops");
    assert_eq!(status.code(), Some(2));
}
