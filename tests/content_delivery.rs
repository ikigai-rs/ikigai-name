//! Opt-in verified delivery of content names from a trustless gateway, over canned gateway
//! answers through the real `urn:http:get` (no live network).
//!
//! `fixtures/dir-with-files.car` is the IPFS gateway-conformance fixture
//! `fixtures/path_gateway_unixfs/dir-with-files.car` at tag v0.8.1 (MIT OR Apache-2.0),
//! SHA-256 `52ba43df5a78d92b9ca006832e8425085c00b4e268b16cf049e54ba9dbd1b0db`. It holds the
//! UnixFS `multiblock.txt` the UnixFS specification's file test vectors describe: a dag-pb
//! root of 245 bytes over five raw leaves, 1,026 bytes in all.

use async_trait::async_trait;
use cid::multihash::Multihash;
use cid::Cid;
use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Clock, Error, Exact, Fallback, FnEndpoint, Invocation, Iri, Kernel,
    ReprType, Representation, Request, Result, Time, Verb,
};
use ikigai_http::{HttpBody, HttpRequest, HttpResponse, HttpTransport};
use ikigai_name::delivery::DELIVERY_SOURCE_IRI;
use ikigai_name::REGISTRY_IRI;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const FIXTURE: &[u8] = include_bytes!("fixtures/dir-with-files.car");

/// multiblock.txt, and its five leaves in order.
const ROOT: &str = "bafybeigcisqd7m5nf3qmuvjdbakl5bdnh4ocrmacaqkpuh77qjvggmt2sa";
const LEAVES: [&str; 5] = [
    "bafkreie5noke3mb7hqxukzcy73nl23k6lxszxi5w3dtmuwz62wnvkpsscm",
    "bafkreih4ephajybraj6wnxsbwjwa77fukurtpl7oj7t7pfq545duhot7cq",
    "bafkreigu7buvm3cfunb35766dn7tmqyh2um62zcio63en2btvxuybgcpue",
    "bafkreicll3huefkc3qnrzeony7zcfo7cr3nbx64hnxrqzsixpceg332fhe",
    "bafkreifst3pqztuvj57lycamoi7z34b4emf7gawxs74nwrc2c7jncmpaqm",
];
/// The fixture's directory, which holds multiblock.txt.
const DIRECTORY: &str = "bafybeihchr7vmgjaasntayyatmp5sv6xza57iy2h4xj7g46bpjij6yhrmy";
/// The raw block `hello application/vnd.ipld.car` and a newline (31 bytes), in the same fixture.
const ASCII: &str = "bafkreifkam6ns4aoolg3wedr4uzrs3kvq66p4pecirz6y2vlrngla62mxm";

const GW1: &str = "https://gw1.example";
const GW2: &str = "https://gw2.example";

// --- building gateway answers ---------------------------------------------------

fn uvarint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn read_uvarint(bytes: &[u8], at: &mut usize) -> u64 {
    let (mut value, mut shift) = (0u64, 0);
    loop {
        let byte = bytes[*at];
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            return value;
        }
    }
}

/// The fixture's blocks by CID, read independently of the code under test.
fn fixture_blocks() -> BTreeMap<String, Vec<u8>> {
    let mut at = 0;
    let header = read_uvarint(FIXTURE, &mut at) as usize;
    at += header;
    let mut blocks = BTreeMap::new();
    while at < FIXTURE.len() {
        let len = read_uvarint(FIXTURE, &mut at) as usize;
        let mut section = &FIXTURE[at..at + len];
        at += len;
        let cid = Cid::read_bytes(&mut section).unwrap();
        blocks.insert(cid.to_string(), section.to_vec());
    }
    blocks
}

fn block(cid: &str) -> Vec<u8> {
    fixture_blocks().remove(cid).expect("in the fixture")
}

/// A CARv1 naming `root`, holding `blocks` in order.
fn car(root: &Cid, blocks: &[(Cid, Vec<u8>)]) -> Vec<u8> {
    let cid = {
        let mut b = vec![0u8];
        b.extend(root.to_bytes());
        b
    };
    let mut header = vec![0xa2, 0x65];
    header.extend(b"roots");
    header.extend([0x81, 0xd8, 0x2a, 0x58, cid.len() as u8]);
    header.extend(&cid);
    header.push(0x67);
    header.extend(b"version");
    header.push(0x01);
    let mut out = Vec::new();
    uvarint(header.len() as u64, &mut out);
    out.extend(header);
    for (cid, data) in blocks {
        let cid = cid.to_bytes();
        uvarint((cid.len() + data.len()) as u64, &mut out);
        out.extend(cid);
        out.extend(data);
    }
    out
}

fn parse(cid: &str) -> Cid {
    Cid::try_from(cid).unwrap()
}

/// multiblock.txt as a trustless gateway answers it: the root, then the leaves, in order.
fn multiblock_car() -> Vec<u8> {
    let mut blocks = vec![(parse(ROOT), block(ROOT))];
    blocks.extend(LEAVES.iter().map(|l| (parse(l), block(l))));
    car(&parse(ROOT), &blocks)
}

fn multiblock_bytes() -> Vec<u8> {
    LEAVES.iter().flat_map(|l| block(l)).collect()
}

fn sha256_cid(codec: u64, bytes: &[u8]) -> Cid {
    Cid::new_v1(
        codec,
        Multihash::wrap(0x12, &Sha256::digest(bytes)).unwrap(),
    )
}

fn field_bytes(number: u64, value: &[u8], out: &mut Vec<u8>) {
    uvarint(number << 3 | 2, out);
    uvarint(value.len() as u64, out);
    out.extend(value);
}

fn field_varint(number: u64, value: u64, out: &mut Vec<u8>) {
    uvarint(number << 3, out);
    uvarint(value, out);
}

/// A dag-pb UnixFS file node over `children`, each with the size of its content.
fn file_node(children: &[(Cid, u64)]) -> Vec<u8> {
    let mut unixfs = Vec::new();
    field_varint(1, 2, &mut unixfs);
    field_varint(3, children.iter().map(|(_, s)| s).sum(), &mut unixfs);
    for (_, size) in children {
        field_varint(4, *size, &mut unixfs);
    }
    let mut node = Vec::new();
    for (cid, _) in children {
        let mut link = Vec::new();
        field_bytes(1, &cid.to_bytes(), &mut link);
        field_varint(3, 0, &mut link);
        field_bytes(2, &link, &mut node);
    }
    field_bytes(1, &unixfs, &mut node);
    node
}

/// A chain of `links` file nodes above multiblock.txt: its root, then its blocks.
fn deep_chain(links: usize) -> (Cid, Vec<(Cid, Vec<u8>)>) {
    let mut blocks: Vec<(Cid, Vec<u8>)> = Vec::new();
    let mut top = parse(ROOT);
    for _ in 0..links {
        let node = file_node(&[(top, 1026)]);
        top = sha256_cid(0x70, &node);
        blocks.push((top, node));
    }
    blocks.reverse();
    blocks.push((parse(ROOT), block(ROOT)));
    blocks.extend(LEAVES.iter().map(|l| (parse(l), block(l))));
    (top, blocks)
}

// --- the world ------------------------------------------------------------------

/// A clock that only the gateway moves.
#[derive(Default)]
struct Manual(AtomicU64);

impl Clock for Manual {
    fn now(&self) -> Time {
        Time::from_millis(self.0.load(Ordering::SeqCst))
    }
}

/// One canned answer: its status, headers and body.
type Canned = (u16, Vec<(String, String)>, Vec<u8>);

/// A canned gateway: an answer per URL, a delay per host, and what it was sent.
#[derive(Default)]
struct Gateway {
    answers: Mutex<BTreeMap<String, Canned>>,
    delays: Mutex<BTreeMap<String, u64>>,
    sent: Mutex<Vec<HttpRequest>>,
    clock: Arc<Manual>,
}

impl Gateway {
    fn answer(&self, url: &str, status: u16, body: Vec<u8>) {
        self.answer_with(url, status, &[], body);
    }
    fn answer_with(&self, url: &str, status: u16, headers: &[(&str, &str)], body: Vec<u8>) {
        let headers = headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        self.answers
            .lock()
            .unwrap()
            .insert(url.to_string(), (status, headers, body));
    }
    fn urls(&self) -> Vec<String> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.url.clone())
            .collect()
    }
}

#[async_trait]
impl HttpTransport for Gateway {
    async fn send(&self, request: HttpRequest) -> std::result::Result<HttpResponse, String> {
        let host = request
            .url
            .split('/')
            .nth(2)
            .unwrap_or_default()
            .to_string();
        if let Some(ms) = self.delays.lock().unwrap().get(&host) {
            self.clock.0.fetch_add(*ms, Ordering::SeqCst);
        }
        let (status, headers, body) = self
            .answers
            .lock()
            .unwrap()
            .get(&request.url)
            .cloned()
            .unwrap_or((404, Vec::new(), b"not found".to_vec()));
        self.sent.lock().unwrap().push(request);
        Ok(HttpResponse {
            status,
            headers,
            body: HttpBody::from(body),
        })
    }
}

fn car_url(gateway: &str, cid: &str) -> String {
    format!("{gateway}/ipfs/{cid}?format=car&dag-scope=all")
}

fn raw_url(gateway: &str, cid: &str) -> String {
    format!("{gateway}/ipfs/{cid}?format=raw")
}

fn registry(entries: &[(&str, &str, Option<&str>)], public: bool) -> String {
    let entries: Vec<String> = entries
        .iter()
        .map(|(name, cid, media)| match media {
            Some(media) => format!(r#""{name}": {{"cid": "{cid}", "media-type": "{media}"}}"#),
            None => format!(r#""{name}": {{"cid": "{cid}"}}"#),
        })
        .collect();
    format!(
        r#"{{"gateway": "https://hint.example/", "namespaces": [
          {{"prefix": "files", "owner": "urn:cap:name:admin:files", "strategy": "content",
            "public": {public}, "entries": {{ {} }} }}]}}"#,
        entries.join(", ")
    )
}

fn default_registry() -> String {
    registry(
        &[
            ("files/multiblock.txt", ROOT, Some("text/plain")),
            ("files/ascii.txt", ASCII, Some("text/plain; charset=utf-8")),
            ("files/page.html", ASCII, Some("text/html")),
            ("files/drawing.svg", ROOT, Some("image/svg+xml")),
            ("files/untyped", ASCII, None),
            ("files/folder", DIRECTORY, None),
        ],
        true,
    )
}

fn config(extra: &str) -> String {
    format!(r#"{{"gateways": ["{GW1}", "{GW2}"]{extra}}}"#)
}

fn json_source(name: &'static str, json: String) -> FnEndpoint {
    FnEndpoint::new(name, move |_: &Invocation<'_>| {
        Ok(Representation::new(
            ReprType::new("application/json"),
            json.as_bytes().to_vec(),
        ))
    })
}

fn world_with(registry: String, delivery: Option<String>) -> (Kernel, Arc<Gateway>) {
    let gateway = Arc::new(Gateway::default());
    let mut space = ikigai_name::space().bind(
        Exact::new(REGISTRY_IRI),
        json_source("registry-source", registry),
    );
    if let Some(delivery) = delivery {
        space = space.bind(
            Exact::new(DELIVERY_SOURCE_IRI),
            json_source("delivery-source", delivery),
        );
    }
    let kernel = Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(space) as Arc<dyn ikigai_core::Space>,
        Arc::new(ikigai_http::space(gateway.clone())),
    ])))
    .with_clock(gateway.clock.clone());
    (kernel, gateway)
}

fn world(delivery: &str) -> (Kernel, Arc<Gateway>) {
    world_with(default_registry(), Some(delivery.to_string()))
}

/// Both gateways granted, and nothing else on the network.
fn granted() -> Capability {
    Capability::scoped(["urn:cap:net:gw1.example", "urn:cap:net:gw2.example"])
}

fn document(kernel: &Kernel, path: &str, cap: &Capability) -> Result<Representation> {
    let request = Request::new(Verb::Source, Iri::parse("urn:name:document").unwrap())
        .with_arg("path", ArgRef::Inline(path.as_bytes().to_vec()));
    block_on(kernel.issue(request, cap))
}

/// An entity envelope's header block and content.
fn entity(repr: &Representation) -> (String, Vec<u8>) {
    assert_eq!(
        repr.repr_type.to_string(),
        "application/vnd.ikigai.http-entity"
    );
    let split = repr
        .bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("a header block");
    (
        String::from_utf8(repr.bytes[..split].to_vec()).unwrap(),
        repr.bytes[split + 4..].to_vec(),
    )
}

fn integrity(err: &Error) -> bool {
    matches!(err, Error::Endpoint(m) if m.contains("do not verify"))
}

// --- the fixture ------------------------------------------------------------------

#[test]
fn the_fixture_is_the_one_the_design_names() {
    assert_eq!(
        hex::encode(Sha256::digest(FIXTURE)),
        "52ba43df5a78d92b9ca006832e8425085c00b4e268b16cf049e54ba9dbd1b0db"
    );
    let root = block(ROOT);
    assert_eq!(root.len(), 245);
    assert!(hex::encode(Sha256::digest(&root)).starts_with("c244a03f"));
    let bytes = multiblock_bytes();
    assert_eq!(bytes.len(), 1026);
    assert!(hex::encode(Sha256::digest(&bytes)).starts_with("998785f1"));
}

#[test]
fn multiblock_txt_reconstructs_from_a_verified_car() {
    let (kernel, gateway) = world(&config(""));
    gateway.answer(&car_url(GW1, ROOT), 200, multiblock_car());
    let served = document(&kernel, "files/multiblock.txt", &granted()).expect("delivered");
    let (head, bytes) = entity(&served);
    assert_eq!(bytes.len(), 1026);
    assert_eq!(bytes, multiblock_bytes());
    assert!(bytes.starts_with(b"Lorem ipsum"));
    assert!(head.contains("Content-Type: text/plain"), "{head}");
    assert!(
        head.contains(&format!(
            "Content-Disposition: attachment; filename=\"{ROOT}\""
        )),
        "{head}"
    );
    // One request, for a CARv1 of the whole DAG.
    let sent = gateway.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].url, car_url(GW1, ROOT));
    assert!(sent[0]
        .headers
        .iter()
        .any(|(k, v)| k == "Accept" && v.starts_with("application/vnd.ipld.car; version=1")));
}

#[test]
fn the_pure_verifier_reconstructs_it_too() {
    let limits = ikigai_name::delivery::Limits::default();
    let bytes = ikigai_name::delivery::verify_car(&parse(ROOT), &multiblock_car(), &limits)
        .expect("verifies");
    assert_eq!(bytes, multiblock_bytes());
}

#[test]
fn a_raw_block_is_served_only_when_it_has_its_digest() {
    let (kernel, gateway) = world(&config(""));
    gateway.answer(&raw_url(GW1, ASCII), 200, block(ASCII));
    let (head, bytes) = entity(&document(&kernel, "files/ascii.txt", &granted()).unwrap());
    assert_eq!(bytes, b"hello application/vnd.ipld.car\n");
    assert!(
        head.contains("Content-Type: text/plain; charset=utf-8"),
        "{head}"
    );

    let (kernel, gateway) = world(&config(""));
    gateway.answer(
        &raw_url(GW1, ASCII),
        200,
        b"hello application/vnd.ipld.cat\n".to_vec(),
    );
    let err = document(&kernel, "files/ascii.txt", &granted()).unwrap_err();
    assert!(integrity(&err), "{err}");
    assert!(err.to_string().contains(GW1), "{err}");
}

// --- malformed answers, wrong roots, omitted blocks ----------------------------------

#[test]
fn malformed_answers_are_integrity_failures() {
    let mut truncated = multiblock_car();
    truncated.truncate(truncated.len() - 10);
    let mut bad_header = multiblock_car();
    bad_header[2] = 0x00; // the map's first key is no longer a text string
    let mut no_cid = car(&parse(ROOT), &[]);
    no_cid.extend([0x03, 0xff, 0xff, 0xff]);
    for (what, body) in [
        ("empty", Vec::new()),
        (
            "an error page",
            b"<html><body>gateway error</body></html>".to_vec(),
        ),
        ("truncated", truncated),
        ("a bad header", bad_header),
        ("a block with no CID", no_cid),
        ("the raw file instead of a CAR", multiblock_bytes()),
    ] {
        let (kernel, gateway) = world(&config(""));
        gateway.answer(&car_url(GW1, ROOT), 200, body.clone());
        gateway.answer(&car_url(GW2, ROOT), 200, body);
        let err = document(&kernel, "files/multiblock.txt", &granted()).unwrap_err();
        assert!(integrity(&err), "{what}: {err}");
    }
}

#[test]
fn a_car_for_another_root_is_refused_even_when_it_holds_the_blocks() {
    // The CAR names the directory as its root, though it carries every block of the file.
    let blocks: Vec<(Cid, Vec<u8>)> = fixture_blocks()
        .into_iter()
        .map(|(c, b)| (parse(&c), b))
        .collect();
    let (kernel, gateway) = world(&config(""));
    gateway.answer(&car_url(GW1, ROOT), 200, car(&parse(DIRECTORY), &blocks));
    let err = document(&kernel, "files/multiblock.txt", &granted()).unwrap_err();
    assert!(integrity(&err), "{err}");
    assert!(err.to_string().contains("for the root"), "{err}");

    // A CAR that names the right root but holds another file's blocks.
    let (kernel, gateway) = world(&config(""));
    gateway.answer(
        &car_url(GW1, ROOT),
        200,
        car(&parse(ROOT), &[(parse(ASCII), block(ASCII))]),
    );
    let err = document(&kernel, "files/multiblock.txt", &granted()).unwrap_err();
    assert!(integrity(&err), "{err}");
    assert!(
        err.to_string().contains(&format!("omits the block {ROOT}")),
        "{err}"
    );
}

#[test]
fn an_omitted_block_is_an_incomplete_file_and_nothing_is_served() {
    let mut blocks = vec![(parse(ROOT), block(ROOT))];
    blocks.extend(
        LEAVES
            .iter()
            .filter(|l| **l != LEAVES[2])
            .map(|l| (parse(l), block(l))),
    );
    let (kernel, gateway) = world(&config(""));
    gateway.answer(&car_url(GW1, ROOT), 200, car(&parse(ROOT), &blocks));
    let err = document(&kernel, "files/multiblock.txt", &granted()).unwrap_err();
    assert!(integrity(&err), "{err}");
    assert!(err.to_string().contains(LEAVES[2]), "{err}");
    assert!(err.to_string().contains("incomplete"), "{err}");
}

#[test]
fn a_substituted_leaf_is_caught_by_its_digest() {
    let mut blocks = vec![(parse(ROOT), block(ROOT))];
    for leaf in LEAVES {
        let mut data = block(leaf);
        if leaf == LEAVES[1] {
            data[0] ^= 1;
        }
        blocks.push((parse(leaf), data));
    }
    let (kernel, gateway) = world(&config(""));
    gateway.answer(&car_url(GW1, ROOT), 200, car(&parse(ROOT), &blocks));
    let err = document(&kernel, "files/multiblock.txt", &granted()).unwrap_err();
    assert!(integrity(&err), "{err}");
    assert!(err.to_string().contains(LEAVES[1]), "{err}");
}

// --- retrying elsewhere, and unavailability kept apart ---------------------------------

#[test]
fn an_integrity_failure_is_retried_from_the_next_gateway() {
    let (kernel, gateway) = world(&config(""));
    gateway.answer(&car_url(GW1, ROOT), 200, b"garbage".to_vec());
    gateway.answer(&car_url(GW2, ROOT), 200, multiblock_car());
    let (_, bytes) = entity(&document(&kernel, "files/multiblock.txt", &granted()).unwrap());
    assert_eq!(bytes, multiblock_bytes());
    assert_eq!(gateway.urls(), [car_url(GW1, ROOT), car_url(GW2, ROOT)]);
}

#[test]
fn unavailability_is_not_an_integrity_failure() {
    // Neither gateway has it: a plain absence, retried, and said as such.
    let (kernel, gateway) = world(&config(""));
    let err = document(&kernel, "files/multiblock.txt", &granted()).unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err}");
    assert!(!integrity(&err), "{err}");
    assert_eq!(gateway.urls().len(), 2);

    // One down, one answering: served.
    let (kernel, gateway) = world(&config(""));
    gateway.answer(&car_url(GW1, ROOT), 502, Vec::new());
    gateway.answer(&car_url(GW2, ROOT), 200, multiblock_car());
    assert!(document(&kernel, "files/multiblock.txt", &granted()).is_ok());

    // One down, one lying: the lie is what is reported, and each gateway is named.
    let (kernel, gateway) = world(&config(""));
    gateway.answer(&car_url(GW1, ROOT), 502, Vec::new());
    gateway.answer(&car_url(GW2, ROOT), 200, b"garbage".to_vec());
    let err = document(&kernel, "files/multiblock.txt", &granted()).unwrap_err();
    assert!(integrity(&err), "{err}");
    let said = err.to_string();
    assert!(said.contains(&format!("{GW1} did not answer it")), "{said}");
    assert!(
        said.contains(&format!("{GW2} answered bytes that do not verify")),
        "{said}"
    );

    // Both down: unavailable, transient.
    let (kernel, gateway) = world(&config(""));
    gateway.answer(&car_url(GW1, ROOT), 502, Vec::new());
    gateway.answer(&car_url(GW2, ROOT), 502, Vec::new());
    let err = document(&kernel, "files/multiblock.txt", &granted()).unwrap_err();
    assert!(matches!(err, Error::Unavailable(_)), "{err}");
}

// --- unsupported formats, refused distinctly -----------------------------------------

#[test]
fn an_unsupported_hash_or_unixfs_type_is_refused_as_unsupported() {
    // A raw block named by BLAKE3: the CID is one this resolver holds, not one it verifies.
    let blake3 = Cid::new_v1(
        0x55,
        Multihash::wrap(0x1e, blake3::hash(b"hello").as_bytes()).unwrap(),
    )
    .to_string();
    let reg = registry(&[("files/b3", &blake3, None)], true);
    let (kernel, gateway) = world_with(reg, Some(config("")));
    gateway.answer(&raw_url(GW1, &blake3), 200, b"hello".to_vec());
    let err = document(&kernel, "files/b3", &granted()).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err}");
    assert!(err.to_string().contains("sha2-256"), "{err}");
    // The answer is the content's, so the next gateway is not asked.
    assert_eq!(gateway.urls().len(), 1);

    // A UnixFS directory is not a file.
    let (kernel, gateway) = world(&config(""));
    let blocks: Vec<(Cid, Vec<u8>)> = fixture_blocks()
        .into_iter()
        .map(|(c, b)| (parse(&c), b))
        .collect();
    gateway.answer(
        &car_url(GW1, DIRECTORY),
        200,
        car(&parse(DIRECTORY), &blocks),
    );
    let err = document(&kernel, "files/folder", &granted()).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err}");
    assert!(err.to_string().contains("a directory, not a file"), "{err}");
}

#[test]
fn a_carv2_is_unsupported_rather_than_malformed() {
    let mut v2 = multiblock_car();
    // The version is the header's last byte.
    let at = usize::from(v2[0]);
    v2[at] = 0x02;
    let (kernel, gateway) = world(&config(""));
    gateway.answer(&car_url(GW1, ROOT), 200, v2);
    let err = document(&kernel, "files/multiblock.txt", &granted()).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err}");
    assert!(err.to_string().contains("CARv1"), "{err}");
}

// --- the media type is the publisher's assertion -----------------------------------------

#[test]
fn an_active_media_type_is_served_as_an_octet_stream_download() {
    let (kernel, gateway) = world(&config(""));
    gateway.answer(&raw_url(GW1, ASCII), 200, block(ASCII));
    gateway.answer(&car_url(GW1, ROOT), 200, multiblock_car());
    for path in ["files/page.html", "files/drawing.svg", "files/untyped"] {
        let (head, _) = entity(&document(&kernel, path, &granted()).unwrap());
        assert!(
            head.contains("Content-Type: application/octet-stream"),
            "{path}: {head}"
        );
        assert!(
            head.contains("Content-Disposition: attachment"),
            "{path}: {head}"
        );
        assert!(
            !head.contains("text/html") && !head.contains("svg"),
            "{path}: {head}"
        );
    }
}

#[test]
fn an_isolated_origin_serves_the_declared_type_inline() {
    let (kernel, gateway) = world(&config(r#", "serve": "isolated-origin""#));
    gateway.answer(&raw_url(GW1, ASCII), 200, block(ASCII));
    let (head, _) = entity(&document(&kernel, "files/page.html", &granted()).unwrap());
    assert_eq!(head, "Content-Type: text/html");
}

// --- each resource limit ------------------------------------------------------------------

fn limited(limit: &str) -> String {
    config(&format!(r#", "limits": {{{limit}}}"#))
}

#[test]
fn the_transfer_limit_refuses_a_larger_answer() {
    let (kernel, gateway) = world(&limited(r#""transfer-bytes": 1000"#));
    gateway.answer(&car_url(GW1, ROOT), 200, multiblock_car());
    gateway.answer(&car_url(GW2, ROOT), 200, multiblock_car());
    let err = document(&kernel, "files/multiblock.txt", &granted()).unwrap_err();
    assert!(matches!(err, Error::OverBudget { .. }), "{err}");
    assert!(err.to_string().contains("transfer limit of 1000"), "{err}");
    // One gateway's padding is not another's: both were asked.
    assert_eq!(gateway.urls().len(), 2);
}

#[test]
fn the_decoded_limit_refuses_before_assembling() {
    let (kernel, gateway) = world(&limited(r#""decoded-bytes": 1025"#));
    gateway.answer(&car_url(GW1, ROOT), 200, multiblock_car());
    let err = document(&kernel, "files/multiblock.txt", &granted()).unwrap_err();
    assert!(matches!(err, Error::OverBudget { .. }), "{err}");
    assert!(err.to_string().contains("decoded limit of 1025"), "{err}");
    // The content itself is too large, from anywhere: the next gateway is not asked.
    assert_eq!(gateway.urls().len(), 1);

    let (kernel, gateway) = world(&limited(r#""decoded-bytes": 1026"#));
    gateway.answer(&car_url(GW1, ROOT), 200, multiblock_car());
    assert!(document(&kernel, "files/multiblock.txt", &granted()).is_ok());
}

#[test]
fn the_block_limit_refuses_an_answer_with_more_blocks() {
    let (kernel, gateway) = world(&limited(r#""blocks": 5"#));
    gateway.answer(&car_url(GW1, ROOT), 200, multiblock_car());
    let err = document(&kernel, "files/multiblock.txt", &granted()).unwrap_err();
    assert!(matches!(err, Error::OverBudget { .. }), "{err}");
    assert!(err.to_string().contains("block limit of 5"), "{err}");

    let (kernel, gateway) = world(&limited(r#""blocks": 6"#));
    gateway.answer(&car_url(GW1, ROOT), 200, multiblock_car());
    assert!(document(&kernel, "files/multiblock.txt", &granted()).is_ok());
}

#[test]
fn the_depth_limit_refuses_a_deeper_dag() {
    // Two file nodes over multiblock.txt: its leaves are three links down.
    let (top, blocks) = deep_chain(2);
    let reg = registry(&[("files/deep", &top.to_string(), None)], true);
    let answer = car(&top, &blocks);
    let (kernel, gateway) = world_with(reg.clone(), Some(limited(r#""depth": 2"#)));
    gateway.answer(&car_url(GW1, &top.to_string()), 200, answer.clone());
    let err = document(&kernel, "files/deep", &granted()).unwrap_err();
    assert!(matches!(err, Error::OverBudget { .. }), "{err}");
    assert!(err.to_string().contains("depth limit"), "{err}");

    let (kernel, gateway) = world_with(reg, Some(limited(r#""depth": 3"#)));
    gateway.answer(&car_url(GW1, &top.to_string()), 200, answer);
    let (_, bytes) = entity(&document(&kernel, "files/deep", &granted()).unwrap());
    assert_eq!(bytes, multiblock_bytes());
}

#[test]
fn a_very_deep_dag_is_refused_by_the_iterative_pre_scan() {
    // Deeper than any thread's stack would survive in a recursive walk.
    let (top, blocks) = deep_chain(20_000);
    let limits = ikigai_name::delivery::Limits {
        blocks: 30_000,
        transfer_bytes: 64 * 1024 * 1024,
        ..Default::default()
    };
    let answer = car(&top, &blocks);
    let failure = ikigai_name::delivery::verify_car(&top, &answer, &limits).unwrap_err();
    assert!(
        matches!(
            failure,
            ikigai_name::delivery::Failure::Limit { content: true, .. }
        ),
        "{failure:?}"
    );
    // Within a limit that admits it, the same DAG is walked and assembled, on this
    // thread's stack, because nothing recurses.
    let deep = ikigai_name::delivery::Limits {
        depth: 30_000,
        ..limits
    };
    let bytes = ikigai_name::delivery::verify_car(&top, &answer, &deep).expect("verifies");
    assert_eq!(bytes, multiblock_bytes());
}

#[test]
fn the_time_limit_refuses_a_slow_gateway_and_moves_on() {
    let (kernel, gateway) = world(&limited(r#""millis": 5000"#));
    gateway
        .delays
        .lock()
        .unwrap()
        .insert("gw1.example".into(), 10_000);
    gateway.answer(&car_url(GW1, ROOT), 200, multiblock_car());
    gateway.answer(&car_url(GW2, ROOT), 200, multiblock_car());
    let (_, bytes) = entity(&document(&kernel, "files/multiblock.txt", &granted()).unwrap());
    assert_eq!(bytes, multiblock_bytes());
    assert_eq!(gateway.urls().len(), 2);

    let (kernel, gateway) = world(&limited(r#""millis": 5000"#));
    gateway
        .delays
        .lock()
        .unwrap()
        .insert("gw1.example".into(), 10_000);
    gateway
        .delays
        .lock()
        .unwrap()
        .insert("gw2.example".into(), 6_000);
    gateway.answer(&car_url(GW1, ROOT), 200, multiblock_car());
    gateway.answer(&car_url(GW2, ROOT), 200, multiblock_car());
    let err = document(&kernel, "files/multiblock.txt", &granted()).unwrap_err();
    assert!(matches!(err, Error::OverBudget { .. }), "{err}");
    assert!(err.to_string().contains("time limit of 5000 ms"), "{err}");
}

// --- off by default, and inside the capability boundary --------------------------------

#[test]
fn without_a_configuration_nothing_is_fetched() {
    let (kernel, gateway) = world_with(default_registry(), None);
    let answer = document(&kernel, "files/multiblock.txt", &Capability::root()).unwrap();
    // The registry's gateway hint, as before: a redirect, not bytes.
    assert_ne!(
        answer.repr_type.to_string(),
        "application/vnd.ikigai.http-entity"
    );
    assert!(gateway.urls().is_empty());
}

#[test]
fn a_private_claim_is_never_asked_of_a_gateway() {
    let reg = registry(&[("files/multiblock.txt", ROOT, None)], false);
    let (kernel, gateway) = world_with(reg, Some(config("")));
    let owner = Capability::root();
    let err = document(&kernel, "files/multiblock.txt", &owner).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err}");
    assert!(err.to_string().contains("serves no bytes"), "{err}");
    assert!(gateway.urls().is_empty());
}

#[test]
fn a_gateway_the_capability_does_not_grant_is_not_asked() {
    let (kernel, gateway) = world(&config(""));
    gateway.answer(&car_url(GW2, ROOT), 200, multiblock_car());
    // Only the second gateway is granted.
    let cap = Capability::scoped(["urn:cap:net:gw2.example"]);
    assert!(document(&kernel, "files/multiblock.txt", &cap).is_ok());
    assert_eq!(gateway.urls(), [car_url(GW2, ROOT)]);

    let (kernel, gateway) = world(&config(""));
    let err = document(
        &kernel,
        "files/multiblock.txt",
        &Capability::scoped(["urn:cap:x"]),
    )
    .unwrap_err();
    assert!(matches!(err, Error::Denied(_)), "{err}");
    assert!(
        err.to_string().contains("needs urn:cap:net:gw1.example"),
        "{err}"
    );
    assert!(gateway.urls().is_empty());
}

#[test]
fn a_redirect_off_the_granted_hosts_is_not_followed() {
    let (kernel, gateway) = world(&config(""));
    let elsewhere = car_url("https://elsewhere.example", ROOT);
    for gw in [GW1, GW2] {
        gateway.answer_with(
            &car_url(gw, ROOT),
            302,
            &[("location", &elsewhere)],
            Vec::new(),
        );
    }
    gateway.answer(&elsewhere, 200, multiblock_car());
    let err = document(&kernel, "files/multiblock.txt", &granted()).unwrap_err();
    assert!(matches!(err, Error::Denied(_)), "{err}");
    assert!(!gateway.urls().contains(&elsewhere), "{:?}", gateway.urls());
}

#[test]
fn a_host_without_an_http_client_says_so() {
    let space = ikigai_name::space()
        .bind(
            Exact::new(REGISTRY_IRI),
            json_source("registry-source", default_registry()),
        )
        .bind(
            Exact::new(DELIVERY_SOURCE_IRI),
            json_source("delivery-source", config("")),
        );
    let kernel = Kernel::new(Arc::new(space)).with_clock(Arc::new(Manual(AtomicU64::new(0))));
    let err = document(&kernel, "files/multiblock.txt", &granted()).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err}");
    assert!(err.to_string().contains("bind urn:http:get"), "{err}");
}

#[test]
fn an_invalid_configuration_is_named() {
    let (kernel, _) = world(r#"{"gateways": ["http://plain.example"]}"#);
    let err = document(&kernel, "files/multiblock.txt", &granted()).unwrap_err();
    assert!(err.to_string().contains(DELIVERY_SOURCE_IRI), "{err}");
}
