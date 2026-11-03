//! Which host a request is for: a tenant, carried on the request's capability.
//!
//! One deployment can answer for several domains (`iriref.org`, `urnid.org`,
//! `idref.org`), each holding its own set of identifiers. That is multi-tenancy,
//! not aliasing: `https://urnid.org/resmud/core#Weapon` and
//! `https://iriref.org/resmud/core#Weapon` are two terms, and the first is never
//! an answer for the second. So a term lives in exactly one tenant, the one its
//! IRI's host names, and any other tenant can only refer to it.
//!
//! ## Why the tenant travels as a capability scope
//!
//! The door that read `Host` mints [`tenant_scope`] into the request's
//! capability, and an endpoint reads it back with [`tenant_of`]. Everything the
//! design needs then follows from what a capability already is:
//!
//! - **The kernel's cache partitions by it.** Every cache key holds the
//!   capability's scopes, so a cached answer for one host cannot be found by a
//!   request for another, at any depth of the call chain.
//! - **A request cannot name its own tenant.** An argument would be forwarded
//!   from `?tenant=…`; a scope comes only from the door.
//! - **It reaches every sub-request.** A sub-request runs under its caller's
//!   capability, so a registry read and a SPARQL query's graph load are for the
//!   same tenant as the page that asked for them. Attenuating the scope away
//!   leaves a request with no tenant, which a tenanted file space refuses.
//!
//! A capability with no tenant scope is what a library host and a single-tenant
//! server issue under, and nothing about it changes.

use ikigai_core::Capability;

/// The scope prefix a door mints a tenant under: `urn:cap:name:tenant:<host>`.
pub const TENANT_SCOPE_PREFIX: &str = "urn:cap:name:tenant:";

/// The tenant key for a `Host` header's value, or `None` if it is not a DNS name.
///
/// Lowercased, with any port and one trailing root dot removed, so
/// `IRIREF.org:8443` and `iriref.org.` are the one tenant `iriref.org`. A label
/// is ASCII letters, digits and inner hyphens (an international name arrives as
/// its `xn--` form), which rules out an IPv6 literal, `/`, `..` and a comma-joined
/// pair of hosts. It also rules out a leading `-`, which a capability would read
/// as an exclusion.
///
/// ```
/// use ikigai_name::tenant::normalize_host;
///
/// assert_eq!(normalize_host("IRIREF.org:8443").as_deref(), Some("iriref.org"));
/// assert_eq!(normalize_host("iriref.org.").as_deref(), Some("iriref.org"));
/// assert_eq!(normalize_host("a.example, b.example"), None);
/// assert_eq!(normalize_host("../urnid.org"), None);
/// ```
pub fn normalize_host(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let name = match trimmed.rsplit_once(':') {
        Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => name,
        Some(_) => return None,
        None => trimmed,
    };
    let name = name.strip_suffix('.').unwrap_or(name).to_ascii_lowercase();
    let label_ok = |label: &str| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    (name.len() <= 253 && name.split('.').all(label_ok)).then_some(name)
}

/// The scope that names `host` as a request's tenant, or `None` if `host` is not
/// a DNS name ([`normalize_host`]).
pub fn tenant_scope(host: &str) -> Option<String> {
    normalize_host(host).map(|host| format!("{TENANT_SCOPE_PREFIX}{host}"))
}

/// The tenant a capability is for: `None` when it names no tenant, or when it
/// names more than one, which is no answer to "whose request is this?".
pub fn tenant_of(capability: &Capability) -> Option<&str> {
    let mut named = capability
        .scopes()?
        .iter()
        .filter_map(|scope| scope.strip_prefix(TENANT_SCOPE_PREFIX));
    let tenant = named.next()?;
    named.next().is_none().then_some(tenant)
}

/// The host an `http:` or `https:` IRI names, normalized as a tenant key. `None`
/// for any other scheme, and for an authority that is not a DNS name.
pub fn iri_host(iri: &str) -> Option<String> {
    let (scheme, rest) = iri.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    // After any userinfo: `https://urnid.org@iriref.org/` is on iriref.org.
    normalize_host(authority.rsplit('@').next()?)
}

/// Whether `iri` is a term of `tenant`: its host is the tenant's. Anything else
/// lives somewhere else, and `tenant` can only refer to it.
pub fn belongs_to(iri: &str, tenant: &str) -> bool {
    iri_host(iri).as_deref() == Some(tenant)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{space, REGISTRY_IRI};
    use futures::executor::block_on;
    use ikigai_core::{
        ArgRef, Error, Exact, FnEndpoint, Invocation, Iri, Kernel, ReprType, Representation,
        Request, Verb,
    };
    use std::sync::Arc;

    #[test]
    fn hosts_normalize_to_one_tenant_key() {
        for spelling in [
            "iriref.org",
            "IRIREF.ORG",
            "iriref.org:443",
            "iriref.org.",
            "iriref.org.:8080",
            " iriref.org ",
        ] {
            assert_eq!(
                normalize_host(spelling).as_deref(),
                Some("iriref.org"),
                "{spelling:?}"
            );
        }
    }

    #[test]
    fn what_is_not_a_dns_name_is_no_tenant() {
        for refused in [
            "",
            ".",
            "..",
            "a..b",
            "../urnid.org",
            "urnid.org/x",
            "a.example,b.example",
            "[::1]:8080",
            "::1",
            "-deny.example",
            "under_score.example",
            "iriref.org:http",
            "ïriref.org",
        ] {
            assert_eq!(normalize_host(refused), None, "{refused:?}");
        }
    }

    /// A host that a capability would read as an exclusion can never be minted.
    #[test]
    fn a_tenant_scope_is_never_deny_shaped() {
        assert_eq!(tenant_scope("-acme"), None);
        let scope = tenant_scope("Acme.example:80").expect("a name");
        assert_eq!(scope, "urn:cap:name:tenant:acme.example");
        assert!(!ikigai_core::is_deny_scope(&scope), "{scope}");
    }

    #[test]
    fn a_capability_names_at_most_one_tenant() {
        let scope = |host| tenant_scope(host).expect("a name");
        assert_eq!(tenant_of(&Capability::root()), None);
        assert_eq!(tenant_of(&Capability::scoped(Vec::<String>::new())), None);
        assert_eq!(
            tenant_of(&Capability::scoped([scope("urnid.org")])),
            Some("urnid.org")
        );
        assert_eq!(
            tenant_of(&Capability::scoped([
                scope("urnid.org"),
                scope("iriref.org")
            ])),
            None,
            "two tenants answer nothing"
        );
    }

    #[test]
    fn an_iris_host_is_its_authority_and_nothing_else() {
        let host = |iri| iri_host(iri);
        assert_eq!(
            host("https://IRIREF.org:8443/a#b").as_deref(),
            Some("iriref.org")
        );
        assert_eq!(host("HTTP://urnid.org").as_deref(), Some("urnid.org"));
        assert_eq!(host("https://urnid.org?q").as_deref(), Some("urnid.org"));
        assert_eq!(
            host("https://urnid.org@iriref.org/x").as_deref(),
            Some("iriref.org"),
            "the host follows any userinfo"
        );
        assert_eq!(host("urn:file:vocab/core.ttl"), None);
        assert_eq!(host("ftp://urnid.org/x"), None);
        assert!(belongs_to("https://urnid.org/a#b", "urnid.org"));
        assert!(!belongs_to("https://urnid.org.evil.example/a", "urnid.org"));
        assert!(!belongs_to("https://evil.example/urnid.org", "urnid.org"));
    }

    /// Below the HTTP door: one kernel, one `urn:name:resolve`, one path, and a
    /// cacheable registry source that keys on the tenant. Each tenant gets its
    /// own answer, in either order, and each keeps its own cache entry.
    #[test]
    fn the_kernel_cache_is_partitioned_by_tenant() {
        let source = FnEndpoint::new("registry-source", |inv: &Invocation<'_>| {
            let tenant = tenant_of(inv.capability).ok_or_else(|| {
                Error::Denied(format!("{REGISTRY_IRI} is read only for a tenant"))
            })?;
            let body = format!(
                r#"{{"namespaces":[{{"prefix":"resmud","owner":"urn:cap:name:admin:resmud",
                   "strategy":"hosted","source":"urn:file:{tenant}"}}]}}"#
            );
            Ok(
                Representation::new(ReprType::new("application/json"), body.into_bytes())
                    .cacheable(),
            )
        });
        let kernel = Kernel::new(Arc::new(space().bind(Exact::new(REGISTRY_IRI), source)));
        let resolve = |host: &str| {
            let cap = Capability::scoped([tenant_scope(host).expect("a name")]);
            let request = Request::new(Verb::Source, Iri::parse("urn:name:resolve").unwrap())
                .with_arg("path", ArgRef::Inline(b"resmud/core".to_vec()));
            let repr = block_on(kernel.issue(request, &cap)).expect("resolves");
            String::from_utf8(repr.bytes.clone()).expect("utf-8")
        };
        for _ in 0..3 {
            assert_eq!(resolve("urnid.org"), "resmud\thosted\turn:file:urnid.org\n");
            assert_eq!(
                resolve("iriref.org"),
                "resmud\thosted\turn:file:iriref.org\n"
            );
        }
    }
}
