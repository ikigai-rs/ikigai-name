//! Active names: how many distinct names a namespace resolved in a month.
//!
//! A namespace's volume is the names it actually answers for, not its registry
//! entries, since one entry can answer for any number of names. This is resolver
//! telemetry an operator reads to see what a namespace serves.
//!
//! ## Aggregate only
//!
//! What is stored is the resolved name and the period, and nothing about the
//! request: no address, no user agent, no cookie, no per-visitor identifier. A
//! name is what was asked for, not who asked. The stored set has nowhere to put
//! anything else, and reading a stored set refuses a field it does not name.
//!
//! ## The resolution path only touches memory
//!
//! Resolving a name adds it to a buffer in the space and awaits nothing, so a slow
//! or failing store cannot slow or fail a resolution: the store is not on that path
//! at all. It also could not be, because a request arrives holding read authority at
//! most, and the store needs write. [`FLUSH_IRI`] moves the buffer to the store under
//! the capability of whoever issues it, which is a job the host runs on a timer (core
//! has no runtime to run it on). A failed write keeps the names buffered and answers
//! the failure to that job, which logs it.
//!
//! **What a crash can lose** is the names first seen since the last successful flush,
//! never more than [`MAX_BUFFERED_NAMES`]. The standalone server flushes every 60
//! seconds by default. The buffer has that ceiling because a client chooses the names:
//! past it a new name is dropped and the next flush reports how many were, and so is
//! one over [`MAX_NAME_BYTES`], or one past [`MAX_SEEN_NAMES`] in a stored set, which
//! is what stops a client growing a set without limit. A count at that ceiling is a
//! floor.
//!
//! ## A cached answer must not hide a new month
//!
//! The kernel answers a cached resolution without running the face, so nothing here
//! sees it. Within a month that is harmless, as the name is already in the set. Across
//! a boundary it would lose the name from the new month, so a counted answer is
//! cacheable only until its period ends. An HTTP door still projects that as
//! `no-cache` whenever the answer hangs from a thread, as a registry-backed one does.
//!
//! The period comes from the kernel's clock, in UTC. **A kernel with no clock counts
//! nothing**, because it cannot say which month it is, and its answers stay as
//! cacheable as they ever were.
//!
//! ## The seam
//!
//! A set is read and written through a bound IRI, as the registry is, so this crate
//! never touches a file. A host binds [`SEEN_TEMPLATE`], and keys it on
//! [`tenant_of`] if it has tenants. Each set lives at
//! `urn:name:seen-source:namespace:{prefix}:period:{yyyy-mm}` as
//! `{"period":"2026-11","names":["resmud/core"]}`, and the periods a namespace holds
//! at `urn:name:seen-source:namespace:{prefix}` as `{"periods":["2026-11"]}`.
//!
//! Source answers the document, or [`Error::NotFound`] if there is none yet. Sink takes
//! `content`, and `if-match`, the content ID of the Source representation being
//! replaced; with no `if-match` the document must not exist yet. It compares and
//! replaces as one operation, or refuses with [`Error::Conflict`], as
//! [`REGISTRY_IRI`](crate::REGISTRY_IRI) does. The next flush reads again and retries.

use crate::admin::{admin_scope, refuse_reserved_prefix, CAP_ADMIN_ANY};
use crate::history::{epoch_days, format_utc, AsOf};
use crate::load;
use crate::tenant::tenant_of;
use ikigai_core::{
    ActionSpec, ArgRef, ArgSpec, AsyncFnEndpoint, ContentId, Description, Error, Expiry,
    Invocation, InvokeFuture, Iri, ReprType, Representation, Request, Result, Time, Verb,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

/// The template a host binds the store at; see the [module documentation](self).
pub const SEEN_TEMPLATE: &str = "urn:name:seen-source:namespace:{key}";

/// The Sink that moves buffered names to the store, under the caller's capability.
pub const FLUSH_IRI: &str = "urn:name:seen-flush";

/// The capability [`FLUSH_IRI`] requires. A host's flush job holds it; no request does.
pub const CAP_FLUSH: &str = "urn:cap:name:seen-flush";

/// The most names held in memory between flushes, across every tenant and namespace.
pub const MAX_BUFFERED_NAMES: usize = 16_384;

/// The longest name counted, in bytes.
pub const MAX_NAME_BYTES: usize = 1_024;

/// The most names one stored set holds.
pub const MAX_SEEN_NAMES: usize = 1_000_000;

/// Where one namespace's count for one month is answered.
pub(crate) const COUNT_TEMPLATE: &str = "urn:name:stat:namespace:{prefix}:period:{period}";

/// Where one namespace's report is answered.
pub(crate) const REPORT_TEMPLATE: &str = "urn:name:stat:namespace:{prefix}";

const SEEN_PREFIX: &str = "urn:name:seen-source:namespace:";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const TEXT_PLAIN_UTF8: &str = "text/plain;charset=utf-8";
const TEXT_PLAIN: &str = "text/plain";
const TEXT_TURTLE: &str = "text/turtle";

/// The stored atom: one namespace's names for one period. Its partition keys are in
/// its IRI, so the document holds the period and the names and nothing else.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SeenSet {
    period: String,
    names: BTreeSet<String>,
}

/// The periods a namespace holds a set for.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HeldPeriods {
    periods: BTreeSet<String>,
}

/// One set's identity: its tenant, if any, its namespace and its period.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SetKey {
    tenant: Option<String>,
    prefix: String,
    period: String,
}

#[derive(Default)]
struct Buffer {
    sets: BTreeMap<SetKey, BTreeSet<String>>,
    held: usize,
    dropped: BTreeMap<Option<String>, usize>,
}

/// The names resolved since the last flush. One per [`crate::space`].
#[derive(Default)]
pub(crate) struct Seen {
    buffer: Mutex<Buffer>,
}

impl Seen {
    /// A poisoned lock still holds a good buffer: nothing here panics between edits
    /// that matter, and counting must not fail a resolution.
    fn lock(&self) -> MutexGuard<'_, Buffer> {
        self.buffer.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record that `path` resolved under `prefix`, answering when its period ends, or
    /// `None` on a kernel with no clock, which counts nothing.
    pub(crate) fn note(&self, inv: &Invocation<'_>, prefix: &str, path: &str) -> Option<Time> {
        let period = period_of(inv.now()?)?;
        let end = period_end(&period)?;
        // `resmud/core`, `/resmud/core` and `resmud//core` are one name, to the
        // registry and so here.
        let name = crate::iri::name_key(path);
        let key = SetKey {
            tenant: tenant_of(inv.capability).map(str::to_string),
            prefix: prefix.to_string(),
            period,
        };
        let mut buffer = self.lock();
        let known = buffer.sets.get(&key).is_some_and(|set| set.contains(&name));
        if !known {
            if name.len() > MAX_NAME_BYTES || buffer.held >= MAX_BUFFERED_NAMES {
                *buffer.dropped.entry(key.tenant).or_default() += 1;
            } else {
                buffer.sets.entry(key).or_default().insert(name);
                buffer.held += 1;
            }
        }
        Some(end)
    }

    /// The names buffered for one set.
    fn pending(&self, key: &SetKey) -> BTreeSet<String> {
        self.lock().sets.get(key).cloned().unwrap_or_default()
    }

    /// The periods a namespace has names buffered for.
    fn pending_periods(&self, tenant: Option<&str>, prefix: &str) -> Vec<String> {
        self.lock()
            .sets
            .keys()
            .filter(|key| key.tenant.as_deref() == tenant && key.prefix == prefix)
            .map(|key| key.period.clone())
            .collect()
    }

    /// One tenant's sets. They stay in the buffer until written, so a count taken
    /// during a flush still sees them.
    fn snapshot(&self, tenant: Option<&str>) -> Vec<(SetKey, BTreeSet<String>)> {
        self.lock()
            .sets
            .iter()
            .filter(|(key, _)| key.tenant.as_deref() == tenant)
            .map(|(key, names)| (key.clone(), names.clone()))
            .collect()
    }

    /// Forget names now stored. A name resolved again meanwhile comes back on its next
    /// miss, and the flush after that finds it already stored.
    fn forget(&self, key: &SetKey, written: &BTreeSet<String>) {
        let mut buffer = self.lock();
        let Some(set) = buffer.sets.get_mut(key) else {
            return;
        };
        let before = set.len();
        set.retain(|name| !written.contains(name));
        let removed = before - set.len();
        if set.is_empty() {
            buffer.sets.remove(key);
        }
        buffer.held -= removed;
    }

    fn add_dropped(&self, tenant: Option<&str>, count: usize) {
        if count > 0 {
            *self
                .lock()
                .dropped
                .entry(tenant.map(str::to_string))
                .or_default() += count;
        }
    }

    fn dropped(&self, tenant: Option<&str>) -> usize {
        let buffer = self.lock();
        buffer
            .dropped
            .get(&tenant.map(str::to_string))
            .copied()
            .unwrap_or(0)
    }

    fn clear_dropped(&self, tenant: Option<&str>) {
        self.lock().dropped.remove(&tenant.map(str::to_string));
    }
}

/// A face's successful answer for `path`: recorded, unless it is about the past, and
/// cacheable no longer than the period it was recorded in.
pub(crate) fn counted(
    seen: Option<&Seen>,
    inv: &Invocation<'_>,
    at: Option<&AsOf>,
    prefix: &str,
    path: &str,
    answer: Representation,
) -> Representation {
    if at.is_some() {
        return answer;
    }
    match seen.and_then(|seen| seen.note(inv, prefix, path)) {
        Some(end) => {
            let expiry = answer.expiry.most_restrictive(Expiry::At(end));
            answer.with_expiry(expiry)
        }
        None => answer,
    }
}

/// The `yyyy-mm` an instant falls in, in UTC.
fn period_of(now: Time) -> Option<String> {
    let seconds = i64::try_from(now.as_millis() / 1_000).ok()?;
    Some(format_utc(seconds)?[..7].to_string())
}

/// Whether `text` is a real `yyyy-mm`.
fn is_period(text: &str) -> bool {
    text.len() == 7 && epoch_days(&format!("{text}-01")).is_some()
}

/// The first instant after a period.
fn period_end(period: &str) -> Option<Time> {
    let (year, month) = period.split_once('-')?;
    let (year, month): (u32, u32) = (year.parse().ok()?, month.parse().ok()?);
    let (year, month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let days = epoch_days(&format!("{year:04}-{month:02}-01"))?;
    Some(Time::from_millis(u64::try_from(days).ok()? * 86_400_000))
}

// --- the store --------------------------------------------------------------

fn set_iri(prefix: &str, period: &str) -> String {
    format!("{SEEN_PREFIX}{prefix}:period:{period}")
}

fn periods_iri(prefix: &str) -> String {
    format!("{SEEN_PREFIX}{prefix}")
}

fn parsed_iri(target: &str) -> Result<Iri> {
    Iri::parse(target).map_err(|e| Error::Endpoint(format!("name: {e}")))
}

fn unbound(e: Error, target: &str) -> Error {
    match e {
        Error::Unresolved(missing) if missing.as_str() == target => Error::Unsupported {
            verb: None,
            detail: format!(
                "name: this host keeps no count of names resolved; bind {SEEN_TEMPLATE} to add it"
            ),
        },
        other => other,
    }
}

/// A stored document and its content ID, or `None` if there is none yet.
async fn read_stored<T: serde::de::DeserializeOwned>(
    inv: &Invocation<'_>,
    target: &str,
) -> Result<Option<(T, ContentId)>> {
    let repr = match inv.source(&parsed_iri(target)?).await {
        Ok(repr) => repr,
        Err(Error::NotFound(_)) => return Ok(None),
        Err(e) => return Err(unbound(e, target)),
    };
    let parsed = serde_json::from_slice(&repr.bytes).map_err(|e| {
        Error::Endpoint(format!(
            "name: {target} is not a set of names this module wrote: {e}"
        ))
    })?;
    Ok(Some((parsed, repr.content_id())))
}

/// Replace a stored document, conditional on the version read.
async fn write_stored<T: Serialize>(
    inv: &Invocation<'_>,
    target: &str,
    value: &T,
    expected: Option<ContentId>,
) -> Result<()> {
    let body =
        serde_json::to_vec_pretty(value).map_err(|e| Error::Endpoint(format!("name: {e}")))?;
    let mut request =
        Request::new(Verb::Sink, parsed_iri(target)?).with_arg("content", ArgRef::Inline(body));
    if let Some(expected) = expected {
        request = request.with_arg(
            "if-match",
            ArgRef::Inline(expected.to_string().into_bytes()),
        );
    }
    inv.issue(request).await.map_err(|e| unbound(e, target))?;
    Ok(())
}

/// Add `names` to their stored set. Answers how many were new and how many did not
/// fit. The period goes on the list first: a crash between the two writes leaves a
/// listed period with no set, which reads as zero, rather than a set nothing lists.
async fn store_names(
    inv: &Invocation<'_>,
    key: &SetKey,
    names: &BTreeSet<String>,
) -> Result<(usize, usize)> {
    let target = set_iri(&key.prefix, &key.period);
    let stored = read_stored::<SeenSet>(inv, &target).await?;
    let (mut set, token) = match stored {
        Some((set, token)) => (set, Some(token)),
        None => (
            SeenSet {
                period: key.period.clone(),
                names: BTreeSet::new(),
            },
            None,
        ),
    };
    let (mut added, mut refused) = (0, 0);
    for name in names {
        if set.names.contains(name) {
            continue;
        }
        if set.names.len() >= MAX_SEEN_NAMES {
            refused += 1;
        } else {
            set.names.insert(name.clone());
            added += 1;
        }
    }
    if added == 0 {
        return Ok((0, refused));
    }

    let list = periods_iri(&key.prefix);
    let listed = read_stored::<HeldPeriods>(inv, &list).await?;
    let (mut held, held_token) = match listed {
        Some((held, token)) => (held, Some(token)),
        None => (HeldPeriods::default(), None),
    };
    if held.periods.insert(key.period.clone()) {
        write_stored(inv, &list, &held, held_token).await?;
    }
    write_stored(inv, &target, &set, token).await?;
    Ok((added, refused))
}

// --- the faces --------------------------------------------------------------

fn bound<'a>(inv: &'a Invocation<'_>, name: &str) -> Result<&'a str> {
    inv.bindings
        .get(name)
        .ok_or_else(|| Error::MissingArgument(name.to_string()))
}

/// The namespace's administrator or the operator, whose root capability allows every
/// scope. The kernel has enforced the wildcard floor; this is the half that tells one
/// namespace from another. Checked before the registry is read, so a refusal does not
/// say whether the namespace exists.
async fn authorized_prefix<'a>(inv: &'a Invocation<'_>) -> Result<&'a str> {
    let prefix = bound(inv, "prefix")?;
    refuse_reserved_prefix(prefix)?;
    let scope = admin_scope(prefix);
    if !inv.capability.allows(&scope) {
        return Err(Error::Denied(format!(
            "name: reading the count for {prefix:?} needs `{scope}`"
        )));
    }
    let registry = load(inv).await?;
    if !registry.namespaces.iter().any(|ns| ns.prefix == prefix) {
        return Err(Error::NotFound(format!(
            "name: no namespace claims {prefix:?}"
        )));
    }
    Ok(prefix)
}

/// One month's count, and whether it may be cached: only for a past period, once its
/// set is stored and nothing for it is still buffered, which makes it a pure function
/// of the atom, under the atom's thread. Anything else is live, including every period
/// on a kernel with no clock.
async fn active_names(
    inv: &Invocation<'_>,
    seen: &Seen,
    prefix: &str,
    period: &str,
) -> Result<(usize, bool)> {
    let stored = read_stored::<SeenSet>(inv, &set_iri(prefix, period)).await?;
    let key = SetKey {
        tenant: tenant_of(inv.capability).map(str::to_string),
        prefix: prefix.to_string(),
        period: period.to_string(),
    };
    let pending = seen.pending(&key);
    let past = inv
        .now()
        .and_then(period_of)
        .is_some_and(|current| period < current.as_str());
    let settled = past && stored.is_some() && pending.is_empty();
    let mut names = stored.map(|(set, _)| set.names).unwrap_or_default();
    names.extend(pending);
    Ok((names.len(), settled))
}

pub(crate) fn text_answer(body: String) -> Representation {
    Representation::new(
        ReprType::new(TEXT_PLAIN).with_param("charset", "utf-8"),
        body.into_bytes(),
    )
}

/// The count as an observation. Its subject is the IRI that answered it, so there is no
/// blank node. `ikigai-vocab` has no term for a count of names over a period, so these
/// are schema.org's and Dublin Core's.
fn observation(iri: &str, prefix: &str, period: &str, count: usize) -> Result<Representation> {
    let namespace =
        serde_json::to_string(prefix).map_err(|e| Error::Endpoint(format!("name: {e}")))?;
    let body = format!(
        "@prefix dcterms: <http://purl.org/dc/terms/> .\n\
         @prefix schema: <https://schema.org/> .\n\
         @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n\n\
         <{iri}> a schema:Observation ;\n    \
             schema:observationAbout {namespace} ;\n    \
             schema:variableMeasured \"active names\" ;\n    \
             dcterms:temporal \"{period}\"^^xsd:gYearMonth ;\n    \
             schema:value {count} .\n"
    );
    Ok(Representation::new(
        ReprType::new(TEXT_TURTLE),
        body.into_bytes(),
    ))
}

fn prefix_input() -> ArgSpec {
    ArgSpec::new("prefix")
        .summary("the namespace, as claimed, e.g. resmud")
        .binding()
        .class(XSD_STRING)
}

/// `urn:name:stat:namespace:{prefix}:period:{yyyy-mm}` — the size of one month's set.
pub(crate) fn count(seen: Arc<Seen>) -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("active-name-count", move |inv: &Invocation<'_>| {
        let seen = seen.clone();
        Box::pin(async move {
            let period = bound(inv, "period")?;
            if !is_period(period) {
                return Err(Error::InvalidArgument {
                    name: "period".into(),
                    detail: format!("{period:?} is not a month: write yyyy-mm, such as 2026-11"),
                });
            }
            let wanted = inv.inline_str("as").unwrap_or(TEXT_PLAIN);
            let wanted = wanted.split(';').next().unwrap_or(wanted).trim();
            if wanted != TEXT_PLAIN && wanted != TEXT_TURTLE {
                return Err(Error::InvalidArgument {
                    name: "as".into(),
                    detail: format!(
                        "{wanted:?} is not a form of this count: {TEXT_PLAIN} answers the number, \
                         {TEXT_TURTLE} an observation"
                    ),
                });
            }
            let prefix = authorized_prefix(inv).await?;
            let (count, settled) = active_names(inv, &seen, prefix, period).await?;
            let answer = if wanted == TEXT_TURTLE {
                observation(inv.request.target.as_str(), prefix, period, count)?
            } else {
                text_answer(format!("{count}\n"))
            };
            Ok(if settled { answer.cacheable() } else { answer })
        }) as InvokeFuture<'_>
    })
    .with_description(
        Description::new("active-name-count")
            .title("Active names in a namespace, for one month")
            .summary(
                "How many distinct names this namespace resolved in the month, in UTC. A past \
                 month is cacheable under its stored set; the current one is live.",
            )
            .action(
                ActionSpec::new(Verb::Source)
                    .summary("count the names a namespace resolved in a month")
                    .requires(CAP_ADMIN_ANY)
                    .input(prefix_input())
                    .input(
                        ArgSpec::new("period")
                            .summary("the month, as yyyy-mm")
                            .binding()
                            .class(XSD_STRING),
                    )
                    .input(
                        ArgSpec::new("as")
                            .summary("text/plain for the number, text/turtle for an observation")
                            .default_value(TEXT_PLAIN)
                            .optional()
                            .class(XSD_STRING),
                    )
                    .output(TEXT_PLAIN_UTF8)
                    .output(TEXT_TURTLE),
            ),
    )
}

/// `urn:name:stat:namespace:{prefix}` — one line per period held. Always live: the
/// current month can appear in it at any moment.
pub(crate) fn report(seen: Arc<Seen>) -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("namespace-report", move |inv: &Invocation<'_>| {
        let seen = seen.clone();
        Box::pin(async move {
            let prefix = authorized_prefix(inv).await?;
            let listed = read_stored::<HeldPeriods>(inv, &periods_iri(prefix)).await?;
            let mut periods = listed.map(|(held, _)| held.periods).unwrap_or_default();
            periods.extend(seen.pending_periods(tenant_of(inv.capability), prefix));
            if periods.is_empty() {
                return Ok(text_answer(format!(
                    "name: {prefix:?} has no names counted yet\n"
                )));
            }
            let mut lines = String::new();
            for period in periods {
                let (count, _) = active_names(inv, &seen, prefix, &period).await?;
                lines.push_str(&format!("{period}\t{count}\n"));
            }
            Ok(text_answer(lines))
        }) as InvokeFuture<'_>
    })
    .with_description(
        Description::new("namespace-report")
            .title("Active names in a namespace, by month")
            .summary(
                "One line per month this namespace holds names for: the month, a tab, and how \
                 many distinct names it resolved.",
            )
            .action(
                ActionSpec::new(Verb::Source)
                    .summary("report a namespace's active names, month by month")
                    .requires(CAP_ADMIN_ANY)
                    .input(prefix_input())
                    .output(TEXT_PLAIN_UTF8),
            ),
    )
}

/// `urn:name:seen-flush` — move one tenant's buffered names to the store.
///
/// The tenant is the one the capability names, so a host with tenants issues this once
/// for each. One set failing does not stop the rest, and its names stay buffered.
pub(crate) fn flush(seen: Arc<Seen>) -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("seen-flush", move |inv: &Invocation<'_>| {
        let seen = seen.clone();
        Box::pin(async move {
            let tenant = tenant_of(inv.capability);
            let mut written = 0;
            let mut failures = Vec::new();
            for (key, names) in seen.snapshot(tenant) {
                match store_names(inv, &key, &names).await {
                    Ok((added, refused)) => {
                        seen.forget(&key, &names);
                        seen.add_dropped(tenant, refused);
                        written += added;
                    }
                    Err(e) => failures.push(format!("{:?} for {}: {e}", key.prefix, key.period)),
                }
            }
            let dropped = seen.dropped(tenant);
            if !failures.is_empty() {
                return Err(Error::Endpoint(format!(
                    "name: could not store the names resolved in {}; they stay buffered, and \
                     {dropped} more were dropped since the last report",
                    failures.join("; ")
                )));
            }
            seen.clear_dropped(tenant);
            Ok(text_answer(format!(
                "names stored: {written}, dropped: {dropped}\n"
            )))
        }) as InvokeFuture<'_>
    })
    .with_description(
        Description::new("seen-flush")
            .title("Store the names resolved since the last flush")
            .summary(
                "Adds the buffered names to their stored sets, under the caller's capability, for \
                 the tenant it names. A host runs this on a timer.",
            )
            .action(
                ActionSpec::new(Verb::Sink)
                    .summary("store the buffered names")
                    .requires(CAP_FLUSH)
                    .output(TEXT_PLAIN_UTF8),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_period_is_the_month_an_instant_falls_in_in_utc() {
        // 2026-11-30T23:59:59Z, then the next second.
        assert_eq!(
            period_of(Time(1_796_083_199_000)).as_deref(),
            Some("2026-11")
        );
        assert_eq!(
            period_of(Time(1_796_083_200_000)).as_deref(),
            Some("2026-12")
        );
    }

    #[test]
    fn a_period_ends_where_the_next_begins() {
        assert_eq!(period_end("2026-11"), Some(Time(1_796_083_200_000)));
        assert_eq!(
            period_of(period_end("2026-12").unwrap()).as_deref(),
            Some("2027-01")
        );
        assert_eq!(
            period_of(period_end("2028-02").unwrap()).as_deref(),
            Some("2028-03")
        );
    }

    #[test]
    fn only_a_real_month_is_a_period() {
        assert!(is_period("2026-11"));
        for bad in [
            "", "2026", "2026-13", "2026-00", "2026-1", "26-11-01", "abcd-11", "2026/11",
        ] {
            assert!(!is_period(bad), "{bad:?}");
        }
    }
}
