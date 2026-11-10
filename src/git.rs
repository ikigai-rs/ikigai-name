//! A namespace served from a file in someone's git repository.
//!
//! The owner points a logical identifier at a file, and changes what it answers by
//! pushing, with no server of their own. A `hosted` or `mirror` namespace names the
//! repository in the registry and serves [`SOURCE_TEMPLATE`]:
//!
//! ```json
//! {
//!   "prefix": "acme",
//!   "owner": "urn:cap:name:admin:acme",
//!   "strategy": "mirror",
//!   "origin": "urn:name:git:acme",
//!   "repository": {
//!     "url": "https://github.com/acme/vocab",
//!     "branch": "main",
//!     "path": "vocab/core.ttl",
//!     "shape": "urn:file:shapes/acme.ttl",
//!     "private": true
//!   }
//! }
//! ```
//!
//! ## A request never reaches the repository
//!
//! A read answers from the last good copy, in a store. Only a refresh
//! ([`REFRESH_TEMPLATE`]) reads the repository, so a client can neither make this host
//! fetch nor slow a resolution by way of a slow forge, and a request keeps the read-only
//! authority it has always had. A host issues a refresh when a push webhook arrives
//! ([`verify_push`] checks its signature) and on a timer, which also fills the store the
//! first time and catches a missed webhook.
//!
//! The golden thread is the store's. A refresh that accepts a new version writes it, and
//! a `Sink` cuts the thread named after its target, so every answer derived from the old
//! version recomputes on its next read. A refresh that changes nothing cuts nothing.
//!
//! ## Validation on change
//!
//! A version is parsed and checked against the SHACL shapes the namespace names, through
//! `urn:shacl:validate`, before anything can serve it. A version that fails is refused:
//! the last good one keeps serving, and the reason is recorded at [`STATUS_TEMPLATE`], for
//! the namespace's administrator to read. If the check itself cannot run (the shapes
//! cannot be read, or the host binds no validator), nothing is accepted and that is
//! recorded instead. An unchecked version is never served.
//!
//! ## When the repository disappears
//!
//! A `mirror` keeps serving its last good copy, which is what the strategy means. A
//! `hosted` namespace stops, with [`Error::NotFound`], because deleting the repository is
//! how such an owner withdraws it. Only [`Error::NotFound`] from [`READ_IRI`] counts as
//! disappearing; any other failure is recorded, and both strategies keep serving.
//!
//! ## What a repository can supply
//!
//! A repository's bytes reach the resolver's origin as Turtle or not at all. The path
//! must end `.ttl`, so a registry cannot name `index.html` or `app.js`. The media type
//! comes from that rule and never from the repository, a version a reader labels as
//! HTML, JavaScript or SVG is refused, and the validator's parse refuses anything else
//! that is not Turtle. RDF/XML is left out on purpose: a browser runs a script inside
//! XML. The HTML this module serves is the documentation page, generated from the graph
//! with every name and comment escaped.
//!
//! ## Authority
//!
//! A registry is written by claimants, and a refresh holds authority a claimant does not:
//!
//! - **A token is never in the registry.** A private repository's token is read from
//!   `urn:secret:` under a name derived from the namespace ([`token_secret`]). The entry
//!   holds only `private`, so an owner cannot name another namespace's token and send it
//!   to a host of their choosing. A `token` field refuses the whole file, and so does a
//!   URL with credentials in it. The refusal echoes neither.
//! - **The shapes are read with less.** `shape` must be a `urn:file:` IRI, and it and the
//!   validator run under only the read scopes of the refresh's capability, so that a shape
//!   cannot be pointed at a secret and read back through a parse error. A refresh issued
//!   under the root capability cannot be narrowed this way, and has only the `urn:file:` rule.
//! - **A refusal names no token.** Each one is scrubbed of it before it is stored or answered.
//!
//! ## The seams
//!
//! Like the registry, each is a bound IRI, so this crate holds no git, no HTTP client and
//! no file:
//!
//! - [`READ_IRI`]: Source taking `url`, `branch`, `path`, and optionally `token` and
//!   `as-of` (an instant in UTC). It answers the file's bytes at the branch's head, or at
//!   the newest commit touching the file at or before `as-of`, and [`Error::NotFound`] if
//!   the repository, branch or file is not there. It must answer afresh, as a push has
//!   just happened. [`forge_reader`] is one binding, over `urn:http:get`.
//! - [`STORE_TEMPLATE`]: `document:{prefix}` holds the last good bytes and `status:{prefix}`
//!   the JSON the status face reads. Source answers the document, or [`Error::NotFound`] if
//!   there is none yet. Sink takes `content` and `if-match`, as
//!   [`SEEN_TEMPLATE`](crate::SEEN_TEMPLATE) does. A host with tenants keys it on
//!   [`tenant_of`].
//!
//! ## As of a date
//!
//! `as-of` on a repository-backed namespace reads the same repository's history through
//! [`READ_IRI`], rather than [`HISTORY_IRI`](crate::HISTORY_IRI), which still supplies the
//! registry as it stood. History records what was pushed rather than what was served, so a
//! version refused at the time is answered as committed.

use crate::document::CANONICAL;
use crate::history::{absent, as_of_arg, AsOf, AS_OF};
use crate::load;
use crate::registry::{segments, ClaimError, Namespace, Registry, Strategy};
use crate::stat::text_answer;
use crate::tenant::tenant_of;
use hmac::{Hmac, Mac};
use ikigai_core::{
    ActionSpec, ArgRef, ArgSpec, AsyncFnEndpoint, ContentId, Description, Error, Invocation,
    InvokeFuture, Iri, ReprType, Representation, Request, Result, Verb,
};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

/// Where a repository-backed namespace's document is served. Its `source` or `origin`
/// names this, with its own prefix.
pub const SOURCE_TEMPLATE: &str = "urn:name:git:{prefix}";

/// The Source that reads a repository; see the [module documentation](self).
pub const READ_IRI: &str = "urn:name:git-read";

/// The template a host binds the last good copies at; see the
/// [module documentation](self).
pub const STORE_TEMPLATE: &str = "urn:name:git-store:{key}";

/// The Sink that reads the repository again, and accepts or refuses what it finds.
pub const REFRESH_TEMPLATE: &str = "urn:name:git-refresh:{prefix}";

/// The Source a namespace's administrator reads what happened at.
pub const STATUS_TEMPLATE: &str = "urn:name:git-status:{prefix}";

/// The capability [`REFRESH_TEMPLATE`] requires. A host's webhook door and timer hold
/// it; an ordinary request does not.
pub const CAP_REFRESH: &str = "urn:cap:name:git-refresh";

const SOURCE_PREFIX: &str = "urn:name:git:";
const STORE_PREFIX: &str = "urn:name:git-store:";
const SECRET_PREFIX: &str = "urn:secret:";
const VALIDATE_IRI: &str = "urn:shacl:validate";
const HTTP_GET_IRI: &str = "urn:http:get";

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const TEXT_PLAIN_UTF8: &str = "text/plain;charset=utf-8";

/// A longer refusal is cut here. It is repository-supplied text, in a record the owner reads.
const MAX_REASON_CHARS: usize = 2_000;

/// How many violations a refusal lists before it counts the rest.
const MAX_LISTED_VIOLATIONS: usize = 3;

/// What may never be served from a repository, whatever its file is called.
const ACTIVE_TYPES: [&str; 6] = [
    "text/html",
    "application/xhtml+xml",
    "text/javascript",
    "application/javascript",
    "application/ecmascript",
    "image/svg+xml",
];

/// A remote git repository, at a branch and a path, as the registry names it.
///
/// There is no field for a token, and that is the point: see the
/// [module documentation](self).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    /// The repository's `https:` address, with no credentials in it.
    pub url: String,
    /// The branch served.
    pub branch: String,
    /// The file served, relative to the repository's root. It must end `.ttl`.
    pub path: String,
    /// The `urn:file:` IRI of the SHACL shapes each new version must conform to.
    pub shape: String,
    /// Whether reading it takes a token, read from [`token_secret`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
}

impl Repository {
    /// Whether a registry may hold this binding, or a clause saying why not. A clause
    /// about the URL does not repeat it, as it may hold credentials.
    pub fn check(&self) -> std::result::Result<(), String> {
        let clean = |text: &str| !text.chars().any(|c| c.is_whitespace() || c.is_control());
        let Some(rest) = self.url.strip_prefix("https://") else {
            return Err("must have an https: url".into());
        };
        let (host, _) = rest.split_once('/').unwrap_or((rest, ""));
        if host.contains('@') {
            return Err(
                "has credentials in its url, and a token never goes in the registry: set \
                 private, and store it in the secret store"
                    .into(),
            );
        }
        if host.is_empty() || !clean(&self.url) || self.url.contains(['?', '#']) {
            return Err("must have a url naming a host, with no spaces, query or fragment".into());
        }
        let branch = &self.branch;
        if branch.is_empty()
            || branch.starts_with('-')
            || branch.contains("..")
            || !clean(branch)
            || branch.contains(['~', '^', ':', '?', '*', '[', '\\'])
        {
            return Err(format!("has {branch:?}, which is not a branch name"));
        }
        let path = &self.path;
        if !clean(path)
            || path.starts_with(['/', '-'])
            || path
                .split('/')
                .any(|segment| segment == ".." || segment == ".")
        {
            return Err(format!("has {path:?}, which is not a path inside it"));
        }
        if !path.ends_with(".ttl") {
            return Err(format!(
                "has the path {path:?}, and only a Turtle file ending .ttl is served, so that \
                 repository HTML or JavaScript never reaches this origin"
            ));
        }
        if !self.shape.starts_with("urn:file:") || !clean(&self.shape) {
            return Err(format!(
                "has the shape {:?}, and a shape must be a urn:file: IRI",
                self.shape
            ));
        }
        Ok(())
    }
}

/// The refusal for a `repository` a file cannot parse into a [`Repository`], if that is
/// what stopped it. The value of a misplaced token is never repeated.
pub(crate) fn repository_refusal(raw: &serde_json::Value) -> Option<ClaimError> {
    let reason = serde_json::from_value::<Repository>(raw.clone()).err()?;
    Some(ClaimError::BadRepository(
        if ["token", "password", "secret"]
            .iter()
            .any(|field| raw.get(field).is_some())
        {
            "holds a token, and a token never goes in the registry: set private, and store it in \
             the secret store"
                .to_string()
        } else {
            format!("cannot be read: {reason}")
        },
    ))
}

/// The IRI a namespace's document is served at, which its `source` or `origin` names.
pub fn source_iri(prefix: &str) -> String {
    format!("{SOURCE_PREFIX}{prefix}")
}

/// A name's text for a secret name: letters, digits, `-` and `_`, and everything else as
/// `~XX`, so that a `.` only ever separates the parts and no name can reach another's.
fn secret_part(text: &str) -> String {
    let mut part = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' {
            part.push(char::from(byte));
        } else {
            part.push_str(&format!("~{byte:02X}"));
        }
    }
    part
}

fn secret_name(kind: &str, tenant: Option<&str>, prefix: &str) -> String {
    match tenant {
        Some(tenant) => format!(
            "name-git-{kind}.{}.{}",
            secret_part(tenant),
            secret_part(prefix)
        ),
        None => format!("name-git-{kind}.{}", secret_part(prefix)),
    }
}

/// The secret holding a private repository's read-only token. A capability reading it
/// holds `urn:cap:secret:read:` and this name.
///
/// ```
/// use ikigai_name::git::token_secret;
/// assert_eq!(token_secret(None, "acme/vocab"), "name-git-token.acme~2Fvocab");
/// assert_eq!(
///     token_secret(Some("iriref.org"), "acme"),
///     "name-git-token.iriref~2Eorg.acme"
/// );
/// ```
pub fn token_secret(tenant: Option<&str>, prefix: &str) -> String {
    secret_name("token", tenant, prefix)
}

/// The secret holding the shared key a namespace's push webhook signs with.
pub fn hook_secret(tenant: Option<&str>, prefix: &str) -> String {
    secret_name("hook", tenant, prefix)
}

/// Whether `signature`, a forge's `X-Hub-Signature-256` header (`sha256=` and the hex of
/// an HMAC-SHA256), is `key`'s over exactly `body`. GitHub, Gitea and Forgejo send it.
///
/// A door checks this before a refresh runs, as a request's headers do not reach an
/// endpoint. An empty key verifies nothing, so an unset secret cannot be forged against.
///
/// ```
/// use ikigai_name::git::verify_push;
/// // The example in GitHub's webhook documentation.
/// let key = b"It's a Secret to Everybody";
/// let signature = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
/// assert!(verify_push(key, b"Hello, World!", signature));
/// assert!(!verify_push(key, b"Hello, World?", signature));
/// ```
pub fn verify_push(key: &[u8], body: &[u8], signature: &str) -> bool {
    let Some(hex) = signature.trim().strip_prefix("sha256=") else {
        return false;
    };
    let Some(expected) = from_hex(hex) else {
        return false;
    };
    if key.is_empty() {
        return false;
    }
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(key) else {
        return false;
    };
    mac.update(body);
    // In constant time, so a guess learns nothing from how long it took.
    mac.verify_slice(&expected).is_ok()
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).ok())
        .collect()
}

// --- the store --------------------------------------------------------------

fn parsed_iri(target: &str) -> Result<Iri> {
    Iri::parse(target).map_err(|e| Error::Endpoint(format!("name: {e}")))
}

fn document_iri(prefix: &str) -> String {
    format!("{STORE_PREFIX}document:{prefix}")
}

fn status_iri(prefix: &str) -> String {
    format!("{STORE_PREFIX}status:{prefix}")
}

fn unbound_store(e: Error, target: &str) -> Error {
    match e {
        Error::Unresolved(missing) if missing.as_str() == target => Error::Unsupported {
            verb: None,
            detail: format!(
                "name: this host keeps no copy of a repository's file; bind {STORE_TEMPLATE} to \
                 add it"
            ),
        },
        other => other,
    }
}

async fn read_stored(inv: &Invocation<'_>, target: &str) -> Result<Option<Representation>> {
    match inv.source(&parsed_iri(target)?).await {
        Ok(repr) => Ok(Some(repr)),
        Err(Error::NotFound(_)) => Ok(None),
        Err(e) => Err(unbound_store(e, target)),
    }
}

async fn write_stored(
    inv: &Invocation<'_>,
    target: &str,
    body: Vec<u8>,
    expected: Option<ContentId>,
) -> Result<()> {
    let mut request =
        Request::new(Verb::Sink, parsed_iri(target)?).with_arg("content", ArgRef::Inline(body));
    if let Some(expected) = expected {
        request = request.with_arg(
            "if-match",
            ArgRef::Inline(expected.to_string().into_bytes()),
        );
    }
    inv.issue(request)
        .await
        .map_err(|e| unbound_store(e, target))?;
    Ok(())
}

/// The version being served.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Serving {
    version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    since: Option<String>,
}

/// The newest version refused, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Refused {
    version: String,
    reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    at: Option<String>,
}

/// Why the last refresh accepted or refused nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Unread {
    reason: String,
    /// The repository, branch or file is not there, rather than the read having failed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    gone: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    at: Option<String>,
}

/// What the owner reads. Stored only when it changes, so a timer that finds nothing new
/// writes nothing, and the time on each part is when that part became true.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Status {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    serving: Option<Serving>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refused: Option<Refused>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unread: Option<Unread>,
}

async fn read_status(inv: &Invocation<'_>, prefix: &str) -> Result<(Status, Option<ContentId>)> {
    let target = status_iri(prefix);
    let Some(repr) = read_stored(inv, &target).await? else {
        return Ok((Status::default(), None));
    };
    let status = serde_json::from_slice(&repr.bytes).map_err(|e| {
        Error::Endpoint(format!(
            "name: {target} is not a status this module wrote: {e}"
        ))
    })?;
    Ok((status, Some(repr.content_id())))
}

// --- reading the repository -------------------------------------------------

/// The namespace at exactly `prefix`, and its repository.
fn bound_namespace<'r>(
    registry: &'r Registry,
    prefix: &str,
) -> Result<(&'r Namespace, &'r Repository)> {
    let namespace = registry
        .namespaces
        .iter()
        .find(|ns| ns.prefix == prefix)
        .ok_or_else(|| Error::NotFound(format!("name: no namespace claims {prefix:?}")))?;
    let repository = namespace.repository.as_ref().ok_or_else(|| {
        Error::NotFound(format!("name: {prefix:?} is not served from a repository"))
    })?;
    Ok((namespace, repository))
}

/// A private repository's token, read from the secret store under the namespace's own
/// name. Never logged, and scrubbed from anything recorded.
async fn read_token(
    inv: &Invocation<'_>,
    prefix: &str,
    repository: &Repository,
) -> Result<Option<String>> {
    if !repository.private {
        return Ok(None);
    }
    let name = token_secret(tenant_of(inv.capability), prefix);
    let iri = parsed_iri(&format!("{SECRET_PREFIX}{name}"))?;
    let secret = inv.source(&iri).await?;
    let token = String::from_utf8(secret.bytes).map_err(|_| Error::InvalidArgument {
        name: "token".into(),
        detail: format!("the secret {name} is not text"),
    })?;
    Ok(Some(token.trim().to_string()))
}

async fn read_repository(
    inv: &Invocation<'_>,
    repository: &Repository,
    token: Option<&str>,
    at: Option<&AsOf>,
) -> Result<Representation> {
    let mut request = Request::new(Verb::Source, parsed_iri(READ_IRI)?)
        .with_arg("url", ArgRef::Inline(repository.url.clone().into_bytes()))
        .with_arg(
            "branch",
            ArgRef::Inline(repository.branch.clone().into_bytes()),
        )
        .with_arg("path", ArgRef::Inline(repository.path.clone().into_bytes()));
    if let Some(token) = token {
        request = request.with_arg("token", ArgRef::Inline(token.as_bytes().to_vec()));
    }
    if let Some(at) = at {
        request = request.with_arg(AS_OF, ArgRef::Inline(at.to_string().into_bytes()));
    }
    inv.issue(request).await.map_err(|e| match e {
        Error::Unresolved(missing) if missing.as_str() == READ_IRI => Error::Unsupported {
            verb: None,
            detail: format!("name: this host reads no repositories; bind {READ_IRI} to add it"),
        },
        other => other,
    })
}

/// A type a browser would run, if a reader answered with one.
fn active_type(repr: &Representation) -> Option<&str> {
    let media = repr.repr_type.media_type.as_str();
    let media = media.split(';').next().unwrap_or(media).trim();
    ACTIVE_TYPES
        .iter()
        .find(|active| **active == media)
        .copied()
}

/// A resource named in the registry, under only the read scopes of the capability.
async fn issue_read_only(inv: &Invocation<'_>, request: Request) -> Result<Representation> {
    match inv.capability.scopes() {
        None => inv.issue(request).await,
        Some(held) => {
            let reads = held.iter().filter(|scope| {
                scope.starts_with("urn:cap:fs:read:") || scope.starts_with("urn:cap:name:tenant:")
            });
            inv.issue_attenuated(request, reads.cloned()).await
        }
    }
}

async fn validate(inv: &Invocation<'_>, data: &str, shapes: &str) -> Result<serde_json::Value> {
    let request = Request::new(Verb::Source, parsed_iri(VALIDATE_IRI)?)
        .with_arg("data", ArgRef::Inline(data.as_bytes().to_vec()))
        .with_arg("shapes", ArgRef::Inline(shapes.as_bytes().to_vec()))
        .with_arg("as", ArgRef::Inline(b"application/json".to_vec()));
    let report = issue_read_only(inv, request).await?;
    serde_json::from_slice(&report.bytes)
        .map_err(|e| Error::Endpoint(format!("name: unreadable {VALIDATE_IRI} report: {e}")))
}

/// The failures that say nothing about the version: the host's, or a moment's.
fn says_nothing_about_the_version(e: &Error) -> bool {
    e.is_transient()
        || matches!(
            e,
            Error::Unresolved(_) | Error::Denied(_) | Error::Unsupported { .. }
        )
}

/// What a refresh found.
enum Found {
    /// A version that passed.
    Good(Vec<u8>),
    /// A version that did not, with its identity and the reason.
    Refused(String, String),
    /// Nothing that could be judged.
    Unread { reason: String, gone: bool },
}

/// `version`'s content ID, as the status names it: of the Turtle it would be served as.
fn version_of(bytes: &[u8]) -> String {
    Representation::new(ReprType::new(CANONICAL), bytes.to_vec())
        .content_id()
        .to_string()
}

/// Read a version and judge it. Nothing here writes.
async fn fetch_and_judge(
    inv: &Invocation<'_>,
    repository: &Repository,
    ceiling: usize,
    token: Option<&str>,
) -> Result<Found> {
    let read = match read_repository(inv, repository, token, None).await {
        Ok(read) => read,
        // The host's own gap, not the repository's: nothing to record for the owner.
        Err(e @ Error::Unsupported { .. }) if e.to_string().contains(READ_IRI) => return Err(e),
        Err(Error::NotFound(why)) => {
            return Ok(Found::Unread {
                reason: format!(
                    "{} on {} has no {}: {why}",
                    repository.url, repository.branch, repository.path
                ),
                gone: true,
            })
        }
        Err(e) => {
            return Ok(Found::Unread {
                reason: format!("the repository could not be read: {e}"),
                gone: false,
            })
        }
    };
    let bytes = read.bytes.clone();
    let version = version_of(&bytes);
    if let Some(active) = active_type(&read) {
        return Ok(Found::Refused(
            version,
            format!("the repository answered {active}, and only Turtle is served"),
        ));
    }
    if bytes.len() > ceiling {
        return Ok(Found::Refused(
            version,
            format!(
                "it is {} bytes, over this host's {ceiling}-byte ceiling (raise \
                 limits.max_document_bytes in the registry)",
                bytes.len()
            ),
        ));
    }
    let Ok(data) = std::str::from_utf8(&bytes) else {
        return Ok(Found::Refused(version, "it is not UTF-8 text".into()));
    };
    if data.trim().is_empty() {
        return Ok(Found::Refused(version, "the file is empty".into()));
    }

    // The shapes are checked apart from the version, so that a bad shape is never
    // reported as the owner's push having failed, and marked as inline text, so that
    // what a shapes file holds is never followed as a reference.
    let shape = &repository.shape;
    let unchecked = |e: Error| Found::Unread {
        reason: format!("the version could not be checked against {shape}: {e}"),
        gone: false,
    };
    let shapes = match issue_read_only(inv, Request::new(Verb::Source, parsed_iri(shape)?)).await {
        Ok(shapes) => shapes,
        Err(e) => return Ok(unchecked(e)),
    };
    let shapes = format!("# {shape}\n{}", String::from_utf8_lossy(&shapes.bytes));
    if let Err(e) = validate(inv, "", &shapes).await {
        return Ok(unchecked(e));
    }

    let report = match validate(inv, data, &shapes).await {
        Ok(report) => report,
        Err(e) if says_nothing_about_the_version(&e) => return Ok(unchecked(e)),
        Err(e) => {
            return Ok(Found::Refused(version, format!("it cannot be read: {e}")));
        }
    };
    if report["conforms"].as_bool() == Some(true) {
        return Ok(Found::Good(bytes));
    }
    Ok(Found::Refused(version, violations(&report, shape)))
}

/// A report as a sentence: the first few violations, and how many more.
fn violations(report: &serde_json::Value, shape: &str) -> String {
    let found = report["violations"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    let listed: Vec<String> = found
        .iter()
        .take(MAX_LISTED_VIOLATIONS)
        .map(|violation| {
            let node = violation["focus_node"].as_str().unwrap_or("a node");
            let why = violation["message"]
                .as_str()
                .or(violation["component"].as_str())
                .unwrap_or("a violation");
            match violation["path"].as_str() {
                Some(path) => format!("{node}, at {path}: {why}"),
                None => format!("{node}: {why}"),
            }
        })
        .collect();
    let more = found.len().saturating_sub(MAX_LISTED_VIOLATIONS);
    let more = if more > 0 {
        format!("; and {more} more")
    } else {
        String::new()
    };
    format!(
        "it does not conform to {shape}: {}{more}",
        listed.join("; ")
    )
}

/// A reason as one line of bounded length that holds no token.
fn recorded(reason: &str, token: Option<&str>) -> String {
    let scrubbed = match token {
        Some(token) if !token.is_empty() => reason.replace(token, "[token]"),
        _ => reason.to_string(),
    };
    let line: String = scrubbed
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_REASON_CHARS)
        .collect();
    line
}

/// The current instant in UTC, on a kernel with a clock.
fn now_utc(inv: &Invocation<'_>) -> Option<String> {
    let seconds = i64::try_from(inv.now()?.as_millis() / 1_000).ok()?;
    crate::history::format_utc(seconds)
}

/// The branch a push payload names, if it names another. A payload that cannot be read
/// means a refresh: it costs one read, and its signature has already been checked.
fn other_branch(inv: &Invocation<'_>, repository: &Repository) -> Option<String> {
    let body = inv.inline_arg("content").ok()?;
    let payload: serde_json::Value = serde_json::from_slice(body).ok()?;
    let pushed = payload.get("ref")?.as_str()?;
    (pushed != format!("refs/heads/{}", repository.branch)).then(|| pushed.to_string())
}

/// Read the repository, and record what that found. Answers one line.
async fn refresh_namespace(inv: &Invocation<'_>, prefix: &str) -> Result<String> {
    let registry = load(inv).await?;
    let (_, repository) = bound_namespace(&registry, prefix)?;
    if let Some(pushed) = other_branch(inv, repository) {
        return Ok(format!(
            "{prefix:?}: nothing to do: this push is to {pushed}, and {} is served",
            repository.branch
        ));
    }

    let token = match read_token(inv, prefix, repository).await {
        Ok(token) => token,
        Err(e) => {
            let name = token_secret(tenant_of(inv.capability), prefix);
            return record_unread(
                inv,
                prefix,
                format!("its token could not be read from the secret {name}: {e}"),
                false,
                None,
            )
            .await;
        }
    };
    let found = fetch_and_judge(
        inv,
        repository,
        registry.limits.max_document_bytes,
        token.as_deref(),
    )
    .await?;
    match found {
        Found::Good(bytes) => record_good(inv, prefix, bytes).await,
        Found::Refused(version, reason) => {
            let reason = recorded(&reason, token.as_deref());
            let (mut status, expected) = read_status(inv, prefix).await?;
            let before = status.clone();
            status.unread = None;
            if status
                .refused
                .as_ref()
                .is_none_or(|held| held.version != version)
            {
                status.refused = Some(Refused {
                    version: version.clone(),
                    reason: reason.clone(),
                    at: now_utc(inv),
                });
            }
            store_status(inv, prefix, &status, &before, expected).await?;
            Ok(format!(
                "{prefix:?}: refused {version}, and the last good version keeps serving: {reason}"
            ))
        }
        Found::Unread { reason, gone } => {
            record_unread(inv, prefix, reason, gone, token.as_deref()).await
        }
    }
}

async fn store_status(
    inv: &Invocation<'_>,
    prefix: &str,
    status: &Status,
    before: &Status,
    expected: Option<ContentId>,
) -> Result<()> {
    if status == before {
        return Ok(());
    }
    let body =
        serde_json::to_vec_pretty(status).map_err(|e| Error::Endpoint(format!("name: {e}")))?;
    write_stored(inv, &status_iri(prefix), body, expected).await
}

async fn record_good(inv: &Invocation<'_>, prefix: &str, bytes: Vec<u8>) -> Result<String> {
    let version = version_of(&bytes);
    let target = document_iri(prefix);
    let stored = read_stored(inv, &target).await?;
    let unchanged = stored.as_ref().is_some_and(|held| held.bytes == bytes);
    if !unchanged {
        // The write is what cuts the thread every answer from the old version hangs from.
        write_stored(inv, &target, bytes, stored.map(|held| held.content_id())).await?;
    }
    let (mut status, expected) = read_status(inv, prefix).await?;
    let before = status.clone();
    status.refused = None;
    status.unread = None;
    if status
        .serving
        .as_ref()
        .is_none_or(|held| held.version != version)
    {
        status.serving = Some(Serving {
            version: version.clone(),
            since: now_utc(inv),
        });
    }
    store_status(inv, prefix, &status, &before, expected).await?;
    Ok(if unchanged {
        format!("{prefix:?}: unchanged, serving {version}")
    } else {
        format!("{prefix:?}: now serving {version}")
    })
}

async fn record_unread(
    inv: &Invocation<'_>,
    prefix: &str,
    reason: String,
    gone: bool,
    token: Option<&str>,
) -> Result<String> {
    let reason = recorded(&reason, token);
    let (mut status, expected) = read_status(inv, prefix).await?;
    let before = status.clone();
    if status
        .unread
        .as_ref()
        .is_none_or(|held| held.reason != reason || held.gone != gone)
    {
        status.unread = Some(Unread {
            reason: reason.clone(),
            gone,
            at: now_utc(inv),
        });
    }
    store_status(inv, prefix, &status, &before, expected).await?;
    Ok(format!("{prefix:?}: nothing accepted: {reason}"))
}

// --- as of a date -----------------------------------------------------------

/// The namespace's file as it stood at `at`, from the same repository's history.
pub(crate) async fn read_past(
    inv: &Invocation<'_>,
    prefix: &str,
    repository: &Repository,
    at: &AsOf,
) -> Result<Representation> {
    let token = read_token(inv, prefix, repository).await?;
    let past = read_repository(inv, repository, token.as_deref(), Some(at))
        .await
        .map_err(|e| match e {
            Error::NotFound(_) => absent(&source_iri(prefix), at),
            other => other,
        })?;
    if let Some(active) = active_type(&past) {
        return Err(Error::Endpoint(format!(
            "name: {} as of {at} answered {active}, and only Turtle is served",
            source_iri(prefix)
        )));
    }
    // Deliberately not `.cacheable()`: the cache is not the archive.
    Ok(Representation::new(ReprType::new(CANONICAL), past.bytes))
}

// --- the faces --------------------------------------------------------------

fn bound_prefix<'a>(inv: &'a Invocation<'_>) -> Result<&'a str> {
    inv.bindings
        .get("prefix")
        .ok_or_else(|| Error::MissingArgument("prefix".into()))
}

fn prefix_input() -> ArgSpec {
    ArgSpec::new("prefix")
        .summary("the namespace, as claimed, e.g. acme")
        .binding()
        .class(XSD_STRING)
}

/// [`SOURCE_TEMPLATE`] — the last good copy. It reads the store and never the repository.
pub(crate) fn source() -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("git-source", |inv: &Invocation<'_>| -> InvokeFuture<'_> {
        Box::pin(async move {
            let prefix = bound_prefix(inv)?;
            let registry = load(inv).await?;
            let (namespace, _) = bound_namespace(&registry, prefix)?;
            let (status, _) = read_status(inv, prefix).await?;
            let gone = status.unread.as_ref().is_some_and(|unread| unread.gone);
            if gone && matches!(namespace.strategy, Strategy::Hosted { .. }) {
                return Err(Error::NotFound(format!(
                    "name: the repository serving {prefix:?} no longer has its file"
                )));
            }
            let target = document_iri(prefix);
            let Some(document) = read_stored(inv, &target).await? else {
                // Temporary by nature: the next good refresh fills it.
                return Err(Error::Unavailable(format!(
                    "name: {prefix:?} has no version to serve yet; its administrator can read \
                     why at urn:name:git-status:{prefix}"
                )));
            };
            // The type is this module's, never the store's or the repository's.
            Ok(Representation::new(ReprType::new(CANONICAL), document.bytes).cacheable())
        })
    })
    .with_description(
        Description::new("git-source")
            .title("A namespace's file from its repository")
            .summary(
                "The last version of a repository's file that passed its shapes, as Turtle. It \
                 never reads the repository: a refresh does that.",
            )
            .action(
                ActionSpec::new(Verb::Source)
                    .summary("read the last good copy")
                    .input(prefix_input())
                    .output(CANONICAL),
            ),
    )
}

/// [`REFRESH_TEMPLATE`] — read the repository again.
pub(crate) fn refresh() -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("git-refresh", |inv: &Invocation<'_>| -> InvokeFuture<'_> {
        Box::pin(async move {
            let prefix = bound_prefix(inv)?;
            let line = refresh_namespace(inv, prefix).await?;
            Ok(text_answer(format!("{line}\n")))
        })
    })
    .with_description(
        Description::new("git-refresh")
            .title("Read a namespace's repository again")
            .summary(
                "Reads the file, checks it against the namespace's shapes, and either serves it \
                 from now on or records why not, leaving the last good version in place. A host \
                 issues this when a push webhook arrives, and on a timer.",
            )
            .action(
                ActionSpec::new(Verb::Sink)
                    .summary("read the repository, and accept or refuse what it finds")
                    .requires(CAP_REFRESH)
                    .input(prefix_input())
                    .input(
                        ArgSpec::new("content")
                            .summary("the push payload, whose `ref` skips a push to another branch")
                            .optional()
                            .class(XSD_STRING),
                    )
                    .output(TEXT_PLAIN_UTF8),
            ),
    )
}

/// [`STATUS_TEMPLATE`] — what the owner reads.
pub(crate) fn status() -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("git-status", |inv: &Invocation<'_>| -> InvokeFuture<'_> {
        Box::pin(async move {
            let prefix = bound_prefix(inv)?;
            crate::admin::refuse_reserved_prefix(prefix)?;
            let registry = load(inv).await?;
            crate::admin::require_admin(
                &registry,
                inv.capability,
                prefix,
                "reading what happened to",
            )?;
            let (_, repository) = bound_namespace(&registry, prefix)?;
            let (status, _) = read_status(inv, prefix).await?;
            Ok(text_answer(status_lines(repository, &status)))
        })
    })
    .with_description(
        Description::new("git-status")
            .title("What happened to a namespace's repository")
            .summary(
                "One labeled line each for the repository, the version serving, the version \
                 last refused and why, and a repository that could not be read.",
            )
            .action(
                ActionSpec::new(Verb::Source)
                    .summary("read what the last refreshes found")
                    .requires(crate::admin::CAP_ADMIN_ANY)
                    .input(prefix_input())
                    .output(TEXT_PLAIN_UTF8),
            ),
    )
}

/// `, at <instant>`, or nothing on a kernel with no clock.
fn at_clause(word: &str, at: Option<&String>) -> String {
    at.map(|at| format!(", {word} {at}")).unwrap_or_default()
}

fn status_lines(repository: &Repository, status: &Status) -> String {
    let mut lines = format!(
        "repository: {}, branch {}, file {}\n",
        repository.url, repository.branch, repository.path
    );
    match &status.serving {
        Some(serving) => lines.push_str(&format!(
            "serving: {}{}\n",
            serving.version,
            at_clause("since", serving.since.as_ref())
        )),
        None => lines.push_str("serving: nothing yet\n"),
    }
    if let Some(refused) = &status.refused {
        lines.push_str(&format!(
            "refused: {}{}: {}\n",
            refused.version,
            at_clause("at", refused.at.as_ref()),
            refused.reason
        ));
    }
    if let Some(unread) = &status.unread {
        let label = if unread.gone { "gone" } else { "not read" };
        lines.push_str(&format!(
            "{label}{}: {}\n",
            at_clause("at", unread.at.as_ref()),
            unread.reason
        ));
    }
    lines
}

// --- a reader, over HTTP ----------------------------------------------------

/// The API a forge answers. Each is a different URL for the same two questions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeKind {
    /// GitHub, or GitHub Enterprise.
    GitHub,
    /// GitLab.
    GitLab,
    /// Forgejo or Gitea. Its commit listing ignores a date, so `as-of` is refused.
    Forgejo,
}

/// A host [`forge_reader`] will read, and where its API is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forge {
    /// The host in a repository's URL, e.g. `github.com`.
    pub host: String,
    /// The API it answers.
    pub kind: ForgeKind,
    /// Where its API is, with no trailing `/`, e.g. `https://api.github.com`.
    pub api: String,
}

/// The public forges [`forge_reader`] knows.
pub fn known_forges() -> Vec<Forge> {
    let forge = |host: &str, kind, api: &str| Forge {
        host: host.into(),
        kind,
        api: api.into(),
    };
    vec![
        forge("github.com", ForgeKind::GitHub, "https://api.github.com"),
        forge("gitlab.com", ForgeKind::GitLab, "https://gitlab.com/api/v4"),
        forge(
            "codeberg.org",
            ForgeKind::Forgejo,
            "https://codeberg.org/api/v1",
        ),
    ]
}

/// Everything outside `A-Z a-z 0-9 - . _ ~` as `%XX`, and `/` too unless it is kept.
fn encoded(text: &str, keep_slash: bool) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~')
            || (keep_slash && byte == b'/')
        {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

impl Forge {
    fn file_url(&self, slug: &str, path: &str, rev: &str) -> String {
        let (api, rev) = (&self.api, encoded(rev, false));
        match self.kind {
            ForgeKind::GitHub => {
                format!(
                    "{api}/repos/{slug}/contents/{}?ref={rev}",
                    encoded(path, true)
                )
            }
            ForgeKind::GitLab => format!(
                "{api}/projects/{}/repository/files/{}/raw?ref={rev}",
                encoded(slug, false),
                encoded(path, false)
            ),
            ForgeKind::Forgejo => {
                format!("{api}/repos/{slug}/raw/{}?ref={rev}", encoded(path, true))
            }
        }
    }

    /// The URL listing the newest commit touching `path` by `until`, and the field of
    /// each entry that names it.
    fn commit_url(
        &self,
        slug: &str,
        branch: &str,
        path: &str,
        until: &str,
    ) -> Option<(String, &'static str)> {
        let api = &self.api;
        let (branch, file, until) = (
            encoded(branch, false),
            encoded(path, false),
            encoded(until, false),
        );
        match self.kind {
            ForgeKind::GitHub => Some((
                format!(
                    "{api}/repos/{slug}/commits?sha={branch}&path={file}&until={until}&per_page=1"
                ),
                "sha",
            )),
            ForgeKind::GitLab => Some((
                format!(
                    "{api}/projects/{}/repository/commits?ref_name={branch}&path={file}\
                     &until={until}&per_page=1",
                    encoded(slug, false)
                ),
                "id",
            )),
            ForgeKind::Forgejo => None,
        }
    }

    /// `owner/name` from a repository's URL, or `None` if it is not one of this forge's.
    fn slug(&self, url: &str) -> Option<String> {
        let rest = url.strip_prefix("https://")?;
        let (host, path) = rest.split_once('/')?;
        if !host.eq_ignore_ascii_case(&self.host) {
            return None;
        }
        let path = path.trim_end_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        let parts = segments(path);
        let fits = match self.kind {
            ForgeKind::GitLab => parts.len() >= 2,
            ForgeKind::GitHub | ForgeKind::Forgejo => parts.len() == 2,
        };
        let safe = parts.iter().all(|part| {
            *part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
        });
        (fits && safe).then(|| parts.join("/"))
    }
}

async fn http_get(inv: &Invocation<'_>, url: &str, token: Option<&str>) -> Result<Representation> {
    let mut request = Request::new(Verb::Source, parsed_iri(HTTP_GET_IRI)?)
        .with_arg("url", ArgRef::Inline(url.as_bytes().to_vec()))
        // A push has just happened, so an origin's own freshness must not answer this.
        .with_arg("max_age", ArgRef::Inline(b"0".to_vec()))
        .with_arg(
            "headers",
            ArgRef::Inline(
                b"Accept: application/vnd.github.raw, text/plain;q=0.9, */*;q=0.1\n\
                  User-Agent: ikigai-name\n"
                    .to_vec(),
            ),
        );
    if let Some(token) = token {
        request = request.with_arg(
            "authorization",
            ArgRef::Inline(format!("Bearer {token}").into_bytes()),
        );
    }
    inv.issue(request).await.map_err(|e| match e {
        Error::Unresolved(missing) if missing.as_str() == HTTP_GET_IRI => Error::Unsupported {
            verb: None,
            detail: format!(
                "name: this host has no HTTP client, so it reads no repositories; bind \
                 {HTTP_GET_IRI} to add it"
            ),
        },
        other => other,
    })
}

/// A [`READ_IRI`] binding over `urn:http:get`, for [`known_forges`].
pub fn forge_reader() -> AsyncFnEndpoint {
    forge_reader_for(known_forges())
}

/// A [`READ_IRI`] binding over `urn:http:get`, for these forges and no others.
///
/// A registry is written by claimants, and a read goes where it says, so a host outside
/// this list is refused rather than fetched. The capability must also grant
/// `urn:cap:net:` for each API, and `urn:http:get` checks that at every redirect.
pub fn forge_reader_for(forges: Vec<Forge>) -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("git-read", move |inv: &Invocation<'_>| {
        let forges = forges.clone();
        Box::pin(async move {
            let (url, branch, path) = (
                inv.inline_str("url")?,
                inv.inline_str("branch")?,
                inv.inline_str("path")?,
            );
            let token = inv.inline_str("token").ok();
            let at = as_of_arg(inv)?;
            let Some((forge, slug)) = forges
                .iter()
                .find_map(|forge| Some((forge, forge.slug(url)?)))
            else {
                return Err(Error::Unsupported {
                    verb: None,
                    detail: format!(
                        "name: this host does not read repositories at {url}; it reads {}",
                        forges
                            .iter()
                            .map(|forge| forge.host.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                });
            };
            let rev = match &at {
                None => branch.to_string(),
                Some(at) => {
                    let until = at.to_string();
                    let Some((list, field)) = forge.commit_url(&slug, branch, path, &until) else {
                        return Err(Error::Unsupported {
                            verb: None,
                            detail: format!(
                                "name: {} cannot list commits by date, so this host cannot read \
                                 {url} as of {at}",
                                forge.host
                            ),
                        });
                    };
                    let commits = http_get(inv, &list, token).await?;
                    let commits: serde_json::Value = serde_json::from_slice(&commits.bytes)
                        .map_err(|e| {
                            Error::Endpoint(format!("name: unreadable commits from {url}: {e}"))
                        })?;
                    commits[0][field]
                        .as_str()
                        .ok_or_else(|| {
                            Error::NotFound(format!("name: {path} did not exist as of {at}"))
                        })?
                        .to_string()
                }
            };
            let file = http_get(inv, &forge.file_url(&slug, path, &rev), token).await?;
            // Not marked cacheable: this must answer afresh.
            Ok(Representation::new(file.repr_type.clone(), file.bytes))
        }) as InvokeFuture<'_>
    })
    .with_description(
        Description::new("git-read")
            .title("A file in a repository on a forge")
            .action(
                ActionSpec::new(Verb::Source)
                    .summary("read a file at a branch's head, or as of a date")
                    .requires("urn:cap:net:*")
                    .input(
                        ArgSpec::new("url")
                            .summary("the repository's https: address")
                            .class(XSD_STRING),
                    )
                    .input(
                        ArgSpec::new("branch")
                            .summary("the branch")
                            .class(XSD_STRING),
                    )
                    .input(
                        ArgSpec::new("path")
                            .summary("the file, relative to the repository's root")
                            .class(XSD_STRING),
                    )
                    .input(
                        ArgSpec::new("token")
                            .summary("a read-only token, for a private repository")
                            .optional()
                            .class(XSD_STRING),
                    )
                    .input(
                        ArgSpec::new(AS_OF)
                            .summary("read the newest commit touching it at or before this instant")
                            .optional()
                            .class(XSD_STRING),
                    ),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> Repository {
        Repository {
            url: "https://github.com/acme/vocab".into(),
            branch: "main".into(),
            path: "vocab/core.ttl".into(),
            shape: "urn:file:shapes/acme.ttl".into(),
            private: false,
        }
    }

    #[test]
    fn a_good_binding_passes() {
        assert_eq!(good().check(), Ok(()));
    }

    #[test]
    fn repository_html_or_javascript_cannot_be_named() {
        for path in [
            "index.html",
            "app.js",
            "page.xhtml",
            "image.svg",
            "vocab.rdf",
            "vocab.ttl.html",
            "ttl",
            "",
        ] {
            let bad = Repository {
                path: path.into(),
                ..good()
            };
            let reason = bad.check().expect_err(path);
            assert!(reason.contains(".ttl"), "{path}: {reason}");
        }
    }

    #[test]
    fn a_path_outside_the_repository_is_refused() {
        for path in [
            "../x.ttl",
            "a/../../x.ttl",
            "/etc/x.ttl",
            "--flag.ttl",
            "a b.ttl",
            "./x.ttl",
        ] {
            let bad = Repository {
                path: path.into(),
                ..good()
            };
            let reason = bad.check().expect_err(path);
            assert!(reason.contains("not a path inside"), "{path}: {reason}");
        }
    }

    #[test]
    fn a_bad_branch_is_refused() {
        for branch in ["", "-x", "a..b", "a b", "a:b", "a~1", "a^", "a\nb"] {
            let bad = Repository {
                branch: branch.into(),
                ..good()
            };
            assert!(bad.check().is_err(), "{branch:?}");
        }
    }

    /// The refusal must not repeat what it refuses: this is a token in a URL.
    #[test]
    fn credentials_in_a_url_are_refused_without_repeating_them() {
        for url in [
            "https://ghp_secret@github.com/acme/vocab",
            "https://user:ghp_secret@github.com/acme/vocab",
        ] {
            let bad = Repository {
                url: url.into(),
                ..good()
            };
            let reason = bad.check().expect_err(url);
            assert!(reason.contains("credentials"), "{reason}");
            assert!(!reason.contains("ghp_secret"), "{reason}");
        }
    }

    #[test]
    fn a_url_that_is_not_https_is_refused() {
        for url in [
            "http://github.com/acme/vocab",
            "ssh://git@github.com/acme/vocab",
            "git@github.com:acme/vocab",
            "https://",
            "https://github.com/a?b",
            "https://github.com/a#b",
            "https://github.com/a b",
        ] {
            let bad = Repository {
                url: url.into(),
                ..good()
            };
            assert!(bad.check().is_err(), "{url}");
        }
    }

    /// A shape is read under the refresh's authority, so it cannot be just any IRI.
    #[test]
    fn a_shape_must_be_a_file() {
        for shape in [
            "urn:secret:other",
            "https://evil.example/s.ttl",
            "urn:kernel:cache",
            "",
        ] {
            let bad = Repository {
                shape: shape.into(),
                ..good()
            };
            assert!(bad.check().is_err(), "{shape}");
        }
    }

    fn signature(key: &[u8], body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
        mac.update(body);
        let hex: String = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        format!("sha256={hex}")
    }

    #[test]
    fn only_the_right_key_over_the_exact_body_verifies() {
        let (key, body) = (
            b"key".as_slice(),
            b"{\"ref\":\"refs/heads/main\"}".as_slice(),
        );
        let good = signature(key, body);
        assert!(verify_push(key, body, &good));
        assert!(!verify_push(b"other", body, &good));
        assert!(!verify_push(key, b"{}", &good));
        for bad in ["", "sha256=", "sha256=zz", "sha1=abcd", "sha256=abc"] {
            assert!(!verify_push(key, body, bad), "{bad:?}");
        }
        // An unset secret is not a secret everyone knows.
        assert!(!verify_push(b"", body, &signature(b"", body)));
    }

    #[test]
    fn secret_names_cannot_reach_each_other() {
        // `a.b` the prefix and tenant `a`, prefix `b` must not collide.
        assert_ne!(token_secret(None, "a.b"), token_secret(Some("a"), "b"));
        assert_ne!(token_secret(None, "a"), hook_secret(None, "a"));
        assert_eq!(token_secret(None, "a/b"), "name-git-token.a~2Fb");
        assert!(!token_secret(None, "../x").contains('/'));
    }

    #[test]
    fn a_reason_is_one_bounded_line_with_no_token() {
        let reason = recorded("bad\ttok-123\nline", Some("tok-123"));
        assert_eq!(reason, "bad [token] line");
        assert_eq!(recorded(&"x".repeat(9_000), None).len(), MAX_REASON_CHARS);
    }

    fn forge(kind: ForgeKind) -> Forge {
        Forge {
            host: "f.example".into(),
            kind,
            api: "https://api.f.example".into(),
        }
    }

    #[test]
    fn each_forge_is_asked_for_a_file_in_its_own_words() {
        for (kind, want) in [
            (
                ForgeKind::GitHub,
                "https://api.f.example/repos/acme/vocab/contents/a%20b/c.ttl?ref=rel%2F1",
            ),
            (
                ForgeKind::GitLab,
                "https://api.f.example/projects/acme%2Fvocab/repository/files/a%20b%2Fc.ttl/raw\
                 ?ref=rel%2F1",
            ),
            (
                ForgeKind::Forgejo,
                "https://api.f.example/repos/acme/vocab/raw/a%20b/c.ttl?ref=rel%2F1",
            ),
        ] {
            assert_eq!(
                forge(kind).file_url("acme/vocab", "a b/c.ttl", "rel/1"),
                want,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn a_forge_with_no_dated_listing_says_so() {
        assert!(forge(ForgeKind::Forgejo)
            .commit_url("a/b", "main", "c.ttl", "2026-03-31T23:59:59Z")
            .is_none());
        let (url, field) = forge(ForgeKind::GitHub)
            .commit_url("a/b", "main", "d/c.ttl", "2026-03-31T23:59:59Z")
            .unwrap();
        assert_eq!(field, "sha");
        assert_eq!(
            url,
            "https://api.f.example/repos/a/b/commits?sha=main&path=d%2Fc.ttl\
             &until=2026-03-31T23%3A59%3A59Z&per_page=1"
        );
    }

    #[test]
    fn a_slug_comes_only_from_the_forge_s_own_host_and_a_safe_path() {
        let github = forge(ForgeKind::GitHub);
        let gitlab = forge(ForgeKind::GitLab);
        for (url, want) in [
            ("https://f.example/acme/vocab", Some("acme/vocab")),
            ("https://f.example/acme/vocab.git", Some("acme/vocab")),
            ("https://f.example/acme/vocab/", Some("acme/vocab")),
            ("https://F.EXAMPLE/acme/vocab", Some("acme/vocab")),
            ("https://evil.example/acme/vocab", None),
            ("https://f.example.evil.example/acme/vocab", None),
            ("https://f.example/acme", None),
            ("https://f.example/acme/group/vocab", None),
            ("https://f.example/../vocab", None),
            ("https://f.example/acme/vo%2Fcab", None),
        ] {
            assert_eq!(github.slug(url).as_deref(), want, "{url}");
        }
        assert_eq!(
            gitlab.slug("https://f.example/acme/group/vocab").as_deref(),
            Some("acme/group/vocab")
        );
    }

    #[test]
    fn the_status_is_one_labeled_line_each() {
        let status = Status {
            serving: Some(Serving {
                version: "v1".into(),
                since: Some("2026-11-05T00:00:00Z".into()),
            }),
            refused: Some(Refused {
                version: "v2".into(),
                reason: "it does not conform".into(),
                at: None,
            }),
            unread: Some(Unread {
                reason: "no file".into(),
                gone: true,
                at: None,
            }),
        };
        assert_eq!(
            status_lines(&good(), &status),
            "repository: https://github.com/acme/vocab, branch main, file vocab/core.ttl\n\
             serving: v1, since 2026-11-05T00:00:00Z\n\
             refused: v2: it does not conform\n\
             gone: no file\n"
        );
        assert_eq!(
            status_lines(&good(), &Status::default()),
            "repository: https://github.com/acme/vocab, branch main, file vocab/core.ttl\n\
             serving: nothing yet\n"
        );
    }
}
