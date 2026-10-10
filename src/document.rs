//! The negotiated face: one IRI, many representations.
//!
//! `https://iriref.org/resmud/core` has to answer a browser with a page, an RDF
//! tool with Turtle, and a JavaScript client with JSON-LD — **without any of
//! them using a different IRI**, because a term's identity is its IRI and a
//! `.ttl` suffix would fork it into three.
//!
//! ## Only one format is produced here
//!
//! Turtle is the hub, so it is served as stored. HTML is produced by asking
//! [`crate::docs`](mod@crate::docs) through the kernel. Everything else is reached by *selecting
//! a transreptor chain* and driving it — the same mechanism the kernel's own
//! `Meta` path uses, which means this face gains every format the host has a
//! transreptor for and carries conversion code for none of them.
//!
//! ## Caching, and where the ETag comes from
//!
//! A negotiated document is a pure function of the stored bytes and the
//! requested type, so it is cacheable and inherits the golden threads of the
//! document it came from: editing the vocabulary cuts the thread and every
//! negotiated form recomputes.
//!
//! The **ETag is deliberately not minted here**. A representation already
//! carries a content address, and the HTTP face is what turns that into an
//! `ETag` and answers `If-None-Match` — inventing a second one at this layer
//! would give the same bytes two identities.

use crate::registry::Strategy;
use crate::{load, resolve_path};
use ikigai_core::{
    ActionSpec, ArgRef, ArgSpec, AsyncFnEndpoint, Description, Error, Invocation, InvokeFuture,
    Iri, Request, Verb,
};

/// Turtle: the hub representation, and the default when nothing is asked for.
pub const CANONICAL: &str = "text/turtle";
/// The HTML face is produced by [`crate::docs`](mod@crate::docs), not by a transreptor.
pub const HTML: &str = "text/html";

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const TEXT_HTML_UTF8: &str = "text/html;charset=utf-8";

/// The media type alone, without parameters — `text/turtle; charset=utf-8`
/// negotiates as `text/turtle`.
fn bare(media_type: &str) -> &str {
    media_type.split(';').next().unwrap_or(media_type).trim()
}

/// The IRI template a host binds [`doc`] at: the trailing variable captures the
/// rest of the IRI, slashes and all, so `urn:name:doc:resmud/core` carries the
/// path `resmud/core` in its own identity.
pub const DOC_TEMPLATE: &str = "urn:name:doc:{path}";

/// Where an entry's `path` arrives from, which is a property of how it is
/// BOUND rather than of what it does — hence two descriptions over one handler.
#[derive(Clone, Copy)]
enum PathFrom {
    /// An exact IRI (`urn:name:document`): the path is a by-value argument.
    Argument,
    /// [`DOC_TEMPLATE`]: the path is the template's `{path}` binding.
    Binding,
}

/// The document behind a namespace, in the representation asked for, at an
/// exact IRI: the path is an argument (`path=resmud/core`, or piped content).
///
/// Bind it at `urn:name:document`, as [`crate::space`] does. For the IRI whose
/// tail IS the path, bind [`doc`] at [`DOC_TEMPLATE`] instead.
pub fn document() -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("document", negotiate).with_description(described(PathFrom::Argument))
}

/// The same negotiated document at [`DOC_TEMPLATE`], where the path is the
/// template's `{path}` binding rather than an argument.
///
/// One handler, a second description, because one description cannot declare a
/// name as both an argument and a binding (ledger #194). Bound at the template
/// with [`document`]'s description, the manifold read `path` as an argument the
/// IRI could not supply, so the entry was undrivable: `urn:kernel:actions`
/// offered nothing for it and an HTTP face's `?description` answered 404 for
/// the resolver's only public resource family.
///
/// ```
/// use ikigai_core::{Endpoint, InputSource, Verb};
///
/// let described = ikigai_name::doc().describe();
/// let source = described
///     .action_specs()
///     .into_iter()
///     .find(|a| a.verb == Verb::Source)
///     .expect("a Source action");
/// let path = source.inputs.iter().find(|i| i.name == "path").expect("declares path");
/// assert_eq!(path.source, InputSource::Binding);
/// assert!(ikigai_name::DOC_TEMPLATE.ends_with("{path}"));
/// ```
pub fn doc() -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("doc", negotiate).with_description(described(PathFrom::Binding))
}

/// The handler both entries share: one face, however the path arrived.
fn negotiate<'a>(inv: &'a Invocation<'_>) -> InvokeFuture<'a> {
    Box::pin(async move {
        // Three ways in, most specific first. The BINDING is what lets an
        // IRI carry the path (`urn:name:doc:resmud/core` via a template
        // whose trailing variable captures the remainder, slashes and all)
        // — which is what makes an HTTP path map onto a resource identity
        // rather than a query parameter.
        let path = match inv.bindings.get("path") {
            Some(bound) => bound.trim().to_string(),
            None => inv
                .inline_str("path")
                .or_else(|_| inv.inline_str("content"))
                .map_err(|_| Error::MissingArgument("path".into()))?
                .trim()
                .to_string(),
        };
        let wanted = inv
            .inline_str("as")
            .map(|t| bare(t).to_string())
            .unwrap_or_else(|_| CANONICAL.to_string());

        let registry = load(inv).await?;
        let found = resolve_path(&registry, &path)?;
        let prefix = found.prefix.clone();
        let strategy = found.strategy.clone();

        let source = match &strategy {
            Strategy::Hosted { source } => source.clone(),
            Strategy::Mirror { origin } => origin.clone(),
            // The owner serves this one. Reporting where is what lets the
            // HTTP face answer with a redirect instead of guessing.
            Strategy::Redirect { target } => {
                return Err(Error::Endpoint(format!(
                    "name: {prefix:?} redirects to {target}"
                )))
            }
            Strategy::Retired { reason } => {
                return Err(Error::NotFound(format!(
                    "name: {prefix:?} is retired: {reason}"
                )))
            }
        };

        // HTML is a different artifact, not a conversion of the graph: it is
        // a page ABOUT the vocabulary, which is why it is rendered rather
        // than transrepted.
        if bare(&wanted) == HTML {
            let iri =
                Iri::parse("urn:name:docs").map_err(|e| Error::Endpoint(format!("name: {e}")))?;
            let mut request = Request::new(Verb::Source, iri)
                .with_arg("prefix", ArgRef::Inline(prefix.into_bytes()));
            if let Ok(theme) = inv.inline_str("theme") {
                request = request.with_arg("theme", ArgRef::Inline(theme.as_bytes().to_vec()));
            }
            return inv.issue(request).await;
        }

        let iri = Iri::parse(&source)
            .map_err(|e| Error::Endpoint(format!("name: bad source {source:?}: {e}")))?;
        let stored = inv.source(&iri).await?;

        // The ceiling is checked on the way OUT, not on the way in: by the
        // time it is here the bytes are already resident, so this bounds
        // what leaves rather than what arrives. It is still worth having —
        // a transreptor chain multiplies a large document several times
        // over, and refusing early is what stops one namespace's mistake
        // from being every request's problem.
        let ceiling = registry.limits.max_document_bytes;
        if stored.bytes.len() > ceiling {
            return Err(Error::Endpoint(format!(
                "name: {path:?} is {} bytes, over this host's {ceiling}-byte ceiling \
                     (raise limits.max_document_bytes in the registry)",
                stored.bytes.len()
            )));
        }

        let have = bare(&stored.repr_type.media_type).to_string();

        if have == wanted {
            return Ok(stored);
        }

        let Some(plan) = inv.select_transreptor(&have, &wanted) else {
            // Naming what IS reachable turns a dead end into a next step,
            // and a namespace's whole job is being dereferenceable.
            return Err(Error::Endpoint(format!(
                "name: nothing converts {have} to {wanted} (this host serves {have} \
                     and {HTML}; bind a transreptor for {wanted} to add it)"
            )));
        };

        // Drive the chain exactly as the kernel drives its own: pipe
        // `content`, set `as`, one step at a time.
        let mut current = stored;
        for step in plan {
            let step_iri = Iri::parse(&step.endpoint).map_err(|e| {
                Error::Endpoint(format!("name: bad transreptor {}: {e}", step.endpoint))
            })?;
            current = inv
                .issue(
                    Request::new(Verb::Source, step_iri)
                        .with_arg("content", ArgRef::Inline(current.bytes))
                        .with_arg("as", ArgRef::Inline(step.to.into_bytes())),
                )
                .await?;
        }
        Ok(current)
    })
}

/// The contract shared by [`document`] and [`doc`]; only where `path` comes
/// from, and so the id, differ.
fn described(from: PathFrom) -> Description {
    let (id, path) = match from {
        PathFrom::Argument => (
            "document",
            ArgSpec::new("path")
                .summary("the curated path, e.g. resmud/core — a named argument or piped content"),
        ),
        PathFrom::Binding => (
            "doc",
            ArgSpec::new("path")
                .summary(
                    "the curated path, e.g. resmud/core — the rest of the IRI after \
                     urn:name:doc:, slashes and all",
                )
                .binding(),
        ),
    };
    Description::new(id)
        .title("A namespace document, negotiated")
        .summary(
            "The document behind a curated path, in the representation asked for: Turtle \
                 as stored, HTML rendered as documentation, and anything else reached through \
                 a transreptor chain. One IRI for every representation — a term's identity is \
                 its IRI, and a per-format suffix would fork it.",
        )
        .action(
            ActionSpec::new(Verb::Source)
                .summary("negotiate a namespace document")
                .input(path.class(XSD_STRING))
                .input(
                    ArgSpec::new("as")
                        .summary(
                            "the representation wanted; defaults to text/turtle. \
                                 text/html renders the documentation face, and any other \
                                 type is reached through a transreptor if one is bound.",
                        )
                        .default_value(CANONICAL)
                        .optional()
                        .class(XSD_STRING),
                )
                .input(
                    ArgSpec::new("theme")
                        .summary("passed through to the HTML face")
                        .optional()
                        .class(XSD_STRING),
                )
                // The two faces produced HERE. Everything else is reached
                // through a transreptor the host may or may not bind, so it
                // is not this action's to declare (the `as` input says so).
                // ⚠ An HTTP face negotiates over these two ALONE (ikigai-web
                // from 0.1.22): a transreptor a host binds is reachable through
                // `as=` in the kernel but answers 406 over HTTP. Whether the
                // adapter should consult the transreptor graph is ledger #249;
                // the standalone server binds none, so for it 406 is the truth.
                .output(CANONICAL)
                .output(TEXT_HTML_UTF8),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space;
    use futures::executor::block_on;
    use ikigai_core::{Capability, Exact, FnEndpoint, Kernel};
    use ikigai_core::{ReprType, Representation, Result};
    use std::sync::Arc;

    const VOCAB: &str = "@prefix rm: <https://iriref.org/resmud/core#> .\n\
                         rm:Weapon a <http://www.w3.org/2000/01/rdf-schema#Class> .\n";

    const REGISTRY: &str = r#"{"namespaces":[
        {"prefix":"resmud","owner":"urn:cap:name:admin:resmud",
         "strategy":"hosted","source":"urn:test:vocab"},
        {"prefix":"acme","owner":"urn:cap:name:admin:acme",
         "strategy":"redirect","target":"https://acme.example/ns"}
    ]}"#;

    fn serving(media: &'static str, body: &'static str) -> FnEndpoint {
        FnEndpoint::new("fixture", move |_: &Invocation<'_>| {
            Ok(Representation::new(
                ReprType::new(media),
                body.as_bytes().to_vec(),
            ))
        })
    }

    /// A transreptor that only records that it ran. Enough to prove the chain is
    /// selected and driven; the real conversion is another module's business.
    fn fake_jsonld() -> FnEndpoint {
        FnEndpoint::new("turtle-to-jsonld", |inv: &Invocation<'_>| {
            let source = inv.inline_str("content").unwrap_or_default();
            Ok(Representation::new(
                ReprType::new("application/ld+json"),
                format!("{{\"transrepted\":{}}}", source.len()).into_bytes(),
            ))
        })
        .with_description(
            Description::new("turtle-to-jsonld")
                .verb(Verb::Source)
                .transreptor([CANONICAL], ["application/ld+json"]),
        )
    }

    fn kernel(with_transreptor: bool) -> Kernel {
        let mut space = space()
            .bind(Exact::new("urn:test:vocab"), serving(CANONICAL, VOCAB))
            .bind(
                Exact::new("urn:name:registry-source"),
                serving("application/json", REGISTRY),
            );
        if with_transreptor {
            space = space.bind(Exact::new("urn:test:jsonld"), fake_jsonld());
        }
        Kernel::new(Arc::new(space))
    }

    fn get(kernel: &Kernel, args: &[(&str, &str)]) -> Result<Representation> {
        let mut request = Request::new(Verb::Source, Iri::parse("urn:name:document").unwrap());
        for (name, value) in args {
            request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
        }
        block_on(kernel.issue(request, &Capability::root()))
    }

    #[test]
    fn turtle_is_served_as_stored() {
        let repr = get(&kernel(false), &[("path", "resmud/core")]).expect("serves");
        assert_eq!(bare(&repr.repr_type.media_type), CANONICAL);
        assert!(String::from_utf8_lossy(&repr.bytes).contains("rm:Weapon"));
    }

    /// The default matters: an RDF tool that sends no `as` must still get the
    /// hub representation rather than an error.
    #[test]
    fn turtle_is_the_default_when_nothing_is_asked_for() {
        let with = get(
            &kernel(false),
            &[("path", "resmud/core"), ("as", CANONICAL)],
        )
        .expect("serves");
        let without = get(&kernel(false), &[("path", "resmud/core")]).expect("serves");
        assert_eq!(with.bytes, without.bytes);
    }

    /// Parameters must not defeat negotiation — `text/turtle; charset=utf-8` is
    /// still Turtle, and treating it as a distinct type would send it looking
    /// for a transreptor that cannot exist.
    #[test]
    fn media_type_parameters_are_ignored_when_negotiating() {
        let repr = get(
            &kernel(false),
            &[
                ("path", "resmud/core"),
                ("as", "text/turtle; charset=utf-8"),
            ],
        )
        .expect("serves");
        assert!(String::from_utf8_lossy(&repr.bytes).contains("rm:Weapon"));
    }

    // The `as=text/html` path delegates to `urn:name:docs`, which needs the real
    // SPARQL module bound — so it is proved in the integration test rather than
    // against a kernel that has no engine in it.

    #[test]
    fn another_type_is_reached_through_a_transreptor_chain() {
        let repr = get(
            &kernel(true),
            &[("path", "resmud/core"), ("as", "application/ld+json")],
        )
        .expect("transrepts");
        assert_eq!(bare(&repr.repr_type.media_type), "application/ld+json");
        assert!(String::from_utf8_lossy(&repr.bytes).contains("transrepted"));
    }

    /// A dead end should name what the host *can* do, since being
    /// dereferenceable is the whole job.
    #[test]
    fn an_unreachable_type_says_what_is_available() {
        let err = get(
            &kernel(false),
            &[("path", "resmud/core"), ("as", "application/ld+json")],
        )
        .expect_err("no transreptor bound");
        let text = err.to_string();
        assert!(text.contains("text/turtle"), "names the hub: {text}");
        assert!(
            text.contains("transreptor"),
            "says what would fix it: {text}"
        );
    }

    /// A redirect namespace is someone else's to serve; the face needs to know
    /// where so it can answer with a redirect rather than a guess.
    #[test]
    fn a_redirected_namespace_reports_its_target() {
        let err = get(&kernel(false), &[("path", "acme/thing")]).expect_err("not ours");
        assert!(
            err.to_string().contains("https://acme.example/ns"),
            "names the target: {err}"
        );
    }

    #[test]
    fn an_unclaimed_path_is_not_found() {
        let err = get(&kernel(false), &[("path", "nobody/here")]).expect_err("unclaimed");
        assert!(matches!(err, Error::NotFound(_)), "got: {err:?}");
    }

    /// The ceiling is the only bound on what one request can make this host
    /// hold, so it has to actually refuse — and say how to raise it.
    #[test]
    fn a_document_over_the_ceiling_is_refused_and_says_how_to_raise_it() {
        const TINY: &str = r#"{"namespaces":[{"prefix":"resmud","owner":"x",
            "strategy":"hosted","source":"urn:test:vocab"}],
            "limits":{"max_document_bytes":10}}"#;
        let kernel = Kernel::new(Arc::new(
            space()
                .bind(Exact::new("urn:test:vocab"), serving(CANONICAL, VOCAB))
                .bind(
                    Exact::new("urn:name:registry-source"),
                    serving("application/json", TINY),
                ),
        ));
        let err = get(&kernel, &[("path", "resmud/core")]).expect_err("over the ceiling");
        let text = err.to_string();
        assert!(text.contains("ceiling"), "says what happened: {text}");
        assert!(
            text.contains("max_document_bytes"),
            "names the setting that would fix it: {text}"
        );
    }

    #[test]
    fn a_document_under_the_ceiling_is_served() {
        let repr = get(&kernel(false), &[("path", "resmud/core")]).expect("under the ceiling");
        assert!(!repr.bytes.is_empty());
    }

    #[test]
    fn document_declares_its_contract() {
        let described = format!("{:?}", ikigai_core::Endpoint::describe(&document()));
        assert!(described.contains("path"), "declares path: {described}");
        assert!(described.contains("as"), "declares as");
    }
}
