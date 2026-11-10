//! The namespace registry: who owns which prefix, and how it resolves.
//!
//! The registry is **data an operator edits**, never something compiled in — a
//! resolution rule can change without a rebuild, and because the registry is
//! read back through the kernel it is golden-threaded (edit the file, the thread
//! cuts, the next resolve is correct), diffable, and as-of resolvable like any
//! other resource (see [`history`](crate::history)).
//!
//! ## Why the claim rule is the interesting part
//!
//! Two tenants sharing one resolver must not be able to answer for the same
//! IRI. That could be arbitrated at resolve time — precedence rules, longest
//! match, an ordering — but every such scheme has a wrong answer somewhere and
//! has to be got right on every request. Instead the conflict is refused at
//! **claim** time: a prefix may be claimed only when no claimed prefix contains
//! it and it contains no claimed prefix. Afterwards overlap is not merely
//! forbidden, it is *unrepresentable*, so resolution never arbitrates.
//!
//! "Unrepresentable" has to hold for a registry that arrives from a file as
//! much as for one built by claims — the operator edits the file by hand, and a
//! hand can write `resmud` and `resmud/core` side by side. So [`Registry::from_json`]
//! does not merely parse: it rebuilds the registry by claiming each entry in
//! turn, through the same [`Registry::claim`], and refuses the **whole** file
//! at the first entry the claim rule would have refused. There is one rule,
//! not a load rule and a claim rule that must be kept in step.

use crate::git::repository_refusal;
pub use crate::git::Repository;
use crate::iri::name_segments;
use crate::rules::{status_from_json, status_refusal};
pub use crate::rules::{RedirectStatus, Rule};
use serde::{Deserialize, Serialize};

/// How a claimed namespace produces its answer.
///
/// The three cases are why this is a resolver rather than a redirector: a
/// redirect service can only ever express the middle one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "strategy")]
pub enum Strategy {
    /// The documents live here, served from `source`.
    Hosted {
        /// The resource the documents are read from.
        source: String,
    },
    /// The owner serves it themselves; we answer with a redirect to `target`, or
    /// to where the first matching rule says.
    Redirect {
        /// Where the owner serves it, when no rule matches.
        target: String,
        /// The status a redirect answers with, unless a rule says otherwise.
        #[serde(default)]
        status: RedirectStatus,
        /// How to reshape a path, in order: the first that matches answers.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        rules: Vec<Rule>,
    },
    /// A cached copy is served, but `origin` remains the source of truth, so
    /// the namespace survives its origin going dark.
    Mirror {
        /// The source of truth this copy tracks.
        origin: String,
    },
    /// Withdrawn from service, and **still claimed**.
    ///
    /// Releasing a prefix back into the pool would let a later owner answer for
    /// IRIs the earlier one minted — an identifier hijack, and precisely the
    /// failure a permanence service exists to prevent. So withdrawal is a
    /// tombstone: the claim survives, resolution reports the retirement, and the
    /// prefix can never be claimed again.
    Retired {
        /// Why it was withdrawn, for whoever dereferences an IRI under it.
        reason: String,
    },
    /// Answered by another resolver: a name under it is forwarded to `upstream`, which
    /// is where a local resolver sends what it does not hold itself. Forwarding needs
    /// `urn:cap:net:<upstream's host>` ([`Strategy::net_scope`]), which is checked
    /// before anything leaves.
    Delegate {
        /// The upstream resolver's `https:` (or `http:`) address, such as
        /// `https://urnid.org/`.
        upstream: String,
    },
}

impl Strategy {
    /// The strategy's name, as the registry and every answer write it.
    pub fn name(&self) -> &'static str {
        match self {
            Strategy::Hosted { .. } => "hosted",
            Strategy::Redirect { .. } => "redirect",
            Strategy::Mirror { .. } => "mirror",
            Strategy::Retired { .. } => "retired",
            Strategy::Delegate { .. } => "delegate",
        }
    }

    /// The network scope this strategy needs to answer, if it needs one: for a
    /// `delegate`, `urn:cap:net:<host>` of its upstream.
    pub fn net_scope(&self) -> Option<String> {
        match self {
            Strategy::Delegate { upstream } => {
                crate::tenant::iri_host(upstream).map(|host| format!("urn:cap:net:{host}"))
            }
            _ => None,
        }
    }
}

/// One claimed namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Namespace {
    /// The claimed prefix: a path such as `resmud`, or an IRI prefix such as
    /// `urn:urnid:acme:` or `tag:urnid.org,2026:acme:` (see [`crate::iri`]).
    pub prefix: String,
    /// The capability that administers it. Holding it is what permits editing
    /// this entry, reading its counts and what happened to its repository, and,
    /// when it is not public, seeing it at all; it is not a display name. A claim
    /// that names none records [`admin_scope`](crate::admin::admin_scope).
    pub owner: String,
    /// How this namespace produces its answer.
    #[serde(flatten)]
    pub strategy: Strategy,
    /// The git repository a `hosted` or `mirror` namespace is served from, if any.
    /// See [`git`](crate::git).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<Repository>,
    /// Whether anyone may resolve a name under it. A namespace that is not public
    /// answers only a capability holding its `owner`; to anyone else it answers exactly
    /// as a name nobody claims does, so it cannot be found by asking.
    #[serde(default = "public_by_default", skip_serializing_if = "is_public")]
    pub public: bool,
}

fn public_by_default() -> bool {
    true
}

fn is_public(public: &bool) -> bool {
    *public
}

/// Where a `redirect` namespace sends one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirection<'a> {
    /// The URL, with any captures substituted.
    pub target: String,
    /// The status it answers with: the rule's, or the namespace's.
    pub status: RedirectStatus,
    /// The `match` of the rule that answered, or `None` for the namespace's target.
    pub rule: Option<&'a str>,
}

impl Namespace {
    /// Where this namespace sends `path`, or `None` if it is not a `redirect`.
    pub fn redirection(&self, path: &str) -> Option<Redirection<'_>> {
        let Strategy::Redirect {
            target,
            status,
            rules,
        } = &self.strategy
        else {
            return None;
        };
        let (_, wanted) = name_segments(path);
        let wanted: Vec<&str> = wanted.iter().map(String::as_str).collect();
        let below = wanted
            .get(name_segments(&self.prefix).1.len()..)
            .unwrap_or(&[]);
        let answered = rules.iter().find_map(|rule| {
            rule.rewrite(below).map(|target| Redirection {
                target,
                status: rule.status.unwrap_or(*status),
                rule: Some(rule.pattern.as_str()),
            })
        });
        Some(answered.unwrap_or_else(|| Redirection {
            target: target.clone(),
            status: *status,
            rule: None,
        }))
    }
}

/// What this deployment refuses to do, whatever a namespace asks for.
///
/// A ceiling the operator sets, not a tuning knob: the resolver reads whole
/// documents into memory, so an unbounded one is the only way a single request
/// can hurt the host. Stated in the registry rather than a second config file,
/// because the registry is already the operator's editing surface and a second
/// channel is a second thing to keep in step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// Largest document this host will serve, in bytes.
    #[serde(default = "default_max_document_bytes")]
    pub max_document_bytes: usize,
}

/// 8 MiB — far above any vocabulary (ResMUD core is ~4 KB) and far below what
/// would trouble the smallest sensible host.
fn default_max_document_bytes() -> usize {
    8 * 1024 * 1024
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_document_bytes: default_max_document_bytes(),
        }
    }
}

/// The whole registry, as parsed from the operator's file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    /// Every claimed namespace. No two overlap: [`Registry::claim`] refuses an
    /// overlapping claim and [`Registry::from_json`] refuses a file containing
    /// one. (A `Registry` deserialized some other way carries no such promise —
    /// `from_json` is the entry point precisely because it checks.)
    #[serde(default)]
    pub namespaces: Vec<Namespace>,
    /// This deployment's ceilings.
    #[serde(default)]
    pub limits: Limits,
    /// How a name no namespace claims is answered, if not by "nobody claims it": a
    /// `delegate` strategy, forwarding it to an upstream resolver. Nothing else may be
    /// written here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unclaimed: Option<Strategy>,
}

/// Why a claim was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimError {
    /// The prefix was empty, or every segment of it was.
    Empty,
    /// The prefix begins with `-`, so `urn:cap:name:admin:<prefix>` would be an
    /// exclusion, which core never grants.
    ReservedPrefix,
    /// An existing claim overlaps: it contains the new prefix, the new prefix
    /// contains it, or they are equal. Carries the existing prefix so the
    /// refusal can name it.
    Overlaps(String),
    /// A status that is not 301, 302, 303, 307 or 308. Carries the sentence naming it.
    BadStatus(String),
    /// `rules` was something other than a list. Carries the value as written.
    NotARuleList(String),
    /// A rewrite rule the registry cannot hold.
    BadRule {
        /// Its place in the namespace's `rules`, counting from 1.
        rule: usize,
        /// Its `match` as written, empty if it has none.
        pattern: String,
        /// A clause saying what is wrong with it.
        reason: String,
    },
    /// A repository binding the registry cannot hold. Carries the clause saying why.
    BadRepository(String),
    /// A `delegate` upstream that is not an `http:` or `https:` address with a host.
    /// Carries the upstream as written.
    BadUpstream(String),
}

impl std::fmt::Display for ClaimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaimError::Empty => write!(f, "a namespace prefix cannot be empty"),
            ClaimError::ReservedPrefix => write!(
                f,
                "a namespace prefix cannot begin with `-`: its administrative capability \
                 would be an exclusion, which no grant can satisfy"
            ),
            ClaimError::Overlaps(existing) => {
                write!(f, "overlaps the claimed namespace {existing:?}")
            }
            ClaimError::BadStatus(reason) => f.write_str(reason),
            ClaimError::NotARuleList(value) => {
                write!(f, "its rules must be a list, and {value} is not one")
            }
            ClaimError::BadRule {
                rule,
                pattern,
                reason,
            } => write!(f, "in rule {rule} ({pattern:?}), {reason}"),
            ClaimError::BadRepository(reason) => write!(f, "its repository {reason}"),
            ClaimError::BadUpstream(upstream) => write!(
                f,
                "its upstream {upstream:?} is not an https: or http: address with a host"
            ),
        }
    }
}

/// Split a prefix into path segments, ignoring leading, trailing and doubled
/// separators so `acme`, `/acme` and `acme/` are one claim rather than three.
pub(crate) fn segments(prefix: &str) -> Vec<&str> {
    prefix.split('/').filter(|s| !s.is_empty()).collect()
}

/// Whether two prefixes overlap — i.e. whether one is the other or contains it.
///
/// Comparison is **segment-wise, never textual**: `acme` contains `acme/vocab`
/// but is unrelated to `acmecorp`, which a plain `starts_with` would wrongly
/// call a conflict and refuse. An IRI prefix's segments are colon-separated, in its
/// canonical form ([`crate::iri`]), so `urn:urnid:acme:` contains
/// `tag:urnid.org,2026:acme:x:` and is unrelated to `urn:urnid:acmecorp:`. A path
/// prefix never overlaps an IRI prefix.
pub fn overlaps(a: &str, b: &str) -> bool {
    let ((a_iri, a), (b_iri, b)) = (name_segments(a), name_segments(b));
    let shared = a.len().min(b.len());
    a_iri == b_iri && a[..shared] == b[..shared]
}

/// Why a registry file was refused as a whole.
///
/// Both cases are the operator's file being wrong, and both refuse the whole
/// registry rather than the offending line: a resolver serving the entries it
/// could make sense of would answer some IRIs from a file that is known to be
/// bad, and the operator would learn about it one missing namespace at a time.
#[derive(Debug)]
pub enum LoadError {
    /// The bytes were not the documented JSON shape.
    Json(serde_json::Error),
    /// `unclaimed` named something other than a `delegate` to an upstream with a host.
    /// Carries the clause saying why.
    Unclaimed(String),
    /// An entry the claim rule refuses. `prefix` is the entry that could not be
    /// claimed; for an overlap, `error` names the earlier entry it collides
    /// with, so the refusal names both offending prefixes.
    Refused {
        /// The prefix of the entry that was refused.
        prefix: String,
        /// Why the claim rule refused it.
        error: ClaimError,
    },
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Json(e) => write!(f, "not valid registry JSON: {e}"),
            LoadError::Unclaimed(why) => write!(f, "its unclaimed {why}"),
            LoadError::Refused { prefix, error } => {
                write!(f, "the namespace {prefix:?} cannot be claimed: {error}")
            }
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LoadError::Json(e) => Some(e),
            LoadError::Refused { .. } | LoadError::Unclaimed(_) => None,
        }
    }
}

impl From<serde_json::Error> for LoadError {
    fn from(e: serde_json::Error) -> Self {
        LoadError::Json(e)
    }
}

impl Namespace {
    /// Whether a registry may hold this entry: its strategy, and any repository
    /// together with the strategy that serves it.
    pub fn check(&self) -> Result<(), ClaimError> {
        self.strategy.check()?;
        let Some(repository) = &self.repository else {
            return Ok(());
        };
        repository.check().map_err(ClaimError::BadRepository)?;
        let served = crate::git::source_iri(&self.prefix);
        match &self.strategy {
            Strategy::Hosted { source: named } | Strategy::Mirror { origin: named }
                if *named == served =>
            {
                Ok(())
            }
            Strategy::Hosted { .. } | Strategy::Mirror { .. } => {
                Err(ClaimError::BadRepository(format!(
                    "is served at {served}, so the namespace must name that as its source or origin"
                )))
            }
            _ => Err(ClaimError::BadRepository(
                "belongs to a hosted or mirror namespace, and this is neither".into(),
            )),
        }
    }
}

impl Strategy {
    /// Whether a registry may hold this strategy: every rule of a `redirect` parses,
    /// and no capture it uses is undefined.
    pub fn check(&self) -> Result<(), ClaimError> {
        if let Strategy::Delegate { upstream } = self {
            return match self.net_scope() {
                Some(_) => Ok(()),
                None => Err(ClaimError::BadUpstream(upstream.clone())),
            };
        }
        let Strategy::Redirect { rules, .. } = self else {
            return Ok(());
        };
        for (place, rule) in rules.iter().enumerate() {
            rule.check().map_err(|reason| ClaimError::BadRule {
                rule: place + 1,
                pattern: rule.pattern.clone(),
                reason,
            })?;
        }
        Ok(())
    }
}

/// The refusal naming the namespace whose status or rule stopped a file parsing, if
/// that is what stopped it. A parser's message could only name a position in the file.
fn refuse_by_name(raw: &serde_json::Value) -> Option<LoadError> {
    raw.get("namespaces")?.as_array()?.iter().find_map(|entry| {
        let prefix = entry.get("prefix")?.as_str()?;
        let refusal = if entry.get("strategy")?.as_str()? == "redirect" {
            redirect_refusal(entry)
        } else {
            None
        };
        refusal
            .or_else(|| repository_refusal(entry.get("repository")?))
            .map(|error| LoadError::Refused {
                prefix: prefix.to_string(),
                error,
            })
    })
}

fn redirect_refusal(entry: &serde_json::Value) -> Option<ClaimError> {
    if let Some(status) = entry.get("status") {
        if status_from_json(status).is_none() {
            return Some(ClaimError::BadStatus(status_refusal(status)));
        }
    }
    let rules = entry.get("rules")?;
    let Some(rules) = rules.as_array() else {
        return Some(ClaimError::NotARuleList(rules.to_string()));
    };
    rules.iter().enumerate().find_map(|(place, rule)| {
        let reason = serde_json::from_value::<Rule>(rule.clone()).err()?;
        Some(ClaimError::BadRule {
            rule: place + 1,
            pattern: rule
                .get("match")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            reason: reason.to_string(),
        })
    })
}

impl Registry {
    /// Parse a registry from its JSON representation, refusing the whole file
    /// if any two entries overlap.
    ///
    /// Overlap is checked by **claiming each entry in turn** into an empty
    /// registry, so the file is held to exactly the rule [`Registry::claim`]
    /// applies — the same predicate, not a second one that could drift. A
    /// refusal is a typed [`LoadError::Refused`] naming both prefixes, and
    /// nothing from the file is served: a bad registry is a configuration
    /// error of the same shape as malformed JSON, and a partially-served one
    /// would arbitrate by file order, which is the thing this module exists
    /// not to do.
    ///
    /// A status or a rule that is wrong is refused the same way, naming the
    /// namespace and the rule rather than a position in the file.
    pub fn from_json(bytes: &[u8]) -> Result<Registry, LoadError> {
        let parsed: Registry = serde_json::from_slice(bytes).map_err(|e| {
            // Only a file already refused pays for reading it twice.
            serde_json::from_slice(bytes)
                .ok()
                .and_then(|raw| refuse_by_name(&raw))
                .unwrap_or(LoadError::Json(e))
        })?;
        match &parsed.unclaimed {
            None => {}
            Some(delegate @ Strategy::Delegate { upstream }) => {
                if delegate.check().is_err() {
                    return Err(LoadError::Unclaimed(format!(
                        "upstream {upstream:?} is not an https: or http: address with a host"
                    )));
                }
            }
            Some(other) => {
                return Err(LoadError::Unclaimed(format!(
                    "can only be a delegate, and this is {}",
                    other.name()
                )))
            }
        }
        let mut registry = Registry {
            namespaces: Vec::with_capacity(parsed.namespaces.len()),
            limits: parsed.limits,
            unclaimed: parsed.unclaimed,
        };
        for namespace in parsed.namespaces {
            let prefix = namespace.prefix.clone();
            registry
                .claim(namespace)
                .map_err(|error| LoadError::Refused { prefix, error })?;
        }
        Ok(registry)
    }

    /// The namespace claiming `name` — a path, or a `urn:` or `tag:` IRI — if any.
    ///
    /// Because claims cannot overlap, at most one can match and no precedence
    /// rule is needed to choose between candidates. A path is held only by a path
    /// claim, and an IRI only by an IRI claim, compared in its canonical form.
    pub fn lookup(&self, name: &str) -> Option<&Namespace> {
        let (iri, wanted) = name_segments(name);
        self.namespaces.iter().find(|ns| {
            let (claimed_iri, claimed) = name_segments(&ns.prefix);
            claimed_iri == iri
                && claimed.len() <= wanted.len()
                && wanted[..claimed.len()] == claimed[..]
        })
    }

    /// Whether `prefix` may be claimed, without mutating anything.
    ///
    /// Separated from [`Registry::claim`] so a UI can offer the answer before a
    /// user commits to a name — refusing a permanent identifier *after* someone
    /// has designed around it is the expensive order to do this in.
    pub fn may_claim(&self, prefix: &str) -> Result<(), ClaimError> {
        if segments(prefix).is_empty() {
            return Err(ClaimError::Empty);
        }
        if prefix.starts_with('-') {
            return Err(ClaimError::ReservedPrefix);
        }
        match self
            .namespaces
            .iter()
            .find(|ns| overlaps(&ns.prefix, prefix))
        {
            Some(existing) => Err(ClaimError::Overlaps(existing.prefix.clone())),
            None => Ok(()),
        }
    }

    /// Claim `prefix`, or refuse with the reason.
    ///
    /// The strategy is checked here too, so a file and a claim are held to one rule.
    pub fn claim(&mut self, namespace: Namespace) -> Result<(), ClaimError> {
        self.may_claim(&namespace.prefix)?;
        namespace.check()?;
        self.namespaces.push(namespace);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosted(prefix: &str) -> Namespace {
        Namespace {
            prefix: prefix.to_string(),
            owner: format!("urn:cap:name:admin:{prefix}"),
            strategy: Strategy::Hosted {
                source: format!("urn:file:{prefix}"),
            },
            repository: None,
            public: true,
        }
    }

    fn registry(prefixes: &[&str]) -> Registry {
        Registry {
            namespaces: prefixes.iter().map(|p| hosted(p)).collect(),
            ..Registry::default()
        }
    }

    /// The file is held to the rule a claim is: `-acme` would be administered by an
    /// exclusion, which core never grants.
    #[test]
    fn a_file_with_a_prefix_beginning_with_a_dash_is_refused() {
        let err = Registry::from_json(
            br#"{"namespaces":[{"prefix":"-acme","owner":"o","strategy":"hosted","source":"urn:file:x"}]}"#,
        )
        .expect_err("reserved position");
        assert!(
            matches!(&err, LoadError::Refused { prefix, error: ClaimError::ReservedPrefix }
                if prefix == "-acme"),
            "got: {err:?}"
        );
        let text = err.to_string();
        assert!(text.contains("\"-acme\""), "names it: {text}");
        assert!(text.contains("cannot begin with `-`"), "says why: {text}");
        assert_eq!(
            Registry::default().may_claim("-acme"),
            Err(ClaimError::ReservedPrefix)
        );
        for fine in ["acme-corp", "acme/-draft", "/-acme"] {
            assert_eq!(Registry::default().may_claim(fine), Ok(()), "{fine}");
        }
    }

    #[test]
    fn an_unrelated_prefix_is_claimable() {
        assert_eq!(registry(&["resmud"]).may_claim("acme"), Ok(()));
    }

    #[test]
    fn the_same_prefix_cannot_be_claimed_twice() {
        assert_eq!(
            registry(&["resmud"]).may_claim("resmud"),
            Err(ClaimError::Overlaps("resmud".into()))
        );
    }

    #[test]
    fn a_prefix_under_an_existing_claim_is_refused() {
        assert_eq!(
            registry(&["resmud"]).may_claim("resmud/core"),
            Err(ClaimError::Overlaps("resmud".into()))
        );
    }

    /// The reverse direction matters just as much: claiming a parent would
    /// swallow an existing child's namespace.
    #[test]
    fn a_prefix_above_an_existing_claim_is_refused() {
        assert_eq!(
            registry(&["resmud/core"]).may_claim("resmud"),
            Err(ClaimError::Overlaps("resmud/core".into()))
        );
    }

    /// The bug a textual `starts_with` would introduce: `acme` is not a prefix
    /// of `acmecorp` in any sense that matters, and refusing it would hand one
    /// tenant a veto over every name sharing its opening letters.
    #[test]
    fn a_shared_text_prefix_is_not_a_conflict() {
        assert_eq!(registry(&["acme"]).may_claim("acmecorp"), Ok(()));
        assert!(!overlaps("acme", "acmecorp"));
    }

    #[test]
    fn separators_do_not_make_a_new_claim() {
        for spelling in ["/resmud", "resmud/", "/resmud/", "resmud//"] {
            assert_eq!(
                registry(&["resmud"]).may_claim(spelling),
                Err(ClaimError::Overlaps("resmud".into())),
                "{spelling:?} should be the same claim as resmud"
            );
        }
    }

    #[test]
    fn an_empty_prefix_is_refused() {
        assert_eq!(registry(&[]).may_claim(""), Err(ClaimError::Empty));
        assert_eq!(registry(&[]).may_claim("///"), Err(ClaimError::Empty));
    }

    #[test]
    fn claiming_adds_it_and_the_second_attempt_fails() {
        let mut reg = Registry::default();
        assert_eq!(reg.claim(hosted("resmud")), Ok(()));
        assert_eq!(
            reg.claim(hosted("resmud")),
            Err(ClaimError::Overlaps("resmud".into()))
        );
        assert_eq!(
            reg.namespaces.len(),
            1,
            "a refused claim must not be stored"
        );
    }

    #[test]
    fn lookup_finds_the_claim_containing_a_path() {
        let reg = registry(&["resmud", "acme"]);
        assert_eq!(
            reg.lookup("resmud/core").map(|n| &*n.prefix),
            Some("resmud")
        );
        assert_eq!(reg.lookup("resmud").map(|n| &*n.prefix), Some("resmud"));
        assert_eq!(reg.lookup("nobody/here"), None);
    }

    /// Lookup must not match a partial segment either, for the same reason the
    /// claim rule does not.
    #[test]
    fn lookup_does_not_match_a_partial_segment() {
        assert_eq!(registry(&["acme"]).lookup("acmecorp/vocab"), None);
    }

    /// The permanence rule: a retired namespace is still claimed, so its
    /// prefix cannot be handed to someone else who would then answer for the
    /// previous owner's IRIs.
    #[test]
    fn a_retired_prefix_stays_unclaimable() {
        let reg = Registry {
            namespaces: vec![Namespace {
                prefix: "gone".into(),
                owner: "urn:cap:name:admin:gone".into(),
                strategy: Strategy::Retired {
                    reason: "project ended".into(),
                },
                repository: None,
                public: true,
            }],
            ..Registry::default()
        };
        assert_eq!(
            reg.may_claim("gone"),
            Err(ClaimError::Overlaps("gone".into()))
        );
        assert_eq!(
            reg.may_claim("gone/vocab"),
            Err(ClaimError::Overlaps("gone".into()))
        );
    }

    #[test]
    fn a_registry_round_trips_through_json() {
        let reg = Registry {
            namespaces: vec![
                hosted("resmud"),
                Namespace {
                    prefix: "acme".into(),
                    owner: "urn:cap:name:admin:acme".into(),
                    strategy: Strategy::Redirect {
                        target: "https://acme.example/ns".into(),
                        status: RedirectStatus::default(),
                        rules: vec![Rule {
                            pattern: "old/{x}".into(),
                            target: "https://acme.example/new/{x}".into(),
                            status: RedirectStatus::new(301),
                        }],
                    },
                    repository: None,
                    public: true,
                },
            ],
            ..Registry::default()
        };
        let json = serde_json::to_vec(&reg).expect("serializes");
        assert_eq!(Registry::from_json(&json).expect("parses"), reg);
    }

    /// The shape an operator actually writes, kept honest as a test so the
    /// documented format cannot drift from the parser.
    #[test]
    fn the_documented_file_shape_parses() {
        let json = br#"{
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
        let reg = Registry::from_json(json).expect("the documented shape parses");
        assert_eq!(reg.namespaces.len(), 2);
        assert_eq!(
            reg.lookup("resmud/core").map(|n| &n.strategy),
            Some(&Strategy::Hosted {
                source: "urn:file:resmud-vocab".into()
            })
        );
    }

    /// An operator who has never heard of the ceiling still gets one — a limit
    /// that only exists when configured is not a limit.
    #[test]
    fn the_document_ceiling_has_a_default() {
        let reg = Registry::from_json(b"{}").expect("parses");
        assert_eq!(reg.limits.max_document_bytes, 8 * 1024 * 1024);
    }

    #[test]
    fn the_ceiling_can_be_lowered_in_the_file() {
        let reg =
            Registry::from_json(br#"{"limits":{"max_document_bytes":1024}}"#).expect("parses");
        assert_eq!(reg.limits.max_document_bytes, 1024);
    }

    #[test]
    fn an_absent_namespaces_key_is_an_empty_registry() {
        assert_eq!(
            Registry::from_json(b"{}").expect("parses"),
            Registry::default()
        );
    }

    /// The finding from the conformance adoption (#9): a hand-edited file with
    /// `resmud` and `resmud/core` used to load, and `lookup` answered with
    /// whichever came first. Now the whole file is refused, the refusal names
    /// both prefixes, and nothing in it resolves.
    #[test]
    fn a_file_with_overlapping_prefixes_is_refused_whole() {
        let json = br#"{
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
            }
          ]
        }"#;
        let err = Registry::from_json(json).expect_err("overlap must refuse the file");
        match &err {
            LoadError::Refused { prefix, error } => {
                assert_eq!(prefix, "resmud/core");
                assert_eq!(error, &ClaimError::Overlaps("resmud".into()));
            }
            other => panic!("want a typed Refused, got: {other:?}"),
        }
        let message = err.to_string();
        assert!(
            message.contains("\"resmud/core\"") && message.contains("\"resmud\""),
            "the message names both prefixes: {message}"
        );
    }

    /// Order in the file must not matter: the parent-after-child spelling of
    /// the same file is refused too, naming the same two prefixes.
    #[test]
    fn overlap_is_refused_whichever_entry_comes_first() {
        let json = serde_json::to_vec(&registry(&["resmud/core", "resmud"])).expect("serializes");
        match Registry::from_json(&json).expect_err("overlap must refuse the file") {
            LoadError::Refused { prefix, error } => {
                assert_eq!(prefix, "resmud");
                assert_eq!(error, ClaimError::Overlaps("resmud/core".into()));
            }
            other => panic!("want a typed Refused, got: {other:?}"),
        }
    }

    /// The load rule IS the claim rule. For every pair of prefixes, a file
    /// holding both loads exactly when the second could be claimed over the
    /// first — including the empty-prefix case, which the claim rule refuses
    /// and a file must not smuggle past it. `from_json` gets this by
    /// construction (it claims each entry), and this pins that construction so
    /// a future "faster" load path cannot quietly grow a second predicate.
    #[test]
    fn load_and_claim_share_one_rule() {
        let pairs = [
            ("resmud", "acme"),
            ("resmud", "resmud"),
            ("resmud", "resmud/core"),
            ("resmud/core", "resmud"),
            ("acme", "acmecorp"),
            ("resmud", "/resmud/"),
            ("resmud", ""),
            ("resmud", "///"),
            ("a/b/c", "a/b"),
            ("a/b", "a/c"),
        ];
        for (first, second) in pairs {
            let by_claim = registry(&[first]).may_claim(second);
            let json = serde_json::to_vec(&registry(&[first, second])).expect("serializes");
            let by_load = Registry::from_json(&json).map(|_| ()).map_err(|e| match e {
                LoadError::Refused { prefix, error } => {
                    assert_eq!(prefix, second, "the refused entry is the second");
                    error
                }
                other => panic!("the file is well-formed JSON: {other}"),
            });
            assert_eq!(
                by_load, by_claim,
                "loading [{first:?}, {second:?}] must agree with claiming {second:?} over {first:?}"
            );
        }
    }

    fn redirect_with(fields: &str) -> Vec<u8> {
        format!(
            r#"{{"namespaces":[
              {{"prefix":"fine","owner":"o","strategy":"hosted","source":"urn:file:x"}},
              {{"prefix":"acme","owner":"o","strategy":"redirect",
                "target":"https://acme.example/ns"{fields}}}
            ]}}"#
        )
        .into_bytes()
    }

    fn redirect_status(fields: &str) -> RedirectStatus {
        match &Registry::from_json(&redirect_with(fields))
            .expect("loads")
            .namespaces[1]
            .strategy
        {
            Strategy::Redirect { status, .. } => *status,
            other => panic!("not a redirect: {other:?}"),
        }
    }

    #[test]
    fn every_status_loads_and_the_default_is_302() {
        for code in [301, 302, 303, 307, 308] {
            assert_eq!(
                redirect_status(&format!(r#","status":{code}"#)).code(),
                code
            );
        }
        assert_eq!(redirect_status("").code(), 302);
    }

    /// Whatever the JSON holds where a status should be, the whole file is refused
    /// by a sentence naming the namespace, never a position in the file.
    #[test]
    fn a_bad_status_refuses_the_whole_file_naming_the_namespace() {
        for bad in [
            "300", "304", "404", "200", "0", "99999", "-1", "301.5", "\"301\"", "null", "true",
            "[301]",
        ] {
            let err =
                Registry::from_json(&redirect_with(&format!(r#","status":{bad}"#))).expect_err(bad);
            assert!(
                matches!(&err, LoadError::Refused { prefix, error: ClaimError::BadStatus(_) }
                    if prefix == "acme"),
                "{bad}: {err:?}"
            );
            let message = err.to_string();
            assert!(
                message.contains("\"acme\"")
                    && message.contains(bad)
                    && message.contains("301, 302, 303, 307 or 308"),
                "{bad}: {message}"
            );
        }
    }

    /// A rule's status is held to the same list, and the refusal names the rule too.
    #[test]
    fn a_bad_rule_refuses_the_whole_file_naming_the_namespace_and_the_rule() {
        let good = r#"{"match":"ok/{a}","target":"https://acme.example/{a}"}"#;
        for (bad, pattern, want) in [
            // Stops the parser.
            (
                r#"{"match":"old/{x}","target":"https://n.example/{x}","status":404}"#,
                "old/{x}",
                "its status 404",
            ),
            (r#"{"match":"old/{x}"}"#, "old/{x}", "`target`"),
            (r#"{"target":"https://n.example/"}"#, "", "`match`"),
            (
                r#"{"match":"old","target":"https://n.example/","statuss":301}"#,
                "old",
                "`statuss`",
            ),
            (r#""old""#, "", "expected"),
            // Stops the claim.
            (
                r#"{"match":"old/{x}","target":"https://n.example/{y}"}"#,
                "old/{x}",
                "uses {y}",
            ),
            (
                r#"{"match":"old/{x}/{x}","target":"https://n.example/{x}"}"#,
                "old/{x}/{x}",
                "twice",
            ),
            (
                r#"{"match":"old/{x","target":"https://n.example/"}"#,
                "old/{x",
                "whole segment",
            ),
            (
                r#"{"match":"old/{x}","target":"https://{x}/"}"#,
                "old/{x}",
                "inside the host",
            ),
        ] {
            let json = redirect_with(&format!(r#","rules":[{good},{bad}]"#));
            let err = Registry::from_json(&json).expect_err(bad);
            match &err {
                LoadError::Refused {
                    prefix,
                    error:
                        ClaimError::BadRule {
                            rule,
                            pattern: named,
                            ..
                        },
                } => {
                    assert_eq!(prefix, "acme", "{bad}");
                    assert_eq!(*rule, 2, "{bad}");
                    assert_eq!(named, pattern, "{bad}");
                }
                other => panic!("{bad}: want a typed BadRule, got: {other:?}"),
            }
            let message = err.to_string();
            assert!(
                message.contains("\"acme\"")
                    && message.contains("rule 2")
                    && message.contains(want),
                "{bad}: {message}"
            );
        }
    }

    #[test]
    fn rules_that_are_not_a_list_refuse_the_file() {
        let err = Registry::from_json(&redirect_with(r#","rules":{"match":"a"}"#))
            .expect_err("not a list");
        assert!(
            matches!(&err, LoadError::Refused { prefix, error: ClaimError::NotARuleList(_) }
                if prefix == "acme"),
            "{err:?}"
        );
    }

    /// A shape nothing here can name stays the parser's error.
    #[test]
    fn a_file_wrong_some_other_way_is_still_a_json_error() {
        for json in [
            &br#"{"namespaces":[{"prefix":"a","strategy":"redirect","target":"t"}]}"#[..],
            br#"{"namespaces":[{"prefix":"a","owner":"o","strategy":"nonsense"}]}"#,
            br#"{"namespaces":"none"}"#,
        ] {
            assert!(matches!(Registry::from_json(json), Err(LoadError::Json(_))));
        }
    }

    /// One rule, for a claim as for a file.
    #[test]
    fn a_claim_is_held_to_the_rules_a_file_is() {
        let mut reg = Registry::default();
        let err = reg
            .claim(Namespace {
                prefix: "acme".into(),
                owner: "o".into(),
                strategy: Strategy::Redirect {
                    target: "https://acme.example/".into(),
                    status: RedirectStatus::default(),
                    rules: vec![Rule {
                        pattern: "{x}".into(),
                        target: "https://{x}/".into(),
                        status: None,
                    }],
                },
                repository: None,
                public: true,
            })
            .expect_err("a capture in the host");
        assert!(
            matches!(err, ClaimError::BadRule { rule: 1, .. }),
            "{err:?}"
        );
        assert!(reg.namespaces.is_empty());
    }

    fn acme_with_rules() -> Namespace {
        let status = |code| RedirectStatus::new(code);
        let rule = |pattern: &str, target: &str, status| Rule {
            pattern: pattern.into(),
            target: target.into(),
            status,
        };
        Namespace {
            prefix: "acme/vocab".into(),
            owner: "o".into(),
            strategy: Strategy::Redirect {
                target: "https://acme.example/ns".into(),
                status: status(307).expect("a status"),
                rules: vec![
                    rule("old/{x}", "https://one.example/{x}", status(301)),
                    rule("old/{y}", "https://never.example/{y}", None),
                    rule("old/{x}/{y}", "https://two.example/{y}/{x}", None),
                    rule("docs/{rest*}", "https://three.example/{rest}", status(308)),
                ],
            },
            repository: None,
            public: true,
        }
    }

    /// Against each path: the target, the status and the rule named.
    #[test]
    fn the_first_match_answers_and_the_namespace_target_is_the_fallback() {
        let namespace = acme_with_rules();
        for (path, target, status, rule) in [
            // The second rule has the same shape and is never reached.
            (
                "acme/vocab/old/a",
                "https://one.example/a",
                301,
                Some("old/{x}"),
            ),
            (
                "acme/vocab/old/a/b",
                "https://two.example/b/a",
                307,
                Some("old/{x}/{y}"),
            ),
            (
                "acme/vocab/docs/a/b/c",
                "https://three.example/a/b/c",
                308,
                Some("docs/{rest*}"),
            ),
            // However it was spelled, as for a prefix.
            (
                "/acme//vocab/old/a/",
                "https://one.example/a",
                301,
                Some("old/{x}"),
            ),
            // No rule matches: the namespace's own target and status.
            ("acme/vocab", "https://acme.example/ns", 307, None),
            ("acme/vocab/other", "https://acme.example/ns", 307, None),
            ("acme/vocab/docs", "https://acme.example/ns", 307, None),
            // A dot segment matches no capture, so it falls through too.
            ("acme/vocab/old/..", "https://acme.example/ns", 307, None),
        ] {
            let answer = namespace.redirection(path).expect("a redirect");
            assert_eq!(answer.target, target, "{path}");
            assert_eq!(answer.status.code(), status, "{path}");
            assert_eq!(answer.rule, rule, "{path}");
        }
    }

    #[test]
    fn only_a_redirect_has_a_redirection() {
        assert_eq!(hosted("acme").redirection("acme/x"), None);
    }

    /// A match is relative to the prefix, so moving a namespace leaves its rules alone.
    #[test]
    fn a_match_does_not_repeat_the_prefix() {
        let namespace = acme_with_rules();
        let answer = namespace.redirection("acme/vocab/acme/vocab/old/a");
        assert_eq!(answer.expect("a redirect").rule, None);
    }

    /// The other half of "refuse the whole file": a registry that does load
    /// is exactly the file's entries, in the file's order, with its limits.
    #[test]
    fn a_valid_file_loads_every_entry_in_order() {
        let reg = Registry {
            namespaces: vec![hosted("resmud"), hosted("acme"), hosted("acme-labs")],
            limits: Limits {
                max_document_bytes: 4096,
            },
            unclaimed: None,
        };
        let json = serde_json::to_vec(&reg).expect("serializes");
        assert_eq!(Registry::from_json(&json).expect("loads"), reg);
    }
}
