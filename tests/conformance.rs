//! The module recipe as one test: `ikigai-conformance` walks the seven endpoints
//! [`ikigai_name::space`] binds and reports every violation at once.
//!
//! ## The registry is a fixture — and it decides what is cacheable
//!
//! Every read face here is a pure function of the registry document (and, for
//! `document` and `docs`, of the vocabulary the namespace points at). None
//! declares a thread of its own: the registry is read THROUGH the kernel from
//! `urn:name:registry-source`, and the kernel folds that sub-resolution's expiry
//! and golden threads into the result. So every resolve is exactly as cacheable
//! as the registry resource — a claim this file pins in both directions:
//!
//! - **Threaded** ([`RegistryStore`] served `.cacheable()` under its own IRI, as
//!   an `ikigai-fs` cacheable mount or a watched file would): the five read faces
//!   are declared `cacheable` and held to a cache hit under the registry's thread
//!   ([`conforms`]).
//! - **Live** (the same store served uncacheable, as a file read with no watcher
//!   is): undeclared, the walk is clean; declared, the suite reports the downgrade
//!   on all five and nothing else ([`over_a_live_registry_nothing_is_cached`]) —
//!   the only way the ~2000× incident becomes visible, the types being identical.
//!
//! The half the suite cannot see (conformance PENDING #64): the probe's second
//! resolution IS the cache hit, so "byte-identical" never compares two
//! COMPUTATIONS. [`a_forced_recomputation_is_the_same_bytes`] cuts every thread
//! between two resolutions and asserts the bytes match; then edits the registry
//! (and the vocabulary), cuts, and asserts every face recomputed.
//!
//! ## Fixtures
//!
//! The suite's minimal scalar (`x`) is not a curated path, so `resolve`,
//! `document` and `docs` each take a [`Fixture`]; `claim` and `admin` take one
//! naming a prefix the registry does (or does not yet) hold. The registry seeds
//! `resmud` (hosted, what the read faces resolve) and `acme` (redirect, what
//! `admin`'s probes change) so the walk's own Sinks never break the faces
//! probed after them (PENDING #40). The claim fixture claims a third prefix.
//!
//! The docs face queries `urn:sparql:select` through the kernel. [`conforms`]
//! binds a stand-in that answers a canned result set after resolving `graph=`
//! (so the vocabulary's thread propagates); [`conforms_over_the_real_engine`]
//! walks the same suite with `ikigai-sparql` bound, holding the MODULE's
//! endpoints clean while recording the engine's own findings (0.1.7 predates its
//! adoption; 0.1.8 is not yet published).
//!
//! ## What the suite cannot see, pinned by hand
//!
//! - **Claim-time prefix exclusion** ([`a_claim_overlapping_a_prefix_is_refused_typed`]):
//!   an overlap in either direction is a typed, permanent `Denied` naming the
//!   claim in the way, nothing is written, and the path still resolves to the
//!   one namespace that holds it — resolution never arbitrates.
//! - **Declared outputs against what is served** (PENDING #11/#31,
//!   [`declared_outputs_are_the_media_types_served`]), Sinks and Delete fired by
//!   hand under the right grants.
//! - **Required means required** (PENDING #49, [`required_inputs_are_required`]).
//! - **The gates beyond the kernel's floor** (PENDING #46,
//!   [`the_gates_are_the_exact_scopes`]): ENFORCED proves the wildcard floor on
//!   `urn:cap:name:admin:*`; the per-prefix rule is reached only under a grant on
//!   ANOTHER prefix.
//! - **Error text echoes no registry body** (PENDING #67,
//!   [`errors_carry_no_registry_body`]).
//! - **A template binding the manifold cannot form** (PENDING, this module): the
//!   server binds `urn:name:doc:{path}` and `document` reads `path` from the
//!   binding, but declares it by value; [`bound_at_the_servers_template`] pins
//!   the one ARGSPECS finding that draws, so the limit is in the printed record.
//!
//! No module namespace: the only Turtle face (`document`) serves the hosted
//! document as stored, so its terms are the tenant's, not this module's (PENDING
//! #27). No opt-out on the module; the fixture store's Sink is opted out with
//! the reason printed. NAMES runs: every id is a kebab-case noun.

use async_trait::async_trait;
use futures::executor::block_on;
use ikigai_conformance::{Check, Fixture, Report, Suite};
use ikigai_core::{
    ActionSpec, ArgRef, ArgSpec, AsyncFnEndpoint, Capability, Description, Endpoint, Error, Exact,
    Expiry, FnEndpoint, Invocation, InvokeFuture, Iri, Kernel, ReprType, Representation, Request,
    Result as CoreResult, UriTemplate, Verb,
};
use ikigai_name::admin::admin_scope;
use ikigai_name::{Registry, Strategy, CAP_ADMIN_ANY, CAP_CLAIM, REGISTRY_IRI};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// The seven endpoints `space()` binds, by description id (which here equals
/// `name()` — PENDING #57) and IRI.
const REGISTRY: &str = "registry";
const RESOLVE: &str = "resolve";
const CLAIM: &str = "claim";
const ADMIN: &str = "admin";
const DOCS: &str = "docs";
const DOCUMENT: &str = "document";
const HEALTH: &str = "health";

const REGISTRY_FACE: &str = "urn:name:registry";
const RESOLVE_IRI: &str = "urn:name:resolve";
const CLAIM_IRI: &str = "urn:name:claim";
const ADMIN_IRI: &str = "urn:name:admin";
const DOCS_IRI: &str = "urn:name:docs";
const DOCUMENT_IRI: &str = "urn:name:document";
const HEALTH_IRI: &str = "urn:name:health";

/// The five read faces, every one `.cacheable()` and none with a thread of its
/// own: exactly as cacheable as the registry (and the vocabulary) they read.
const READ_FACES: [&str; 5] = [REGISTRY, RESOLVE, DOCS, DOCUMENT, HEALTH];

/// Where the fixtures bind, doubling as the golden threads they name.
const VOCAB_IRI: &str = "urn:conformance:vocab";
const SPARQL_IRI: &str = "urn:sparql:select";

/// The template the standalone server binds `document` at, beside the exact IRI.
const DOC_TEMPLATE: &str = "urn:name:doc:{path}";

/// Two namespaces: one hosted (what the read faces resolve) and one redirect
/// (what `admin`'s probes rewrite, so the hosted one is untouched by the walk).
const REGISTRY_JSON: &str = r#"{"namespaces":[
  {"prefix":"resmud","owner":"urn:cap:name:admin:resmud","strategy":"hosted","source":"urn:conformance:vocab"},
  {"prefix":"acme","owner":"urn:cap:name:admin:acme","strategy":"redirect","target":"https://acme.example/ns"}
]}"#;

/// The hosted document. Skolem subjects under the tenant's namespace, well-known
/// predicates and classes only — a blank node here would be the TENANT's
/// finding, surfacing through `document`'s Turtle face (PENDING #27).
const VOCAB: &str = r#"@prefix rm: <https://iriref.org/resmud/core#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
rm:Weapon a rdfs:Class ; rdfs:comment "Anything that can inflict harm." .
rm:damage a rdf:Property ; rdfs:domain rm:Weapon .
"#;

/// What the SPARQL stand-in answers for any query: the vocabulary's two terms,
/// in the results format `ikigai-sparql` emits.
const RESULTS: &str = r#"{"head":{"vars":["term","comment","domain"]},"results":{"bindings":[
  {"term":{"type":"uri","value":"https://iriref.org/resmud/core#Weapon"},
   "comment":{"type":"literal","value":"Anything that can inflict harm."}},
  {"term":{"type":"uri","value":"https://iriref.org/resmud/core#damage"},
   "domain":{"type":"uri","value":"https://iriref.org/resmud/core#Weapon"}}
]}}"#;

// --- the registry store ------------------------------------------------------

/// The operator's registry file, as a kernel resource: readable, writable
/// through `content`, every read counted and every write logged. `threaded`
/// serves it `.cacheable()` under its own IRI — the `ikigai-fs` cacheable-mount
/// convention, so a Sink through the kernel auto-cuts it; `false` serves it
/// live, as a file read with no watcher is. The module never learns which.
struct RegistryStore {
    text: Arc<RwLock<String>>,
    writes: Arc<Mutex<Vec<String>>>,
    reads: Arc<AtomicUsize>,
    threaded: bool,
}

#[async_trait]
impl Endpoint for RegistryStore {
    async fn invoke(&self, inv: &Invocation<'_>) -> CoreResult<Representation> {
        if inv.request.verb == Verb::Sink {
            let body = String::from_utf8_lossy(inv.inline_arg("content")?).into_owned();
            self.writes.lock().expect("write log").push(body.clone());
            *self.text.write().expect("registry lock") = body;
            return Ok(Representation::new(
                ReprType::new("text/plain"),
                b"stored".to_vec(),
            ));
        }
        self.reads.fetch_add(1, Ordering::SeqCst);
        let text = self.text.read().expect("registry lock").clone();
        let repr = Representation::new(ReprType::new("application/json"), text.into_bytes());
        Ok(if self.threaded {
            repr.cacheable().depends_on(REGISTRY_IRI)
        } else {
            repr
        })
    }

    fn name(&self) -> &str {
        "registry-source"
    }

    /// Walked beside the module's endpoints (PENDING #17), so it carries the
    /// contract a module endpoint must: a kebab-case id, per-verb actions, a
    /// classed `content` on the Sink, an output each.
    fn describe(&self) -> Description {
        Description::new("registry-source")
            .title("Conformance registry store")
            .summary("The registry document, served as a kernel resource for the walk.")
            .action(
                ActionSpec::new(Verb::Source)
                    .summary("the registry document")
                    .output("application/json"),
            )
            .action(
                ActionSpec::new(Verb::Sink)
                    .summary("replace the registry document")
                    .input(
                        ArgSpec::new("content")
                            .summary("the whole registry, as JSON")
                            .class(XSD_STRING),
                    )
                    .output("text/plain"),
            )
    }
}

/// The hosted vocabulary, under its own thread when `threaded`.
fn vocab(text: Arc<RwLock<String>>, threaded: bool) -> FnEndpoint {
    FnEndpoint::new("vocab", move |_: &Invocation<'_>| {
        let body = text.read().expect("vocab lock").clone();
        let repr = Representation::new(ReprType::new("text/turtle"), body.into_bytes());
        Ok(if threaded {
            repr.cacheable().depends_on(VOCAB_IRI)
        } else {
            repr
        })
    })
    .with_description(
        Description::new("vocab")
            .title("Conformance vocabulary")
            .summary("The hosted namespace document, served as a kernel resource for the walk.")
            .verb(Verb::Source)
            .output("text/turtle"),
    )
}

/// `urn:sparql:select` without the engine: resolves `graph=` through the kernel
/// (so the graph's expiry and thread fold into the answer, as the real engine's
/// federation does) and answers [`RESULTS`]. Counted, so a test can say whether
/// the docs face recomputed. A pure function of its inputs when `graph=` is
/// absent, which is what the walk's minimal call is — hence declared `pure`.
fn sparql_stand_in(calls: Arc<AtomicUsize>) -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("sparql-select", move |inv: &Invocation<'_>| {
        let calls = Arc::clone(&calls);
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            inv.inline_str("query")
                .map_err(|_| Error::MissingArgument("query".into()))?;
            if let Ok(graph) = inv.inline_str("graph") {
                let iri = Iri::parse(graph).map_err(|e| Error::Endpoint(e.to_string()))?;
                inv.source(&iri).await?;
            }
            Ok(Representation::new(
                ReprType::new("application/sparql-results+json"),
                RESULTS.as_bytes().to_vec(),
            )
            .cacheable())
        }) as InvokeFuture<'_>
    })
    .with_description(
        Description::new("sparql-select")
            .title("Conformance SPARQL stand-in")
            .summary("Answers a fixed result set after resolving the graph it is pointed at.")
            .verb(Verb::Source)
            .input(
                ArgSpec::new("query")
                    .summary("the SPARQL query")
                    .class(XSD_STRING),
            )
            .input(
                ArgSpec::new("graph")
                    .summary("the graph source IRI, resolved through the kernel")
                    .optional()
                    .class(XSD_STRING),
            )
            .output("application/sparql-results+json"),
    )
}

/// The module's space over one registry store and one vocabulary, with handles
/// to both texts (so a test can edit them), the write log, and the counters.
struct World {
    kernel: Kernel,
    registry: Arc<RwLock<String>>,
    vocabulary: Arc<RwLock<String>>,
    writes: Arc<Mutex<Vec<String>>>,
    registry_reads: Arc<AtomicUsize>,
    sparql_calls: Arc<AtomicUsize>,
}

impl World {
    fn registry_reads(&self) -> usize {
        self.registry_reads.load(Ordering::SeqCst)
    }

    fn sparql_calls(&self) -> usize {
        self.sparql_calls.load(Ordering::SeqCst)
    }

    fn writes(&self) -> Vec<String> {
        self.writes.lock().expect("write log").clone()
    }

    fn stored(&self) -> Registry {
        Registry::from_json(self.registry.read().expect("registry lock").as_bytes())
            .expect("the store holds a valid registry")
    }
}

/// `threaded` selects the store's kind (see the file docs); `template` also
/// binds `document` at the server's `urn:name:doc:{path}`; `engine` binds
/// `ikigai-sparql` in place of the stand-in.
fn world_with(threaded: bool, template: bool, engine: bool) -> World {
    let registry = Arc::new(RwLock::new(REGISTRY_JSON.to_string()));
    let vocabulary = Arc::new(RwLock::new(VOCAB.to_string()));
    let writes = Arc::new(Mutex::new(Vec::new()));
    let registry_reads = Arc::new(AtomicUsize::new(0));
    let sparql_calls = Arc::new(AtomicUsize::new(0));

    let mut space = ikigai_name::space();
    if template {
        space = space.bind(
            UriTemplate::parse(DOC_TEMPLATE).expect("a valid template"),
            ikigai_name::document(),
        );
    }
    let space = space
        .bind(
            Exact::new(REGISTRY_IRI),
            RegistryStore {
                text: Arc::clone(&registry),
                writes: Arc::clone(&writes),
                reads: Arc::clone(&registry_reads),
                threaded,
            },
        )
        .bind(
            Exact::new(VOCAB_IRI),
            vocab(Arc::clone(&vocabulary), threaded),
        );
    let kernel = if engine {
        Kernel::new(Arc::new(ikigai_core::Fallback::new(vec![
            Arc::new(space),
            Arc::new(ikigai_sparql::space()),
        ])))
    } else {
        Kernel::new(Arc::new(space.bind(
            Exact::new(SPARQL_IRI),
            sparql_stand_in(Arc::clone(&sparql_calls)),
        )))
    };
    World {
        kernel,
        registry,
        vocabulary,
        writes,
        registry_reads,
        sparql_calls,
    }
}

fn world(threaded: bool) -> World {
    world_with(threaded, false, false)
}

// --- requests -----------------------------------------------------------------

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    let mut request = Request::new(verb, Iri::parse(iri).expect("a valid IRI"));
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    request
}

fn issue(kernel: &Kernel, request: Request, capability: &Capability) -> CoreResult<Representation> {
    block_on(kernel.issue(request, capability))
}

/// The minimal call for each read face — the same inputs the walk's fixtures use.
fn read_request(id: &str) -> Request {
    match id {
        REGISTRY => request(Verb::Source, REGISTRY_FACE, &[]),
        RESOLVE => request(Verb::Source, RESOLVE_IRI, &[("path", "resmud/core")]),
        DOCS => request(Verb::Source, DOCS_IRI, &[("prefix", "resmud")]),
        DOCUMENT => request(Verb::Source, DOCUMENT_IRI, &[("path", "resmud/core")]),
        HEALTH => request(Verb::Source, HEALTH_IRI, &[]),
        other => panic!("not a read face: {other}"),
    }
}

/// A caller holding nothing at all.
fn nobody() -> Capability {
    Capability::scoped(Vec::<String>::new())
}

/// The signup gate and nothing else.
fn claimant() -> Capability {
    Capability::scoped([CAP_CLAIM])
}

/// One namespace's administrator and nothing else.
fn owner_of(prefix: &str) -> Capability {
    Capability::scoped([admin_scope(prefix)])
}

fn text(repr: &Representation) -> String {
    String::from_utf8(repr.bytes.clone()).expect("utf-8")
}

// --- the suite ------------------------------------------------------------------

/// The suite, configured for this module (see the file docs for why each line).
fn suite() -> Suite {
    Suite::new()
        .fixture(Fixture::new(RESOLVE, Verb::Source).arg("path", "resmud/core"))
        .fixture(
            Fixture::new(DOCUMENT, Verb::Source)
                .arg("path", "resmud/core")
                .binding("path", "resmud/core"),
        )
        .fixture(Fixture::new(DOCS, Verb::Source).arg("prefix", "resmud"))
        // A prefix nobody holds: the pipeline probe lands this claim under root.
        .fixture(
            Fixture::new(CLAIM, Verb::Sink)
                .arg("prefix", "example")
                .arg("strategy", "hosted")
                .arg("source", VOCAB_IRI),
        )
        // The redirect namespace, so the hosted one the read faces resolve is
        // untouched whatever the walk order (PENDING #40).
        .fixture(
            Fixture::new(ADMIN, Verb::Sink)
                .arg("prefix", "acme")
                .arg("strategy", "mirror")
                .arg("origin", "https://acme.example/ns"),
        )
        .fixture(Fixture::new(ADMIN, Verb::Delete).arg("prefix", "acme"))
        .opt_out(
            "registry-source",
            Some(Verb::Sink),
            "fixture store: fired directly, the walk's `content=x` would replace the registry \
             with a non-document; it is exercised through `claim`'s and `admin`'s probes and \
             asserted from the write log",
        )
}

/// The suite over the stand-in engine, which is a pure function of its inputs
/// when `graph=` is absent — the walk's minimal call.
fn stand_in_suite() -> Suite {
    suite().pure("sparql-select")
}

/// Every read face declared cacheable, so the threaded walk holds them to it.
fn declared(suite: Suite) -> Suite {
    READ_FACES.iter().fold(suite, |s, id| s.cacheable(*id))
}

/// The walk saw the seven module endpoints and the three fixtures, skipped
/// nothing, and opted out only the fixture store's Sink. An eighth module
/// endpoint bound without a line here would be held to a weaker standard.
fn assert_shape(report: &Report) {
    assert_eq!(
        report.endpoints, 10,
        "seven module endpoints, three fixtures: {report}"
    );
    // registry, resolve, docs, document, health: one Source each; claim: one Sink;
    // admin: Sink + Delete; the store: Source + Sink; vocab and sparql: one each.
    assert_eq!(report.actions, 12, "{report}");
    assert_eq!(
        report.checks.skipped().count(),
        0,
        "every check runs: {report}"
    );
    assert_eq!(report.declared.opted_out.len(), 1, "{report}");
    assert_eq!(report.declared.opted_out[0].endpoint, "registry-source");
    assert_eq!(report.declared.pure, ["sparql-select"], "{report}");
    assert!(report.declared.namespaces.is_empty(), "{report}");
}

/// The threaded registry: every read face declared cacheable and held to a cache
/// hit, byte-identical, under a non-empty thread set; the Sinks land exactly
/// once each under root and never under no grants; and the registry face —
/// cached before `claim` fired — serves the new claim afterwards, because a
/// Sink through the kernel cut the store's thread.
#[test]
fn conforms() {
    let w = world(true);
    let report = declared(stand_in_suite()).run_blocking(&w.kernel);
    // Printed even when clean (`--nocapture`): the report is the record.
    eprintln!("[threaded registry]\n{report}");
    assert!(report.is_clean(), "{report}");
    assert_shape(&report);
    assert_eq!(report.declared.cacheable, READ_FACES, "{report}");

    // The walk's footprint on the store (PENDING #13/#40/#60): the pipeline probe
    // fired `claim` once, `admin`'s Sink once AND its Delete once, under root —
    // every action declaring `content` is fired by PIPELINE, so #4's "Delete is
    // never fired" stops holding the moment rule 6 is applied to a Delete.
    // ENFORCED reached none of them (refused at the kernel's floor).
    let writes = w.writes();
    assert_eq!(writes.len(), 3, "three writes: {writes:#?}");
    let after_claim = Registry::from_json(writes[0].as_bytes()).expect("valid registry");
    assert_eq!(
        after_claim.lookup("example").map(|ns| &ns.strategy),
        Some(&Strategy::Hosted {
            source: VOCAB_IRI.into()
        }),
        "the claim landed"
    );
    let after_admin = Registry::from_json(writes[1].as_bytes()).expect("valid registry");
    assert_eq!(
        after_admin.lookup("acme").map(|ns| &ns.strategy),
        Some(&Strategy::Mirror {
            origin: "https://acme.example/ns".into()
        }),
        "the update landed"
    );
    let after_delete = Registry::from_json(writes[2].as_bytes()).expect("valid registry");
    assert_eq!(
        after_delete.lookup("acme").map(|ns| &ns.strategy),
        Some(&Strategy::Retired {
            reason: "withdrawn".into()
        }),
        "the retirement landed, with the default reason"
    );
    assert_eq!(w.stored(), after_delete, "the store holds the last write");
    assert_eq!(
        w.stored().lookup("resmud").map(|ns| &ns.strategy),
        Some(&Strategy::Hosted {
            source: VOCAB_IRI.into()
        }),
        "the hosted namespace the read faces resolve was untouched by the walk"
    );

    // The registry face was cached (and probed twice) BEFORE claim fired. That it
    // now carries `example` is the kernel's auto-cut on a Sink to the store's
    // IRI — the mechanism a file-backed deployment relies on.
    let face = issue(&w.kernel, read_request(REGISTRY), &Capability::root()).unwrap();
    assert!(
        text(&face).contains("\"example\""),
        "the cached registry face was cut by the walk's claim: {}",
        text(&face)
    );
    assert_eq!(
        text(
            &issue(
                &w.kernel,
                request(Verb::Source, RESOLVE_IRI, &[("path", "example/thing")]),
                &Capability::root()
            )
            .unwrap()
        ),
        format!("example\thosted\t{VOCAB_IRI}\n")
    );
}

/// The other registry: served uncacheable, as a file read with no watcher is.
/// Every read face still says `.cacheable()`, and the kernel hands each back
/// uncacheable — the effective expiry is the registry's — so every read
/// re-reads the registry and nothing is ever served from the cache. Undeclared,
/// that is correct and the walk is clean; DECLARED, the suite reports the
/// downgrade on all five and nothing else.
#[test]
fn over_a_live_registry_nothing_is_cached() {
    let w = world(false);
    for id in READ_FACES {
        let before = w.registry_reads();
        for _ in 0..2 {
            let repr = issue(&w.kernel, read_request(id), &Capability::root())
                .unwrap_or_else(|e| panic!("{id}: {e}"));
            assert_eq!(repr.expiry, Expiry::Always, "{id}: as live as its registry");
        }
        assert_eq!(
            w.registry_reads() - before,
            2,
            "{id}: every read re-reads the registry"
        );
        assert!(
            !w.kernel.is_cached(&read_request(id), &Capability::root()),
            "{id}: nothing over an uncacheable registry is cached"
        );
    }

    let report = stand_in_suite().run_blocking(&world(false).kernel);
    eprintln!("[live registry, undeclared]\n{report}");
    assert!(report.is_clean(), "{report}");
    assert_shape(&report);

    let report = declared(stand_in_suite()).run_blocking(&world(false).kernel);
    eprintln!("[live registry, declared cacheable]\n{report}");
    assert_eq!(report.findings.len(), READ_FACES.len(), "{report}");
    let mut flagged: Vec<&str> = report
        .findings
        .iter()
        .map(|f| {
            assert_eq!(f.check, Check::Cacheable, "{f}");
            assert_eq!(f.verb, Some(Verb::Source), "{f}");
            assert!(
                f.detail.contains("declared cacheable"),
                "the finding names the declaration: {f}"
            );
            f.endpoint.as_str()
        })
        .collect();
    flagged.sort_unstable();
    let mut expected = READ_FACES.to_vec();
    expected.sort_unstable();
    assert_eq!(flagged, expected, "{report}");
}

/// PENDING #64: the probe's second resolution is the cache hit, so it never
/// compares two computations. Here every thread is cut between two resolutions
/// of each read face — the second is a real recomputation, witnessed by the
/// registry read count — and the bytes are identical: each face is a function
/// of its inputs. Then the inputs change: a registry edit plus a cut recomputes
/// `registry`, `resolve` and `health` to different bytes; a vocabulary edit plus
/// a cut recomputes `document` and `docs` (the SPARQL stand-in's call count says
/// the page was re-rendered, not served).
#[test]
fn a_forced_recomputation_is_the_same_bytes() {
    let w = world(true);
    let root = Capability::root();

    for id in READ_FACES {
        let first =
            issue(&w.kernel, read_request(id), &root).unwrap_or_else(|e| panic!("{id}: {e}"));
        assert_ne!(
            first.expiry,
            Expiry::Always,
            "{id}: cacheable over a threaded registry"
        );
        let threads: Vec<String> = first.threads().iter().map(|t| t.to_string()).collect();
        assert!(
            threads.iter().any(|t| t == REGISTRY_IRI),
            "{id}: under the registry's thread, got {threads:?}"
        );
        if id == DOCUMENT || id == DOCS {
            assert!(
                threads.iter().any(|t| t == VOCAB_IRI),
                "{id}: under the vocabulary's thread too, got {threads:?}"
            );
        }
        assert!(w.kernel.is_cached(&read_request(id), &root), "{id}: cached");

        let reads = w.registry_reads();
        w.kernel.cut(REGISTRY_IRI);
        w.kernel.cut(VOCAB_IRI);
        assert!(
            !w.kernel.is_cached(&read_request(id), &root),
            "{id}: the cut evicted it"
        );
        let second = issue(&w.kernel, read_request(id), &root).unwrap();
        assert_eq!(
            w.registry_reads(),
            reads + 1,
            "{id}: the cut forced a recomputation"
        );
        assert_eq!(second.bytes, first.bytes, "{id}: a function of its inputs");
        assert_eq!(second.repr_type, first.repr_type, "{id}");
    }

    // The registry changes under the faces: the hosted namespace becomes a
    // mirror of the same graph, written the way an operator edits the file (no
    // Sink through the kernel), then the cut a watcher would issue. Nothing is
    // served stale, and the two graph faces still read the same document.
    let before: Vec<Vec<u8>> = [REGISTRY, RESOLVE, HEALTH]
        .iter()
        .map(|id| issue(&w.kernel, read_request(id), &root).unwrap().bytes)
        .collect();
    let mut edited = w.stored();
    edited.namespaces[0].strategy = Strategy::Mirror {
        origin: VOCAB_IRI.into(),
    };
    *w.registry.write().unwrap() = serde_json::to_string(&edited).unwrap();
    w.kernel.cut(REGISTRY_IRI);
    for (id, old) in [REGISTRY, RESOLVE, HEALTH].iter().zip(before) {
        let new = issue(&w.kernel, read_request(id), &root).unwrap().bytes;
        assert_ne!(new, old, "{id}: recomputed over the edited registry");
    }
    assert_eq!(
        text(&issue(&w.kernel, read_request(RESOLVE), &root).unwrap()),
        format!("resmud\tmirror\t{VOCAB_IRI}\n")
    );
    assert!(
        text(&issue(&w.kernel, read_request(HEALTH), &root).unwrap()).contains("\"mirrored\": 1")
    );

    // The vocabulary changes: the negotiated document and the page follow.
    let old_document = issue(&w.kernel, read_request(DOCUMENT), &root)
        .unwrap()
        .bytes;
    let old_page = issue(&w.kernel, read_request(DOCS), &root).unwrap().bytes;
    let calls = w.sparql_calls();
    w.vocabulary
        .write()
        .unwrap()
        .push_str("rm:Shield a rdfs:Class .\n");
    w.kernel.cut(VOCAB_IRI);
    let new_document = issue(&w.kernel, read_request(DOCUMENT), &root)
        .unwrap()
        .bytes;
    assert_ne!(
        new_document, old_document,
        "document recomputed over the edited vocabulary"
    );
    assert!(String::from_utf8_lossy(&new_document).contains("rm:Shield"));
    let new_page = issue(&w.kernel, read_request(DOCS), &root).unwrap().bytes;
    assert_eq!(
        w.sparql_calls(),
        calls + 1,
        "the page was re-rendered, not served"
    );
    // The stand-in answers the same terms whatever the graph says, so the page
    // is byte-identical — which is exactly the determinism claim, and the real
    // engine's half is `the_page_follows_the_vocabulary_through_the_real_engine`.
    assert_eq!(new_page, old_page);
}

/// The module's invariant, end to end through the kernel: a claim overlapping a
/// held prefix — above it, below it, or equal — is a typed, permanent `Denied`
/// naming the claim in the way; nothing is written; and the path resolves to the
/// one namespace that holds it, before and after. A prefix that merely shares
/// text (`resmudx`) is unrelated and claimable.
#[test]
fn a_claim_overlapping_a_prefix_is_refused_typed() {
    let w = world(true);
    let claim = |prefix: &str| {
        issue(
            &w.kernel,
            request(
                Verb::Sink,
                CLAIM_IRI,
                &[
                    ("prefix", prefix),
                    ("strategy", "redirect"),
                    ("target", "https://elsewhere.example/"),
                ],
            ),
            &claimant(),
        )
    };
    let resolve = |path: &str| {
        issue(
            &w.kernel,
            request(Verb::Source, RESOLVE_IRI, &[("path", path)]),
            &Capability::root(),
        )
        .map(|r| text(&r))
    };

    let answer = resolve("resmud/core").unwrap();
    assert_eq!(answer, format!("resmud\thosted\t{VOCAB_IRI}\n"));

    for overlapping in ["resmud", "resmud/core", "/resmud/", "acme/vocab/deep"] {
        let err = claim(overlapping).expect_err(overlapping);
        assert!(matches!(err, Error::Denied(_)), "{overlapping}: {err:?}");
        assert!(
            !err.is_transient(),
            "{overlapping}: permanent, retrying cannot help"
        );
        let held = if overlapping.starts_with("acme") {
            "acme"
        } else {
            "resmud"
        };
        assert!(
            err.to_string().contains(&format!("{held:?}")),
            "{overlapping}: names the claim in the way: {err}"
        );
    }
    assert!(w.writes().is_empty(), "a refused claim writes nothing");
    assert_eq!(w.stored().namespaces.len(), 2);

    // Claiming a parent of a held prefix would swallow it: refused the same way.
    let mut deeper = w.stored();
    deeper.namespaces.push(ikigai_name::Namespace {
        prefix: "org/unit".into(),
        owner: "urn:cap:name:admin:org-unit".into(),
        strategy: Strategy::Retired {
            reason: "ended".into(),
        },
    });
    *w.registry.write().unwrap() = serde_json::to_string(&deeper).unwrap();
    w.kernel.cut(REGISTRY_IRI);
    let err = claim("org").expect_err("a parent of a held (retired!) prefix");
    assert!(matches!(err, Error::Denied(_)), "{err:?}");
    assert!(err.to_string().contains("\"org/unit\""), "{err}");

    // Unrelated text is not an overlap.
    claim("resmudx").expect("shares letters, not segments");
    assert_eq!(w.writes().len(), 1);

    // Resolution never arbitrates: one namespace answers, the same one as before.
    assert_eq!(resolve("resmud/core").unwrap(), answer);
    assert!(resolve("resmudx/thing")
        .unwrap()
        .starts_with("resmudx\tredirect\t"));
    assert!(matches!(resolve("org").unwrap_err(), Error::NotFound(_)));
}

/// What `ikigai-conformance` 0.1.0 does not check (PENDING #11/#31): a declared
/// output that is not an RDF face is never compared with what the action serves.
/// Read by hand, then pinned for every action — Sinks and Delete fired under the
/// grants they need. `document` is compared with `as` omitted (its own choice,
/// Turtle) and with `as=text/html` (the second face it produces itself).
#[test]
fn declared_outputs_are_the_media_types_served() {
    let w = world(true);
    let root = Capability::root();
    let served: Vec<(&str, Verb, Representation)> = vec![
        (
            REGISTRY_FACE,
            Verb::Source,
            issue(&w.kernel, read_request(REGISTRY), &root).unwrap(),
        ),
        (
            RESOLVE_IRI,
            Verb::Source,
            issue(&w.kernel, read_request(RESOLVE), &root).unwrap(),
        ),
        (
            DOCS_IRI,
            Verb::Source,
            issue(&w.kernel, read_request(DOCS), &root).unwrap(),
        ),
        (
            DOCUMENT_IRI,
            Verb::Source,
            issue(&w.kernel, read_request(DOCUMENT), &root).unwrap(),
        ),
        (
            DOCUMENT_IRI,
            Verb::Source,
            issue(
                &w.kernel,
                request(
                    Verb::Source,
                    DOCUMENT_IRI,
                    &[("path", "resmud/core"), ("as", "text/html")],
                ),
                &root,
            )
            .unwrap(),
        ),
        (
            HEALTH_IRI,
            Verb::Source,
            issue(&w.kernel, read_request(HEALTH), &root).unwrap(),
        ),
        (
            CLAIM_IRI,
            Verb::Sink,
            issue(
                &w.kernel,
                request(
                    Verb::Sink,
                    CLAIM_IRI,
                    &[
                        ("prefix", "example"),
                        ("strategy", "hosted"),
                        ("source", VOCAB_IRI),
                    ],
                ),
                &claimant(),
            )
            .unwrap(),
        ),
        (
            ADMIN_IRI,
            Verb::Sink,
            issue(
                &w.kernel,
                request(
                    Verb::Sink,
                    ADMIN_IRI,
                    &[
                        ("prefix", "acme"),
                        ("strategy", "mirror"),
                        ("origin", "https://acme.example/ns"),
                    ],
                ),
                &owner_of("acme"),
            )
            .unwrap(),
        ),
        (
            ADMIN_IRI,
            Verb::Delete,
            issue(
                &w.kernel,
                request(
                    Verb::Delete,
                    ADMIN_IRI,
                    &[("prefix", "acme"), ("reason", "ended")],
                ),
                &owner_of("acme"),
            )
            .unwrap(),
        ),
    ];
    for (iri, verb, repr) in served {
        let description = w
            .kernel
            .describe_pattern(iri)
            .unwrap_or_else(|| panic!("{iri} describes itself"));
        let spec = description
            .action_specs()
            .into_iter()
            .find(|s| s.verb == verb)
            .unwrap_or_else(|| panic!("{iri} declares {verb:?}"));
        let got = ikigai_conformance::rdf::bare_media_type(&repr.repr_type.media_type);
        let declared: Vec<String> = spec
            .outputs
            .iter()
            .map(|o| ikigai_conformance::rdf::bare_media_type(o))
            .collect();
        assert!(
            declared.contains(&got),
            "{iri} {verb:?} served `{got}`, declared only {declared:?}"
        );
    }
    let document = w.kernel.describe_pattern(DOCUMENT_IRI).unwrap();
    assert_eq!(
        document.action_specs()[0].outputs,
        ["text/turtle", "text/html;charset=utf-8"],
        "document declares exactly the two faces it produces itself"
    );
}

/// PENDING #49: every input the manifold marks required IS required — dropped
/// from an otherwise valid call, a typed `MissingArgument` naming it, with no
/// write on the way — and every optional one is optional. The one substitute
/// the contract admits is the pipe: `content` in place of `path` on `resolve`
/// and `document`, and in place of `prefix` on `claim` and `admin`.
#[test]
fn required_inputs_are_required() {
    let w = world(true);
    let missing =
        |verb: Verb, iri: &str, args: &[(&str, &str)], cap: &Capability, expected: &str| {
            let err = issue(&w.kernel, request(verb, iri, args), cap)
                .err()
                .unwrap_or_else(|| panic!("{iri} {verb:?} resolved without `{expected}`"));
            assert!(
                matches!(&err, Error::MissingArgument(name) if name == expected),
                "{iri} {verb:?} without `{expected}`: {err:?}"
            );
            assert!(!err.is_transient(), "{err:?}");
        };
    let root = Capability::root();
    missing(Verb::Source, RESOLVE_IRI, &[], &root, "path");
    missing(
        Verb::Source,
        DOCUMENT_IRI,
        &[("as", "text/turtle")],
        &root,
        "path",
    );
    missing(
        Verb::Source,
        DOCS_IRI,
        &[("theme", "dark")],
        &root,
        "prefix",
    );
    missing(
        Verb::Sink,
        CLAIM_IRI,
        &[("strategy", "hosted"), ("source", VOCAB_IRI)],
        &claimant(),
        "prefix",
    );
    missing(
        Verb::Sink,
        CLAIM_IRI,
        &[("prefix", "example"), ("source", VOCAB_IRI)],
        &claimant(),
        "strategy",
    );
    // A strategy's companion is required BY the strategy, not by the manifold
    // (the manifold cannot say "source iff hosted"): still typed, still named.
    missing(
        Verb::Sink,
        CLAIM_IRI,
        &[("prefix", "example"), ("strategy", "hosted")],
        &claimant(),
        "source",
    );
    missing(
        Verb::Sink,
        ADMIN_IRI,
        &[("strategy", "mirror"), ("origin", "https://x.example/")],
        &owner_of("acme"),
        "prefix",
    );
    missing(
        Verb::Sink,
        ADMIN_IRI,
        &[("prefix", "acme")],
        &owner_of("acme"),
        "strategy",
    );
    missing(
        Verb::Delete,
        ADMIN_IRI,
        &[("reason", "ended")],
        &owner_of("acme"),
        "prefix",
    );
    assert!(
        w.writes().is_empty(),
        "a call refused for a missing input writes nothing"
    );

    // The manifold agrees: exactly these are required, everything else optional.
    let required_by_manifold = |iri: &str, verb: Verb| -> Vec<String> {
        w.kernel
            .describe_pattern(iri)
            .unwrap()
            .action_specs()
            .into_iter()
            .find(|s| s.verb == verb)
            .unwrap()
            .inputs
            .iter()
            .filter(|i| i.required)
            .map(|i| i.name.clone())
            .collect()
    };
    assert_eq!(required_by_manifold(RESOLVE_IRI, Verb::Source), ["path"]);
    assert_eq!(required_by_manifold(DOCUMENT_IRI, Verb::Source), ["path"]);
    assert_eq!(required_by_manifold(DOCS_IRI, Verb::Source), ["prefix"]);
    assert_eq!(
        required_by_manifold(HEALTH_IRI, Verb::Source),
        Vec::<String>::new()
    );
    assert_eq!(
        required_by_manifold(CLAIM_IRI, Verb::Sink),
        ["prefix", "strategy"]
    );
    assert_eq!(
        required_by_manifold(ADMIN_IRI, Verb::Sink),
        ["prefix", "strategy"]
    );
    assert_eq!(required_by_manifold(ADMIN_IRI, Verb::Delete), ["prefix"]);

    // Optional means optional: no `owner` (defaults to the claimant's scope), no
    // `reason` (defaults to "withdrawn"), no `as`/`theme`.
    let out = text(
        &issue(
            &w.kernel,
            request(
                Verb::Sink,
                CLAIM_IRI,
                &[
                    ("prefix", "example"),
                    ("strategy", "hosted"),
                    ("source", VOCAB_IRI),
                ],
            ),
            &claimant(),
        )
        .unwrap(),
    );
    assert_eq!(out, "claimed example\turn:cap:name:admin:example\n");
    let out = text(
        &issue(
            &w.kernel,
            request(Verb::Delete, ADMIN_IRI, &[("prefix", "acme")]),
            &owner_of("acme"),
        )
        .unwrap(),
    );
    assert_eq!(out, "retired acme\twithdrawn\n");

    // The pipe's spelling: `content` where `path` or `prefix` would be.
    let piped = |verb: Verb, iri: &str, args: &[(&str, &str)], cap: &Capability| {
        text(&issue(&w.kernel, request(verb, iri, args), cap).unwrap())
    };
    assert_eq!(
        piped(
            Verb::Source,
            RESOLVE_IRI,
            &[("content", "resmud/core\n")],
            &root
        ),
        format!("resmud\thosted\t{VOCAB_IRI}\n")
    );
    assert!(piped(
        Verb::Source,
        DOCUMENT_IRI,
        &[("content", "resmud/core")],
        &root
    )
    .contains("rm:Weapon"));
    assert_eq!(
        piped(
            Verb::Sink,
            CLAIM_IRI,
            &[
                ("content", "piped\n"),
                ("strategy", "redirect"),
                ("target", "https://piped.example/")
            ],
            &claimant()
        ),
        "claimed piped\turn:cap:name:admin:piped\n"
    );
    assert_eq!(
        piped(
            Verb::Sink,
            ADMIN_IRI,
            &[
                ("content", "piped"),
                ("strategy", "mirror"),
                ("origin", "https://piped.example/")
            ],
            &owner_of("piped")
        ),
        "updated piped\n"
    );
    assert_eq!(
        piped(
            Verb::Delete,
            ADMIN_IRI,
            &[("content", "piped"), ("reason", "done")],
            &owner_of("piped")
        ),
        "retired piped\tdone\n"
    );
    // By name wins over the pipe when both are present.
    let err = issue(
        &w.kernel,
        request(
            Verb::Delete,
            ADMIN_IRI,
            &[("prefix", "ghost"), ("content", "acme")],
        ),
        &owner_of("ghost"),
    )
    .unwrap_err();
    assert!(
        matches!(err, Error::NotFound(_)),
        "`prefix` was read, not `content`: {err:?}"
    );
}

/// Denied before anything is read or written. ENFORCED sees a typed `Denied`
/// under no grants — the KERNEL's floor on `urn:cap:name:claim` and on the
/// wildcard `urn:cap:name:admin:*` (PENDING #46). Pinned here beyond that: the
/// per-prefix rule the module enforces itself is reached only under a grant on
/// ANOTHER prefix, refuses typed and permanent, names the exact scope needed,
/// and writes nothing; and the signup gate confers no administration.
#[test]
fn the_gates_are_the_exact_scopes() {
    let w = world(true);
    let claim_args = [
        ("prefix", "example"),
        ("strategy", "hosted"),
        ("source", VOCAB_IRI),
    ];
    let admin_args = [
        ("prefix", "acme"),
        ("strategy", "mirror"),
        ("origin", "https://x.example/"),
    ];

    // The kernel's floor: no grant at all, and a grant on the wrong family.
    for capability in [
        nobody(),
        owner_of("acme"),
        Capability::scoped(["urn:cap:fs:read:*"]),
    ] {
        let err = issue(
            &w.kernel,
            request(Verb::Sink, CLAIM_IRI, &claim_args),
            &capability,
        )
        .expect_err("claim needs the signup gate");
        assert!(matches!(err, Error::Denied(_)), "{err:?}");
        assert!(!err.is_transient(), "{err:?}");
        assert!(
            err.to_string().contains(CAP_CLAIM),
            "names the scope: {err}"
        );
    }
    for capability in [nobody(), claimant()] {
        for verb in [Verb::Sink, Verb::Delete] {
            let err = issue(
                &w.kernel,
                request(verb, ADMIN_IRI, &admin_args),
                &capability,
            )
            .err()
            .unwrap_or_else(|| panic!("admin {verb:?} resolved under {capability:?}"));
            assert!(matches!(err, Error::Denied(_)), "{verb:?}: {err:?}");
            assert!(
                err.to_string().contains(CAP_ADMIN_ANY),
                "names the floor: {err}"
            );
        }
    }

    // The module's own rule: SOME admin grant passes the floor and is refused
    // here, naming the exact scope. Holding one namespace confers nothing over
    // another.
    for verb in [Verb::Sink, Verb::Delete] {
        let err = issue(
            &w.kernel,
            request(verb, ADMIN_IRI, &admin_args),
            &owner_of("resmud"),
        )
        .err()
        .unwrap_or_else(|| panic!("admin {verb:?} resolved under resmud's grant"));
        assert!(matches!(err, Error::Denied(_)), "{verb:?}: {err:?}");
        assert!(!err.is_transient(), "{err:?}");
        assert!(
            err.to_string().contains("urn:cap:name:admin:acme"),
            "names the scope needed: {err}"
        );
    }
    assert!(w.writes().is_empty(), "no refused call reached the store");
    assert_eq!(
        w.registry_reads(),
        0,
        "refused before the registry was read"
    );

    // The right grants, and nothing wider, do the work.
    issue(
        &w.kernel,
        request(Verb::Sink, CLAIM_IRI, &claim_args),
        &claimant(),
    )
    .unwrap();
    issue(
        &w.kernel,
        request(Verb::Sink, ADMIN_IRI, &admin_args),
        &owner_of("acme"),
    )
    .unwrap();
    assert_eq!(w.writes().len(), 2);
}

/// PENDING #67: error text is a face. The registry holds nothing secret, but a
/// malformed registry must not be echoed back — it is the operator's file, and
/// the error travels through traces, logs and MCP replies — and a refused claim
/// or update must not echo the registry either. Every error a caller can
/// provoke is searched for the registry's body and for its parse failure's.
#[test]
fn errors_carry_no_registry_body() {
    let w = world(true);
    let root = Capability::root();
    let body = w.registry.read().unwrap().clone();
    let leaks = |err: &Error| {
        let text = err.to_string();
        assert!(!text.contains(&body), "the registry body is echoed: {text}");
    };

    for (verb, iri, args, cap) in [
        (
            Verb::Source,
            RESOLVE_IRI,
            vec![("path", "nobody/here")],
            root.clone(),
        ),
        (
            Verb::Source,
            DOCS_IRI,
            vec![("prefix", "acme")],
            root.clone(),
        ),
        (
            Verb::Source,
            DOCUMENT_IRI,
            vec![("path", "acme/thing")],
            root.clone(),
        ),
        (
            Verb::Source,
            DOCUMENT_IRI,
            vec![("path", "resmud/core"), ("as", "image/png")],
            root.clone(),
        ),
        (
            Verb::Sink,
            CLAIM_IRI,
            vec![
                ("prefix", "resmud/x"),
                ("strategy", "hosted"),
                ("source", "urn:x"),
            ],
            claimant(),
        ),
        (
            Verb::Sink,
            CLAIM_IRI,
            vec![("prefix", "new"), ("strategy", "teleport")],
            claimant(),
        ),
        (
            Verb::Sink,
            ADMIN_IRI,
            vec![("prefix", "acme"), ("strategy", "retired")],
            owner_of("acme"),
        ),
        (
            Verb::Sink,
            ADMIN_IRI,
            vec![
                ("prefix", "ghost"),
                ("strategy", "hosted"),
                ("source", "urn:x"),
            ],
            owner_of("ghost"),
        ),
    ] {
        let err = issue(&w.kernel, request(verb, iri, &args), &cap)
            .err()
            .unwrap_or_else(|| panic!("{iri} {verb:?} {args:?} resolved"));
        leaks(&err);
    }

    // The registry itself malformed: the error names the RESOURCE to open, not
    // its contents.
    let garbage = "{ \"namespaces\": [ { \"prefix\": \"secret-tenant\" ";
    *w.registry.write().unwrap() = garbage.to_string();
    w.kernel.cut(REGISTRY_IRI);
    for id in READ_FACES {
        let err = issue(&w.kernel, read_request(id), &root).expect_err(id);
        let text = err.to_string();
        assert!(
            text.contains(REGISTRY_IRI),
            "{id}: names the resource: {text}"
        );
        assert!(
            !text.contains("secret-tenant"),
            "{id}: echoes the body: {text}"
        );
        assert!(!text.contains(garbage), "{id}: echoes the body: {text}");
    }
}

/// The server binds `document` at `urn:name:doc:{path}` beside the exact IRI,
/// and `document` reads `path` from the binding first. The description declares
/// `path` BY VALUE (it must — the exact binding takes it as an argument), so the
/// template entry draws exactly one ARGSPECS finding: the manifold cannot form
/// the templated IRI from the contract. One description cannot declare a name
/// as both argument and binding; recorded for the hub rather than routed around.
#[test]
fn bound_at_the_servers_template() {
    let w = world_with(true, true, false);
    let report = declared(stand_in_suite()).run_blocking(&w.kernel);
    eprintln!("[threaded registry, document also bound at {DOC_TEMPLATE}]\n{report}");
    assert_eq!(report.findings.len(), 1, "{report}");
    let finding = &report.findings[0];
    assert_eq!(finding.check, Check::ArgSpecs, "{report}");
    assert_eq!(finding.endpoint, DOCUMENT, "{report}");
    assert!(
        finding.detail.contains("template variable `path`"),
        "{finding}"
    );
    // The entry itself works: the binding carries the path, as the server relies on.
    let repr = issue(
        &w.kernel,
        request(Verb::Source, "urn:name:doc:resmud/core", &[]),
        &Capability::root(),
    )
    .unwrap();
    assert!(text(&repr).contains("rm:Weapon"));
    assert_eq!(
        report.endpoints, 10,
        "one description, two entries: {report}"
    );
    assert_eq!(report.actions, 13, "{report}");
}

/// The same suite over the real engine. The MODULE's endpoints are held clean —
/// the docs face's terms come from a real `SELECT … GRAPH <…>` — while the
/// engine's own findings are recorded, not hidden: `ikigai-sparql` 0.1.7 is what
/// crates.io serves and its inputs predate its adoption (ikigai-linkeddata #20,
/// 0.1.8 unpublished). Once the lock moves to 0.1.8 this walk is clean outright.
#[test]
fn conforms_over_the_real_engine() {
    let w = world_with(true, false, true);
    let forms = [
        ("sparql-select", "SELECT ?s WHERE { ?s ?p ?o } LIMIT 1"),
        ("sparql-ask", "ASK { ?s ?p ?o }"),
        (
            "sparql-describe",
            "DESCRIBE <https://ikigai-rs.dev/ns#Transreptor>",
        ),
        (
            "sparql-construct",
            "CONSTRUCT { ?s a ?c } WHERE { ?s a ?c }",
        ),
    ];
    let suite = forms.iter().fold(declared(suite()), |s, (id, query)| {
        s.fixture(Fixture::new(*id, Verb::Source).arg("query", *query))
            .pure(*id)
            .cacheable(*id)
    });
    let report = suite.run_blocking(&w.kernel);
    eprintln!("[threaded registry, real ikigai-sparql]\n{report}");
    let module: Vec<&ikigai_conformance::Finding> = report
        .findings
        .iter()
        .filter(|f| !f.endpoint.starts_with("sparql-"))
        .collect();
    assert!(
        module.is_empty(),
        "the module's own findings: {module:?}\n{report}"
    );
    for f in &report.findings {
        assert_eq!(
            f.check,
            Check::ArgSpecs,
            "the engine's residue is its untyped inputs: {f}"
        );
    }
    assert_eq!(
        report.endpoints, 13,
        "seven, two fixtures, four forms: {report}"
    );

    // The page is real: every term the vocabulary defines, none it does not.
    let page = text(&issue(&w.kernel, read_request(DOCS), &Capability::root()).unwrap());
    assert!(page.contains("Weapon") && page.contains("damage"), "{page}");
    assert!(page.contains("2 terms"), "{page}");
}

/// The determinism claim's other half, through the real engine (the stand-in
/// answers the same terms whatever the graph says): after a vocabulary edit
/// and a cut, the page documents the new term.
#[test]
fn the_page_follows_the_vocabulary_through_the_real_engine() {
    let w = world_with(true, false, true);
    let root = Capability::root();
    let first = issue(&w.kernel, read_request(DOCS), &root).unwrap();
    assert_ne!(
        first.expiry,
        Expiry::Always,
        "cacheable over threaded sources"
    );
    assert!(!text(&first).contains("Shield"));

    // Served from the cache until the cut.
    let again = issue(&w.kernel, read_request(DOCS), &root).unwrap();
    assert_eq!(again.bytes, first.bytes);
    w.vocabulary
        .write()
        .unwrap()
        .push_str("rm:Shield a rdfs:Class ; rdfs:comment \"Turns harm aside.\" .\n");
    let stale = issue(&w.kernel, read_request(DOCS), &root).unwrap();
    assert_eq!(stale.bytes, first.bytes, "no cut yet: the cached page");

    w.kernel.cut(VOCAB_IRI);
    let fresh = text(&issue(&w.kernel, read_request(DOCS), &root).unwrap());
    assert!(
        fresh.contains("Shield") && fresh.contains("3 terms"),
        "{fresh}"
    );
}

/// The contract as the manifold states it, in one place: verbs, scopes, the
/// inputs and their classes, the faces. A drift here is a contract change
/// consumers can see (the reason this adoption is a patch bump).
#[test]
fn the_manifold_states_the_contract() {
    let w = world(true);
    let s = XSD_STRING;
    /// One action as the manifold must state it.
    struct Action {
        iri: &'static str,
        id: &'static str,
        verb: Verb,
        requires: &'static [&'static str],
        /// `(name, required)`, in declaration order; every class is `xsd:string`.
        inputs: &'static [(&'static str, bool)],
        outputs: &'static [&'static str],
    }
    const TEXT: &[&str] = &["text/plain;charset=utf-8"];
    const JSON: &[&str] = &["application/json"];
    let contract = [
        Action {
            iri: REGISTRY_FACE,
            id: REGISTRY,
            verb: Verb::Source,
            requires: &[],
            inputs: &[],
            outputs: JSON,
        },
        Action {
            iri: RESOLVE_IRI,
            id: RESOLVE,
            verb: Verb::Source,
            requires: &[],
            inputs: &[("path", true)],
            outputs: TEXT,
        },
        Action {
            iri: CLAIM_IRI,
            id: CLAIM,
            verb: Verb::Sink,
            requires: &[CAP_CLAIM],
            inputs: &[
                ("prefix", true),
                ("content", false),
                ("strategy", true),
                ("source", false),
                ("target", false),
                ("origin", false),
                ("owner", false),
            ],
            outputs: TEXT,
        },
        Action {
            iri: ADMIN_IRI,
            id: ADMIN,
            verb: Verb::Sink,
            requires: &[CAP_ADMIN_ANY],
            inputs: &[
                ("prefix", true),
                ("content", false),
                ("strategy", true),
                ("source", false),
                ("target", false),
                ("origin", false),
            ],
            outputs: TEXT,
        },
        Action {
            iri: ADMIN_IRI,
            id: ADMIN,
            verb: Verb::Delete,
            requires: &[CAP_ADMIN_ANY],
            inputs: &[("prefix", true), ("content", false), ("reason", false)],
            outputs: TEXT,
        },
        Action {
            iri: DOCS_IRI,
            id: DOCS,
            verb: Verb::Source,
            requires: &[],
            inputs: &[("prefix", true), ("term", false), ("theme", false)],
            outputs: &["text/html;charset=utf-8"],
        },
        Action {
            iri: DOCUMENT_IRI,
            id: DOCUMENT,
            verb: Verb::Source,
            requires: &[],
            inputs: &[("path", true), ("as", false), ("theme", false)],
            outputs: &["text/turtle", "text/html;charset=utf-8"],
        },
        Action {
            iri: HEALTH_IRI,
            id: HEALTH,
            verb: Verb::Source,
            requires: &[],
            inputs: &[],
            outputs: JSON,
        },
    ];
    for Action {
        iri,
        id,
        verb,
        requires,
        inputs,
        outputs,
    } in contract
    {
        let description = w.kernel.describe_pattern(iri).unwrap();
        assert_eq!(description.id, id);
        let spec = description
            .action_specs()
            .into_iter()
            .find(|a| a.verb == verb)
            .unwrap_or_else(|| panic!("{id} declares {verb:?}"));
        assert_eq!(
            spec.requires, requires,
            "{id} {verb:?}: declared = enforced"
        );
        let declared: Vec<(&str, bool)> = spec
            .inputs
            .iter()
            .map(|i| {
                assert_eq!(
                    i.class.as_deref(),
                    Some(s),
                    "{id} {verb:?}: `{}` is a string",
                    i.name
                );
                (i.name.as_str(), i.required)
            })
            .collect();
        assert_eq!(
            declared, inputs,
            "{id} {verb:?}: inputs and whether required"
        );
        assert_eq!(spec.outputs, outputs, "{id} {verb:?}: the faces");
    }
    let strategy = |iri: &str, verb: Verb| {
        w.kernel
            .describe_pattern(iri)
            .unwrap()
            .action_specs()
            .into_iter()
            .find(|a| a.verb == verb)
            .unwrap()
            .inputs
            .into_iter()
            .find(|i| i.name == "strategy")
            .unwrap()
            .one_of
    };
    assert_eq!(
        strategy(CLAIM_IRI, Verb::Sink),
        ["hosted", "redirect", "mirror"]
    );
    assert_eq!(
        strategy(ADMIN_IRI, Verb::Sink),
        ["hosted", "redirect", "mirror"]
    );
}
