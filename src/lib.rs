//! `urn:name:*` — persistent-identifier resolution as an ikigai module.
//!
//! A curated IRI outlives the machine, the domain and the organization that
//! first served it. This module is the resolver behind such an IRI: it maps a
//! claimed namespace onto the answer for it, and — because resolution runs
//! through the kernel — that mapping is itself a resource, editable without a
//! rebuild and golden-threaded so an edit takes effect on the next request.
//!
//! ## Resolution is not redirection
//!
//! A redirect service can express exactly one strategy. This one names three
//! ([`Strategy`]): documents **hosted** here, a **redirect** to the owner's own
//! node, or a **mirror** whose origin remains the source of truth. Being able to
//! answer all three is the difference between a resolver and a forwarding table,
//! and it is what lets a namespace survive its origin going dark.
//!
//! ## Tenancy
//!
//! Namespaces are owned. Each carries the capability that administers it, and
//! prefixes cannot overlap — enforced when a claim is made rather than
//! arbitrated when a request arrives (see [`registry`]). Two tenants therefore
//! cannot answer for the same IRI, by construction rather than by policy.
//!
//! One deployment can also answer for several hosts, each with its own registry
//! and documents. The host a request is for travels on its capability; see
//! [`tenant`].
//!
//! ## Endpoints
//!
//! | IRI | verb | what |
//! |---|---|---|
//! | `urn:name:registry` | Source | the parsed registry, as JSON |
//! | `urn:name:resolve` | Source | how a path or an IRI resolves, as one line, or an error naming why not |
//! | `urn:name:resolution:iri:{iri}` | Source | the same, as data: JSON, Turtle or that line |
//! | `urn:name:resolution` | Source | the same, for `iri=` |
//! | `urn:name:delegate` | Source | forward a name to an upstream resolver |
//! | `urn:name:health` | Source | what this deployment holds, and what it refuses |
//! | `urn:name:stat:namespace:{prefix}:period:{yyyy-mm}` | Source | how many distinct names a namespace resolved in a month |
//! | `urn:name:stat:namespace:{prefix}` | Source | that count for every month held |
//! | `urn:name:seen-flush` | Sink | store the names buffered since the last flush |
//! | `urn:name:git:{prefix}` | Source | the last good copy of a repository's file |
//! | `urn:name:git-refresh:{prefix}` | Sink | read the repository again, and accept or refuse what it finds |
//! | `urn:name:git-status:{prefix}` | Source | what those refreshes found, for the namespace's administrator |
//!
//! `urn:name:registry`, `urn:name:resolve` and `urn:name:document` also answer as
//! of a date, from the vocabulary's history rather than any cache; see [`history`].
//! The counts are in [`stat`]. A namespace served from a git repository is in [`git`].

#![deny(missing_docs)]

pub mod admin;
pub mod docs;
pub mod document;
pub mod git;
pub mod health;
pub mod history;
pub mod iri;
pub mod registry;
pub mod resolution;
pub mod rules;
pub mod stat;
pub mod tenant;

pub use admin::{admin, claim, CAP_ADMIN_ANY, CAP_CLAIM};
pub use docs::{docs, style};
pub use document::{doc, document, DOC_TEMPLATE};
pub use git::Repository;
pub use health::health;
use history::{as_of_arg, as_of_input, load_as_of, AsOf};
pub use history::{repo_history, HISTORY_IRI};
use ikigai_core::{
    ArgSpec, AsyncFnEndpoint, ContentId, Description, Error, Exact, Invocation, InvokeFuture, Iri,
    ReprType, Representation, Result, UriTemplate, Verb,
};
pub use registry::{
    ClaimError, LoadError, Namespace, RedirectStatus, Redirection, Registry, Rule, Strategy,
};
pub use resolution::{
    delegate, resolution_at_iri, resolution_endpoint, Resolution, DELEGATE_IRI, RESOLUTION_IRI,
    RESOLUTION_TEMPLATE,
};
use stat::Seen;
pub use stat::{CAP_FLUSH, FLUSH_IRI, SEEN_TEMPLATE};
use std::sync::Arc;

/// The capability gating administration of a namespace. Real grants attenuate
/// it per prefix (`urn:cap:name:admin:resmud`); the bare form is the wildcard
/// that such grants hang under.
pub const CAP_ADMIN: &str = "urn:cap:name:admin";

/// Where the registry is read from. A bound IRI rather than a path, so an
/// operator can point it at a file, a remote node, or a store without this
/// module knowing which.
///
/// A writable binding must implement atomic conditional replacement: its Sink
/// accepts `content` (the new JSON) and `if-match` (the content ID of the Source
/// representation read before editing). It must compare against the current
/// Source content ID and replace as one operation, or refuse with
/// [`Error::Conflict`] naming the current token. It must never ignore `if-match`.
/// Read-only bindings need only implement Source.
pub const REGISTRY_IRI: &str = "urn:name:registry-source";

/// The XSD `string` datatype IRI — the `class` of a path argument.
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

const APPLICATION_JSON: &str = "application/json";
const TEXT_PLAIN_UTF8: &str = "text/plain;charset=utf-8";

/// Read and parse the registry back through the kernel.
///
/// Deliberately not a filesystem read: going through the kernel is what makes
/// the registry golden-threaded (an edit cuts the thread and the next resolve
/// sees it), keeps this module wasm-clean, and lets a deployment re-bind
/// [`REGISTRY_IRI`] to a different backing without touching this code — which
/// is also the migration path to a real store when file-and-page-cache stops
/// being enough.
pub(crate) async fn load(inv: &Invocation<'_>) -> Result<Registry> {
    Ok(load_versioned(inv).await?.0)
}

/// Keep the exact Source representation's identity alongside the parsed data.
/// Hashing reserialized JSON would change the token when formatting changes.
pub(crate) async fn load_versioned(inv: &Invocation<'_>) -> Result<(Registry, ContentId)> {
    let iri = Iri::parse(REGISTRY_IRI).map_err(|e| Error::Endpoint(format!("name: {e}")))?;
    let repr = inv.source(&iri).await?;
    // Name the resource, since the operator edits it by hand and a bare
    // serde message gives no clue which file to open. Malformed JSON and an
    // overlapping pair of prefixes are the same failure here: a bad file, and
    // nothing from it is served.
    Registry::from_json(&repr.bytes)
        .map(|registry| (registry, repr.content_id()))
        .map_err(|e| Error::Endpoint(format!("name: {REGISTRY_IRI} is not a valid registry: {e}")))
}

/// The `path` argument, with the piped-`content` fallback every pipeline
/// citizen offers. Neither present is a typed `MissingArgument` naming `path`
/// — the contract's name for it — so a caller (or `urn:kernel:validate`) can
/// act on it rather than parse prose.
fn path_arg(inv: &Invocation<'_>) -> Result<String> {
    let raw = inv
        .inline_str("path")
        .or_else(|_| inv.inline_str("content"))
        .map_err(|_| Error::MissingArgument("path".into()))?;
    Ok(raw.trim().to_string())
}

/// The namespace claiming `path`, or a permanent absence.
///
/// A path in no claimed namespace is permanently absent rather than temporarily
/// unavailable — nothing about retrying changes it — so this is `NotFound` and
/// not `Unavailable`.
///
/// A namespace that is not public is absent to a capability not holding its owner, in
/// exactly the words an unclaimed path is (see [`resolution`]).
pub(crate) fn resolve_path<'r>(
    registry: &'r Registry,
    path: &str,
    capability: &ikigai_core::Capability,
    at: Option<&AsOf>,
) -> Result<&'r registry::Namespace> {
    registry
        .lookup(path)
        .filter(|ns| resolution::visible(ns, capability))
        .ok_or_else(|| resolution::unclaimed(path, at))
}

/// The registry a read face answers from: as of the date it was asked about, or
/// the current one.
pub(crate) async fn load_at(inv: &Invocation<'_>, at: Option<&AsOf>) -> Result<Registry> {
    match at {
        Some(at) => load_as_of(inv, at).await,
        None => load(inv).await,
    }
}

/// Cacheable only for the present: a past answer is read from history every time,
/// because the cache is not an archive (see [`history`]).
pub(crate) fn cacheable_unless_past(repr: Representation, at: Option<&AsOf>) -> Representation {
    match at {
        Some(_) => repr,
        None => repr.cacheable(),
    }
}

/// `urn:name:registry` — the registry as JSON.
///
/// Cacheable as a pure re-serialization of its source — freshness is the
/// registry resource's business, not this endpoint's. A file-backed registry
/// with a watcher therefore caches and invalidates on edit; one without a
/// watcher is uncacheable and clamps this to uncacheable too.
pub fn registry_endpoint() -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("registry", |inv: &Invocation<'_>| -> InvokeFuture<'_> {
        Box::pin(async move {
            let at = as_of_arg(inv)?;
            let registry = load_at(inv, at.as_ref()).await?;
            let body = serde_json::to_vec_pretty(&registry)
                .map_err(|e| Error::Endpoint(format!("name: {e}")))?;
            Ok(cacheable_unless_past(
                Representation::new(ReprType::new(APPLICATION_JSON), body),
                at.as_ref(),
            ))
        })
    })
    .with_description(
        Description::new("registry")
            .title("The namespace registry")
            .summary(
                "Every claimed namespace: its prefix, the capability that administers it, and \
                 how it resolves. Read through the kernel from urn:name:registry-source, so an \
                 operator edits it without a rebuild.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(as_of_input())
            .output(APPLICATION_JSON),
    )
}

/// `urn:name:resolve` — how a path resolves.
///
/// Bound on its own this counts nothing: the counted one is what [`space`] binds,
/// because the names it sees belong to that space's buffer.
pub fn resolve() -> AsyncFnEndpoint {
    resolve_counted_by(None)
}

fn resolve_counted_by(seen: Option<Arc<Seen>>) -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("resolve", move |inv: &Invocation<'_>| -> InvokeFuture<'_> {
        let seen = seen.clone();
        Box::pin(async move {
            let path = path_arg(inv)?;
            // The same resolution `urn:name:resolution` answers, as its one line.
            resolution::resolve_in(inv, seen.as_deref(), &path, TEXT_PLAIN_UTF8).await
        })
    })
    .with_description(
        Description::new("resolve")
            .title("Resolve a curated path")
            .summary(
                "Reports the namespace claiming this path and how it answers — hosted, \
                 redirect or mirror — or an error naming the path if no namespace claims it. A \
                 redirect also reports its status, and the match of the rule that answered, if \
                 one did.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(
                ArgSpec::new("path")
                    .summary("the curated path, e.g. resmud/core (piped content is the fallback)")
                    .class(XSD_STRING),
            )
            .input(as_of_input())
            .output(TEXT_PLAIN_UTF8),
    )
}

/// The name [`space`] claims: `urn:iki:space:name`.
pub const SPACE_ID: &str = "urn:iki:space:name";

/// Every `urn:name:*` endpoint, ready to bind into a host.
///
/// Configuration-free (it reads nothing while building: the registry is read
/// through the kernel at request time), so it names itself [`SPACE_ID`]. A host
/// that binds more doors onto it gets an anonymous space back, since core
/// 0.1.89, and names the result itself.
///
/// Each call holds its own buffer of names resolved (see [`stat`]), shared by the
/// faces that resolve a name and the faces that report them, which is why the
/// template face [`doc`] is bound here rather than by each host.
pub fn space() -> ikigai_core::EndpointSpace {
    let seen = Arc::new(Seen::default());
    let template = |pattern: &str| UriTemplate::parse(pattern).expect("a valid template");
    let (by_argument, by_iri) = resolution::counted(seen.clone());
    ikigai_core::EndpointSpace::new()
        .bind(Exact::new("urn:name:registry"), registry_endpoint())
        .bind(
            Exact::new("urn:name:resolve"),
            resolve_counted_by(Some(seen.clone())),
        )
        .bind(Exact::new(RESOLUTION_IRI), by_argument)
        .bind(template(RESOLUTION_TEMPLATE), by_iri)
        .bind(Exact::new(DELEGATE_IRI), delegate())
        .bind(Exact::new("urn:name:claim"), claim())
        .bind(Exact::new("urn:name:admin"), admin())
        .bind(Exact::new("urn:name:docs"), docs())
        .bind(Exact::new(docs::STYLE_IRI), style())
        .bind(
            Exact::new("urn:name:document"),
            document::document_counted_by(Some(seen.clone())),
        )
        .bind(
            template(DOC_TEMPLATE),
            document::doc_counted_by(Some(seen.clone())),
        )
        .bind(Exact::new("urn:name:health"), health())
        // The count first: the report's trailing `{prefix}` would capture it too.
        .bind(template(stat::COUNT_TEMPLATE), stat::count(seen.clone()))
        .bind(template(stat::REPORT_TEMPLATE), stat::report(seen.clone()))
        .bind(Exact::new(FLUSH_IRI), stat::flush(seen))
        .bind(template(git::SOURCE_TEMPLATE), git::source())
        .bind(template(git::REFRESH_TEMPLATE), git::refresh())
        .bind(template(git::STATUS_TEMPLATE), git::status())
        .named(ikigai_core::space_iri("name"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use ikigai_core::{ArgRef, Capability, Endpoint, FnEndpoint, Kernel, Request};
    use std::sync::Arc;

    const REGISTRY_JSON: &str = r#"{
      "namespaces": [
        {
          "prefix": "resmud",
          "owner": "urn:cap:name:admin:resmud",
          "strategy": "hosted",
          "source": "urn:file:resmud-vocab"
        },
        {
          "prefix": "acme",
          "owner": "urn:cap:name:admin:acme",
          "strategy": "redirect",
          "target": "https://acme.example/ns"
        }
      ]
    }"#;

    /// A kernel whose registry source is `json`, standing in for the operator's
    /// file. Binding it as an ordinary endpoint is the point: the module never
    /// learns whether the registry came from a file, a peer, or a store.
    fn kernel_with(json: &'static str) -> Kernel {
        let source = FnEndpoint::new("registry-source", move |_: &Invocation<'_>| {
            Ok(Representation::new(
                ReprType::new(APPLICATION_JSON),
                json.as_bytes().to_vec(),
            ))
        });
        Kernel::new(Arc::new(space().bind(Exact::new(REGISTRY_IRI), source)))
    }

    fn resolve_path(kernel: &Kernel, path: &str) -> Result<String> {
        let repr = block_on(
            kernel.issue(
                Request::new(Verb::Source, Iri::parse("urn:name:resolve").unwrap())
                    .with_arg("path", ArgRef::Inline(path.as_bytes().to_vec())),
                &Capability::root(),
            ),
        )?;
        Ok(String::from_utf8(repr.bytes.clone()).expect("utf-8"))
    }

    #[test]
    fn a_hosted_namespace_resolves_to_its_source() {
        let out = resolve_path(&kernel_with(REGISTRY_JSON), "resmud/core").expect("resolves");
        assert_eq!(out, "resmud\thosted\turn:file:resmud-vocab\n");
    }

    #[test]
    fn a_redirected_namespace_resolves_to_its_target() {
        let out = resolve_path(&kernel_with(REGISTRY_JSON), "acme/thing").expect("resolves");
        assert_eq!(out, "acme\tredirect\thttps://acme.example/ns\t302\n");
    }

    const RULES_JSON: &str = r#"{"namespaces":[{
      "prefix": "legacy", "owner": "urn:cap:name:admin:legacy",
      "strategy": "redirect", "target": "https://new.example/", "status": 303,
      "rules": [
        {"match": "old/{x}/{y}", "target": "https://new.example/{y}/{x}", "status": 301},
        {"match": "docs/{rest*}", "target": "https://new.example/d/{rest}"}
      ]
    }]}"#;

    /// The status is always there; the rule is there when one answered.
    #[test]
    fn resolve_names_the_status_and_the_rule_that_answered() {
        let kernel = kernel_with(RULES_JSON);
        for (path, want) in [
            (
                "legacy/old/a/b",
                "legacy\tredirect\thttps://new.example/b/a\t301\told/{x}/{y}\n",
            ),
            (
                "legacy/docs/a/b",
                "legacy\tredirect\thttps://new.example/d/a/b\t303\tdocs/{rest*}\n",
            ),
            (
                "legacy/other",
                "legacy\tredirect\thttps://new.example/\t303\n",
            ),
            // A request cannot reach the host, however it is written.
            (
                "legacy/old/..",
                "legacy\tredirect\thttps://new.example/\t303\n",
            ),
            (
                "legacy/old/%2F/%2E%2E",
                "legacy\tredirect\thttps://new.example/%252E%252E/%252F\t301\told/{x}/{y}\n",
            ),
            (
                "legacy/docs/https://evil.example",
                "legacy\tredirect\thttps://new.example/d/https%3A/evil.example\t303\t\
                 docs/{rest*}\n",
            ),
        ] {
            assert_eq!(
                resolve_path(&kernel, path).expect("resolves"),
                want,
                "{path}"
            );
        }
    }

    /// A status the list refuses stops everything in the file, like an overlap.
    #[test]
    fn nothing_resolves_from_a_registry_with_a_bad_status() {
        let kernel = kernel_with(
            r#"{"namespaces":[
              {"prefix":"resmud","owner":"o","strategy":"hosted","source":"urn:file:x"},
              {"prefix":"acme","owner":"o","strategy":"redirect","target":"https://a.example/",
               "status":404}
            ]}"#,
        );
        for path in ["resmud/core", "acme/thing"] {
            let err = resolve_path(&kernel, path).expect_err("refused whole");
            assert!(
                matches!(&err, Error::Endpoint(m) if m.contains("\"acme\"") && m.contains("404")),
                "{path}: {err:?}"
            );
        }
    }

    /// An unclaimed path is permanently absent, not transiently unavailable —
    /// the distinction a caller needs in order to decide whether retrying could
    /// ever help.
    #[test]
    fn an_unclaimed_path_is_not_found() {
        let err = resolve_path(&kernel_with(REGISTRY_JSON), "nobody/here")
            .expect_err("nothing claims it");
        assert!(
            matches!(err, Error::NotFound(_)),
            "want a permanent NotFound, got: {err:?}"
        );
        assert!(
            err.to_string().contains("nobody/here"),
            "names the path: {err}"
        );
    }

    /// A registry file with overlapping prefixes is refused as a whole at load,
    /// so through the kernel NOTHING in it resolves — not the parent, not the
    /// child, not an unrelated entry. Before this, `resmud/core` resolved to
    /// whichever entry came first in the file.
    #[test]
    fn nothing_resolves_from_a_registry_with_overlapping_prefixes() {
        const OVERLAPPING: &str = r#"{
          "namespaces": [
            {
              "prefix": "resmud",
              "owner": "urn:cap:name:admin:resmud",
              "strategy": "hosted",
              "source": "urn:file:resmud-vocab"
            },
            {
              "prefix": "resmud/core",
              "owner": "urn:cap:name:admin:resmud-core",
              "strategy": "redirect",
              "target": "https://resmud.example/core"
            },
            {
              "prefix": "acme",
              "owner": "urn:cap:name:admin:acme",
              "strategy": "redirect",
              "target": "https://acme.example/ns"
            }
          ]
        }"#;
        let kernel = kernel_with(OVERLAPPING);
        for path in ["resmud/core", "resmud", "acme/thing"] {
            let err = resolve_path(&kernel, path).expect_err("a refused registry serves nothing");
            assert!(
                matches!(&err, Error::Endpoint(m) if m.contains("\"resmud/core\"") && m.contains("\"resmud\"")),
                "{path}: want the load refusal naming both prefixes, got: {err:?}"
            );
        }
    }

    #[test]
    fn the_claimed_prefix_itself_resolves() {
        let out = resolve_path(&kernel_with(REGISTRY_JSON), "resmud").expect("resolves");
        assert!(out.starts_with("resmud\thosted"), "got: {out}");
    }

    /// Piped input is the pipeline convention; `path` is only the named form.
    #[test]
    fn content_is_the_fallback_for_path() {
        let repr = block_on(
            kernel_with(REGISTRY_JSON).issue(
                Request::new(Verb::Source, Iri::parse("urn:name:resolve").unwrap())
                    .with_arg("content", ArgRef::Inline(b"resmud/core\n".to_vec())),
                &Capability::root(),
            ),
        )
        .expect("resolves");
        assert_eq!(
            String::from_utf8(repr.bytes.clone()).unwrap(),
            "resmud\thosted\turn:file:resmud-vocab\n"
        );
    }

    /// A hand-edited registry WILL be malformed sometimes, and the operator has
    /// to be told which resource to open.
    #[test]
    fn a_malformed_registry_names_the_resource() {
        let err = resolve_path(&kernel_with("{ not json"), "resmud/core").expect_err("bad json");
        let text = err.to_string();
        assert!(text.contains(REGISTRY_IRI), "names the source: {text}");
    }

    /// A missing path is a typed `MissingArgument` naming the input the contract
    /// declares, not prose a caller would have to parse.
    #[test]
    fn resolve_with_no_path_names_the_missing_input() {
        let err = block_on(kernel_with(REGISTRY_JSON).issue(
            Request::new(Verb::Source, Iri::parse("urn:name:resolve").unwrap()),
            &Capability::root(),
        ))
        .expect_err("no path");
        assert!(
            matches!(&err, Error::MissingArgument(name) if name == "path"),
            "got: {err:?}"
        );
    }

    #[test]
    fn the_registry_endpoint_serves_the_parsed_registry() {
        let repr = block_on(kernel_with(REGISTRY_JSON).issue(
            Request::new(Verb::Source, Iri::parse("urn:name:registry").unwrap()),
            &Capability::root(),
        ))
        .expect("serves");
        let parsed = Registry::from_json(&repr.bytes).expect("valid JSON");
        assert_eq!(parsed.namespaces.len(), 2);
    }

    /// The description is the routing and projection contract, not decoration:
    /// the engine routes named arguments by it and MCP projects it.
    #[test]
    fn resolve_declares_its_contract() {
        let described = resolve().describe();
        let rendered = format!("{described:?}");
        assert!(
            rendered.contains("path"),
            "declares its path arg: {rendered}"
        );
        assert!(rendered.contains("Source"), "declares Source: {rendered}");
    }
}
