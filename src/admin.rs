//! Claiming and administering namespaces.
//!
//! ## Two authorities, deliberately different
//!
//! An unclaimed prefix has no owner, so it cannot be gated by its own
//! administrative capability — nobody holds one yet. Claiming and administering
//! are therefore separate authorities:
//!
//! | capability | grants |
//! |---|---|
//! | [`CAP_CLAIM`] | may become a tenant at all — create a namespace nobody holds |
//! | `urn:cap:name:admin:<prefix>` | owns that namespace: may change how it resolves, or retire it |
//!
//! An operator hands out the first sparingly (it is the signup gate) and the
//! second automatically, to whoever claimed the prefix.
//!
//! Both actions declare the **wildcard** form in `requires`, which is the coarse
//! floor the kernel enforces — "holds some grant under this family". The exact
//! per-prefix grant is checked here, at invocation, because only this code knows
//! which prefix the request names. That is the parameterized-ACL shape used for
//! filesystem and network scopes.
//!
//! ## A prefix never begins with `-`
//!
//! The prefix is the last segment of `urn:cap:name:admin:<prefix>`, which is one
//! of the two positions core RESERVES for exclusions ([`is_deny_scope`]: a rule
//! beginning with `-`). So `-acme` would be administered by
//! `urn:cap:name:admin:-acme`, a token core keeps through every narrowing and
//! never grants: the claimant would own a namespace nobody, not even they, could
//! ever administer, and the refusal would read as a puzzling `Denied` naming the
//! very token they hold. Both faces refuse such a prefix up front instead, as an
//! [`Error::InvalidArgument`] that says why, before any capability check. For
//! the same reason a claim refuses an `owner` that is itself deny-shaped.
//!
//! ## Retirement, not release
//!
//! There is no way to free a prefix. See [`Strategy::Retired`]: handing a used
//! prefix to a new owner would let them answer for IRIs the previous owner
//! minted, which is the one failure a permanence service must not have.

use crate::registry::{ClaimError, Namespace, Registry, Strategy};
use crate::{load, REGISTRY_IRI};
use ikigai_core::{
    is_deny_scope, ActionSpec, ArgRef, ArgSpec, AsyncFnEndpoint, Description, Error, Invocation,
    InvokeFuture, Iri, ReprType, Representation, Request, Result, Verb,
};

/// The capability permitting a *new* namespace to be claimed — the signup gate.
pub const CAP_CLAIM: &str = "urn:cap:name:claim";

/// The wildcard form of the per-namespace administrative capability, as declared
/// in `requires`. The exact grant is `urn:cap:name:admin:<prefix>`.
pub const CAP_ADMIN_ANY: &str = "urn:cap:name:admin:*";

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const TEXT_PLAIN_UTF8: &str = "text/plain;charset=utf-8";

fn text_plain_utf8() -> ReprType {
    ReprType::new("text/plain").with_param("charset", "utf-8")
}

/// The administrative capability for one namespace.
pub fn admin_scope(prefix: &str) -> String {
    format!("urn:cap:name:admin:{prefix}")
}

/// Require the exact per-prefix grant.
///
/// The kernel has already enforced the wildcard floor; this is the half that
/// distinguishes one tenant from another, so **omitting it would let any tenant
/// administer any namespace**.
fn require_admin(inv: &Invocation<'_>, prefix: &str) -> Result<()> {
    let scope = admin_scope(prefix);
    if inv.capability.allows(&scope) {
        Ok(())
    } else {
        Err(Error::Denied(format!(
            "name: administering {prefix:?} needs `{scope}`"
        )))
    }
}

fn required(inv: &Invocation<'_>, name: &'static str) -> Result<String> {
    let value = inv
        .inline_str(name)
        .map_err(|_| Error::MissingArgument(name.to_string()))?
        .trim()
        .to_string();
    if value.is_empty() {
        return Err(Error::MissingArgument(name.to_string()));
    }
    Ok(value)
}

/// The `prefix` argument, with the piped-`content` fallback every mutating
/// pipeline citizen offers: `echo acme | sink urn:name:claim strategy=…` names
/// the prefix on the pipe, and a top-level `sink`'s body lands the same way.
/// `prefix` by name wins when both are present.
fn prefix_arg(inv: &Invocation<'_>) -> Result<String> {
    let value = inv
        .inline_str("prefix")
        .or_else(|_| inv.inline_str("content"))
        .map_err(|_| Error::MissingArgument("prefix".into()))?
        .trim()
        .to_string();
    if value.is_empty() {
        return Err(Error::MissingArgument("prefix".into()));
    }
    refuse_reserved_prefix(&value)?;
    Ok(value)
}

/// Refuse a prefix that begins with `-` (see the module docs).
///
/// Exactly the reserved position and no more: a `-` anywhere else (`acme-corp`,
/// `acme/-draft`, `/-acme`) lands past it in `urn:cap:name:admin:<prefix>`, so
/// the scope is an ordinary grant and the namespace is administrable.
fn refuse_reserved_prefix(prefix: &str) -> Result<()> {
    if prefix.starts_with('-') {
        return Err(Error::InvalidArgument {
            name: "prefix".into(),
            detail: format!(
                "a namespace prefix cannot begin with `-`: its administrative capability \
                 `{}` would be an exclusion (a deny-shaped scope), which no grant can \
                 satisfy, so nobody could ever administer {prefix:?}",
                admin_scope(prefix)
            ),
        });
    }
    Ok(())
}

/// Refuse an `owner` that is an exclusion rather than a grant: it could never be
/// held as authority, so it cannot administer anything.
fn refuse_deny_owner(owner: &str) -> Result<()> {
    if is_deny_scope(owner) {
        return Err(Error::InvalidArgument {
            name: "owner".into(),
            detail: format!(
                "`{owner}` is an exclusion (a deny-shaped capability scope), not a grant; \
                 no capability can ever satisfy it, so it cannot own a namespace"
            ),
        });
    }
    Ok(())
}

/// The `content` input every mutating action declares: the prefix, when piped.
fn content_spec() -> ArgSpec {
    ArgSpec::new("content")
        .summary("the prefix, when piped or sent as the body (the fallback for `prefix`)")
        .optional()
        .class(XSD_STRING)
}

/// Build the strategy named by `strategy=`, reading whichever companion
/// argument that choice requires.
fn strategy_from_args(inv: &Invocation<'_>) -> Result<Strategy> {
    let named = required(inv, "strategy")?;
    match named.as_str() {
        "hosted" => Ok(Strategy::Hosted {
            source: required(inv, "source")?,
        }),
        "redirect" => Ok(Strategy::Redirect {
            target: required(inv, "target")?,
        }),
        "mirror" => Ok(Strategy::Mirror {
            origin: required(inv, "origin")?,
        }),
        // Retirement is reached through Delete, never by setting a strategy —
        // so it cannot be arrived at by a typo in an ordinary update.
        "retired" => Err(Error::InvalidArgument {
            name: "strategy".into(),
            detail: "retire a namespace with the Delete verb, not by setting a strategy".into(),
        }),
        other => Err(Error::InvalidArgument {
            name: "strategy".into(),
            detail: format!("unknown strategy {other:?} (hosted, redirect, mirror)"),
        }),
    }
}

/// Write the registry back through the kernel.
///
/// ⚠ This is a read-modify-write with no compare-and-set, so two administrative
/// writes racing can lose one. Acceptable while writes are rare and operator-
/// driven; the fix is a validity-token precondition on the Sink (the kernel
/// already mints validity tokens, and the generic PATCH design specifies exactly
/// this), not a lock.
async fn store(inv: &Invocation<'_>, registry: &Registry) -> Result<()> {
    let iri = Iri::parse(REGISTRY_IRI).map_err(|e| Error::Endpoint(format!("name: {e}")))?;
    let body =
        serde_json::to_vec_pretty(registry).map_err(|e| Error::Endpoint(format!("name: {e}")))?;
    inv.issue(Request::new(Verb::Sink, iri).with_arg("content", ArgRef::Inline(body)))
        .await?;
    Ok(())
}

fn claim_error(prefix: &str, error: ClaimError) -> Error {
    match error {
        ClaimError::Empty => Error::InvalidArgument {
            name: "prefix".into(),
            detail: "a namespace prefix cannot be empty".into(),
        },
        // Not InvalidArgument: the prefix is well-formed, it simply belongs to
        // someone else. A caller retrying with the same input will always fail,
        // and the message names the claim standing in the way.
        ClaimError::Overlaps(existing) => Error::Denied(format!(
            "name: {prefix:?} overlaps the claimed namespace {existing:?}"
        )),
    }
}

fn ok(message: String) -> Representation {
    Representation::new(text_plain_utf8(), message.into_bytes())
}

/// `urn:name:claim` — claim a namespace nobody holds.
pub fn claim() -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("claim", |inv: &Invocation<'_>| -> InvokeFuture<'_> {
        Box::pin(async move {
            let prefix = prefix_arg(inv)?;
            let strategy = strategy_from_args(inv)?;
            // `owner` is optional so the ordinary case needs no ceremony: the
            // claimant becomes the administrator. An operator provisioning on
            // someone else's behalf can name a different one.
            let owner = inv
                .inline_str("owner")
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| admin_scope(&prefix));
            refuse_deny_owner(&owner)?;

            let mut registry = load(inv).await?;
            registry
                .claim(Namespace {
                    prefix: prefix.clone(),
                    owner: owner.clone(),
                    strategy,
                })
                .map_err(|e| claim_error(&prefix, e))?;
            store(inv, &registry).await?;
            Ok(ok(format!("claimed {prefix}\t{owner}\n")))
        })
    })
    .with_description(
        Description::new("claim")
            .title("Claim a namespace")
            .summary(
                "Claims a prefix nobody holds and records how it resolves. Refused if any \
                 claimed prefix contains it or it contains one — including retired prefixes, \
                 which stay claimed forever so a later owner can never answer for an earlier \
                 owner's IRIs.",
            )
            .action(
                ActionSpec::new(Verb::Sink)
                    .summary("claim a prefix and set how it resolves")
                    .requires(CAP_CLAIM)
                    .input(
                        ArgSpec::new("prefix")
                            .summary(
                                "the prefix to claim, e.g. resmud; never beginning with `-` \
                                 (piped content is the fallback)",
                            )
                            .class(XSD_STRING),
                    )
                    .input(content_spec())
                    .input(
                        ArgSpec::new("strategy")
                            .summary("how it resolves")
                            .one_of(["hosted", "redirect", "mirror"])
                            .class(XSD_STRING),
                    )
                    .input(
                        ArgSpec::new("source")
                            .summary("hosted: the resource the documents are read from")
                            .optional()
                            .class(XSD_STRING),
                    )
                    .input(
                        ArgSpec::new("target")
                            .summary("redirect: where the owner serves it")
                            .optional()
                            .class(XSD_STRING),
                    )
                    .input(
                        ArgSpec::new("origin")
                            .summary("mirror: the source of truth this copy tracks")
                            .optional()
                            .class(XSD_STRING),
                    )
                    .input(
                        ArgSpec::new("owner")
                            .summary("the administering capability (defaults to the claimant's)")
                            .optional()
                            .class(XSD_STRING),
                    )
                    .output(TEXT_PLAIN_UTF8),
            ),
    )
}

/// `urn:name:admin` — change how a namespace resolves (Sink), or retire it
/// (Delete).
pub fn admin() -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("admin", |inv: &Invocation<'_>| -> InvokeFuture<'_> {
        Box::pin(async move {
            let prefix = prefix_arg(inv)?;
            require_admin(inv, &prefix)?;
            let mut registry = load(inv).await?;
            let entry = registry
                .namespaces
                .iter_mut()
                .find(|ns| ns.prefix == prefix)
                .ok_or_else(|| Error::NotFound(format!("name: no namespace claims {prefix:?}")))?;

            let message = match inv.request.verb {
                Verb::Delete => {
                    let reason = inv
                        .inline_str("reason")
                        .map(|s| s.trim().to_string())
                        .unwrap_or_else(|_| "withdrawn".to_string());
                    entry.strategy = Strategy::Retired {
                        reason: reason.clone(),
                    };
                    format!("retired {prefix}\t{reason}\n")
                }
                _ => {
                    let strategy = strategy_from_args(inv)?;
                    entry.strategy = strategy;
                    format!("updated {prefix}\n")
                }
            };
            store(inv, &registry).await?;
            Ok(ok(message))
        })
    })
    .with_description(
        Description::new("admin")
            .title("Administer a claimed namespace")
            .summary(
                "Sink changes how a namespace resolves; Delete retires it. Both need the \
                 administrative capability for that specific prefix.",
            )
            .action(
                ActionSpec::new(Verb::Sink)
                    .summary("change how this namespace resolves")
                    .requires(CAP_ADMIN_ANY)
                    .input(
                        ArgSpec::new("prefix")
                            .summary("the namespace to administer (piped content is the fallback)")
                            .class(XSD_STRING),
                    )
                    .input(content_spec())
                    .input(
                        ArgSpec::new("strategy")
                            .summary("how it should resolve from now on")
                            .one_of(["hosted", "redirect", "mirror"])
                            .class(XSD_STRING),
                    )
                    .input(
                        ArgSpec::new("source")
                            .summary("hosted: the resource the documents are read from")
                            .optional()
                            .class(XSD_STRING),
                    )
                    .input(
                        ArgSpec::new("target")
                            .summary("redirect: where the owner serves it")
                            .optional()
                            .class(XSD_STRING),
                    )
                    .input(
                        ArgSpec::new("origin")
                            .summary("mirror: the source of truth this copy tracks")
                            .optional()
                            .class(XSD_STRING),
                    )
                    .output(TEXT_PLAIN_UTF8),
            )
            .action(
                ActionSpec::new(Verb::Delete)
                    .summary(
                        "retire this namespace — a tombstone, never a release: the claim \
                         survives so the prefix can never be re-issued",
                    )
                    .requires(CAP_ADMIN_ANY)
                    .input(
                        ArgSpec::new("prefix")
                            .summary("the namespace to retire (piped content is the fallback)")
                            .class(XSD_STRING),
                    )
                    .input(content_spec())
                    .input(
                        ArgSpec::new("reason")
                            .summary("why, for whoever dereferences an IRI under it")
                            .optional()
                            .class(XSD_STRING),
                    )
                    .output(TEXT_PLAIN_UTF8),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;
    use crate::space;
    use futures::executor::block_on;
    use ikigai_core::{Capability, Endpoint, FnEndpoint, Kernel};
    use std::sync::{Arc, Mutex};

    /// A registry source that is both readable and writable, standing in for
    /// the operator's file. The module never learns which it is.
    #[derive(Clone, Default)]
    struct Store(Arc<Mutex<String>>);

    impl Store {
        fn new(json: &str) -> Store {
            Store(Arc::new(Mutex::new(json.to_string())))
        }
        fn read(&self) -> Registry {
            Registry::from_json(self.0.lock().unwrap().as_bytes()).expect("valid registry")
        }
        fn endpoint(&self) -> FnEndpoint {
            let cell = self.0.clone();
            FnEndpoint::new("registry-source", move |inv: &Invocation<'_>| {
                if inv.request.verb == Verb::Sink {
                    let body = inv.inline_arg("content")?;
                    *cell.lock().unwrap() = String::from_utf8_lossy(body).into_owned();
                    return Ok(Representation::new(text_plain_utf8(), b"stored".to_vec()));
                }
                let json = cell.lock().unwrap().clone();
                Ok(Representation::new(
                    ReprType::new("application/json"),
                    json.into_bytes(),
                ))
            })
        }
    }

    const EMPTY: &str = r#"{"namespaces":[]}"#;
    const ONE: &str = r#"{"namespaces":[{"prefix":"resmud","owner":"urn:cap:name:admin:resmud",
        "strategy":"hosted","source":"urn:file:resmud-vocab"}]}"#;

    fn kernel(store: &Store) -> Kernel {
        Kernel::new(Arc::new(
            space().bind(ikigai_core::Exact::new(REGISTRY_IRI), store.endpoint()),
        ))
    }

    fn call(
        kernel: &Kernel,
        verb: Verb,
        target: &str,
        args: &[(&str, &str)],
        cap: &Capability,
    ) -> Result<String> {
        let mut request = Request::new(verb, Iri::parse(target).unwrap());
        for (name, value) in args {
            request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
        }
        let repr = block_on(kernel.issue(request, cap))?;
        Ok(String::from_utf8(repr.bytes.clone()).expect("utf-8"))
    }

    fn claimant() -> Capability {
        Capability::scoped([CAP_CLAIM])
    }

    // --- claiming ----------------------------------------------------------

    #[test]
    fn a_claim_records_the_namespace_and_defaults_the_owner() {
        let store = Store::new(EMPTY);
        let out = call(
            &kernel(&store),
            Verb::Sink,
            "urn:name:claim",
            &[
                ("prefix", "resmud"),
                ("strategy", "hosted"),
                ("source", "urn:file:resmud-vocab"),
            ],
            &claimant(),
        )
        .expect("claims");
        assert!(out.contains("urn:cap:name:admin:resmud"), "got: {out}");

        let stored = store.read();
        assert_eq!(stored.namespaces.len(), 1);
        assert_eq!(stored.namespaces[0].owner, "urn:cap:name:admin:resmud");
    }

    /// The signup gate. Without it anyone reaching the endpoint could become a
    /// tenant, which is the whole of the multi-tenancy authority story.
    #[test]
    fn claiming_without_the_claim_capability_is_denied() {
        let store = Store::new(EMPTY);
        let err = call(
            &kernel(&store),
            Verb::Sink,
            "urn:name:claim",
            &[
                ("prefix", "resmud"),
                ("strategy", "hosted"),
                ("source", "urn:file:resmud-vocab"),
            ],
            &Capability::scoped(["urn:cap:something:else"]),
        )
        .expect_err("no claim capability");
        assert!(matches!(err, Error::Denied(_)), "got: {err:?}");
        assert!(store.read().namespaces.is_empty(), "nothing was written");
    }

    #[test]
    fn a_conflicting_claim_is_refused_and_writes_nothing() {
        let store = Store::new(ONE);
        let err = call(
            &kernel(&store),
            Verb::Sink,
            "urn:name:claim",
            &[
                ("prefix", "resmud/core"),
                ("strategy", "redirect"),
                ("target", "https://example.test/"),
            ],
            &claimant(),
        )
        .expect_err("overlaps");
        assert!(err.to_string().contains("resmud"), "names the claim: {err}");
        assert_eq!(store.read().namespaces.len(), 1, "registry unchanged");
    }

    #[test]
    fn a_strategy_missing_its_companion_argument_is_refused() {
        let store = Store::new(EMPTY);
        let err = call(
            &kernel(&store),
            Verb::Sink,
            "urn:name:claim",
            &[("prefix", "acme"), ("strategy", "redirect")],
            &claimant(),
        )
        .expect_err("redirect needs a target");
        assert!(
            matches!(err, Error::MissingArgument(ref a) if a == "target"),
            "got: {err:?}"
        );
    }

    #[test]
    fn an_unknown_strategy_lists_the_real_ones() {
        let store = Store::new(EMPTY);
        let err = call(
            &kernel(&store),
            Verb::Sink,
            "urn:name:claim",
            &[("prefix", "acme"), ("strategy", "teleport")],
            &claimant(),
        )
        .expect_err("no such strategy");
        assert!(
            err.to_string().contains("hosted"),
            "offers the choices: {err}"
        );
    }

    // --- administering -----------------------------------------------------

    fn owner_of(prefix: &str) -> Capability {
        Capability::scoped([admin_scope(prefix)])
    }

    #[test]
    fn the_owner_can_change_how_a_namespace_resolves() {
        let store = Store::new(ONE);
        call(
            &kernel(&store),
            Verb::Sink,
            "urn:name:admin",
            &[
                ("prefix", "resmud"),
                ("strategy", "redirect"),
                ("target", "https://resmud.example/ns"),
            ],
            &owner_of("resmud"),
        )
        .expect("updates");
        assert_eq!(
            store.read().namespaces[0].strategy,
            Strategy::Redirect {
                target: "https://resmud.example/ns".into()
            }
        );
    }

    /// The check that makes tenancy real: holding SOME admin capability must
    /// not confer authority over SOMEONE ELSE'S namespace. The kernel's
    /// wildcard floor passes here — only the exact per-prefix check refuses.
    #[test]
    fn another_tenants_admin_capability_does_not_reach_this_namespace() {
        let store = Store::new(ONE);
        let err = call(
            &kernel(&store),
            Verb::Sink,
            "urn:name:admin",
            &[
                ("prefix", "resmud"),
                ("strategy", "redirect"),
                ("target", "https://attacker.example/"),
            ],
            &owner_of("acme"),
        )
        .expect_err("not their namespace");
        assert!(matches!(err, Error::Denied(_)), "got: {err:?}");
        assert!(
            err.to_string().contains("urn:cap:name:admin:resmud"),
            "names the capability needed: {err}"
        );
        assert_eq!(
            store.read().namespaces[0].strategy,
            Strategy::Hosted {
                source: "urn:file:resmud-vocab".into()
            },
            "the registry must be untouched"
        );
    }

    /// Claiming is not administering: the signup capability confers no
    /// authority over namespaces already claimed.
    #[test]
    fn the_claim_capability_does_not_confer_administration() {
        let store = Store::new(ONE);
        let err = call(
            &kernel(&store),
            Verb::Sink,
            "urn:name:admin",
            &[
                ("prefix", "resmud"),
                ("strategy", "mirror"),
                ("origin", "https://elsewhere.example/"),
            ],
            &claimant(),
        )
        .expect_err("claim is not admin");
        assert!(matches!(err, Error::Denied(_)), "got: {err:?}");
    }

    // --- retirement --------------------------------------------------------

    #[test]
    fn delete_retires_a_namespace_rather_than_removing_it() {
        let store = Store::new(ONE);
        let out = call(
            &kernel(&store),
            Verb::Delete,
            "urn:name:admin",
            &[("prefix", "resmud"), ("reason", "project ended")],
            &owner_of("resmud"),
        )
        .expect("retires");
        assert!(out.contains("project ended"), "got: {out}");

        let stored = store.read();
        assert_eq!(stored.namespaces.len(), 1, "the claim must survive");
        assert_eq!(
            stored.namespaces[0].strategy,
            Strategy::Retired {
                reason: "project ended".into()
            }
        );
    }

    /// The permanence guarantee, end to end: once retired, the prefix can never
    /// be handed to anyone else — so no later owner can answer for IRIs an
    /// earlier owner minted.
    #[test]
    fn a_retired_prefix_cannot_be_reclaimed() {
        let store = Store::new(ONE);
        let kernel = kernel(&store);
        call(
            &kernel,
            Verb::Delete,
            "urn:name:admin",
            &[("prefix", "resmud")],
            &owner_of("resmud"),
        )
        .expect("retires");

        let err = call(
            &kernel,
            Verb::Sink,
            "urn:name:claim",
            &[
                ("prefix", "resmud"),
                ("strategy", "redirect"),
                ("target", "https://someone-else.example/"),
            ],
            &claimant(),
        )
        .expect_err("still claimed");
        assert!(matches!(err, Error::Denied(_)), "got: {err:?}");
    }

    #[test]
    fn a_retired_namespace_resolves_as_a_permanent_absence_that_explains_itself() {
        let store = Store::new(ONE);
        let kernel = kernel(&store);
        call(
            &kernel,
            Verb::Delete,
            "urn:name:admin",
            &[("prefix", "resmud"), ("reason", "superseded")],
            &owner_of("resmud"),
        )
        .expect("retires");

        let err = call(
            &kernel,
            Verb::Source,
            "urn:name:resolve",
            &[("path", "resmud/core")],
            &Capability::root(),
        )
        .expect_err("retired");
        assert!(matches!(err, Error::NotFound(_)), "got: {err:?}");
        assert!(
            err.to_string().contains("superseded"),
            "explains why: {err}"
        );
    }

    /// Retirement must not be reachable by mistyping an ordinary update.
    #[test]
    fn retirement_cannot_be_set_as_a_strategy() {
        let store = Store::new(ONE);
        let err = call(
            &kernel(&store),
            Verb::Sink,
            "urn:name:admin",
            &[("prefix", "resmud"), ("strategy", "retired")],
            &owner_of("resmud"),
        )
        .expect_err("not this way");
        assert!(
            err.to_string().contains("Delete"),
            "points at the verb: {err}"
        );
    }

    #[test]
    fn administering_an_unclaimed_namespace_is_not_found() {
        let store = Store::new(EMPTY);
        let err = call(
            &kernel(&store),
            Verb::Sink,
            "urn:name:admin",
            &[
                ("prefix", "ghost"),
                ("strategy", "hosted"),
                ("source", "urn:file:x"),
            ],
            &owner_of("ghost"),
        )
        .expect_err("nothing to administer");
        assert!(matches!(err, Error::NotFound(_)), "got: {err:?}");
    }

    // --- the reserved deny position (ledger #874) ---------------------------

    fn is_invalid(err: &Error, arg: &str) -> bool {
        matches!(err, Error::InvalidArgument { name, .. } if name == arg)
    }

    /// The defect this refusal exists for, pinned against core: the scope a
    /// `-acme` claim would hand its owner is an exclusion, and core never grants
    /// one even to its holder. If core ever stops treating it so, the refusal
    /// below is guarding nothing and this test says so first.
    #[test]
    fn a_dash_prefix_would_get_an_admin_scope_core_never_grants() {
        let scope = admin_scope("-acme");
        assert!(is_deny_scope(&scope), "{scope} is in the reserved position");
        assert!(
            !owner_of("-acme").allows(&scope),
            "held, and still not granted"
        );
        assert!(!is_deny_scope(&admin_scope("acme-corp")));
        assert!(!is_deny_scope(&admin_scope("acme/-draft")));
        assert!(!is_deny_scope(&admin_scope("/-acme")));
    }

    /// Without the refusal this claim SUCCEEDED (reproduced on core 0.1.86) and
    /// recorded an owner that could never administer it: a later admin call as
    /// that owner came back `Denied` naming the very token the caller held.
    #[test]
    fn a_prefix_beginning_with_a_dash_cannot_be_claimed() {
        let store = Store::new(EMPTY);
        let kernel = kernel(&store);
        let err = call(
            &kernel,
            Verb::Sink,
            "urn:name:claim",
            &[
                ("prefix", "-acme"),
                ("strategy", "redirect"),
                ("target", "https://acme.example/ns"),
            ],
            &claimant(),
        )
        .expect_err("reserved position");
        assert!(is_invalid(&err, "prefix"), "got: {err:?}");
        let text = err.to_string();
        assert!(text.contains("cannot begin with `-`"), "says why: {text}");
        assert!(
            text.contains("urn:cap:name:admin:-acme"),
            "names the scope: {text}"
        );

        // The piped form reaches the same check.
        let err = call(
            &kernel,
            Verb::Sink,
            "urn:name:claim",
            &[
                ("content", "-acme\n"),
                ("strategy", "redirect"),
                ("target", "https://acme.example/ns"),
            ],
            &claimant(),
        )
        .expect_err("piped, still reserved");
        assert!(is_invalid(&err, "prefix"), "got: {err:?}");
        assert!(store.read().namespaces.is_empty(), "nothing was written");
    }

    /// Only the reserved position is refused: a `-` anywhere else is an ordinary
    /// character and the namespace is administrable by its owner.
    #[test]
    fn a_dash_past_the_reserved_position_is_an_ordinary_prefix() {
        for prefix in ["acme-corp", "acme/-draft"] {
            let store = Store::new(EMPTY);
            let kernel = kernel(&store);
            call(
                &kernel,
                Verb::Sink,
                "urn:name:claim",
                &[
                    ("prefix", prefix),
                    ("strategy", "redirect"),
                    ("target", "https://acme.example/ns"),
                ],
                &claimant(),
            )
            .unwrap_or_else(|e| panic!("{prefix} claims: {e}"));
            call(
                &kernel,
                Verb::Sink,
                "urn:name:admin",
                &[
                    ("prefix", prefix),
                    ("strategy", "redirect"),
                    ("target", "https://acme.example/v2"),
                ],
                &owner_of(prefix),
            )
            .unwrap_or_else(|e| panic!("{prefix} is administrable: {e}"));
        }
    }

    /// A hand-edited registry can still carry a `-` prefix (the file loader is
    /// not this face). Administering it says why it cannot work, BEFORE the
    /// capability check, rather than a `Denied` naming the token the caller holds.
    #[test]
    fn administering_a_dash_prefix_is_refused_before_the_capability_check() {
        let store = Store::new(
            r#"{"namespaces":[{"prefix":"-acme","owner":"urn:cap:name:admin:-acme",
                "strategy":"redirect","target":"https://acme.example/ns"}]}"#,
        );
        let kernel = kernel(&store);
        for (verb, args) in [
            (
                Verb::Sink,
                vec![
                    ("prefix", "-acme"),
                    ("strategy", "redirect"),
                    ("target", "https://x.example/"),
                ],
            ),
            (Verb::Delete, vec![("prefix", "-acme")]),
        ] {
            let err = call(&kernel, verb, "urn:name:admin", &args, &owner_of("-acme"))
                .expect_err("reserved position");
            assert!(is_invalid(&err, "prefix"), "{verb:?} got: {err:?}");
            assert!(err.to_string().contains("cannot begin with `-`"));
        }
        assert_eq!(
            store.read().namespaces[0].strategy,
            Strategy::Redirect {
                target: "https://acme.example/ns".into()
            },
            "the registry must be untouched"
        );
    }

    /// The other caller-chosen value that can land in a reserved position: an
    /// explicit `owner`. A deny-shaped one can never be held as authority.
    #[test]
    fn a_deny_shaped_owner_is_refused() {
        for owner in ["urn:cap:name:admin:-acme", "urn:cap:net:-acme.example"] {
            let store = Store::new(EMPTY);
            let err = call(
                &kernel(&store),
                Verb::Sink,
                "urn:name:claim",
                &[
                    ("prefix", "acme"),
                    ("strategy", "redirect"),
                    ("target", "https://acme.example/ns"),
                    ("owner", owner),
                ],
                &claimant(),
            )
            .expect_err("an exclusion owns nothing");
            assert!(is_invalid(&err, "owner"), "got: {err:?}");
            assert!(err.to_string().contains("exclusion"), "says why: {err}");
            assert!(store.read().namespaces.is_empty(), "nothing was written");
        }
    }

    // --- the declared contract ---------------------------------------------

    /// Declared = enforced. If an action stops declaring its capability the
    /// manifold over-offers, which is the failure the field guide names.
    #[test]
    fn the_actions_declare_the_capabilities_they_enforce() {
        let claim_desc = format!("{:?}", claim().describe());
        assert!(
            claim_desc.contains(CAP_CLAIM),
            "claim declares it: {claim_desc}"
        );

        let admin_desc = format!("{:?}", admin().describe());
        assert!(admin_desc.contains(CAP_ADMIN_ANY), "admin declares it");
        assert!(admin_desc.contains("Delete"), "admin declares Delete");
        assert!(admin_desc.contains("Sink"), "admin declares Sink");
    }
}
