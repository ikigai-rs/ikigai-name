//! Names that are IRIs: a claim on a `urn:` or `tag:` prefix, and the one rule that
//! makes two spellings of a urnid name the same name.
//!
//! A path claim (`resmud`) holds a run of `/`-separated segments. An IRI claim
//! (`urn:urnid:acme:`, `tag:urnid.org,2026:acme:`) holds a run of `:`-separated ones,
//! after the IRI is put in its canonical form. The claim rule is the same for both:
//! two claims of one kind overlap when one's segments begin the other's, and a path
//! claim never overlaps an IRI claim, because a path is never an IRI.
//!
//! ## The urnid equivalence
//!
//! `urn:urnid:{owner}:{local}` and `tag:urnid.org,2026:{owner}:{local}` name one
//! thing. [`canonical`] writes both as the `urn:` form, so a claim on either holds
//! both, and claiming the other is an overlap. The owner label compares
//! case-insensitively (it is lowercased); the local part is case-sensitive, apart
//! from percent-encoding normalization (hex digits uppercased, and an encoded
//! unreserved character decoded), so `%7e`, `%7E` and `~` are one name and `A` and `a`
//! are two.

/// The tagging entity whose `tag:` names are urnid names.
pub const URNID_TAG: &str = "tag:urnid.org,2026:";

/// The `urn:` form of a urnid name, which [`canonical`] writes both forms as.
pub const URNID_URN: &str = "urn:urnid:";

/// Whether `name` is an IRI a claim can hold (a `urn:` or a `tag:`), rather than a path.
///
/// ```
/// use ikigai_name::iri::is_iri;
///
/// assert!(is_iri("urn:urnid:acme:thing"));
/// assert!(is_iri("TAG:urnid.org,2026:acme:thing"));
/// assert!(!is_iri("resmud/core"));
/// ```
pub fn is_iri(name: &str) -> bool {
    let name = name.trim();
    ["urn:", "tag:"].iter().any(|scheme| {
        name.get(..scheme.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(scheme))
    })
}

/// `name` in its canonical form: the scheme lowercased; for a `urn:` its namespace
/// identifier lowercased, and for a `tag:` its tagging entity; a urnid `tag:` written as
/// its `urn:`, with the owner lowercased; and percent-encoding normalized throughout.
///
/// ```
/// use ikigai_name::iri::canonical;
///
/// assert_eq!(canonical("tag:urnid.org,2026:ACME:Thing"), "urn:urnid:acme:Thing");
/// assert_eq!(canonical("URN:URNID:Acme:%7ex%2f"), "urn:urnid:acme:~x%2F");
/// ```
pub fn canonical(name: &str) -> String {
    let name = name.trim();
    let Some((scheme, rest)) = name.split_once(':') else {
        return normalize_percent(name);
    };
    let scheme = scheme.to_ascii_lowercase();
    let (head, specific) = rest.split_once(':').unwrap_or((rest, ""));
    let has_specific = rest.contains(':');
    let head = normalize_percent(head).to_ascii_lowercase();
    let mut out = match (scheme.as_str(), head.as_str()) {
        ("tag", "urnid.org,2026") | ("urn", "urnid") => {
            let (owner, local) = specific.split_once(':').unwrap_or((specific, ""));
            let mut out = format!(
                "{URNID_URN}{}",
                normalize_percent(owner).to_ascii_lowercase()
            );
            if specific.contains(':') {
                out.push(':');
                out.push_str(&normalize_percent(local));
            }
            return out;
        }
        _ => format!("{scheme}:{head}"),
    };
    if has_specific {
        out.push(':');
        out.push_str(&normalize_percent(specific));
    }
    out
}

/// The segments a claim or a name is compared by, and whether they are an IRI's.
///
/// A path's are its `/`-separated segments; an IRI's are the `:`-separated segments of
/// its [`canonical`] form. Empty segments are dropped either way, so `acme/` and
/// `urn:urnid:acme:` are claims of one segment more than their scheme.
pub(crate) fn name_segments(name: &str) -> (bool, Vec<String>) {
    if is_iri(name) {
        let canonical = canonical(name);
        let parts = canonical
            .split(':')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        (true, parts)
    } else {
        let parts = crate::registry::segments(name)
            .into_iter()
            .map(str::to_string)
            .collect();
        (false, parts)
    }
}

/// The one spelling a name is counted and compared under: a path's segments joined by
/// `/`, or an IRI's canonical form.
pub(crate) fn name_key(name: &str) -> String {
    if is_iri(name) {
        canonical(name)
    } else {
        crate::registry::segments(name).join("/")
    }
}

/// Uppercase every `%xx`, and decode one that encodes an unreserved character. A `%`
/// that does not begin a valid escape is left as it is.
fn normalize_percent(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let (hi, lo) = (bytes[i + 1], bytes[i + 2]);
            if let (Some(h), Some(l)) = (hex(hi), hex(lo)) {
                let byte = h * 16 + l;
                if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                    out.push(byte);
                } else {
                    out.push(b'%');
                    out.push(hi.to_ascii_uppercase());
                    out.push(lo.to_ascii_uppercase());
                }
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    // Only ASCII was rewritten, and only whole escapes, so this is still UTF-8.
    String::from_utf8(out).unwrap_or_else(|_| text.to_string())
}

fn hex(b: u8) -> Option<u8> {
    char::from(b).to_digit(16).map(|d| d as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One rule, both ways: each form canonicalizes to the same name as the other.
    #[test]
    fn the_urn_and_the_tag_are_one_name() {
        for (urn, tag) in [
            ("urn:urnid:acme:thing", "tag:urnid.org,2026:acme:thing"),
            ("urn:urnid:ACME:thing", "tag:urnid.org,2026:acme:thing"),
            ("URN:UrnId:acme:a:b", "TAG:URNID.ORG,2026:Acme:a:b"),
            ("urn:urnid:acme:%7Ex", "tag:urnid.org,2026:acme:~x"),
            ("urn:urnid:acme:", "tag:urnid.org,2026:acme:"),
        ] {
            assert_eq!(canonical(urn), canonical(tag), "{urn} ⇄ {tag}");
            assert!(canonical(urn).starts_with(URNID_URN), "{urn}");
        }
    }

    #[test]
    fn the_owner_folds_case_and_the_local_part_does_not() {
        assert_eq!(canonical("urn:urnid:AcMe:x"), "urn:urnid:acme:x");
        assert_ne!(
            canonical("urn:urnid:acme:Thing"),
            canonical("urn:urnid:acme:thing")
        );
        assert_ne!(
            canonical("tag:urnid.org,2026:acme:Thing"),
            canonical("urn:urnid:acme:thing")
        );
    }

    #[test]
    fn percent_encoding_is_normalized_and_nothing_else_is_decoded() {
        assert_eq!(canonical("urn:urnid:acme:%7e"), "urn:urnid:acme:~");
        assert_eq!(canonical("urn:urnid:acme:%2f"), "urn:urnid:acme:%2F");
        assert_eq!(canonical("urn:urnid:acme:%41"), "urn:urnid:acme:A");
        assert_eq!(canonical("urn:urnid:acme:%zz%"), "urn:urnid:acme:%zz%");
        assert_eq!(canonical("urn:urnid:acme:100%"), "urn:urnid:acme:100%");
    }

    /// A tag of another authority, or another year, is another name.
    #[test]
    fn only_urnid_dot_org_2026_is_the_urnid_tag() {
        assert_eq!(
            canonical("tag:URNID.org,2027:acme:x"),
            "tag:urnid.org,2027:acme:x"
        );
        assert_eq!(
            canonical("tag:example.com,2026:acme:x"),
            "tag:example.com,2026:acme:x"
        );
        assert_eq!(canonical("urn:ISBN:0451450523"), "urn:isbn:0451450523");
    }

    #[test]
    fn segments_are_colon_separated_for_an_iri_and_slash_separated_for_a_path() {
        assert_eq!(
            name_segments("tag:urnid.org,2026:ACME:"),
            (true, vec!["urn".into(), "urnid".into(), "acme".into()])
        );
        assert_eq!(
            name_segments("/resmud//core/"),
            (false, vec!["resmud".into(), "core".into()])
        );
        assert_eq!(name_key("resmud//core"), "resmud/core");
        assert_eq!(name_key("tag:urnid.org,2026:A:b"), "urn:urnid:a:b");
    }
}
