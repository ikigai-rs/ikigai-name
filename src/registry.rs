//! The namespace registry: who owns which prefix, and how it resolves.
//!
//! The registry is **data an operator edits**, never something compiled in — a
//! resolution rule can change without a rebuild, and because the registry is
//! read back through the kernel it is golden-threaded (edit the file, the thread
//! cuts, the next resolve is correct), diffable, and as-of resolvable like any
//! other resource.
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
    /// The owner serves it themselves; we answer with a redirect to `target`.
    Redirect {
        /// Where the owner serves it.
        target: String,
    },
    /// A cached copy is served, but `origin` remains the source of truth, so
    /// the namespace survives its origin going dark.
    Mirror {
        /// The source of truth this copy tracks.
        origin: String,
    },
}

/// One claimed namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Namespace {
    /// The claimed path prefix, e.g. `resmud`.
    pub prefix: String,
    /// The capability that administers it. Holding it is what permits editing
    /// this entry; it is not a display name.
    pub owner: String,
    /// How this namespace produces its answer.
    #[serde(flatten)]
    pub strategy: Strategy,
}

/// The whole registry, as parsed from the operator's file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    /// Every claimed namespace. No two may overlap; see [`Registry::claim`].
    #[serde(default)]
    pub namespaces: Vec<Namespace>,
}

/// Why a claim was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimError {
    /// The prefix was empty, or every segment of it was.
    Empty,
    /// An existing claim overlaps: it contains the new prefix, the new prefix
    /// contains it, or they are equal. Carries the existing prefix so the
    /// refusal can name it.
    Overlaps(String),
}

impl std::fmt::Display for ClaimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaimError::Empty => write!(f, "a namespace prefix cannot be empty"),
            ClaimError::Overlaps(existing) => {
                write!(f, "overlaps the claimed namespace {existing:?}")
            }
        }
    }
}

/// Split a prefix into path segments, ignoring leading, trailing and doubled
/// separators so `acme`, `/acme` and `acme/` are one claim rather than three.
fn segments(prefix: &str) -> Vec<&str> {
    prefix.split('/').filter(|s| !s.is_empty()).collect()
}

/// Whether two prefixes overlap — i.e. whether one is the other or contains it.
///
/// Comparison is **segment-wise, never textual**: `acme` contains `acme/vocab`
/// but is unrelated to `acmecorp`, which a plain `starts_with` would wrongly
/// call a conflict and refuse.
pub fn overlaps(a: &str, b: &str) -> bool {
    let (a, b) = (segments(a), segments(b));
    let shared = a.len().min(b.len());
    a[..shared] == b[..shared]
}

impl Registry {
    /// Parse a registry from its JSON representation.
    pub fn from_json(bytes: &[u8]) -> Result<Registry, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    /// The namespace claiming `path`, if any.
    ///
    /// Because claims cannot overlap, at most one can match and no precedence
    /// rule is needed to choose between candidates.
    pub fn lookup(&self, path: &str) -> Option<&Namespace> {
        let wanted = segments(path);
        self.namespaces.iter().find(|ns| {
            let claimed = segments(&ns.prefix);
            claimed.len() <= wanted.len() && wanted[..claimed.len()] == claimed[..]
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
    pub fn claim(&mut self, namespace: Namespace) -> Result<(), ClaimError> {
        self.may_claim(&namespace.prefix)?;
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
        }
    }

    fn registry(prefixes: &[&str]) -> Registry {
        Registry {
            namespaces: prefixes.iter().map(|p| hosted(p)).collect(),
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
                    },
                },
            ],
        };
        let json = serde_json::to_vec(&reg).expect("serialises");
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

    #[test]
    fn an_absent_namespaces_key_is_an_empty_registry() {
        assert_eq!(
            Registry::from_json(b"{}").expect("parses"),
            Registry::default()
        );
    }
}
