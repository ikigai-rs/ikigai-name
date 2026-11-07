//! Resolution as data: what one name resolves to, and who answers for it.
//!
//! [`urn:name:resolve`](crate::resolve) prints one line for a path. This is the same
//! answer as a record, for a path or an IRI (`urn:name:resolution:iri:{iri}`, or
//! `urn:name:resolution` with `iri=`): the ONE claim containing the name, its strategy
//! and target, the tenant it was asked of, and the instant when one was asked about. It
//! answers JSON by default, Turtle with `as=text/turtle`, and with `as=text/plain` the
//! very line `urn:name:resolve` prints, which is computed here.
//!
//! ## Delegation
//!
//! A namespace whose strategy is `delegate`, and any name no namespace claims when the
//! registry's `unclaimed` is a `delegate`, is forwarded to the upstream resolver through
//! [`DELEGATE_IRI`]. That endpoint declares `urn:cap:net:*` and refuses, before anything
//! leaves, a capability not holding `urn:cap:net:<upstream's host>` exactly. It reads the
//! upstream's own resolution at `{upstream}-/resolution/{name}`, or its document at
//! `{upstream}{name}`, through `urn:http:get`; a host with no HTTP client delegates
//! nothing and says so.
//!
//! ## The same answer
//!
//! A namespace that is not public is invisible to anyone not holding its `owner`: they
//! are answered exactly as for a name nobody claims, which is a `NotFound` naming the
//! name, or the upstream's answer when unclaimed names are delegated. So asking cannot
//! tell a private name from an unknown one.

use crate::history::{as_of_arg, as_of_input, when, AsOf};
use crate::iri::name_key;
use crate::registry::{Namespace, Registry, Strategy};
use crate::stat::{self, Seen};
use crate::tenant::tenant_of;
use ikigai_core::{
    ActionSpec, ArgRef, ArgSpec, AsyncFnEndpoint, Capability, Description, Error, Invocation,
    InvokeFuture, Iri, ReprType, Representation, Request, Result, Verb,
};
use serde::Serialize;
use std::sync::Arc;

/// The resolution of a name given as an argument (`iri=`, or piped `content`).
pub const RESOLUTION_IRI: &str = "urn:name:resolution";

/// The resolution of the name the IRI's tail carries: `urn:name:resolution:iri:{iri}`.
pub const RESOLUTION_TEMPLATE: &str = "urn:name:resolution:iri:{iri}";

/// Forward a name to an upstream resolver. See the [module documentation](self).
pub const DELEGATE_IRI: &str = "urn:name:delegate";

/// The outbound client delegation reads through, as `ikigai-http` binds it.
const HTTP_GET_IRI: &str = "urn:http:get";

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const APPLICATION_JSON: &str = "application/json";
const TEXT_TURTLE: &str = "text/turtle";
const TEXT_PLAIN_UTF8: &str = "text/plain;charset=utf-8";

/// The vocabulary the Turtle face writes its record in.
pub const VOCAB: &str = "urn:name:vocab#";

/// The terms of [`VOCAB`], defined: what the Turtle face's properties mean.
pub const VOCABULARY_TTL: &str = r#"@prefix name: <urn:name:vocab#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .

name:Resolution a rdfs:Class ;
    rdfs:label "resolution" ;
    rdfs:comment "What one name resolves to, and the claim that answers for it." .
name:name a rdf:Property ; rdfs:domain name:Resolution ;
    rdfs:label "name" ; rdfs:comment "The name as it was asked for: a path, or a urn: or tag: IRI." .
name:canonical a rdf:Property ; rdfs:domain name:Resolution ;
    rdfs:label "canonical name" ; rdfs:comment "The name as it is compared: a path's segments joined by a slash, or an IRI's canonical form." .
name:claim a rdf:Property ; rdfs:domain name:Resolution ;
    rdfs:label "claim" ; rdfs:comment "The prefix of the one claim containing the name." .
name:strategy a rdf:Property ; rdfs:domain name:Resolution ;
    rdfs:label "strategy" ; rdfs:comment "How the claim answers: hosted, redirect or mirror." .
name:target a rdf:Property ; rdfs:domain name:Resolution ;
    rdfs:label "target" ; rdfs:comment "Where the name resolves: a hosted source, a mirror's origin, or where a redirect sends it." .
name:status a rdf:Property ; rdfs:domain name:Resolution ;
    rdfs:label "status" ; rdfs:comment "For a redirect, the HTTP status it answers with." .
name:rule a rdf:Property ; rdfs:domain name:Resolution ;
    rdfs:label "rule" ; rdfs:comment "For a redirect, the match of the rewrite rule that answered." .
name:tenant a rdf:Property ; rdfs:domain name:Resolution ;
    rdfs:label "tenant" ; rdfs:comment "The host the name was resolved for." .
name:asOf a rdf:Property ; rdfs:domain name:Resolution ;
    rdfs:label "as of" ; rdfs:comment "The instant the name was resolved as of, in UTC." .
"#;

/// What one name resolves to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Resolution {
    /// The name as it was asked for.
    pub name: String,
    /// The name as it is compared: a path's segments joined by `/`, or an IRI's
    /// canonical form (see [`crate::iri`]).
    pub canonical: String,
    /// The prefix of the claim containing it.
    pub claim: String,
    /// `hosted`, `redirect` or `mirror`.
    pub strategy: &'static str,
    /// Where it resolves: the source a hosted namespace is read from, a mirror's
    /// origin, or where a redirect sends it.
    pub target: String,
    /// For a redirect, the status it answers with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// For a redirect, the `match` of the rule that answered, if one did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    /// The host it was resolved for, when it was resolved for one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// The instant it was resolved as of, in UTC, when it was asked about the past.
    #[serde(rename = "as-of", skip_serializing_if = "Option::is_none")]
    pub as_of: Option<String>,
}

impl Resolution {
    /// The one line `urn:name:resolve` prints: the claim, the strategy and the target,
    /// separated by tabs, and for a redirect its status and any rule that answered.
    pub fn line(&self) -> String {
        let mut line = format!("{}\t{}\t{}", self.claim, self.strategy, self.target);
        if let Some(status) = self.status {
            line.push_str(&format!("\t{status}"));
        }
        if let Some(rule) = &self.rule {
            line.push_str(&format!("\t{rule}"));
        }
        line.push('\n');
        line
    }

    /// The record as Turtle, about the resolution's own IRI,
    /// `urn:name:resolution:iri:{canonical}`, so a path that is no IRI is described the
    /// same way as an IRI is.
    pub fn turtle(&self) -> String {
        let subject = format!("urn:name:resolution:iri:{}", self.canonical);
        let mut out = format!(
            "@prefix name: <{VOCAB}> .\n\n{} a name:Resolution ;\n    name:name {}",
            iri_ref(&subject),
            literal(&self.name)
        );
        let mut field = |property: &str, object: String| {
            out.push_str(&format!(" ;\n    name:{property} {object}"));
        };
        field("canonical", literal(&self.canonical));
        field("claim", literal(&self.claim));
        field("strategy", literal(self.strategy));
        field("target", iri_ref(&self.target));
        if let Some(status) = self.status {
            field("status", status.to_string());
        }
        if let Some(rule) = &self.rule {
            field("rule", literal(rule));
        }
        if let Some(tenant) = &self.tenant {
            field("tenant", literal(tenant));
        }
        if let Some(at) = &self.as_of {
            field(
                "asOf",
                format!(
                    "{}^^<http://www.w3.org/2001/XMLSchema#dateTime>",
                    literal(at)
                ),
            );
        }
        out.push_str(" .\n");
        out
    }
}

/// A Turtle string literal.
fn literal(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A Turtle IRI reference, any character an IRIREF cannot hold written as a `\u` escape.
fn iri_ref(iri: &str) -> String {
    let mut out = String::from("<");
    for c in iri.chars() {
        if c <= ' ' || "<>\"{}|^`\\".contains(c) {
            out.push_str(&format!("\\u{:04X}", u32::from(c)));
        } else {
            out.push(c);
        }
    }
    out.push('>');
    out
}

/// Who answers for a name.
pub(crate) enum Answer<'r> {
    /// A namespace this registry holds and the caller may see.
    Claimed(&'r Namespace),
    /// An upstream resolver: the namespace's, or the registry's for what it does not hold.
    Delegated(&'r str),
}

/// Whether `capability` may see `namespace`: anyone, when it is public, and otherwise
/// only a capability holding its owner.
pub(crate) fn visible(namespace: &Namespace, capability: &Capability) -> bool {
    namespace.public || capability.allows(&namespace.owner)
}

/// The refusal for a name nobody claims, and for one the caller may not see.
pub(crate) fn unclaimed(name: &str, at: Option<&AsOf>) -> Error {
    Error::NotFound(format!("name: no namespace claims {name:?}{}", when(at)))
}

/// Who answers for `name` in `registry`, for `capability`.
pub(crate) fn answer<'r>(
    registry: &'r Registry,
    name: &str,
    capability: &Capability,
    at: Option<&AsOf>,
) -> Result<Answer<'r>> {
    match registry.lookup(name).filter(|ns| visible(ns, capability)) {
        Some(Namespace {
            strategy: Strategy::Delegate { upstream },
            ..
        }) => Ok(Answer::Delegated(upstream)),
        Some(namespace) => Ok(Answer::Claimed(namespace)),
        None => match &registry.unclaimed {
            Some(Strategy::Delegate { upstream }) => Ok(Answer::Delegated(upstream)),
            _ => Err(unclaimed(name, at)),
        },
    }
}

/// The resolution of `name` under `namespace`, which claims it.
pub(crate) fn resolution(
    namespace: &Namespace,
    name: &str,
    tenant: Option<&str>,
    at: Option<&AsOf>,
) -> Result<Resolution> {
    let (target, status, rule) = match &namespace.strategy {
        Strategy::Hosted { source } => (source.clone(), None, None),
        Strategy::Mirror { origin } => (origin.clone(), None, None),
        Strategy::Redirect { .. } => {
            let found = namespace.redirection(name).ok_or_else(|| {
                Error::Endpoint("name: a redirect namespace named no target".into())
            })?;
            (
                found.target,
                Some(found.status.code()),
                found.rule.map(str::to_string),
            )
        }
        // Retired is an ANSWER, not a miss: the namespace was real, and saying so is
        // more useful than pretending it never existed. HTTP would call this 410 Gone;
        // core has no such variant, so the face maps it and NotFound carries it.
        Strategy::Retired { reason } => {
            return Err(Error::NotFound(format!(
                "name: {:?} is retired{}: {reason}",
                namespace.prefix,
                when(at)
            )))
        }
        Strategy::Delegate { upstream } => {
            return Err(Error::Endpoint(format!(
                "name: {:?} is answered by {upstream}, and was not forwarded",
                namespace.prefix
            )))
        }
    };
    Ok(Resolution {
        name: name.to_string(),
        canonical: name_key(name),
        claim: namespace.prefix.clone(),
        strategy: namespace.strategy.name(),
        target,
        status,
        rule,
        tenant: tenant.map(str::to_string),
        as_of: at.map(ToString::to_string),
    })
}

/// Which of an upstream's faces a delegation reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Face {
    /// `{upstream}-/resolution/{name}`: the record this module answers.
    Resolution,
    /// `{upstream}{name}`: the document.
    Document,
}

impl Face {
    fn as_str(self) -> &'static str {
        match self {
            Face::Resolution => "resolution",
            Face::Document => "document",
        }
    }
}

/// Ask [`DELEGATE_IRI`] to forward `name` to `upstream`, under the caller's capability.
pub(crate) async fn forward(
    inv: &Invocation<'_>,
    upstream: &str,
    name: &str,
    face: Face,
    wanted: Option<&str>,
) -> Result<Representation> {
    let iri = Iri::parse(DELEGATE_IRI).map_err(|e| Error::Endpoint(format!("name: {e}")))?;
    let mut request = Request::new(Verb::Source, iri)
        .with_arg("upstream", ArgRef::Inline(upstream.as_bytes().to_vec()))
        .with_arg("name", ArgRef::Inline(name.as_bytes().to_vec()))
        .with_arg("face", ArgRef::Inline(face.as_str().as_bytes().to_vec()));
    if let Some(wanted) = wanted {
        request = request.with_arg("as", ArgRef::Inline(wanted.as_bytes().to_vec()));
    }
    inv.issue(request).await
}

/// Percent-encode `text` for a URL: everything but unreserved characters and `:`, `@`,
/// `,` and `;`, which a path segment may hold as they are.
fn encode(text: &str, keep: &[u8]) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) || keep.contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The URL a delegation reads.
pub(crate) fn upstream_url(upstream: &str, name: &str, face: Face, wanted: Option<&str>) -> String {
    let base = upstream.trim_end_matches('/');
    let name = encode(name.trim(), b":@,;");
    let mut url = match face {
        Face::Resolution => format!("{base}/-/resolution/{name}"),
        Face::Document => format!("{base}/{name}"),
    };
    if let Some(wanted) = wanted {
        url.push_str("?as=");
        url.push_str(&encode(wanted, b""));
    }
    url
}

/// `urn:name:delegate` — forward a name to an upstream resolver.
pub fn delegate() -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("delegate", |inv: &Invocation<'_>| -> InvokeFuture<'_> {
        Box::pin(async move {
            let upstream = inv.inline_str("upstream")?.trim().to_string();
            let name = inv.inline_str("name")?.trim().to_string();
            let face = match inv.inline_str("face") {
                Ok("document") => Face::Document,
                Ok("resolution") | Err(Error::MissingArgument(_)) => Face::Resolution,
                Ok(other) => {
                    return Err(Error::InvalidArgument {
                        name: "face".into(),
                        detail: format!("{other:?} is not a face: write resolution or document"),
                    })
                }
                Err(other) => return Err(other),
            };
            let wanted = inv.inline_str("as").ok().map(str::to_string);
            let delegate = Strategy::Delegate {
                upstream: upstream.clone(),
            };
            let scope = delegate.net_scope().ok_or_else(|| Error::InvalidArgument {
                name: "upstream".into(),
                detail: format!("{upstream:?} is not an https: or http: address with a host"),
            })?;
            // The kernel has held the caller to `urn:cap:net:*`; this is the half that
            // names the one host, so a grant for one upstream reaches no other.
            if !inv.capability.allows(&scope) {
                return Err(Error::Denied(format!(
                    "name: forwarding {name:?} to {upstream} needs {scope}"
                )));
            }
            let url = upstream_url(&upstream, &name, face, wanted.as_deref());
            let get =
                Iri::parse(HTTP_GET_IRI).map_err(|e| Error::Endpoint(format!("name: {e}")))?;
            let request = Request::new(Verb::Source, get)
                .with_arg("url", ArgRef::Inline(url.into_bytes()))
                .with_arg(
                    "headers",
                    ArgRef::Inline(b"User-Agent: ikigai-name\n".to_vec()),
                );
            let answered = inv.issue(request).await.map_err(|e| match e {
                Error::Unresolved(missing) if missing.as_str() == HTTP_GET_IRI => {
                    Error::Unsupported {
                        verb: None,
                        detail: format!(
                            "name: this host has no HTTP client, so it forwards nothing to \
                             {upstream}; bind {HTTP_GET_IRI} to add it"
                        ),
                    }
                }
                other => other,
            })?;
            // A pure function of the upstream's answer: as cacheable as that is, which
            // the kernel decides by taking the meet with the client's own expiry.
            Ok(Representation::new(answered.repr_type.clone(), answered.bytes.clone()).cacheable())
        })
    })
    .with_description(
        Description::new("delegate")
            .title("Forward a name to an upstream resolver")
            .action(
                ActionSpec::new(Verb::Source)
                    .summary(
                        "read the upstream's resolution of a name, or its document; needs \
                         urn:cap:net:<the upstream's host>",
                    )
                    .requires("urn:cap:net:*")
                    .input(
                        ArgSpec::new("upstream")
                            .summary("the upstream resolver, e.g. https://urnid.org/")
                            .class(XSD_STRING),
                    )
                    .input(
                        ArgSpec::new("name")
                            .summary("the path or IRI to resolve there")
                            .class(XSD_STRING),
                    )
                    .input(
                        ArgSpec::new("face")
                            .summary("resolution (the default) or document")
                            .optional()
                            .class(XSD_STRING),
                    )
                    .input(
                        ArgSpec::new("as")
                            .summary("the media type to ask the upstream for")
                            .optional()
                            .class(XSD_STRING),
                    )
                    // The default face. Whatever `as` asks for is the upstream's to serve,
                    // and is passed back as it came.
                    .output(APPLICATION_JSON),
            ),
    )
}

/// Where a resolution's name arrives from: an argument at [`RESOLUTION_IRI`], or the
/// template's binding at [`RESOLUTION_TEMPLATE`].
#[derive(Clone, Copy)]
enum NameFrom {
    Argument,
    Binding,
}

/// The faces a resolution answers, by `as=`.
fn face_type(inv: &Invocation<'_>) -> Result<&'static str> {
    let wanted = match inv.inline_str("as") {
        Ok(wanted) => wanted
            .split(';')
            .next()
            .unwrap_or(wanted)
            .trim()
            .to_string(),
        Err(Error::MissingArgument(_)) => return Ok(APPLICATION_JSON),
        Err(other) => return Err(other),
    };
    match wanted.as_str() {
        APPLICATION_JSON => Ok(APPLICATION_JSON),
        TEXT_TURTLE => Ok(TEXT_TURTLE),
        "text/plain" => Ok(TEXT_PLAIN_UTF8),
        other => Err(Error::Unsupported {
            verb: None,
            detail: format!(
                "name: a resolution is answered as {APPLICATION_JSON}, {TEXT_TURTLE} or \
                 text/plain, not {other}"
            ),
        }),
    }
}

/// A resolution in the face asked for.
fn render(found: &Resolution, face: &str) -> Result<Representation> {
    Ok(match face {
        TEXT_TURTLE => Representation::new(ReprType::new(TEXT_TURTLE), found.turtle().into_bytes()),
        TEXT_PLAIN_UTF8 => Representation::new(
            ReprType::new("text/plain").with_param("charset", "utf-8"),
            found.line().into_bytes(),
        ),
        _ => Representation::new(
            ReprType::new(APPLICATION_JSON),
            serde_json::to_vec_pretty(found).map_err(|e| Error::Endpoint(format!("name: {e}")))?,
        ),
    })
}

/// The handler both entries share, and `urn:name:resolve` with it: the one answer for
/// `name` in `face`, recorded as a name resolved.
pub(crate) async fn resolve_in(
    inv: &Invocation<'_>,
    seen: Option<&Seen>,
    name: &str,
    face: &str,
) -> Result<Representation> {
    let at = as_of_arg(inv)?;
    let registry = crate::load_at(inv, at.as_ref()).await?;
    match answer(&registry, name, inv.capability, at.as_ref())? {
        Answer::Delegated(upstream) => {
            forward(inv, upstream, name, Face::Resolution, Some(face)).await
        }
        Answer::Claimed(namespace) => {
            let found = resolution(namespace, name, tenant_of(inv.capability), at.as_ref())?;
            // Pure in (registry, name, tenant); the kernel clamps to the registry's own
            // expiry. Read from history every time when it is about the past.
            let answered = crate::cacheable_unless_past(render(&found, face)?, at.as_ref());
            Ok(stat::counted(
                seen,
                inv,
                at.as_ref(),
                &namespace.prefix,
                name,
                answered,
            ))
        }
    }
}

fn name_from(inv: &Invocation<'_>, from: NameFrom) -> Result<String> {
    match from {
        NameFrom::Binding => inv
            .bindings
            .get("iri")
            .map(|bound| bound.trim().to_string())
            .ok_or_else(|| Error::MissingArgument("iri".into())),
        NameFrom::Argument => inv
            .inline_str("iri")
            .or_else(|_| inv.inline_str("content"))
            .map(|raw| raw.trim().to_string())
            .map_err(|_| Error::MissingArgument("iri".into())),
    }
}

fn resolution_counted_by(seen: Option<Arc<Seen>>, from: NameFrom) -> AsyncFnEndpoint {
    let id = match from {
        NameFrom::Argument => "resolution",
        NameFrom::Binding => "resolution-iri",
    };
    AsyncFnEndpoint::new(id, move |inv: &Invocation<'_>| -> InvokeFuture<'_> {
        let seen = seen.clone();
        Box::pin(async move {
            let name = name_from(inv, from)?;
            let face = face_type(inv)?;
            resolve_in(inv, seen.as_deref(), &name, face).await
        })
    })
    .with_description(described(from))
}

/// `urn:name:resolution` — the resolution of the name given as `iri=`.
pub fn resolution_endpoint() -> AsyncFnEndpoint {
    resolution_counted_by(None, NameFrom::Argument)
}

/// The same at [`RESOLUTION_TEMPLATE`], where the name is the template's binding.
pub fn resolution_at_iri() -> AsyncFnEndpoint {
    resolution_counted_by(None, NameFrom::Binding)
}

pub(crate) fn counted(seen: Arc<Seen>) -> (AsyncFnEndpoint, AsyncFnEndpoint) {
    (
        resolution_counted_by(Some(seen.clone()), NameFrom::Argument),
        resolution_counted_by(Some(seen), NameFrom::Binding),
    )
}

fn described(from: NameFrom) -> Description {
    let name = ArgSpec::new("iri")
        .summary("the name: a urn: or tag: IRI, or a curated path such as resmud/core")
        .class(XSD_STRING);
    let name = match from {
        NameFrom::Argument => name,
        NameFrom::Binding => name.binding(),
    };
    let mut action = ActionSpec::new(Verb::Source)
        .summary(
            "the one claim containing a name, and its strategy, target, tenant and as-of \
             instant; a name delegated upstream is the upstream's answer",
        )
        .input(name);
    if let NameFrom::Argument = from {
        action = action.input(
            ArgSpec::new("content")
                .summary("the name, when piped (the fallback for `iri`)")
                .optional()
                .class(XSD_STRING),
        );
    }
    let action = action
        .input(
            ArgSpec::new("as")
                .summary("application/json (the default), text/turtle, or text/plain for the line urn:name:resolve prints")
                .optional()
                .class(XSD_STRING),
        )
        .input(as_of_input())
        .output(APPLICATION_JSON)
        .output(TEXT_TURTLE)
        .output(TEXT_PLAIN_UTF8);
    let id = match from {
        NameFrom::Argument => "resolution",
        NameFrom::Binding => "resolution-iri",
    };
    Description::new(id)
        .title("Resolve a name, as data")
        .action(action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::parse_as_of;

    /// The instant, and the tenant, are part of the record when they were asked about.
    #[test]
    fn a_resolution_names_the_instant_and_the_tenant_it_was_asked_of() {
        let registry = Registry::from_json(
            br#"{"namespaces":[{"prefix":"urn:urnid:acme:","owner":"o",
                "strategy":"hosted","source":"urn:file:x"}]}"#,
        )
        .expect("loads");
        let at = parse_as_of("2026-03-31").expect("a date");
        let namespace = registry.lookup("urn:urnid:acme:a").expect("claimed");
        let found = resolution(namespace, "urn:urnid:acme:a", Some("urnid.org"), Some(&at))
            .expect("resolves");
        assert_eq!(found.as_of.as_deref(), Some("2026-03-31T23:59:59Z"));
        assert_eq!(found.tenant.as_deref(), Some("urnid.org"));
        let turtle = found.turtle();
        assert!(
            turtle.contains("name:asOf \"2026-03-31T23:59:59Z\"^^"),
            "{turtle}"
        );
        assert!(turtle.contains("name:tenant \"urnid.org\""), "{turtle}");
        let json = serde_json::to_value(&found).unwrap();
        assert_eq!(json["as-of"], "2026-03-31T23:59:59Z");
    }
}
