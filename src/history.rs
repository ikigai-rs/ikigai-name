//! As-of resolution: what a name meant at a date.
//!
//! `as-of` is a **resolution parameter, never part of the name**. An IRI is stable
//! and additive-only, so `https://iriref.org/resmud/core#Weapon` is one term at
//! every date, and the date travels beside it as an argument. The registry is a
//! resource too, so "what did our namespace policy look like in March" is this
//! same mechanism: `urn:name:registry as-of=2026-03-31`.
//!
//! ## History is the archive, and the cache is not
//!
//! A past answer comes from the history of the repository holding the vocabulary,
//! never from a retained cache entry, because whether an answer is correct must
//! not depend on what has been evicted. An as-of answer is therefore never marked
//! cacheable here, and a date that has not happened yet can still change as
//! commits land.
//!
//! ## The seam, and what binds it
//!
//! Every past read goes through one bound IRI, [`HISTORY_IRI`], so this crate
//! carries no git and links no exec module. A binding takes `source` (the IRI as
//! the registry names it, or [`REGISTRY_IRI`] for the
//! registry itself) and `as-of` (an instant in UTC), and answers that resource's
//! representation as it stood then, or [`Error::NotFound`] if it did not exist
//! then. A host with tenants keys the binding on
//! [`tenant_of`](crate::tenant::tenant_of), as it does for the registry.
//!
//! With nothing bound, `as-of` is refused. Answering today's document to a
//! question about March would be a wrong answer that looks right.
//!
//! [`repo_history`] is the binding over git, composed from `urn:repo:*` resources
//! through the kernel. It needs two:
//!
//! - [`LOG_IRI`], served by `ikigai-repo` today, picks the commit.
//! - [`SHOW_IRI`] reads a file at that commit. **No `ikigai-repo` release serves
//!   it yet**, so a host must bind it until one does: it takes `dir`, `rev` (a full
//!   commit hash) and `path` (relative to the repository root), answers the file's
//!   bytes, and answers [`Error::NotFound`] when the path is absent at that commit.
//!
//! ## Which commit is "at that date"
//!
//! The newest commit touching the file, in log order, whose date is at or before
//! the instant. `urn:repo:log` reports the **author** date, the only date it
//! offers, so a commit rebased or merged long after it was written counts from
//! when it was written.

use crate::registry::Registry;
use crate::REGISTRY_IRI;
use ikigai_core::{
    ArgRef, ArgSpec, AsyncFnEndpoint, Description, Error, Invocation, InvokeFuture, Iri, ReprType,
    Representation, Request, Result, Verb,
};
use serde::Deserialize;

/// Where a resource's past is read from. A bound IRI, so a host decides what
/// holds the history; see the [module documentation](self) for its contract.
pub const HISTORY_IRI: &str = "urn:name:history-source";

/// The commits touching a path, from `ikigai-repo`.
pub const LOG_IRI: &str = "urn:repo:log";

/// A file at a commit. Not yet served by `ikigai-repo`; see the
/// [module documentation](self).
pub const SHOW_IRI: &str = "urn:repo:show";

/// The argument every as-of face takes.
pub(crate) const AS_OF: &str = "as-of";

/// How many commits [`repo_history`] asks for. A file with more than this many,
/// none of them old enough, is refused rather than reported as not yet existing.
const LOG_LIMIT: usize = 100_000;

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// A moment asked about: seconds since the epoch, and the same instant in UTC,
/// which is how a refusal names it and how it reaches [`HISTORY_IRI`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AsOf {
    seconds: i64,
    utc: String,
}

impl std::fmt::Display for AsOf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.utc)
    }
}

/// The contract line for a face that takes `as-of`.
pub(crate) fn as_of_input() -> ArgSpec {
    ArgSpec::new(AS_OF)
        .summary(
            "answer as of this date (YYYY-MM-DD, meaning the end of that day in UTC) or instant \
             (RFC 3339), read from the vocabulary's history; refused where no history is bound",
        )
        .optional()
        .class(XSD_STRING)
}

/// The `as-of` argument, if one was given.
///
/// A value that cannot be read is refused rather than treated as absent, since
/// treating it as absent would answer today's document to a question about the past.
pub(crate) fn as_of_arg(inv: &Invocation<'_>) -> Result<Option<AsOf>> {
    let raw = match inv.inline_str(AS_OF) {
        Ok(raw) => raw,
        Err(Error::MissingArgument(_)) => return Ok(None),
        Err(other) => return Err(other),
    };
    parse_as_of(raw.trim())
        .map(Some)
        .ok_or_else(|| Error::InvalidArgument {
            name: AS_OF.into(),
            detail: format!(
                "{raw:?} is not a date: write YYYY-MM-DD, or an RFC 3339 instant such as \
                 2026-03-31T12:00:00Z"
            ),
        })
}

/// The refusal for a face that only answers for the present.
pub(crate) fn no_page_as_of() -> Error {
    Error::Unsupported {
        verb: None,
        detail: "name: the documentation page shows only the current vocabulary; ask for \
                 text/turtle to read it as of a date, or leave out as-of"
            .into(),
    }
}

/// A resource as it stood at `at`, through [`HISTORY_IRI`].
pub(crate) async fn read_as_of(
    inv: &Invocation<'_>,
    source: &str,
    at: &AsOf,
) -> Result<Representation> {
    let iri = Iri::parse(HISTORY_IRI).map_err(|e| Error::Endpoint(format!("name: {e}")))?;
    let request = Request::new(Verb::Source, iri)
        .with_arg("source", ArgRef::Inline(source.as_bytes().to_vec()))
        .with_arg(AS_OF, ArgRef::Inline(at.utc.as_bytes().to_vec()));
    inv.issue(request).await.map_err(|e| match e {
        Error::Unresolved(missing) if missing.as_str() == HISTORY_IRI => Error::Unsupported {
            verb: None,
            detail: format!(
                "name: this host keeps no history, so it cannot answer as of {at}; leave out \
                 as-of for the current answer, or bind {HISTORY_IRI} to add it"
            ),
        },
        other => other,
    })
}

/// The registry as it stood at `at`.
pub(crate) async fn load_as_of(inv: &Invocation<'_>, at: &AsOf) -> Result<Registry> {
    let repr = read_as_of(inv, REGISTRY_IRI, at).await?;
    Registry::from_json(&repr.bytes).map_err(|e| {
        Error::Endpoint(format!(
            "name: {REGISTRY_IRI} as of {at} is not a valid registry: {e}"
        ))
    })
}

/// ` as of <instant>`, or nothing, for a refusal that may be about the past.
pub(crate) fn when(at: Option<&AsOf>) -> String {
    at.map(|at| format!(" as of {at}")).unwrap_or_default()
}

// --- the binding over git ---------------------------------------------------

#[derive(Deserialize)]
struct LogEntry {
    hash: String,
    date: String,
}

/// A [`HISTORY_IRI`] binding over one git repository, read through `urn:repo:*`.
///
/// `dir` is the repository's root, and `registry_file` is the registry's path
/// within it. A `urn:file:` source names its path relative to that root; any other
/// source has no history here and is refused. The caller's capability must grant
/// `urn:cap:exec:git`, and `dir` must be inside the roots the host gave `ikigai-repo`.
/// History is read from the repository's `HEAD`.
///
/// **[`SHOW_IRI`] must be bound as well**, because no `ikigai-repo` release serves it
/// yet; see the [module documentation](self).
pub fn repo_history(dir: impl Into<String>, registry_file: impl Into<String>) -> AsyncFnEndpoint {
    let dir = dir.into();
    let registry_file = registry_file.into();
    AsyncFnEndpoint::new("history-source", move |inv: &Invocation<'_>| {
        let dir = dir.clone();
        let registry_file = registry_file.clone();
        Box::pin(async move {
            let source = inv
                .inline_str("source")
                .map_err(|_| Error::MissingArgument("source".into()))?
                .to_string();
            let at = as_of_arg(inv)?.ok_or_else(|| Error::MissingArgument(AS_OF.into()))?;
            let path = repo_path(&source, &registry_file)?;

            let log = issue_repo(
                inv,
                LOG_IRI,
                &[
                    ("dir", dir.as_str()),
                    ("path", path),
                    ("limit", &LOG_LIMIT.to_string()),
                    ("as", "application/json"),
                ],
            )
            .await?;
            let entries: Vec<LogEntry> = serde_json::from_slice(&log.bytes)
                .map_err(|e| Error::Endpoint(format!("name: unreadable {LOG_IRI} answer: {e}")))?;
            let Some(commit) = commit_at(&entries, &at)? else {
                if entries.len() >= LOG_LIMIT {
                    return Err(Error::Endpoint(format!(
                        "name: {source} has at least {LOG_LIMIT} commits newer than {at}, so its \
                         history cannot be read back that far here"
                    )));
                }
                return Err(absent(&source, &at));
            };

            let file = issue_repo(
                inv,
                SHOW_IRI,
                &[("dir", dir.as_str()), ("rev", commit), ("path", path)],
            )
            .await
            .map_err(|e| match e {
                // The newest commit before the date removed it.
                Error::NotFound(_) => absent(&source, &at),
                other => other,
            })?;
            // Deliberately not `.cacheable()`: see the module documentation.
            Ok(Representation::new(media_type(path, &file), file.bytes))
        }) as InvokeFuture<'_>
    })
    .with_description(
        Description::new("history-source")
            .title("A resource as it stood at a date")
            .summary(
                "The newest committed version of a file at or before as-of, read through \
                 urn:repo:log and urn:repo:show, or NotFound if it did not exist then.",
            )
            .verb(Verb::Source)
            .input(
                ArgSpec::new("source")
                    .summary("the resource's IRI, as the registry names it")
                    .class(XSD_STRING),
            )
            .input(
                ArgSpec::new(AS_OF)
                    .summary("the date or RFC 3339 instant to read it at")
                    .class(XSD_STRING),
            ),
    )
}

/// A sub-request to a `urn:repo:*` resource. A missing one is named, since it is
/// something the host can bind.
async fn issue_repo(
    inv: &Invocation<'_>,
    target: &str,
    args: &[(&str, &str)],
) -> Result<Representation> {
    let iri = Iri::parse(target).map_err(|e| Error::Endpoint(format!("name: {e}")))?;
    let mut request = Request::new(Verb::Source, iri);
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    inv.issue(request).await.map_err(|e| match e {
        Error::Unresolved(missing) if missing.as_str() == target => Error::Unsupported {
            verb: None,
            detail: format!(
                "name: this host keeps no readable history, so it cannot answer as-of; bind \
                 {target} to add it"
            ),
        },
        other => other,
    })
}

fn absent(source: &str, at: &AsOf) -> Error {
    Error::NotFound(format!("name: {source} did not exist as of {at}"))
}

/// The file behind a source, relative to the repository's root.
///
/// A source comes from a registry, and an old registry is still a file someone
/// edited, so a path out of the repository or one a tool would read as a flag is
/// refused before anything runs.
fn repo_path<'a>(source: &'a str, registry_file: &'a str) -> Result<&'a str> {
    let path = if source == REGISTRY_IRI {
        registry_file
    } else {
        source
            .strip_prefix("urn:file:")
            .ok_or_else(|| Error::Unsupported {
                verb: None,
                detail: format!(
                    "name: {source} has no history on this host: only urn:file: sources are read \
                 from the repository"
                ),
            })?
    };
    let confined = !path.is_empty()
        && !path.starts_with(['/', '-'])
        && path.split('/').all(|segment| segment != "..");
    if confined {
        Ok(path)
    } else {
        Err(Error::InvalidArgument {
            name: "source".into(),
            detail: format!("{path:?} is not a path inside the repository"),
        })
    }
}

/// The first commit in log order, so the newest, dated at or before `at`.
fn commit_at<'a>(entries: &'a [LogEntry], at: &AsOf) -> Result<Option<&'a str>> {
    for entry in entries {
        let seconds = parse_as_of(&entry.date).ok_or_else(|| {
            Error::Endpoint(format!(
                "name: {LOG_IRI} dated commit {} {:?}, which is not an RFC 3339 instant",
                entry.hash, entry.date
            ))
        })?;
        if seconds.seconds <= at.seconds {
            return Ok(Some(&entry.hash));
        }
    }
    Ok(None)
}

/// What the file is. Git records no media type, so this comes from the name, the
/// same answers `ikigai-fs` 0.2.4 gives for these, so a document's type does not
/// change with the date asked about. A name it does not know keeps whatever type the
/// read answered with.
fn media_type(path: &str, file: &Representation) -> ReprType {
    let known = match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("ttl") => Some("text/turtle"),
        Some("nt") => Some("application/n-triples"),
        Some("nq") => Some("application/n-quads"),
        Some("n3") => Some("text/n3"),
        Some("trig") => Some("application/trig"),
        Some("rdf") => Some("application/rdf+xml"),
        Some("xml") => Some("text/xml"),
        Some("json") => Some("application/json"),
        Some("jsonld") => Some("application/ld+json"),
        Some("txt") => Some("text/plain"),
        Some("md") => Some("text/markdown"),
        _ => None,
    };
    known.map_or_else(|| file.repr_type.clone(), ReprType::new)
}

// --- dates ------------------------------------------------------------------

/// `YYYY-MM-DD` (the last second of that day in UTC) or an RFC 3339 instant.
fn parse_as_of(text: &str) -> Option<AsOf> {
    let seconds = if text.len() == 10 {
        epoch_days(text)? * 86_400 + 86_399
    } else {
        parse_instant(text)?
    };
    let utc = format_utc(seconds)?;
    Some(AsOf { seconds, utc })
}

fn parse_instant(text: &str) -> Option<i64> {
    if !text.is_ascii() || text.len() < 20 || !matches!(&text[10..11], "T" | "t") {
        return None;
    }
    let days = epoch_days(&text[..10])?;
    let bytes = text.as_bytes();
    if bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let (hour, minute, second) = (
        two_digits(&text[11..13])?,
        two_digits(&text[14..16])?,
        two_digits(&text[17..19])?,
    );
    // 60 is a leap second, which RFC 3339 allows.
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    // Fractions of a second are read and dropped: git dates have none.
    let mut rest = &text[19..];
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        rest = &fraction[digits..];
    }
    let offset = match rest {
        "Z" | "z" => 0,
        _ => {
            let sign = match rest.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            if rest.len() != 6 || rest.as_bytes()[3] != b':' {
                return None;
            }
            let (hours, minutes) = (two_digits(&rest[1..3])?, two_digits(&rest[4..6])?);
            if hours > 23 || minutes > 59 {
                return None;
            }
            sign * (hours * 3_600 + minutes * 60)
        }
    };
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second - offset)
}

fn two_digits(text: &str) -> Option<i64> {
    text.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| text.parse().ok())
        .flatten()
}

/// Days since 1970-01-01 of a `YYYY-MM-DD`, refusing a day the month does not have.
fn epoch_days(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if !text.is_ascii() || bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if !text[..4].bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year: i64 = text[..4].parse().ok()?;
    let (month, day) = (two_digits(&text[5..7])?, two_digits(&text[8..10])?);
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let longest = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if day < 1 || day > longest {
        return None;
    }
    // Days from civil, counting years from March so the leap day falls last.
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = shifted.div_euclid(400);
    let year_of_era = shifted.rem_euclid(400);
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

/// An instant as `YYYY-MM-DDTHH:MM:SSZ`, or `None` outside the years 0001 to 9999,
/// which an offset can reach from a date at either end.
fn format_utc(seconds: i64) -> Option<String> {
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    // Civil from days, the inverse of `epoch_days`.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (1..=9999).contains(&year).then(|| {
        format!(
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
            second_of_day / 3_600,
            second_of_day / 60 % 60,
            second_of_day % 60
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(text: &str) -> Option<String> {
        parse_as_of(text).map(|at| at.utc)
    }

    #[test]
    fn a_date_means_the_end_of_that_day_in_utc() {
        assert_eq!(utc("2026-03-10").as_deref(), Some("2026-03-10T23:59:59Z"));
    }

    #[test]
    fn an_instant_is_normalized_to_utc() {
        for (given, want) in [
            ("2026-03-10T12:00:00Z", "2026-03-10T12:00:00Z"),
            ("2026-03-10t12:00:00z", "2026-03-10T12:00:00Z"),
            ("2026-03-10T13:00:00+01:00", "2026-03-10T12:00:00Z"),
            ("2026-03-10T07:30:00-04:30", "2026-03-10T12:00:00Z"),
            ("2026-03-10T12:00:00.75Z", "2026-03-10T12:00:00Z"),
            // Across a month, a leap day and a year.
            ("2024-03-01T00:30:00+01:00", "2024-02-29T23:30:00Z"),
            ("2026-01-01T00:00:00+00:01", "2025-12-31T23:59:00Z"),
            ("1969-12-31T23:59:59Z", "1969-12-31T23:59:59Z"),
        ] {
            assert_eq!(utc(given).as_deref(), Some(want), "{given}");
        }
    }

    #[test]
    fn what_is_not_a_date_is_refused() {
        for bad in [
            "",
            "March",
            "2026-3-10",
            "2026-13-01",
            "2026-00-10",
            "2026-02-29",
            "2026-04-31",
            "2026-03-00",
            "2026-03-10T12:00:00",
            "2026-03-10 12:00:00Z",
            "2026-03-10T24:00:00Z",
            "2026-03-10T12:60:00Z",
            "2026-03-10T12:00:00+0100",
            "2026-03-10T12:00:00+24:00",
            "2026-03-10T12:00:00.Z",
            "+026-03-10",
            "٢٠٢٦-03-10",
            "0000-01-01",
            "9999-12-31T23:59:59-01:00",
        ] {
            assert_eq!(utc(bad), None, "{bad:?}");
        }
    }

    /// Every path that could leave the repository, or that a tool could read as a
    /// flag, stops here rather than at whatever runs git.
    #[test]
    fn a_source_outside_the_repository_is_refused() {
        for source in [
            "urn:file:",
            "urn:file:../secrets",
            "urn:file:vocab/../../secrets",
            "urn:file:/etc/passwd",
            "urn:file:--output=x",
        ] {
            assert!(
                matches!(
                    repo_path(source, "registry.json"),
                    Err(Error::InvalidArgument { .. })
                ),
                "{source}"
            );
        }
        assert!(matches!(
            repo_path("https://origin.example/ns", "registry.json"),
            Err(Error::Unsupported { .. })
        ));
        assert_eq!(
            repo_path("urn:file:vocab/core.ttl", "registry.json").unwrap(),
            "vocab/core.ttl"
        );
        assert_eq!(
            repo_path(REGISTRY_IRI, "registry.json").unwrap(),
            "registry.json"
        );
    }

    /// Log order decides, so a commit dated after an older-looking one above it
    /// never wins: the first one old enough is the answer.
    #[test]
    fn the_newest_commit_at_or_before_the_instant_is_chosen() {
        let entry = |hash: &str, date: &str| LogEntry {
            hash: hash.into(),
            date: date.into(),
        };
        let log = [
            entry("c", "2026-05-10T12:00:00+00:00"),
            entry("b", "2026-03-10T14:00:00+02:00"),
            entry("a", "2026-01-10T12:00:00+00:00"),
        ];
        let at = |text| parse_as_of(text).unwrap();
        let chosen = |text| commit_at(&log, &at(text)).unwrap().map(str::to_string);
        assert_eq!(chosen("2026-06-01").as_deref(), Some("c"));
        assert_eq!(chosen("2026-03-10T12:00:00Z").as_deref(), Some("b"));
        assert_eq!(chosen("2026-03-10T11:59:59Z").as_deref(), Some("a"));
        assert_eq!(chosen("2026-01-09"), None);

        let unreadable = [entry("x", "yesterday")];
        assert!(commit_at(&unreadable, &at("2026-01-09")).is_err());
    }
}
