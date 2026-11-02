//! The standalone server's HTTP face, through the real binary.
//!
//! The faces a client sees are decided as much by `ikigai-web` as by this
//! crate: content negotiation, `?as=`, `?description` and the status a refusal
//! maps to are all the transport's. So a bump of that dependency is a change to
//! this server's public behavior even when not a line here moves, and the
//! library's tests, which call the kernel, cannot see it. These tests spawn the
//! built `ikigai-name-server` against a scratch root and speak HTTP to it.
//!
//! What they pin (ledger #196, #248, #194, #2737):
//!
//! - **Turtle by default and HTML by `Accept`**, at the namespace's own IRI path.
//! - **A face this host cannot produce is a `406` that names the faces it can.**
//!   The server binds no transreptor, so its faces are exactly the two the
//!   document declares. Under `ikigai-web` 0.1.16 an `Accept` for JSON-LD reached
//!   the endpoint and came back a `500`; from 0.1.22 the transport negotiates
//!   against the declared faces first, which is the truthful answer here.
//! - **`?as=` is honored** (0.1.16 ignored it and answered Turtle).
//! - **`?description` describes the resource** rather than answering `404`,
//!   because the template entry declares `path` as its binding (`doc()`), so the
//!   manifold can drive it.
//! - **Every URL the documentation page names is served** (ledger #2737): its
//!   script, its stylesheet, each term's link and htmx fetch, and its theme form.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const VOCAB: &str = "@prefix rm: <https://iriref.org/resmud/core#> .\n\
                     @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
                     rm:Weapon a rdfs:Class ; rdfs:label \"Weapon\" .\n";

const REGISTRY: &str = r#"{"namespaces":[
  {"prefix":"resmud","owner":"urn:cap:name:admin:resmud","strategy":"hosted","source":"urn:file:vocab/core.ttl"}
]}"#;

/// A scratch root no other server in this process shares.
fn scratch_root() -> PathBuf {
    // Nanos alone are not unique: the clock ticks coarser than its unit
    // (microseconds on macOS), so two tests starting together read the same
    // value. The counter is what disambiguates (ledger #163).
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "ikigai-name-http-{}-{nanos}-{n}",
        std::process::id()
    ))
}

/// A running server over a scratch root, killed and cleaned up on drop.
struct Server {
    child: Child,
    addr: SocketAddr,
    root: PathBuf,
}

impl Server {
    fn start() -> Server {
        let root = scratch_root();
        std::fs::create_dir_all(root.join("vocab")).expect("scratch root");
        std::fs::write(root.join("vocab/core.ttl"), VOCAB).expect("vocabulary");
        std::fs::write(root.join("registry.json"), REGISTRY).expect("registry");

        // An ephemeral port, released for the server to take. Another process
        // could take it in between; the server would then fail to bind and
        // exit, which the check after the readiness wait reports.
        let addr = TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("an ephemeral port");

        let child = Command::new(env!("CARGO_BIN_EXE_ikigai-name-server"))
            .arg("--listen")
            .arg(addr.to_string())
            .arg("--root")
            .arg(&root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ikigai-name-server");
        let mut server = Server { child, addr, root };

        let deadline = Instant::now() + Duration::from_secs(20);
        while TcpStream::connect(server.addr).is_err() {
            if let Some(status) = server.child.try_wait().expect("poll the server") {
                panic!("ikigai-name-server exited before listening: {status}");
            }
            assert!(Instant::now() < deadline, "server never listened");
            std::thread::sleep(Duration::from_millis(25));
        }
        if let Some(status) = server.child.try_wait().expect("poll the server") {
            panic!(
                "ikigai-name-server exited (port {} taken?): {status}",
                server.addr
            );
        }
        server
    }

    /// One `GET`, `Connection: close`, read to EOF.
    fn get(&self, target: &str, accept: Option<&str>) -> Response {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .expect("read timeout");
        let mut request = format!("GET {target} HTTP/1.1\r\nHost: {}\r\n", self.addr);
        if let Some(accept) = accept {
            request.push_str(&format!("Accept: {accept}\r\n"));
        }
        request.push_str("Connection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).expect("send");
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).expect("receive");
        Response::parse(&raw)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        // `wait`, not `try_wait`: reap it, so no zombie outlives the test.
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct Response {
    status: u16,
    content_type: String,
    body: String,
}

impl Response {
    fn parse(raw: &[u8]) -> Response {
        let text = String::from_utf8_lossy(raw);
        let (head, body) = text.split_once("\r\n\r\n").expect("a header block");
        let mut lines = head.lines();
        let status = lines
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .expect("a status line");
        let content_type = lines
            .filter_map(|l| l.split_once(':'))
            .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.trim().to_ascii_lowercase())
            .unwrap_or_default();
        Response {
            status,
            content_type,
            body: body.to_string(),
        }
    }

    fn media(&self) -> &str {
        self.content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
    }
}

#[test]
fn the_http_face_negotiates_over_the_declared_faces() {
    let server = Server::start();

    // No preference: the hub representation, as stored.
    let plain = server.get("/resmud/core", None);
    assert_eq!(plain.status, 200, "{}", plain.body);
    assert_eq!(plain.media(), "text/turtle");
    assert!(plain.body.contains("rm:Weapon"), "{}", plain.body);

    // A browser's wildcard tolerates the default rather than asking for HTML.
    let anything = server.get("/resmud/core", Some("*/*"));
    assert_eq!(anything.status, 200);
    assert_eq!(anything.media(), "text/turtle");

    // HTML by `Accept`, and by an explicit `?as=`, which 0.1.16 ignored.
    let page = server.get("/resmud/core", Some("text/html"));
    assert_eq!(page.status, 200, "{}", page.body);
    assert_eq!(page.media(), "text/html");
    assert!(page.body.contains("Weapon"), "{}", page.body);
    let asked = server.get("/resmud/core?as=text/html", None);
    assert_eq!(asked.status, 200, "{}", asked.body);
    assert_eq!(asked.media(), "text/html");

    // A face this host cannot produce: no transreptor is bound here, so JSON-LD
    // is refused as not acceptable, naming what is served (it was a 500 that
    // leaked the endpoint's error before ikigai-web 0.1.22).
    for (target, accept) in [
        ("/resmud/core", Some("application/ld+json")),
        ("/resmud/core?as=application/ld%2Bjson", None),
    ] {
        let refused = server.get(target, accept);
        assert_eq!(refused.status, 406, "{target} {accept:?}: {}", refused.body);
        assert!(
            refused.body.contains("text/turtle") && refused.body.contains("text/html"),
            "names the faces served: {}",
            refused.body
        );
    }

    // An unclaimed namespace is not found, not an error.
    assert_eq!(server.get("/nobody", None).status, 404);
}

/// Every value the page gives `attr` (`href`, `src`, `hx-get`), unescaped, in
/// document order.
fn attribute_values(html: &str, attr: &str) -> Vec<String> {
    let needle = format!(" {attr}=\"");
    html.match_indices(&needle)
        .map(|(at, _)| {
            let rest = &html[at + needle.len()..];
            let value = &rest[..rest.find('"').expect("a closed attribute")];
            value.replace("&amp;", "&")
        })
        .collect()
}

/// A URL the page names, resolved against the page's own path the way a
/// browser resolves it: a query-only reference keeps the path, an absolute
/// path stands, and the fragment never reaches the server.
fn resolve(page_path: &str, reference: &str) -> String {
    let reference = reference.split('#').next().unwrap_or_default();
    if reference.starts_with('?') {
        format!("{page_path}{reference}")
    } else if reference.starts_with('/') {
        reference.to_string()
    } else {
        panic!("the page names {reference:?}, which this test does not resolve")
    }
}

/// Ledger #2737: the documentation page named `/static/htmx.min.js` and
/// `/r/urn:name:docs?…` for its script, its term buttons and its theme chooser,
/// and this server routes neither, so on it a term's detail was unreachable.
/// Every URL the page names must answer here, as what the page uses it for.
#[test]
fn every_url_the_page_names_is_served() {
    let server = Server::start();
    let page_path = "/resmud/core";
    let page = server.get(page_path, Some("text/html"));
    assert_eq!(page.status, 200, "{}", page.body);
    assert_eq!(page.media(), "text/html");

    // The script and the stylesheet: served, and as the type the browser
    // needs (a script served `text/plain` is refused under `nosniff`).
    let scripts = attribute_values(&page.body, "src");
    assert!(!scripts.is_empty(), "the page loads htmx: {}", page.body);
    for src in &scripts {
        let got = server.get(&resolve(page_path, src), None);
        assert_eq!(got.status, 200, "script {src}: {}", got.body);
        assert_eq!(got.media(), "text/javascript", "script {src}");
    }
    let hrefs = attribute_values(&page.body, "href");
    let (sheets, links): (Vec<&String>, Vec<&String>) =
        hrefs.iter().partition(|h| h.starts_with("/-/style"));
    assert_eq!(sheets.len(), 1, "one stylesheet: {}", page.body);
    let sheet = server.get(&resolve(page_path, sheets[0]), None);
    assert_eq!(
        sheet.status, 200,
        "stylesheet {}: {}",
        sheets[0], sheet.body
    );
    assert_eq!(sheet.media(), "text/css");
    assert!(sheet.body.contains("--ground"), "{}", sheet.body);
    // The door's default CSP refuses inline style, so the palette must be the
    // linked file, never a `<style>` the browser drops.
    assert!(!page.body.contains("<style"), "{}", page.body);

    // A term, followed as a plain link (a browser navigating, no JavaScript):
    // the page, with the term's detail in the region.
    assert!(!links.is_empty(), "every term is a link: {}", page.body);
    for href in &links {
        let got = server.get(&resolve(page_path, href), Some("text/html"));
        assert_eq!(got.status, 200, "link {href}: {}", got.body);
        assert_eq!(got.media(), "text/html", "link {href}");
        assert!(
            got.body.contains("<h2>Weapon</h2>"),
            "link {href} shows the term: {}",
            got.body
        );
    }

    // The same term, fetched as htmx fetches it: no `Accept` for HTML, so the
    // URL itself has to ask for the HTML face.
    let fetches = attribute_values(&page.body, "hx-get");
    assert!(
        !fetches.is_empty(),
        "htmx enhances the links: {}",
        page.body
    );
    for url in &fetches {
        let got = server.get(&resolve(page_path, url), None);
        assert_eq!(got.status, 200, "hx-get {url}: {}", got.body);
        assert_eq!(got.media(), "text/html", "hx-get {url}");
        assert!(got.body.contains("<h2>Weapon</h2>"), "{}", got.body);
    }

    // The theme chooser: a GET form with no action submits to the page's own
    // path with its fields as the query.
    assert!(
        page.body
            .contains("<form class=\"chooser\" method=\"get\">"),
        "{}",
        page.body
    );
    let applied = server.get(
        &format!("{page_path}?as=text/html&prefix=resmud&theme=dark"),
        None,
    );
    assert_eq!(applied.status, 200, "{}", applied.body);
    assert!(
        applied.body.contains("href=\"/-/style?theme=dark\""),
        "the chosen theme reaches the stylesheet: {}",
        applied.body
    );
}

#[test]
fn the_template_entry_describes_itself() {
    let server = Server::start();
    let described = server.get("/resmud/core?description", None);
    assert_eq!(described.status, 200, "{}", described.body);
    assert!(
        described.media().contains("openapi"),
        "an API description: {}",
        described.content_type
    );
    assert!(
        described.body.contains("negotiate a namespace document"),
        "the document's Source action is offered: {}",
        described.body
    );
}

#[test]
fn two_servers_started_together_never_share_a_root() {
    // Each server's `Drop` removes its root, so two that shared one would
    // delete each other's registry mid-test. Two starts in one clock tick is
    // the case: this loop is the tight version of two tests starting at once.
    let roots: Vec<PathBuf> = (0..1000).map(|_| scratch_root()).collect();
    let distinct: std::collections::HashSet<&PathBuf> = roots.iter().collect();
    assert_eq!(distinct.len(), roots.len(), "scratch roots collided");
}
