//! Active names: the distinct names a namespace resolved in a month.
//!
//! Over `ikigai_name::space()`, with a clock the tests move and a store that holds
//! `if-match` to the contract, so everything here goes through the kernel the way a
//! host's would. The server's file binding is in `tests/active_names_http.rs`.

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Clock, Error, Exact, Expiry, FnEndpoint, Invocation, Iri, Kernel, ReprType,
    Representation, Request, Result, Time, UriTemplate, Verb,
};
use ikigai_name::admin::admin_scope;
use ikigai_name::stat::{MAX_BUFFERED_NAMES, MAX_NAME_BYTES};
use ikigai_name::tenant::{tenant_of, tenant_scope};
use ikigai_name::{CAP_ADMIN_ANY, CAP_FLUSH, FLUSH_IRI, REGISTRY_IRI, SEEN_TEMPLATE};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const REGISTRY: &str = r#"{"namespaces":[
  {"prefix":"resmud","owner":"urn:cap:name:admin:resmud","strategy":"hosted","source":"urn:test:vocab"},
  {"prefix":"lore","owner":"urn:cap:name:admin:lore","strategy":"hosted","source":"urn:test:vocab"},
  {"prefix":"acme","owner":"urn:cap:name:admin:acme","strategy":"redirect","target":"https://acme.example/ns"},
  {"prefix":"gone","owner":"urn:cap:name:admin:gone","strategy":"retired","reason":"ended"},
  {"prefix":"big","owner":"urn:cap:name:admin:big","strategy":"hosted","source":"urn:test:big"}
],"limits":{"max_document_bytes":64}}"#;

const VOCAB: &str = "<urn:a> <urn:b> <urn:c> .\n";

/// 2026-11-15T00:00:00Z and 2026-12-01T00:00:00Z.
const NOVEMBER: u64 = 1_794_700_800_000;
const DECEMBER: u64 = 1_796_083_200_000;

struct Moving(AtomicU64);

impl Clock for Moving {
    fn now(&self) -> Time {
        Time(self.0.load(Ordering::SeqCst))
    }
}

type Docs = BTreeMap<(Option<String>, String), String>;

/// The store a host binds at `SEEN_TEMPLATE`: compares and replaces as one step, keyed
/// on the tenant and the IRI, cacheable under its own thread.
#[derive(Clone, Default)]
struct Store {
    docs: Arc<Mutex<Docs>>,
    failing: Arc<AtomicBool>,
    conflict_once: Arc<AtomicBool>,
    touched: Arc<AtomicUsize>,
    writes: Arc<AtomicUsize>,
}

impl Store {
    fn endpoint(&self) -> FnEndpoint {
        let store = self.clone();
        FnEndpoint::new("seen-source", move |inv: &Invocation<'_>| {
            store.touched.fetch_add(1, Ordering::SeqCst);
            if store.failing.load(Ordering::SeqCst) {
                return Err(Error::Unavailable("the disk is full".into()));
            }
            let iri = inv.request.target.as_str().to_string();
            let key = (tenant_of(inv.capability).map(str::to_string), iri.clone());
            let mut docs = store.docs.lock().unwrap();
            if inv.request.verb == Verb::Sink {
                let current = docs.get(&key).map(|doc| json(doc).content_id().to_string());
                let expected = inv.inline_str("if-match").ok().map(str::to_string);
                if current != expected || store.conflict_once.swap(false, Ordering::SeqCst) {
                    return Err(Error::Conflict(format!("{iri} changed")));
                }
                store.writes.fetch_add(1, Ordering::SeqCst);
                docs.insert(key, inv.inline_str("content")?.to_string());
                return Ok(Representation::new(
                    ReprType::new("text/plain"),
                    b"stored".to_vec(),
                ));
            }
            match docs.get(&key) {
                Some(doc) => Ok(json(doc).cacheable().depends_on(iri)),
                None => Err(Error::NotFound(format!("{iri} does not exist"))),
            }
        })
    }

    fn set(&self, tenant: Option<&str>, prefix: &str, period: &str) -> Option<String> {
        let iri = format!("urn:name:seen-source:namespace:{prefix}:period:{period}");
        let key = (tenant.map(str::to_string), iri);
        self.docs.lock().unwrap().get(&key).cloned()
    }
}

fn json(doc: &str) -> Representation {
    Representation::new(ReprType::new("application/json"), doc.as_bytes().to_vec())
}

struct World {
    kernel: Kernel,
    clock: Arc<Moving>,
    store: Store,
    vocab_reads: Arc<AtomicUsize>,
}

impl World {
    /// Everything but the store is threaded, as a watched deployment is, so a repeat of
    /// a resolution really is a cache hit that never reaches a face.
    fn new() -> World {
        let clock = Arc::new(Moving(AtomicU64::new(NOVEMBER)));
        let store = Store::default();
        let vocab_reads = Arc::new(AtomicUsize::new(0));
        let reads = Arc::clone(&vocab_reads);
        let space = bound(ikigai_name::space(), &store).bind(
            Exact::new("urn:test:vocab"),
            FnEndpoint::new("vocab", move |_: &Invocation<'_>| {
                reads.fetch_add(1, Ordering::SeqCst);
                Ok(
                    Representation::new(ReprType::new("text/turtle"), VOCAB.as_bytes().to_vec())
                        .cacheable()
                        .depends_on("urn:test:vocab"),
                )
            }),
        );
        World {
            kernel: Kernel::new(Arc::new(space)).with_clock(clock.clone()),
            clock,
            store,
            vocab_reads,
        }
    }

    fn at(&self, millis: u64) {
        self.clock.0.store(millis, Ordering::SeqCst);
    }

    fn resolve(&self, path: &str) -> Result<String> {
        self.resolve_as(path, &Capability::scoped(Vec::<String>::new()))
    }

    fn resolve_as(&self, path: &str, cap: &Capability) -> Result<String> {
        let request = Request::new(Verb::Source, iri("urn:name:resolve"))
            .with_arg("path", ArgRef::Inline(path.as_bytes().to_vec()));
        Ok(text(&block_on(self.kernel.issue(request, cap))?))
    }

    fn doc(&self, path: &str) -> Result<Representation> {
        let target = format!("urn:name:doc:{path}");
        block_on(self.kernel.issue(
            Request::new(Verb::Source, iri(&target)),
            &Capability::scoped(Vec::<String>::new()),
        ))
    }

    fn flush(&self) -> Result<String> {
        self.flush_as(&Capability::scoped([CAP_FLUSH]))
    }

    fn flush_as(&self, cap: &Capability) -> Result<String> {
        let repr = block_on(
            self.kernel
                .issue(Request::new(Verb::Sink, iri(FLUSH_IRI)), cap),
        )?;
        Ok(text(&repr))
    }

    fn read(&self, target: &str, cap: &Capability) -> Result<Representation> {
        block_on(
            self.kernel
                .issue(Request::new(Verb::Source, iri(target)), cap),
        )
    }

    fn count(&self, prefix: &str, period: &str) -> String {
        let target = format!("urn:name:stat:namespace:{prefix}:period:{period}");
        text(&self.read(&target, &Capability::root()).expect("a count"))
    }
}

fn bound(space: ikigai_core::EndpointSpace, store: &Store) -> ikigai_core::EndpointSpace {
    space
        .bind(
            Exact::new(REGISTRY_IRI),
            FnEndpoint::new("registry-source", |_: &Invocation<'_>| {
                Ok(json(REGISTRY).cacheable().depends_on(REGISTRY_IRI))
            }),
        )
        .bind(
            Exact::new("urn:test:big"),
            FnEndpoint::new("big", |_: &Invocation<'_>| {
                Ok(Representation::new(
                    ReprType::new("text/turtle"),
                    vec![b'x'; 65],
                ))
            }),
        )
        .bind(UriTemplate::parse(SEEN_TEMPLATE).unwrap(), store.endpoint())
}

fn iri(target: &str) -> Iri {
    Iri::parse(target).unwrap()
}

fn text(repr: &Representation) -> String {
    String::from_utf8(repr.bytes.clone()).expect("utf-8")
}

/// Three names, resolved over and over and by both faces, are three, whichever way a
/// path is spelled.
#[test]
fn three_names_resolved_several_times_count_as_three() {
    let w = World::new();
    for _ in 0..3 {
        w.resolve("resmud/core").unwrap();
        w.resolve("resmud/core/weapon").unwrap();
        w.resolve("resmud/lore").unwrap();
        w.doc("resmud/core").unwrap();
    }
    for spelling in ["/resmud/core", "resmud/core/", "resmud//core"] {
        w.resolve(spelling).unwrap();
    }
    assert_eq!(w.count("resmud", "2026-11"), "3\n");
    w.flush().unwrap();
    assert_eq!(w.count("resmud", "2026-11"), "3\n", "once stored, too");
}

#[test]
fn two_namespaces_count_separately() {
    let w = World::new();
    w.resolve("resmud/core").unwrap();
    w.resolve("resmud/other").unwrap();
    w.resolve("lore/core").unwrap();
    assert_eq!(w.count("resmud", "2026-11"), "2\n");
    assert_eq!(w.count("lore", "2026-11"), "1\n");
    w.flush().unwrap();
    assert_eq!(w.count("resmud", "2026-11"), "2\n");
    assert_eq!(w.count("lore", "2026-11"), "1\n");
}

/// The boundary is the point of the deadline: the second resolution of a name in
/// November is a cache hit that no face sees, and left at `Never` so would December's
/// be, and the name would be missing from December.
#[test]
fn a_period_boundary_starts_a_new_count() {
    let w = World::new();
    w.resolve("resmud/core").unwrap();
    w.resolve("resmud/other").unwrap();
    w.doc("resmud/core").unwrap();
    let reads = w.vocab_reads.load(Ordering::SeqCst);
    w.doc("resmud/core").unwrap();
    assert_eq!(
        w.vocab_reads.load(Ordering::SeqCst),
        reads,
        "within a month a repeat is a cache hit"
    );

    w.at(DECEMBER);
    assert_eq!(w.count("resmud", "2026-12"), "0\n");
    w.resolve("resmud/core").unwrap();
    w.doc("resmud/core").unwrap();
    assert_eq!(w.count("resmud", "2026-12"), "1\n");
    assert_eq!(w.count("resmud", "2026-11"), "2\n", "November stands");

    w.flush().unwrap();
    assert_eq!(w.count("resmud", "2026-12"), "1\n");
    assert_eq!(w.count("resmud", "2026-11"), "2\n");
}

/// One millisecond before the boundary is still November.
#[test]
fn a_month_ends_exactly_at_its_boundary_in_utc() {
    let w = World::new();
    w.at(DECEMBER - 1);
    w.resolve("resmud/late").unwrap();
    w.at(DECEMBER);
    w.resolve("resmud/early").unwrap();
    assert_eq!(w.count("resmud", "2026-11"), "1\n");
    assert_eq!(w.count("resmud", "2026-12"), "1\n");
}

#[test]
fn a_refused_or_unknown_name_is_not_counted() {
    let w = World::new();
    w.resolve("nobody/here").expect_err("unclaimed");
    w.resolve("gone/core").expect_err("retired");
    w.doc("nobody/here").expect_err("unclaimed");
    w.doc("gone/core").expect_err("retired");
    w.doc("big/core").expect_err("over the ceiling");
    assert_eq!(w.count("gone", "2026-11"), "0\n");
    assert_eq!(w.count("big", "2026-11"), "0\n");
    assert_eq!(w.flush().unwrap(), "names stored: 0, dropped: 0\n");
    assert_eq!(w.store.writes.load(Ordering::SeqCst), 0);
}

/// A failed conversion resolved nothing the caller can use, so it does not count either.
#[test]
fn a_type_nothing_converts_to_is_not_counted() {
    let w = World::new();
    let request = Request::new(Verb::Source, iri("urn:name:document"))
        .with_arg("path", ArgRef::Inline(b"resmud/core".to_vec()))
        .with_arg("as", ArgRef::Inline(b"application/ld+json".to_vec()));
    block_on(w.kernel.issue(request, &Capability::root())).expect_err("no transreptor");
    assert_eq!(w.count("resmud", "2026-11"), "0\n");
}

/// A redirect is an answer, and the name it answered for counts, by either face.
#[test]
fn a_redirected_name_counts() {
    let w = World::new();
    assert_eq!(
        w.resolve("acme/thing").unwrap(),
        "acme\tredirect\thttps://acme.example/ns\t302\n"
    );
    let reported = w.doc("acme/other").expect_err("reported as an error");
    assert!(reported.to_string().contains("redirects to"), "{reported}");
    assert_eq!(w.count("acme", "2026-11"), "2\n");
}

/// A failed write fails the flush, which is the host's to log, and nothing else. The
/// names wait for the next flush, and in the meantime still count.
#[test]
fn a_failed_write_of_the_seen_set_does_not_fail_the_resolution() {
    let w = World::new();
    w.store.failing.store(true, Ordering::SeqCst);
    assert!(w.resolve("resmud/core").is_ok());
    assert!(w.doc("resmud/other").is_ok());

    let err = w.flush().expect_err("the store is down");
    let said = err.to_string();
    assert!(said.contains("the disk is full"), "names why: {said}");
    assert!(said.contains("\"resmud\""), "names the namespace: {said}");
    assert!(said.contains("stay buffered"), "says what happens: {said}");
    assert!(w.resolve("resmud/third").is_ok(), "still resolving");

    w.store.failing.store(false, Ordering::SeqCst);
    assert_eq!(w.count("resmud", "2026-11"), "3\n");
    assert_eq!(w.flush().unwrap(), "names stored: 3, dropped: 0\n");
    assert_eq!(w.count("resmud", "2026-11"), "3\n");
}

/// The store is not on the resolution path at all, so it cannot slow it.
#[test]
fn resolving_a_name_never_touches_the_store() {
    let w = World::new();
    w.resolve("resmud/core").unwrap();
    w.doc("resmud/other").unwrap();
    assert_eq!(w.store.touched.load(Ordering::SeqCst), 0);
    w.flush().unwrap();
    assert!(w.store.touched.load(Ordering::SeqCst) > 0);
}

/// Everything a request could carry is on this one, and the atom holds names and
/// periods. Its keys are compared exactly, so a field added later fails here.
#[test]
fn the_stored_atom_holds_only_names_and_periods() {
    let w = World::new();
    let visitor = Capability::scoped(["urn:cap:principal:urn:person:someone"]);
    let request = Request::new(Verb::Source, iri("urn:name:resolve"))
        .with_arg("path", ArgRef::Inline(b"resmud/core".to_vec()))
        .with_arg("user-agent", ArgRef::Inline(b"Mozilla/5.0 secret".to_vec()))
        .with_arg("x-forwarded-for", ArgRef::Inline(b"203.0.113.9".to_vec()))
        .with_arg("cookie", ArgRef::Inline(b"session=abc123".to_vec()));
    block_on(w.kernel.issue(request, &visitor)).unwrap();
    w.resolve("resmud/other").unwrap();
    w.flush().unwrap();

    let stored = w.store.docs.lock().unwrap().clone();
    assert_eq!(
        stored.len(),
        2,
        "one set and one list of periods: {stored:?}"
    );
    let set = w.store.set(None, "resmud", "2026-11").expect("the set");
    let parsed: serde_json::Value = serde_json::from_str(&set).unwrap();
    assert_eq!(
        parsed,
        serde_json::json!({"period": "2026-11", "names": ["resmud/core", "resmud/other"]})
    );
    let list = &stored[&(None, "urn:name:seen-source:namespace:resmud".to_string())];
    let parsed: serde_json::Value = serde_json::from_str(list).unwrap();
    assert_eq!(parsed, serde_json::json!({"periods": ["2026-11"]}));
    for doc in stored.values() {
        for secret in ["Mozilla", "203.0.113.9", "abc123", "someone", "principal"] {
            assert!(!doc.contains(secret), "{secret} in {doc}");
        }
    }
}

/// Reading a set refuses a field this module would never write, so a store holding one
/// is reported rather than counted as if nothing were wrong.
#[test]
fn a_stored_set_with_anything_else_in_it_is_refused() {
    let w = World::new();
    w.resolve("resmud/core").unwrap();
    w.flush().unwrap();
    let key = (
        None,
        "urn:name:seen-source:namespace:resmud:period:2026-11".to_string(),
    );
    w.store.docs.lock().unwrap().insert(
        key,
        r#"{"period":"2026-11","names":[],"visitors":["203.0.113.9"]}"#.to_string(),
    );
    let err = w
        .read(
            "urn:name:stat:namespace:resmud:period:2026-11",
            &Capability::root(),
        )
        .expect_err("not a set this module wrote");
    assert!(!err.to_string().contains("203.0.113.9"), "{err}");
}

/// Flushing adds to what is stored rather than replacing it, and writes nothing when it
/// has nothing new, so it does not cut a thread for no reason.
#[test]
fn a_flush_adds_to_the_stored_set_and_writes_only_news() {
    let w = World::new();
    w.resolve("resmud/core").unwrap();
    assert_eq!(w.flush().unwrap(), "names stored: 1, dropped: 0\n");
    let writes = w.store.writes.load(Ordering::SeqCst);
    assert_eq!(writes, 2, "the list of periods, then the set");

    assert_eq!(w.flush().unwrap(), "names stored: 0, dropped: 0\n");
    assert_eq!(w.store.writes.load(Ordering::SeqCst), writes);

    w.resolve("resmud/other").unwrap();
    w.flush().unwrap();
    assert_eq!(w.count("resmud", "2026-11"), "2\n");
    assert_eq!(
        w.store.writes.load(Ordering::SeqCst),
        writes + 1,
        "the period is already listed"
    );
}

/// A store that moved on between the read and the write refuses, and the names stay
/// for the next flush rather than overwriting what the other writer put there.
#[test]
fn a_conflicting_write_keeps_the_names_for_the_next_flush() {
    let w = World::new();
    w.resolve("resmud/core").unwrap();
    w.flush().unwrap();
    w.resolve("resmud/other").unwrap();
    w.store.conflict_once.store(true, Ordering::SeqCst);
    let err = w.flush().expect_err("refused");
    assert!(err.to_string().contains("stay buffered"), "{err}");
    assert_eq!(w.count("resmud", "2026-11"), "2\n", "still counted");
    assert_eq!(w.flush().unwrap(), "names stored: 1, dropped: 0\n");
}

/// A flush merges over what is stored now, so two writers lose neither's names.
#[test]
fn two_writers_keep_each_others_names() {
    let w = World::new();
    w.resolve("resmud/core").unwrap();
    w.flush().unwrap();

    // A second kernel over the same store: another process counting.
    let other =
        Kernel::new(Arc::new(bound(ikigai_name::space(), &w.store))).with_clock(w.clock.clone());
    let request = Request::new(Verb::Source, iri("urn:name:resolve"))
        .with_arg("path", ArgRef::Inline(b"resmud/theirs".to_vec()));
    block_on(other.issue(request, &Capability::root())).unwrap();
    block_on(other.issue(
        Request::new(Verb::Sink, iri(FLUSH_IRI)),
        &Capability::scoped([CAP_FLUSH]),
    ))
    .unwrap();

    w.resolve("resmud/ours").unwrap();
    w.flush().unwrap();
    let set = w.store.set(None, "resmud", "2026-11").unwrap();
    for name in ["resmud/core", "resmud/theirs", "resmud/ours"] {
        assert!(set.contains(name), "{name} in {set}");
    }
}

// --- the bounds --------------------------------------------------------------

/// The buffer is bounded because a client picks the names. Past the ceiling a new name
/// is dropped, the resolution still answers, and the next flush says how many.
#[test]
fn the_buffer_is_bounded_and_reports_what_it_dropped() {
    let w = World::new();
    for n in 0..MAX_BUFFERED_NAMES + 5 {
        assert!(w.resolve(&format!("resmud/{n}")).is_ok());
    }
    assert_eq!(
        w.count("resmud", "2026-11"),
        format!("{MAX_BUFFERED_NAMES}\n")
    );
    assert_eq!(
        w.flush().unwrap(),
        format!("names stored: {MAX_BUFFERED_NAMES}, dropped: 5\n")
    );
    assert_eq!(
        w.flush().unwrap(),
        "names stored: 0, dropped: 0\n",
        "reported once"
    );

    // The flush emptied it, so counting resumes.
    w.resolve("resmud/after").unwrap();
    assert_eq!(
        w.count("resmud", "2026-11"),
        format!("{}\n", MAX_BUFFERED_NAMES + 1)
    );
}

#[test]
fn a_name_over_the_length_ceiling_answers_and_is_dropped() {
    let w = World::new();
    let long = format!("resmud/{}", "x".repeat(MAX_NAME_BYTES));
    assert!(w.resolve(&long).is_ok());
    assert_eq!(w.count("resmud", "2026-11"), "0\n");
    assert_eq!(w.flush().unwrap(), "names stored: 0, dropped: 1\n");
}

// --- the faces ---------------------------------------------------------------

#[test]
fn turtle_is_a_skolemized_observation() {
    let w = World::new();
    w.resolve("resmud/core").unwrap();
    let target = "urn:name:stat:namespace:resmud:period:2026-11";
    let request = Request::new(Verb::Source, iri(target))
        .with_arg("as", ArgRef::Inline(b"text/turtle".to_vec()));
    let repr = block_on(w.kernel.issue(request, &Capability::root())).unwrap();
    assert_eq!(repr.repr_type.media_type, "text/turtle");
    let turtle = text(&repr);
    assert!(
        turtle.contains(&format!("<{target}> a schema:Observation")),
        "{turtle}"
    );
    assert!(
        turtle.contains("schema:observationAbout \"resmud\""),
        "{turtle}"
    );
    assert!(
        turtle.contains("dcterms:temporal \"2026-11\"^^xsd:gYearMonth"),
        "{turtle}"
    );
    assert!(turtle.contains("schema:value 1 ."), "{turtle}");
    assert!(!turtle.contains("_:"), "no blank node: {turtle}");
}

#[test]
fn the_report_has_one_line_per_period_held() {
    let w = World::new();
    let report = "urn:name:stat:namespace:resmud";
    assert_eq!(
        text(&w.read(report, &Capability::root()).unwrap()),
        "name: \"resmud\" has no names counted yet\n"
    );
    w.resolve("resmud/a").unwrap();
    w.resolve("resmud/b").unwrap();
    w.flush().unwrap();
    w.at(DECEMBER);
    // Not flushed: a period still in the buffer is held too.
    w.resolve("resmud/c").unwrap();
    assert_eq!(
        text(&w.read(report, &Capability::root()).unwrap()),
        "2026-11\t2\n2026-12\t1\n"
    );
    assert_eq!(
        w.read(report, &Capability::root()).unwrap().expiry,
        Expiry::Always,
        "always live"
    );
}

/// A namespace can hold a `/`, and the count's template still finds the end of it.
#[test]
fn a_prefix_with_a_slash_has_a_count_and_a_report() {
    let registry: &'static str = r#"{"namespaces":[{"prefix":"acme/vocab","owner":"x",
        "strategy":"redirect","target":"https://acme.example/"}]}"#;
    let store = Store::default();
    let space = ikigai_name::space()
        .bind(
            Exact::new(REGISTRY_IRI),
            FnEndpoint::new("registry-source", move |_: &Invocation<'_>| {
                Ok(json(registry))
            }),
        )
        .bind(UriTemplate::parse(SEEN_TEMPLATE).unwrap(), store.endpoint());
    let clock = Arc::new(Moving(AtomicU64::new(NOVEMBER)));
    let kernel = Kernel::new(Arc::new(space)).with_clock(clock);
    let root = Capability::root();
    let resolve = Request::new(Verb::Source, iri("urn:name:resolve"))
        .with_arg("path", ArgRef::Inline(b"acme/vocab/term".to_vec()));
    block_on(kernel.issue(resolve, &root)).unwrap();
    let read = |target: &str| {
        text(&block_on(kernel.issue(Request::new(Verb::Source, iri(target)), &root)).unwrap())
    };
    assert_eq!(
        read("urn:name:stat:namespace:acme/vocab:period:2026-11"),
        "1\n"
    );
    assert_eq!(read("urn:name:stat:namespace:acme/vocab"), "2026-11\t1\n");
}

#[test]
fn a_bad_period_is_refused_by_name() {
    let w = World::new();
    for bad in ["2026-13", "november", "2026"] {
        let target = format!("urn:name:stat:namespace:resmud:period:{bad}");
        let err = w
            .read(&target, &Capability::root())
            .expect_err("no such month");
        assert!(
            matches!(&err, Error::InvalidArgument { name, .. } if name == "period"),
            "{bad}: {err:?}"
        );
        assert!(
            err.to_string().contains("yyyy-mm"),
            "says what to write: {err}"
        );
    }
}

/// With nothing bound at the seam there is no count to read, which is not the same as
/// a count of zero. Resolving is unaffected.
#[test]
fn a_host_with_no_store_refuses_the_count_and_still_resolves() {
    let space = ikigai_name::space().bind(
        Exact::new(REGISTRY_IRI),
        FnEndpoint::new("registry-source", |_: &Invocation<'_>| Ok(json(REGISTRY))),
    );
    let kernel =
        Kernel::new(Arc::new(space)).with_clock(Arc::new(Moving(AtomicU64::new(NOVEMBER))));
    let resolve = Request::new(Verb::Source, iri("urn:name:resolve"))
        .with_arg("path", ArgRef::Inline(b"acme/thing".to_vec()));
    block_on(kernel.issue(resolve, &Capability::root())).unwrap();
    let err = block_on(kernel.issue(
        Request::new(
            Verb::Source,
            iri("urn:name:stat:namespace:acme:period:2026-11"),
        ),
        &Capability::root(),
    ))
    .expect_err("no store");
    match &err {
        Error::Unsupported { detail, .. } => assert!(detail.contains(SEEN_TEMPLATE), "{detail}"),
        other => panic!("want Unsupported, got: {other:?}"),
    }
}

// --- authority ---------------------------------------------------------------

#[test]
fn the_count_is_the_administrators_or_the_operators() {
    let w = World::new();
    w.resolve("resmud/core").unwrap();
    let count = "urn:name:stat:namespace:resmud:period:2026-11";
    let report = "urn:name:stat:namespace:resmud";
    for target in [count, report] {
        let owner = Capability::scoped([admin_scope("resmud")]);
        assert!(w.read(target, &owner).is_ok(), "{target}");
        assert!(w.read(target, &Capability::root()).is_ok(), "{target}");

        let other = w
            .read(target, &Capability::scoped([admin_scope("lore")]))
            .expect_err("another namespace's administrator");
        assert!(matches!(other, Error::Denied(_)), "{other:?}");
        assert!(
            other.to_string().contains("urn:cap:name:admin:resmud"),
            "names what it needs: {other}"
        );

        let nobody = w
            .read(target, &Capability::scoped(Vec::<String>::new()))
            .expect_err("no grants");
        assert!(matches!(nobody, Error::Denied(_)), "{nobody:?}");
    }
}

/// A refusal names the same capability whether or not the namespace is claimed, so it
/// does not say what is claimed.
#[test]
fn a_refusal_does_not_disclose_whether_a_namespace_exists() {
    let w = World::new();
    let outsider = Capability::scoped([admin_scope("lore")]);
    let claimed = w
        .read("urn:name:stat:namespace:resmud", &outsider)
        .unwrap_err();
    let unclaimed = w
        .read("urn:name:stat:namespace:ghost", &outsider)
        .unwrap_err();
    assert!(matches!(claimed, Error::Denied(_)), "{claimed:?}");
    assert!(matches!(unclaimed, Error::Denied(_)), "{unclaimed:?}");

    let err = w
        .read("urn:name:stat:namespace:ghost", &Capability::root())
        .expect_err("the operator may be told");
    assert!(matches!(err, Error::NotFound(_)), "{err:?}");
}

/// The count answers to the namespace's owner, as administering it does, and a private
/// namespace stays absent to anyone else (ledger #883).
#[test]
fn the_count_answers_to_the_owner_the_registry_names() {
    let registry: &'static str = r#"{"namespaces":[
        {"prefix":"acme","owner":"urn:cap:name:admin:staff",
         "strategy":"redirect","target":"https://acme.example/"},
        {"prefix":"hidden","owner":"urn:cap:name:admin:staff","public":false,
         "strategy":"redirect","target":"https://hidden.example/"}]}"#;
    let space = ikigai_name::space()
        .bind(
            Exact::new(REGISTRY_IRI),
            FnEndpoint::new("registry-source", move |_: &Invocation<'_>| {
                Ok(json(registry))
            }),
        )
        .bind(
            UriTemplate::parse(SEEN_TEMPLATE).unwrap(),
            Store::default().endpoint(),
        );
    let kernel =
        Kernel::new(Arc::new(space)).with_clock(Arc::new(Moving(AtomicU64::new(NOVEMBER))));
    let read = |prefix: &str, cap: &Capability| {
        let target = format!("urn:name:stat:namespace:{prefix}:period:2026-11");
        block_on(kernel.issue(Request::new(Verb::Source, iri(&target)), cap))
    };
    let staff = Capability::scoped(["urn:cap:name:admin:staff"]);
    read("acme", &staff).expect("its owner reads the count");
    read("hidden", &staff).expect("its owner reads the count");

    let err = read("acme", &Capability::scoped([admin_scope("acme")])).expect_err("not its owner");
    assert!(matches!(err, Error::Denied(_)), "{err:?}");
    assert!(err.to_string().contains("admin:staff"), "{err}");

    for prefix_scope in ["hidden", "ghost"] {
        let cap = Capability::scoped([admin_scope(prefix_scope)]);
        let held = read(prefix_scope, &cap).expect_err("not visible");
        assert!(matches!(held, Error::NotFound(_)), "{held:?}");
        assert_eq!(
            held.to_string(),
            format!("not found: name: no namespace claims {prefix_scope:?}")
        );
    }
    let other = Capability::scoped([admin_scope("other")]);
    assert_eq!(
        read("hidden", &other).unwrap_err().to_string(),
        read("ghost", &other)
            .unwrap_err()
            .to_string()
            .replace("ghost", "hidden")
    );
}

#[test]
fn declared_capabilities_are_the_ones_enforced() {
    let w = World::new();
    let requires = |target: &str, verb: Verb| {
        let described = w.kernel.describe(&iri(target)).expect("described");
        let action = described
            .action_specs()
            .into_iter()
            .find(|a| a.verb == verb)
            .expect("an action");
        action.requires.clone()
    };
    for target in [
        "urn:name:stat:namespace:resmud:period:2026-11",
        "urn:name:stat:namespace:resmud",
    ] {
        assert_eq!(requires(target, Verb::Source), [CAP_ADMIN_ANY], "{target}");
    }
    assert_eq!(requires(FLUSH_IRI, Verb::Sink), [CAP_FLUSH]);
    assert!(matches!(
        w.flush_as(&Capability::scoped([admin_scope("resmud")])),
        Err(Error::Denied(_))
    ));
}

// --- cacheability ------------------------------------------------------------

/// A past month is a pure function of its atom, cached under its thread: a repeat reads
/// nothing, and a write to that atom cuts it. The current month is read every time.
#[test]
fn a_past_count_is_cacheable_under_its_thread_and_the_current_one_is_live() {
    let w = World::new();
    let target = "urn:name:stat:namespace:resmud:period:2026-11";
    w.resolve("resmud/a").unwrap();
    w.flush().unwrap();

    let current = w.read(target, &Capability::root()).unwrap();
    assert_eq!(current.expiry, Expiry::Always, "the current month is live");

    w.at(DECEMBER);
    let past = w.read(target, &Capability::root()).unwrap();
    assert_eq!(past.expiry, Expiry::Never, "a past month is cacheable");
    assert!(
        past.threads()
            .iter()
            .any(|t| t.as_str() == "urn:name:seen-source:namespace:resmud:period:2026-11"),
        "under its atom's thread: {:?}",
        past.threads()
    );
    let touched = w.store.touched.load(Ordering::SeqCst);
    assert_eq!(text(&w.read(target, &Capability::root()).unwrap()), "1\n");
    assert_eq!(
        w.store.touched.load(Ordering::SeqCst),
        touched,
        "the repeat came from the cache"
    );

    // A name buffered in November and stored only now, as a flush after the boundary does.
    let sink = Request::new(
        Verb::Sink,
        iri("urn:name:seen-source:namespace:resmud:period:2026-11"),
    )
    .with_arg(
        "content",
        ArgRef::Inline(br#"{"period":"2026-11","names":["resmud/a","resmud/b"]}"#.to_vec()),
    )
    .with_arg(
        "if-match",
        ArgRef::Inline(
            json(&w.store.set(None, "resmud", "2026-11").unwrap())
                .content_id()
                .to_string()
                .into_bytes(),
        ),
    );
    block_on(w.kernel.issue(sink, &Capability::root())).unwrap();
    assert_eq!(
        text(&w.read(target, &Capability::root()).unwrap()),
        "2\n",
        "the write cut the cached count"
    );
}

/// Across the boundary, a name still in the buffer belongs to the past month's count,
/// which is therefore not settled, and not cached, until it is stored.
#[test]
fn a_past_count_is_live_until_its_names_are_stored() {
    let w = World::new();
    let target = "urn:name:stat:namespace:resmud:period:2026-11";
    w.resolve("resmud/a").unwrap();
    w.at(DECEMBER);
    let before = w.read(target, &Capability::root()).unwrap();
    assert_eq!(text(&before), "1\n", "counted from the buffer");
    assert_eq!(before.expiry, Expiry::Always);
    w.flush().unwrap();
    let after = w.read(target, &Capability::root()).unwrap();
    assert_eq!(text(&after), "1\n");
    assert_eq!(after.expiry, Expiry::Never);
}

/// A past month nothing was ever stored for reads as zero, and must not be cached as
/// zero: there is no thread for a late write to cut.
#[test]
fn a_past_count_of_nothing_is_not_cached() {
    let w = World::new();
    let repr = w
        .read(
            "urn:name:stat:namespace:resmud:period:2020-01",
            &Capability::root(),
        )
        .unwrap();
    assert_eq!(text(&repr), "0\n");
    assert_eq!(repr.expiry, Expiry::Always);
}

/// A counted answer is cacheable until its month ends, and not beyond.
#[test]
fn a_counted_answer_expires_with_its_month() {
    let w = World::new();
    let repr = w.doc("resmud/core").unwrap();
    assert_eq!(repr.expiry, Expiry::At(Time(DECEMBER)));
}

// --- tenants -----------------------------------------------------------------

#[test]
fn a_tenant_scopes_the_buffer_the_store_and_the_count() {
    let w = World::new();
    let host = |name| tenant_scope(name).unwrap();
    let urnid = Capability::scoped([host("urnid.org")]);
    let iriref = Capability::scoped([host("iriref.org")]);
    w.resolve_as("resmud/a", &urnid).unwrap();
    w.resolve_as("resmud/b", &urnid).unwrap();
    w.resolve_as("resmud/a", &iriref).unwrap();

    let reader = |name| Capability::scoped([host(name), admin_scope("resmud")]);
    let target = "urn:name:stat:namespace:resmud:period:2026-11";
    assert_eq!(text(&w.read(target, &reader("urnid.org")).unwrap()), "2\n");
    assert_eq!(text(&w.read(target, &reader("iriref.org")).unwrap()), "1\n");

    // A flush takes only the tenant its capability names.
    let flusher = |name| Capability::scoped([host(name), CAP_FLUSH.to_string()]);
    assert_eq!(
        w.flush_as(&flusher("urnid.org")).unwrap(),
        "names stored: 2, dropped: 0\n"
    );
    assert!(w
        .store
        .set(Some("urnid.org"), "resmud", "2026-11")
        .is_some());
    assert!(w
        .store
        .set(Some("iriref.org"), "resmud", "2026-11")
        .is_none());
    assert_eq!(
        w.flush_as(&flusher("iriref.org")).unwrap(),
        "names stored: 1, dropped: 0\n"
    );
    assert_eq!(text(&w.read(target, &reader("urnid.org")).unwrap()), "2\n");
    assert_eq!(text(&w.read(target, &reader("iriref.org")).unwrap()), "1\n");
}

// --- what is not counted -----------------------------------------------------

/// With no clock there is no month to put a name in, so nothing is counted, and the
/// answers are exactly as cacheable as before this module counted anything.
#[test]
fn a_kernel_with_no_clock_counts_nothing_and_caches_as_before() {
    let store = Store::default();
    let kernel = Kernel::new(Arc::new(bound(ikigai_name::space(), &store)));
    let resolve = Request::new(Verb::Source, iri("urn:name:resolve"))
        .with_arg("path", ArgRef::Inline(b"resmud/core".to_vec()));
    let repr = block_on(kernel.issue(resolve, &Capability::root())).unwrap();
    assert_eq!(repr.expiry, Expiry::Never);
    let flushed = block_on(kernel.issue(
        Request::new(Verb::Sink, iri(FLUSH_IRI)),
        &Capability::root(),
    ))
    .unwrap();
    assert_eq!(text(&flushed), "names stored: 0, dropped: 0\n");
}

/// A question about the past is not this month's traffic.
#[test]
fn an_as_of_answer_is_not_counted() {
    let store = Store::default();
    let space = bound(ikigai_name::space(), &store).bind(
        Exact::new(ikigai_name::HISTORY_IRI),
        FnEndpoint::new("history", |_: &Invocation<'_>| Ok(json(REGISTRY))),
    );
    let kernel =
        Kernel::new(Arc::new(space)).with_clock(Arc::new(Moving(AtomicU64::new(NOVEMBER))));
    let request = Request::new(Verb::Source, iri("urn:name:resolve"))
        .with_arg("path", ArgRef::Inline(b"resmud/core".to_vec()))
        .with_arg("as-of", ArgRef::Inline(b"2026-03-31".to_vec()));
    block_on(kernel.issue(request, &Capability::root())).unwrap();
    let count = block_on(kernel.issue(
        Request::new(
            Verb::Source,
            iri("urn:name:stat:namespace:resmud:period:2026-11"),
        ),
        &Capability::root(),
    ))
    .unwrap();
    assert_eq!(text(&count), "0\n");
}

/// The bare constructors have no space's buffer to write to, so they count nothing.
#[test]
fn a_face_bound_on_its_own_counts_nothing() {
    let store = Store::default();
    let space = bound(ikigai_core::EndpointSpace::new(), &store)
        .bind(Exact::new("urn:name:resolve"), ikigai_name::resolve());
    let kernel =
        Kernel::new(Arc::new(space)).with_clock(Arc::new(Moving(AtomicU64::new(NOVEMBER))));
    let request = Request::new(Verb::Source, iri("urn:name:resolve"))
        .with_arg("path", ArgRef::Inline(b"resmud/core".to_vec()));
    let repr = block_on(kernel.issue(request, &Capability::root())).unwrap();
    assert_eq!(repr.expiry, Expiry::Never);
}

/// A host written before `space()` bound the template still binds `doc()` beside it. The
/// space's own is reached first, so that host's names are still counted.
#[test]
fn a_host_that_also_binds_doc_still_counts() {
    let store = Store::default();
    let space = bound(ikigai_name::space(), &store)
        .bind(
            UriTemplate::parse(ikigai_name::DOC_TEMPLATE).unwrap(),
            ikigai_name::doc(),
        )
        .bind(
            Exact::new("urn:test:vocab"),
            FnEndpoint::new("vocab", |_: &Invocation<'_>| {
                Ok(Representation::new(
                    ReprType::new("text/turtle"),
                    VOCAB.as_bytes().to_vec(),
                ))
            }),
        );
    let kernel =
        Kernel::new(Arc::new(space)).with_clock(Arc::new(Moving(AtomicU64::new(NOVEMBER))));
    let root = Capability::root();
    block_on(kernel.issue(
        Request::new(Verb::Source, iri("urn:name:doc:resmud/core")),
        &root,
    ))
    .unwrap();
    let count = block_on(kernel.issue(
        Request::new(
            Verb::Source,
            iri("urn:name:stat:namespace:resmud:period:2026-11"),
        ),
        &root,
    ))
    .unwrap();
    assert_eq!(text(&count), "1\n");
}
