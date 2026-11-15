//! Opt-in, host-run redirect target checks, available only with the existing
//! outbound-client build (`git`). Bind [`space`] alongside the regular name space.
//!
//! A host issues [`CHECK_TEMPLATE`] as Sink on a timer, holding [`CAP_CHECK`] and
//! `urn:cap:net:<host>` for every HTTP target. The whole namespace is checked for
//! authority before any probe leaves. Probes use the host-bound `urn:http:head`
//! through the kernel, which checks authority at every redirect hop and drops
//! response bodies unread. HEAD reports presence, not exact HTTP status codes.
//!
//! Only registry targets are probed: the fallback and each rule's literal text
//! before its first capture, deduplicated per namespace. No resolved name, visitor
//! identity, request header, response body, or transport error text is retained.
//! Non-HTTP targets and URLs with credentials are reported as unsupported.
//!
//! The last completed result lives in memory, keyed by tenant and namespace; a
//! restart loses it. Reads need the current recorded owner and are uncacheable.
//! A result for targets changed since the check is refused until the next run.

use crate::admin::require_admin;
use crate::tenant::tenant_of;
use crate::{load, Namespace, Strategy, CAP_ADMIN_ANY, XSD_STRING};
use ikigai_core::{
    ActionSpec, ArgRef, ArgSpec, AsyncFnEndpoint, Description, EndpointSpace, Error, Invocation,
    InvokeFuture, Iri, ReprType, Representation, Request, Result, UriTemplate, Verb,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// The host job's authority, separate from namespace administration.
pub const CAP_CHECK: &str = "urn:cap:name:target-health-check";
/// Sink issued by a host to check one redirect namespace.
pub const CHECK_TEMPLATE: &str = "urn:name:target-health-check:namespace:{prefix}";
/// Source of the last check, readable by the namespace's current administrator.
pub const STATUS_TEMPLATE: &str = "urn:name:target-health:namespace:{prefix}";

#[derive(Clone, Serialize)]
struct Target {
    target: String,
    status: Option<u16>,
    outcome: &'static str,
    elapsed_ms: u64,
}

#[derive(Clone, Serialize)]
struct Report {
    prefix: String,
    checked_at_unix_ms: Option<u64>,
    targets: Vec<Target>,
}

type Reports = Arc<Mutex<BTreeMap<(Option<String>, String), Report>>>;

fn targets(ns: &Namespace) -> Result<Vec<String>> {
    let Strategy::Redirect { target, rules, .. } = &ns.strategy else {
        return Err(Error::Unsupported {
            verb: None,
            detail: format!("name: {:?} is not a redirect namespace", ns.prefix),
        });
    };
    let mut bases = BTreeSet::from([target.clone()]);
    for rule in rules {
        bases.insert(
            rule.target
                .split('{')
                .next()
                .unwrap_or_default()
                .to_string(),
        );
    }
    Ok(bases.into_iter().collect())
}

fn http_url(target: &str) -> Option<reqwest::Url> {
    let url = reqwest::Url::parse(target).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none())
    .then_some(url)
}

fn prefix<'a>(inv: &'a Invocation<'_>) -> Result<&'a str> {
    inv.bindings
        .get("prefix")
        .ok_or_else(|| Error::MissingArgument("prefix".into()))
}

fn prefix_input() -> ArgSpec {
    ArgSpec::new("prefix")
        .summary("the namespace as claimed")
        .binding()
        .class(XSD_STRING)
}

fn json(report: &Report) -> Result<Representation> {
    let bytes =
        serde_json::to_vec_pretty(report).map_err(|e| Error::Endpoint(format!("name: {e}")))?;
    Ok(Representation::new(
        ReprType::new("application/json"),
        bytes,
    ))
}

fn lock_error() -> Error {
    Error::Unavailable("name: target health results are unavailable".into())
}

/// Build the two opt-in faces sharing one in-memory result store.
///
/// The host must bind `urn:http:head` (for example with `ikigai_http::space`)
/// and inject a kernel clock for timestamps. Without a clock, the timestamp is
/// null. No client is constructed here; traffic uses only the host transport
/// when the host issues the check Sink.
pub fn space() -> EndpointSpace {
    let reports: Reports = Arc::new(Mutex::new(BTreeMap::new()));
    let read_reports = reports.clone();
    let status = AsyncFnEndpoint::new(
        "target-health",
        move |inv: &Invocation<'_>| -> InvokeFuture<'_> {
            let reports = read_reports.clone();
            Box::pin(async move {
                let prefix = prefix(inv)?;
                let registry = load(inv).await?;
                let at = require_admin(
                    &registry,
                    inv.capability,
                    prefix,
                    "reading target health for",
                )?;
                let current = targets(&registry.namespaces[at])?;
                let key = (
                    tenant_of(inv.capability).map(str::to_string),
                    prefix.to_string(),
                );
                let held = reports.lock().map_err(|_| lock_error())?;
                let report = held
                    .get(&key)
                    .filter(|report| report.targets.iter().map(|t| &t.target).eq(current.iter()))
                    .ok_or_else(|| {
                        Error::NotFound(format!(
                            "name: {prefix:?} has no target health check for its current targets; \
                             the host must run urn:name:target-health-check:namespace:{prefix}"
                        ))
                    })?;
                json(report)
            })
        },
    )
    .with_description(
        Description::new("target-health")
            .title("The latest redirect target check")
            .action(
                ActionSpec::new(Verb::Source)
                    .summary("read statuses and elapsed times; no request data is collected")
                    .requires(CAP_ADMIN_ANY)
                    .input(prefix_input())
                    .output("application/json"),
            ),
    );

    let check = AsyncFnEndpoint::new(
        "target-health-check",
        move |inv: &Invocation<'_>| -> InvokeFuture<'_> {
            let reports = reports.clone();
            Box::pin(async move {
                let prefix = prefix(inv)?;
                let registry = load(inv).await?;
                let ns = registry
                    .namespaces
                    .iter()
                    .find(|ns| ns.prefix == prefix)
                    .ok_or_else(|| crate::resolution::unclaimed(prefix, None))?;
                let bases = targets(ns)?;
                let urls: Vec<_> = bases.iter().map(|target| http_url(target)).collect();
                // Preflight every host before sending even the first request.
                for url in urls.iter().flatten() {
                    let host = url.host_str().expect("validated host");
                    let scope = format!("urn:cap:net:{host}");
                    if !ikigai_http::net_allows_port(
                        inv.capability,
                        host,
                        url.port_or_known_default(),
                        url.path(),
                    ) {
                        return Err(Error::Denied(format!(
                            "name: checking target health for {prefix:?} needs {scope}"
                        )));
                    }
                }
                let mut report = Report {
                    prefix: prefix.to_string(),
                    checked_at_unix_ms: inv.now().map(|now| now.as_millis()),
                    targets: Vec::new(),
                };
                for (target, url) in bases.into_iter().zip(urls) {
                    let start = Instant::now();
                    let (status, outcome) = match url {
                        None => (None, "unsupported"),
                        Some(url) => {
                            let request = Request::new(
                                Verb::Exists,
                                Iri::parse("urn:http:head").expect("valid IRI"),
                            )
                            .with_arg("url", ArgRef::Inline(url.to_string().into_bytes()))
                            .with_arg("max_age", ArgRef::Inline(b"0".to_vec()));
                            match inv.issue(request).await {
                                Ok(response) => {
                                    let status = response
                                        .repr_type
                                        .params
                                        .get("http-status")
                                        .and_then(|s| s.parse::<u16>().ok());
                                    match response.bytes.as_slice() {
                                        b"true" => (status, "answered"),
                                        b"false" => (status, "gone"),
                                        _ if status.is_some() => (status, "answered"),
                                        _ => (None, "unreachable"),
                                    }
                                }
                                Err(Error::Denied(_) | Error::Refused(_)) => (None, "refused"),
                                Err(Error::NotFound(_)) => (None, "gone"),
                                Err(_) => (None, "unreachable"),
                            }
                        }
                    };
                    report.targets.push(Target {
                        target,
                        status,
                        outcome,
                        elapsed_ms: start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                    });
                }
                let key = (
                    tenant_of(inv.capability).map(str::to_string),
                    prefix.to_string(),
                );
                let mut held = reports.lock().map_err(|_| lock_error())?;
                held.retain(|(tenant, prefix), _| {
                    tenant != &key.0
                        || registry.namespaces.iter().any(|ns| {
                            ns.prefix == *prefix && matches!(ns.strategy, Strategy::Redirect { .. })
                        })
                });
                held.insert(key, report.clone());
                json(&report)
            })
        },
    )
    .with_description(
        Description::new("target-health-check")
            .title("Check a redirect namespace's targets")
            .action(
                ActionSpec::new(Verb::Sink)
                    .summary(
                        "probe each literal target base once, under the host's network authority",
                    )
                    .requires(CAP_CHECK)
                    .requires("urn:cap:net:*")
                    .input(prefix_input())
                    .output("application/json"),
            ),
    );
    EndpointSpace::new()
        .bind(
            UriTemplate::parse(STATUS_TEMPLATE).expect("valid template"),
            status,
        )
        .bind(
            UriTemplate::parse(CHECK_TEMPLATE).expect("valid template"),
            check,
        )
}
