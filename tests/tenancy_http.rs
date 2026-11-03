//! Tenants by `Host`, through the real binary (ledger #3047).
//!
//! `iriref.org`, `urnid.org` and `idref.org` are three tenants on one service:
//! each a different set of hosted identifiers, not three names for one. Every
//! test here speaks raw HTTP so it can write the `Host` header it means.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const IRIREF: &str = "iriref.org";
const URNID: &str = "urnid.org";

/// A vocabulary whose every term is minted under `host`.
fn vocabulary(host: &str, term: &str) -> String {
    format!(
        "@prefix v: <https://{host}/resmud/core#> .\n\
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
         v:{term} a rdfs:Class ; rdfs:comment \"Minted under {host}.\" .\n"
    )
}

fn hosted(prefix: &str, source: &str) -> String {
    format!(
        r#"{{"prefix":"{prefix}","owner":"urn:cap:name:admin:{prefix}","strategy":"hosted","source":"{source}"}}"#
    )
}

/// One namespace, `resmud`, at the same path and the same `urn:file:` name on
/// both tenants, with a different term in each. The names colliding is the point.
fn write_tenant(root: &Path, host: &str, term: &str) {
    let dir = root.join(host);
    std::fs::create_dir_all(dir.join("vocab")).expect("tenant directory");
    std::fs::write(dir.join("vocab/core.ttl"), vocabulary(host, term)).expect("vocabulary");
    write_registry(
        root,
        host,
        &[hosted("resmud", "urn:file:vocab/core.ttl")],
        "",
    );
}

fn write_registry(root: &Path, host: &str, namespaces: &[String], limits: &str) {
    let body = format!(r#"{{"namespaces":[{}]{limits}}}"#, namespaces.join(","));
    std::fs::write(root.join(host).join("registry.json"), body).expect("registry");
}

/// A scratch root, removed when the test ends. Declared before a test's servers,
/// so it outlives them.
struct Scratch(PathBuf);

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch_root() -> Scratch {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("ikigai-name-tenancy-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("scratch root");
    Scratch(root)
}

/// The two standard tenants in a fresh root.
fn two_tenants() -> Scratch {
    let root = scratch_root();
    write_tenant(&root, IRIREF, "Weapon");
    write_tenant(&root, URNID, "Identifier");
    root
}

struct Server {
    child: Child,
    addr: SocketAddr,
}

/// An ephemeral port no other server in this process was given, so a test can
/// never reach another test's server and lose it when that test ends.
fn unique_addr() -> SocketAddr {
    static GIVEN: Mutex<BTreeSet<SocketAddr>> = Mutex::new(BTreeSet::new());
    loop {
        let addr = TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("an ephemeral port");
        if GIVEN.lock().expect("the port list").insert(addr) {
            return addr;
        }
    }
}

/// Whether an HTTP server answers at `addr`. A connection alone proves nothing:
/// these tests open many short connections, so the port just released can be in
/// use as some client's local port, which also makes a server fail to bind it.
fn answers_http(addr: SocketAddr) -> bool {
    let Ok(mut stream) = TcpStream::connect(addr) else {
        return false;
    };
    let probe = "OPTIONS /-/style HTTP/1.1\r\nHost: probe.example\r\nConnection: close\r\n\r\n";
    let timeout = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut answer = [0u8; 9];
    timeout.is_ok()
        && stream.write_all(probe.as_bytes()).is_ok()
        && stream.read_exact(&mut answer).is_ok()
        && answer == *b"HTTP/1.1 "
}

impl Server {
    /// A server over `root`, on a fresh port each time one could not be bound.
    fn start(root: &Path, args: &[&str]) -> Server {
        for _ in 0..8 {
            if let Some(server) = Server::try_start(root, args) {
                return server;
            }
        }
        panic!("ikigai-name-server never started: {args:?}");
    }

    /// `None` if the server exited before it answered, which is a port it could
    /// not bind: a server refusing its configuration is tested on its own, below.
    fn try_start(root: &Path, args: &[&str]) -> Option<Server> {
        let addr = unique_addr();
        let child = Command::new(env!("CARGO_BIN_EXE_ikigai-name-server"))
            .arg("--listen")
            .arg(addr.to_string())
            .arg("--root")
            .arg(root)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ikigai-name-server");
        let mut server = Server { child, addr };
        let deadline = Instant::now() + Duration::from_secs(20);
        while !answers_http(server.addr) {
            if server.child.try_wait().expect("poll the server").is_some() {
                return None;
            }
            assert!(Instant::now() < deadline, "server never listened");
            std::thread::sleep(Duration::from_millis(25));
        }
        Some(server)
    }

    /// The standard tenants' server over `root`.
    fn tenants(root: &Path, more: &[&str]) -> Server {
        let mut args = vec!["--tenant", IRIREF, "--tenant", URNID];
        args.extend_from_slice(more);
        Server::start(root, &args)
    }

    /// One request, written exactly as given.
    fn raw(&self, head: &str) -> Response {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .expect("read timeout");
        stream.write_all(head.as_bytes()).expect("send");
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).expect("receive");
        Response::parse(&raw)
    }

    fn get_with(&self, host: &str, target: &str, headers: &[(&str, &str)]) -> Response {
        let extra: String = headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}\r\n"))
            .collect();
        self.raw(&format!(
            "GET {target} HTTP/1.1\r\nHost: {host}\r\n{extra}Connection: close\r\n\r\n"
        ))
    }

    fn get(&self, host: &str, target: &str) -> Response {
        self.get_with(host, target, &[])
    }

    fn page(&self, host: &str, target: &str) -> Response {
        self.get_with(host, target, &[("Accept", "text/html")])
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Response {
    status: u16,
    head: String,
    body: String,
}

impl Response {
    fn parse(raw: &[u8]) -> Response {
        let text = String::from_utf8_lossy(raw);
        let (head, body) = text.split_once("\r\n\r\n").expect("a header block");
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .expect("a status line");
        Response {
            status,
            head: head.to_string(),
            body: body.to_string(),
        }
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.head
            .lines()
            .filter_map(|l| l.split_once(':'))
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim())
    }
}

/// Every absolute `http:` or `https:` IRI in `text`.
fn absolute_iris(text: &str) -> Vec<&str> {
    ["https://", "http://"]
        .iter()
        .flat_map(|scheme| text.match_indices(scheme))
        .map(|(at, _)| {
            let rest = &text[at..];
            let end = rest
                .find(|c: char| c.is_whitespace() || "<>\"'&".contains(c))
                .unwrap_or(rest.len());
            &rest[..end]
        })
        .collect()
}

/// One path, two hosts, two answers, in every face the server has.
#[test]
fn each_host_answers_from_its_own_data() {
    let root = two_tenants();
    let server = Server::tenants(&root, &[]);

    let turtle = server.get(URNID, "/resmud/core");
    assert_eq!(turtle.status, 200, "{}", turtle.body);
    assert!(turtle.body.contains("v:Identifier"), "{}", turtle.body);
    assert!(!turtle.body.contains(IRIREF), "{}", turtle.body);

    let page = server.page(IRIREF, "/resmud/core");
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(
        page.body.contains("<strong>Weapon</strong>"),
        "{}",
        page.body
    );
    assert!(!page.body.contains("Identifier"), "{}", page.body);

    assert!(server
        .page(URNID, "/resmud/core?term=Identifier")
        .body
        .contains("https://urnid.org/resmud/core#Identifier"));
    assert_eq!(
        server.page(IRIREF, "/resmud/core?term=Identifier").status,
        404,
        "a term of urnid.org is no term of iriref.org"
    );
}

/// The item's named test. With the file cache on, every face is read in turn on
/// both hosts, more than once, so each read could only be wrong by being served
/// the other host's cached answer.
#[test]
fn a_cached_page_is_never_served_to_another_host() {
    let root = two_tenants();
    let server = Server::tenants(&root, &["--cache-files"]);
    let faces = [
        "/resmud/core",
        "/resmud/core?as=text/html",
        "/resmud/core?as=text/html&theme=dark",
    ];
    for _ in 0..3 {
        for face in faces {
            for (host, own, other) in [
                (IRIREF, "Weapon", "Identifier"),
                (URNID, "Identifier", "Weapon"),
            ] {
                let got = server.get(host, face);
                assert_eq!(got.status, 200, "{host} {face}: {}", got.body);
                assert!(got.body.contains(own), "{host} {face}: {}", got.body);
                assert!(!got.body.contains(other), "{host} {face}: {}", got.body);
            }
        }
    }
    // The fragment too: the same arguments on each host, the second a would-be
    // cache hit, and a term of the other host stays absent after it was served.
    for _ in 0..2 {
        let own = server.page(URNID, "/resmud/core?term=Identifier");
        assert_eq!(own.status, 200, "{}", own.body);
        assert_eq!(
            server.page(IRIREF, "/resmud/core?term=Identifier").status,
            404
        );
    }
}

/// An HTTP cache's validator must not cross hosts either: what iriref.org called
/// current is not a `304` from urnid.org.
#[test]
fn a_validator_from_one_host_does_not_revalidate_on_another() {
    let root = two_tenants();
    let server = Server::tenants(&root, &["--cache-files"]);
    let first = server.get(IRIREF, "/resmud/core");
    let etag = first.header("etag").expect("a validator").to_string();
    let same = server.get_with(IRIREF, "/resmud/core", &[("If-None-Match", &etag)]);
    assert_eq!(same.status, 304, "{}", same.head);
    let other = server.get_with(URNID, "/resmud/core", &[("If-None-Match", &etag)]);
    assert_eq!(other.status, 200, "{}", other.head);
    assert!(other.body.contains("v:Identifier"), "{}", other.body);
}

/// A host this server is not, on every answer the door gives before dispatch.
#[test]
fn a_host_that_is_not_a_tenant_is_refused_before_anything_answers() {
    let root = two_tenants();
    let server = Server::tenants(&root, &[]);
    for target in [
        "/resmud/core",
        "/resmud/core?description",
        "/-/style",
        "/-/htmx",
    ] {
        let got = server.get("evil.example", target);
        assert_eq!(got.status, 421, "{target}: {}", got.body);
        assert!(
            got.body.contains("evil.example") && !got.body.contains("Weapon"),
            "{target}: {}",
            got.body
        );
    }
    let options = server
        .raw("OPTIONS /resmud/core HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n\r\n");
    assert_eq!(options.status, 421, "{}", options.head);
    assert_eq!(server.get("127.0.0.1", "/resmud/core").status, 421);
    assert_eq!(server.get("[::1]:8080", "/resmud/core").status, 400);
    assert_eq!(server.get("../urnid.org", "/resmud/core").status, 400);

    let two = server.raw(
        "GET /resmud/core HTTP/1.1\r\nHost: iriref.org\r\nHost: urnid.org\r\n\
         Connection: close\r\n\r\n",
    );
    assert_eq!(two.status, 400, "two hosts name no tenant: {}", two.body);
}

/// One tenant however its host is spelled, and only `Host` picks it.
#[test]
fn only_the_host_header_picks_the_tenant_however_it_is_spelled() {
    let root = two_tenants();
    let server = Server::tenants(&root, &[]);
    for spelling in ["URNID.org", "urnid.org:8443", "urnid.org."] {
        let got = server.get(spelling, "/resmud/core");
        assert_eq!(got.status, 200, "{spelling}: {}", got.body);
        assert!(
            got.body.contains("v:Identifier"),
            "{spelling}: {}",
            got.body
        );
    }
    for (name, value) in [
        ("X-Forwarded-Host", URNID),
        ("Forwarded", "host=urnid.org"),
        ("X-Host", URNID),
    ] {
        let got = server.get_with(IRIREF, "/resmud/core", &[(name, value)]);
        assert!(got.body.contains("v:Weapon"), "{name}: {}", got.body);
    }
    for query in ["tenant=urnid.org", "host=urnid.org"] {
        let got = server.get(IRIREF, &format!("/resmud/core?{query}"));
        assert!(got.body.contains("v:Weapon"), "{query}: {}", got.body);
    }
}

/// A registry cannot reach out of its tenant: not by climbing out of the jail,
/// and not by a link planted in its own directory.
#[test]
fn a_tenant_cannot_read_another_tenants_files() {
    let root = two_tenants();
    std::fs::write(
        root.join(URNID).join("private.ttl"),
        "<urn:secret> a <urn:x> .",
    )
    .expect("a file");
    write_registry(
        &root,
        IRIREF,
        &[
            hosted("resmud", "urn:file:vocab/core.ttl"),
            hosted("climb", "urn:file:../urnid.org/private.ttl"),
            hosted("linked", "urn:file:linked.ttl"),
        ],
        "",
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        root.join(URNID).join("private.ttl"),
        root.join(IRIREF).join("linked.ttl"),
    )
    .expect("a link");

    let server = Server::tenants(&root, &[]);
    assert_eq!(server.get(IRIREF, "/resmud/core").status, 200);
    for path in ["/climb", "/linked"] {
        let got = server.get(IRIREF, path);
        assert_ne!(got.status, 200, "{path}: {}", got.body);
        assert!(!got.body.contains("urn:secret"), "{path}: {}", got.body);
    }
}

/// Separable from day one: a tenant's directory is already a whole deployment.
/// Serving it alone, with nothing in it rewritten, answers the same bytes, so a
/// later split onto its own host is a copy and a DNS change.
#[test]
fn a_tenants_directory_splits_off_unchanged() {
    let root = two_tenants();
    let shared = Server::tenants(&root, &[]);
    let alone = Server::start(&root.join(URNID), &[]);
    for target in [
        "/resmud/core",
        "/resmud/core?as=text/html",
        "/resmud/core?as=text/html&term=Identifier",
    ] {
        let together = shared.get(URNID, target);
        let split = alone.get(URNID, target);
        assert_eq!(together.status, 200, "{target}: {}", together.body);
        assert_eq!(together.body, split.body, "{target}");
        assert_eq!(together.header("etag"), split.header("etag"), "{target}");
    }
}

/// Nothing this server generates names another host for itself.
#[test]
fn a_generated_document_names_only_the_requesting_host() {
    let root = two_tenants();
    let server = Server::tenants(&root, &[]);
    for target in [
        "/resmud/core",
        "/resmud/core?as=text/html",
        "/resmud/core?as=text/html&term=Identifier&view=page",
        "/resmud/core?as=text/html&term=Identifier",
        "/resmud/core?description",
    ] {
        let got = server.get(URNID, target);
        assert_eq!(got.status, 200, "{target}: {}", got.body);
        assert!(!got.body.contains(IRIREF), "{target}: {}", got.body);
        for iri in absolute_iris(&got.body) {
            let allowed = iri.starts_with("https://urnid.org/")
                || iri.starts_with("http://www.w3.org/")
                // The OpenAPI face's own schema reference, not an IRI of a term.
                || iri.starts_with("https://spec.openapis.org/");
            assert!(allowed, "{target} names {iri}: {}", got.body);
        }
    }
}

/// A vocabulary on urnid.org that declares a term of iriref.org, and subclasses
/// another. The first is not documented as urnid.org's: it is listed as defined
/// elsewhere, and the reference keeps its whole IRI.
#[test]
fn a_term_lives_in_one_tenant_and_is_referenced_from_the_others() {
    let root = two_tenants();
    std::fs::write(
        root.join(URNID).join("vocab/core.ttl"),
        "@prefix v: <https://urnid.org/resmud/core#> .\n\
         @prefix other: <https://iriref.org/resmud/core#> .\n\
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
         v:Identifier a rdfs:Class ; rdfs:subClassOf other:Weapon .\n\
         other:Weapon a rdfs:Class .\n",
    )
    .expect("vocabulary");
    let server = Server::tenants(&root, &[]);

    let page = server.page(URNID, "/resmud/core");
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(page.body.contains("1 term ·"), "{}", page.body);
    assert!(
        page.body.contains("<strong>Identifier</strong>"),
        "{}",
        page.body
    );
    assert!(
        !page.body.contains("<strong>Weapon</strong>"),
        "{}",
        page.body
    );
    assert!(
        page.body.contains(
            "<a href=\"https://iriref.org/resmud/core#Weapon\">\
             https://iriref.org/resmud/core#Weapon</a>"
        ),
        "links to where it lives: {}",
        page.body
    );
    assert!(
        page.body.contains("aria-labelledby=\"references-heading\""),
        "the list is labeled: {}",
        page.body
    );

    let refused = server.page(URNID, "/resmud/core?term=Weapon");
    assert_eq!(refused.status, 404, "{}", refused.body);
    assert!(
        refused
            .body
            .contains("https://iriref.org/resmud/core#Weapon"),
        "says where it lives: {}",
        refused.body
    );

    let detail = server.page(URNID, "/resmud/core?term=Identifier");
    assert!(
        detail
            .body
            .contains("<code>https://iriref.org/resmud/core#Weapon</code>"),
        "the reference is not shortened to read as this host's: {}",
        detail.body
    );

    // The term's own host is unaffected by the other tenant referring to it.
    assert_eq!(server.page(IRIREF, "/resmud/core?term=Weapon").status, 200);
}

/// A vocabulary cannot put a `javascript:` link on the page by declaring a term.
#[test]
fn only_a_web_iri_is_a_link_in_the_references() {
    let root = two_tenants();
    std::fs::write(
        root.join(URNID).join("vocab/core.ttl"),
        "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
         <javascript:alert(1)> a rdfs:Class .\n",
    )
    .expect("vocabulary");
    let server = Server::tenants(&root, &[]);
    let page = server.page(URNID, "/resmud/core");
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(
        page.body.contains("<code>javascript:alert(1)</code>"),
        "{}",
        page.body
    );
    assert!(!page.body.contains("href=\"javascript"), "{}", page.body);
}

/// Config is per tenant: claims, strategies and ceilings all come from the one
/// registry that host names.
#[test]
fn registry_config_and_redirects_are_per_tenant() {
    let root = two_tenants();
    write_registry(
        &root,
        URNID,
        &[
            hosted("resmud", "urn:file:vocab/core.ttl"),
            r#"{"prefix":"acme","owner":"urn:cap:name:admin:acme","strategy":"redirect","target":"https://acme.example/ns"}"#
                .to_string(),
        ],
        r#","limits":{"max_document_bytes":10}"#,
    );
    let server = Server::tenants(&root, &[]);

    // The redirect exists on the tenant that claimed it, and nowhere else.
    let there = server.get(URNID, "/acme/thing");
    assert_eq!(
        there.status, 500,
        "redirects are a known 500: {}",
        there.body
    );
    assert_eq!(server.get(IRIREF, "/acme/thing").status, 404);

    // urnid.org's ceiling refuses its own document and leaves iriref.org's alone.
    let capped = server.get(URNID, "/resmud/core");
    assert_ne!(capped.status, 200, "{}", capped.body);
    assert_eq!(server.get(IRIREF, "/resmud/core").status, 200);
}

/// An edit to one tenant's registry changes that tenant alone.
#[test]
fn claiming_a_prefix_on_one_tenant_leaves_it_free_on_another() {
    let root = two_tenants();
    write_registry(
        &root,
        URNID,
        &[
            hosted("resmud", "urn:file:vocab/core.ttl"),
            hosted("only-here", "urn:file:vocab/core.ttl"),
        ],
        "",
    );
    let server = Server::tenants(&root, &[]);
    assert_eq!(server.get(URNID, "/only-here").status, 200);
    assert_eq!(server.get(IRIREF, "/only-here").status, 404);
}

/// With no `--tenant` nothing keys on the host, as before.
#[test]
fn with_no_tenant_named_any_host_reaches_the_one_root() {
    let root = two_tenants();
    let server = Server::start(&root.join(URNID), &[]);
    for host in [URNID, IRIREF, "anything.example"] {
        let got = server.get(host, "/resmud/core");
        assert_eq!(got.status, 200, "{host}: {}", got.body);
        assert!(got.body.contains("v:Identifier"), "{host}: {}", got.body);
    }
}

/// A server that cannot keep its tenants apart does not start.
#[test]
fn a_bad_tenant_configuration_stops_the_server() {
    let root = two_tenants();
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join(URNID), root.join("idref.org")).expect("a link");
    let mut cases: Vec<(Vec<&str>, &str)> = vec![
        (vec!["--tenant", "../urnid.org"], "not a lowercase host"),
        (vec!["--tenant", "URNID.org"], "not a lowercase host"),
        (vec!["--tenant", "urnid.org:80"], "not a lowercase host"),
        (vec!["--tenant", "missing.example"], "missing.example"),
        (vec!["--tenant", URNID, "--tenant", URNID], "named twice"),
    ];
    #[cfg(unix)]
    cases.push((
        vec!["--tenant", URNID, "--tenant", "idref.org"],
        "share files",
    ));
    for (args, says) in cases {
        let out = Command::new(env!("CARGO_BIN_EXE_ikigai-name-server"))
            .arg("--listen")
            .arg("127.0.0.1:0")
            .arg("--root")
            .arg(&*root)
            .args(&args)
            .output()
            .expect("run ikigai-name-server");
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(says), "{args:?}: {stderr}");
        assert!(
            stderr.starts_with("ikigai-name-server: "),
            "{args:?}: {stderr}"
        );
    }
}
