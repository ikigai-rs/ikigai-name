//! Rewrite rules: how a `redirect` namespace reshapes a URL scheme, as data.
//!
//! A rule pairs a `match`, a path template relative to the namespace's prefix, with
//! a `target`, a URL template using what the match captured. The first rule that
//! matches answers; with none, the namespace's own target does.
//!
//! ## Two syntaxes, and whose each is
//!
//! A **target** is an RFC 6570 Level 1 template, parsed by
//! [`ikigai_core::UriTemplate`], so this crate holds no URI-template parser. Its
//! encoding comes from the same type's `expand_encoded`.
//!
//! A **match** is a deliberately small syntax of its own, because RFC 6570 defines
//! expansion and not matching, and core's `UriTemplate` matches by text, so one of
//! its variables runs across a `/`. Here a match is `/`-separated segments, each
//! literal text or one whole capture: `{name}` takes one segment and `{name*}`, in
//! the last place only, takes every segment left, at least one. A target writes
//! either kind as `{name}`.
//!
//! ## A capture cannot change where a target points
//!
//! A captured value comes from whoever made the request, so three things hold:
//!
//! - Every byte outside the RFC 3986 unreserved set is percent-encoded, `%` included,
//!   so `%2F` arrives as the text `%252F` and never as a separator. The `/` between
//!   the segments of a `{name*}` stays a `/`: those segments were separate in the
//!   request, and the rule asked for all of them.
//! - A `.` or `..` segment **matches no capture**. Encoding cannot defuse one: `.` is
//!   unreserved, and URL parsers read `%2E%2E` as `..` anyway. The rule simply does
//!   not match, and resolution moves on to the next.
//! - A target must spell its scheme and host literally, ending them with `/`, `?` or
//!   `#` before its first capture, or the registry is refused. Encoding leaves `.`
//!   alone, so `https://{host}/` would let a request name any host.

use crate::registry::segments;
use ikigai_core::{Bindings, UriTemplate};
use serde::de::{Deserialize, Deserializer, Error as _};
use serde::{Serialize, Serializer};

/// The status a redirect answers with: 301, 302, 303, 307 or 308, and nothing else.
///
/// Holding one is proof of a valid code, because parsing is the only way to get one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RedirectStatus(u16);

/// The list every refusal of a status recites.
pub(crate) const STATUS_CHOICES: &str = "301, 302, 303, 307 or 308";

impl RedirectStatus {
    /// The status for `code`, if it is one a redirect may answer with.
    pub fn new(code: u16) -> Option<RedirectStatus> {
        matches!(code, 301 | 302 | 303 | 307 | 308).then_some(RedirectStatus(code))
    }

    /// The numeric code.
    pub fn code(self) -> u16 {
        self.0
    }
}

/// 302: a logical name usually points at something that will move, and a permanent
/// code would make clients remember the target and skip the name.
impl Default for RedirectStatus {
    fn default() -> Self {
        RedirectStatus(302)
    }
}

impl std::fmt::Display for RedirectStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for RedirectStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u16(self.0)
    }
}

impl<'de> Deserialize<'de> for RedirectStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        status_from_json(&value).ok_or_else(|| D::Error::custom(status_refusal(&value)))
    }
}

/// The status a JSON value names. Only a JSON number names one, so `"301"` does not.
pub(crate) fn status_from_json(value: &serde_json::Value) -> Option<RedirectStatus> {
    let code = u16::try_from(value.as_u64()?).ok()?;
    RedirectStatus::new(code)
}

/// Why a JSON value is not a status, in the words every refusal uses.
pub(crate) fn status_refusal(value: &serde_json::Value) -> String {
    format!("its status {value} is not one of {STATUS_CHOICES}")
}

/// One rewrite rule of a `redirect` namespace. See the [module documentation](self).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// The path template, relative to the namespace's prefix: `old/{x}/{y}`.
    #[serde(rename = "match")]
    pub pattern: String,
    /// The URL template its captures fill: `https://new.example/{y}/{x}`.
    pub target: String,
    /// The status for this rule; the namespace's otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<RedirectStatus>,
}

/// One segment of a parsed match.
enum Piece<'a> {
    Literal(&'a str),
    One(&'a str),
    Rest(&'a str),
}

/// Whether a capture's name is one a target's template can also write.
fn is_capture_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A match's segments, or the sentence saying what is wrong with them.
fn parse_match(pattern: &str) -> Result<Vec<Piece<'_>>, String> {
    if pattern.chars().any(char::is_control) {
        return Err("its match contains a tab, a newline or another control character".into());
    }
    let parts = segments(pattern);
    if parts.is_empty() {
        return Err("its match is empty, so it could never name a path".into());
    }
    let mut pieces = Vec::with_capacity(parts.len());
    let mut names: Vec<&str> = Vec::new();
    for (place, part) in parts.iter().copied().enumerate() {
        if !part.contains(['{', '}']) {
            pieces.push(Piece::Literal(part));
            continue;
        }
        let inner = part
            .strip_prefix('{')
            .and_then(|rest| rest.strip_suffix('}'))
            .ok_or_else(|| {
                format!("its match has {part:?}, but a capture must be a whole segment")
            })?;
        let (name, rest) = match inner.strip_suffix('*') {
            Some(name) => (name, true),
            None => (inner, false),
        };
        if !is_capture_name(name) {
            return Err(format!(
                "its match has {part:?}, but a capture's name is letters, digits and underscores"
            ));
        }
        if names.contains(&name) {
            return Err(format!("its match captures {{{name}}} twice"));
        }
        names.push(name);
        if rest && place + 1 != parts.len() {
            return Err(format!(
                "its match has {part:?} before the end, but that capture takes the rest of the \
                 path"
            ));
        }
        pieces.push(if rest {
            Piece::Rest(name)
        } else {
            Piece::One(name)
        });
    }
    Ok(pieces)
}

/// Refuse a target whose host a capture could reach, or that is not a URL at all.
///
/// Only the text before the first capture counts, because that is the part a request
/// cannot touch: it must hold a scheme and a host, and a `/`, `?` or `#` ending the
/// host, or `https://new.example{x}` would let `.evil.example` extend the host.
fn check_target_head(target: &str) -> Result<(), String> {
    let invalid = || {
        format!(
            "its target {target:?} must begin with a scheme and a host, such as \
             https://new.example/"
        )
    };
    let capture = target.find('{');
    let head = &target[..capture.unwrap_or(target.len())];
    let (scheme, after) = head.split_once("://").ok_or_else(invalid)?;
    let well_formed = scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
    if !well_formed {
        return Err(invalid());
    }
    match after.find(['/', '?', '#']) {
        Some(0) => Err(invalid()),
        Some(_) => Ok(()),
        None if capture.is_some() => Err(format!(
            "its target {target:?} has a capture inside the host: end the host with a / first, \
             so a request cannot change it"
        )),
        None if after.is_empty() => Err(invalid()),
        None => Ok(()),
    }
}

/// A `}` outside a capture, which core's parser reads as literal text. It is a typo,
/// and a URL has no use for a bare one.
fn has_stray_close(target: &str) -> bool {
    let mut rest = target;
    while let Some(open) = rest.find('{') {
        if rest[..open].contains('}') {
            return true;
        }
        match rest[open..].find('}') {
            Some(close) => rest = &rest[open + close + 1..],
            None => return false,
        }
    }
    rest.contains('}')
}

impl Rule {
    /// Whether the registry may hold this rule, or the sentence saying why not.
    pub(crate) fn check(&self) -> Result<(), String> {
        let pieces = parse_match(&self.pattern)?;
        if self
            .target
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(format!(
                "its target {:?} contains a space or a control character",
                self.target
            ));
        }
        if let Some(open) = self.target.find("*}") {
            let start = self.target[..open].rfind('{').map_or(0, |at| at + 1);
            let name = &self.target[start..open];
            return Err(format!(
                "its target writes {{{name}*}}: write {{{name}}}, since the match says how much \
                 it takes"
            ));
        }
        let template = UriTemplate::parse(self.target.as_str())
            .map_err(|e| format!("its target cannot be read: {e}"))?;
        if has_stray_close(&self.target) {
            return Err(format!(
                "its target {:?} has a }} with no {{ before it",
                self.target
            ));
        }
        check_target_head(&self.target)?;
        for used in template.variables() {
            let defined = pieces.iter().any(|piece| match piece {
                Piece::One(name) | Piece::Rest(name) => *name == used,
                Piece::Literal(_) => false,
            });
            if !defined {
                return Err(format!(
                    "its target uses {{{used}}}, which its match does not capture"
                ));
            }
        }
        Ok(())
    }

    /// The target for a path below the prefix, if this rule matches it.
    ///
    /// A rule the registry would refuse matches nothing, for a registry built some
    /// way other than `from_json`.
    pub(crate) fn rewrite(&self, path: &[&str]) -> Option<String> {
        self.check().ok()?;
        let pieces = parse_match(&self.pattern).ok()?;
        let captures = capture(&pieces, path)?;
        UriTemplate::parse(self.target.as_str())
            .ok()?
            .expand(&captures)
    }
}

/// The encoded captures of `path` under a match, or `None` if it does not match.
fn capture(pieces: &[Piece<'_>], path: &[&str]) -> Option<Bindings> {
    let mut bindings = Bindings::new();
    let mut left = path;
    for piece in pieces {
        match piece {
            Piece::Literal(text) => {
                let (first, rest) = left.split_first()?;
                if first != text {
                    return None;
                }
                left = rest;
            }
            Piece::One(name) => {
                let (first, rest) = left.split_first()?;
                bindings.insert((*name).to_string(), encode_segment(first)?);
                left = rest;
            }
            Piece::Rest(name) => {
                if left.is_empty() {
                    return None;
                }
                let encoded = left
                    .iter()
                    .map(|segment| encode_segment(segment))
                    .collect::<Option<Vec<_>>>()?;
                bindings.insert((*name).to_string(), encoded.join("/"));
                left = &[];
            }
        }
    }
    left.is_empty().then_some(bindings)
}

/// One captured segment, encoded by core's RFC 6570 expansion, or `None` for the dot
/// segments encoding cannot defuse.
fn encode_segment(segment: &str) -> Option<String> {
    if matches!(segment, "." | "..") {
        return None;
    }
    let mut value = Bindings::new();
    value.insert("v".to_string(), segment.to_string());
    UriTemplate::parse("{v}").ok()?.expand_encoded(&value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(pattern: &str, target: &str) -> Rule {
        Rule {
            pattern: pattern.into(),
            target: target.into(),
            status: None,
        }
    }

    fn rewrite(pattern: &str, target: &str, path: &str) -> Option<String> {
        let rule = rule(pattern, target);
        rule.check().expect("a valid rule");
        rule.rewrite(&segments(path))
    }

    #[test]
    fn captures_fill_the_target_in_any_order() {
        assert_eq!(
            rewrite("old/{x}/{y}", "https://new.example/{y}/{x}", "old/a/b").as_deref(),
            Some("https://new.example/b/a")
        );
    }

    #[test]
    fn a_capture_takes_exactly_one_segment() {
        let matches = |path| rewrite("old/{x}", "https://new.example/{x}", path);
        assert_eq!(matches("old"), None);
        assert_eq!(matches("old/a/b"), None);
        assert_eq!(matches("other/a"), None);
        assert_eq!(matches("old/a").as_deref(), Some("https://new.example/a"));
    }

    #[test]
    fn a_rest_capture_takes_the_remainder_and_at_least_one_segment() {
        let matches = |path| rewrite("docs/{rest*}", "https://new.example/d/{rest}", path);
        assert_eq!(matches("docs"), None);
        assert_eq!(
            matches("docs/a").as_deref(),
            Some("https://new.example/d/a")
        );
        assert_eq!(
            matches("docs/a/b/c").as_deref(),
            Some("https://new.example/d/a/b/c")
        );
    }

    #[test]
    fn a_capture_may_be_used_twice_or_not_at_all() {
        assert_eq!(
            rewrite("{a}/{b}", "https://new.example/{a}/{a}", "x/y").as_deref(),
            Some("https://new.example/x/x")
        );
    }

    /// The injection cases the item names, through both kinds of capture and in the
    /// query, where a stray delimiter would matter just as much.
    #[test]
    fn a_capture_cannot_change_the_host_scheme_or_segments() {
        const ONE: (&str, &str) = ("old/{x}", "https://new.example/p/{x}");
        const REST: (&str, &str) = ("old/{x*}", "https://new.example/p/{x}");

        // Dot segments match nothing, whatever kind of capture would take them.
        for (pattern, target) in [ONE, REST] {
            assert_eq!(rewrite(pattern, target, "old/.."), None);
            assert_eq!(rewrite(pattern, target, "old/."), None);
        }
        assert_eq!(rewrite(REST.0, REST.1, "old/a/../../b"), None);

        for (path, one, rest) in [
            // `%` is encoded, so an encoded slash stays text.
            ("old/%2F", "%252F", "%252F"),
            ("old/a%2Fb", "a%252Fb", "a%252Fb"),
            // An encoded dot segment stays text as well.
            ("old/%2E%2E", "%252E%252E", "%252E%252E"),
            // Doubled separators collapse, as they do for a prefix.
            ("old//evil.example", "evil.example", "evil.example"),
            // Delimiters are data.
            ("old/a?b=c#d", "a%3Fb%3Dc%23d", "a%3Fb%3Dc%23d"),
            ("old/a\\b", "a%5Cb", "a%5Cb"),
            ("old/é", "%C3%A9", "%C3%A9"),
        ] {
            assert_eq!(
                rewrite(ONE.0, ONE.1, path).as_deref(),
                Some(format!("https://new.example/p/{one}").as_str()),
                "{path}"
            );
            assert_eq!(
                rewrite(REST.0, REST.1, path).as_deref(),
                Some(format!("https://new.example/p/{rest}").as_str()),
                "{path}"
            );
        }

        // A whole URL is too many segments for one capture, and its colon is
        // encoded under a rest capture, so it stays under this host.
        let url = "old/https://evil.example/x";
        assert_eq!(rewrite(ONE.0, ONE.1, url), None);
        assert_eq!(
            rewrite(REST.0, REST.1, url).as_deref(),
            Some("https://new.example/p/https%3A/evil.example/x")
        );
        // Sent as one segment, the same text cannot make a separator at all.
        assert_eq!(
            rewrite(
                "{x}",
                "https://new.example/?to={x}",
                "https:%2F%2Fevil.example"
            )
            .as_deref(),
            Some("https://new.example/?to=https%3A%252F%252Fevil.example")
        );
    }

    #[test]
    fn every_bad_rule_is_refused_with_its_reason() {
        for (pattern, target, want) in [
            ("", "https://new.example/", "its match is empty"),
            ("///", "https://new.example/", "its match is empty"),
            ("a\tb", "https://new.example/", "control character"),
            ("{x", "https://new.example/", "whole segment"),
            ("x}", "https://new.example/", "whole segment"),
            ("{x}.html", "https://new.example/", "whole segment"),
            ("{}", "https://new.example/", "letters, digits"),
            ("{*}", "https://new.example/", "letters, digits"),
            ("{a-b}", "https://new.example/", "letters, digits"),
            ("{x}/{x}", "https://new.example/", "captures {x} twice"),
            ("{x}/{x*}", "https://new.example/", "captures {x} twice"),
            ("{x*}/y", "https://new.example/", "before the end"),
            ("{x}", "https://new.example/{y}", "uses {y}"),
            ("{x}", "https://new.example/{x", "cannot be read"),
            ("{x}", "https://new.example/{a b}", "space or a control"),
            ("{x}", "https://new.example/{x}{x}", "cannot be read"),
            ("{x*}", "https://new.example/{x*}", "write {x}"),
            ("{x}", "https://new.example/x}", "with no {"),
            ("{x}", "{x}", "scheme and a host"),
            ("{x}", "{x}://new.example/", "scheme and a host"),
            ("{x}", "new.example/{x}", "scheme and a host"),
            ("{x}", "/{x}", "scheme and a host"),
            ("{x}", "https:///{x}", "scheme and a host"),
            ("{x}", "https://{x}/", "inside the host"),
            ("{x}", "https://{x}.new.example/", "inside the host"),
            ("{x}", "https://new.example{x}", "inside the host"),
            ("{x}", "https://user:{x}@new.example/", "inside the host"),
        ] {
            let refusal = rule(pattern, target).check().expect_err(pattern);
            assert!(
                refusal.contains(want),
                "{pattern:?} -> {target:?}: want {want:?}, got {refusal:?}"
            );
        }
    }

    #[test]
    fn a_target_needs_no_capture_and_may_capture_into_a_query() {
        for target in [
            "https://new.example",
            "https://new.example/fixed",
            "https://new.example?to={x}",
            "https://new.example#{x}",
        ] {
            assert_eq!(rule("{x}", target).check(), Ok(()), "{target}");
        }
    }

    #[test]
    fn only_a_listed_code_is_a_status() {
        for code in [301, 302, 303, 307, 308] {
            assert_eq!(
                RedirectStatus::new(code).map(RedirectStatus::code),
                Some(code)
            );
        }
        for code in [0, 200, 300, 304, 305, 306, 309, 404, 410] {
            assert_eq!(RedirectStatus::new(code), None, "{code}");
        }
        assert_eq!(RedirectStatus::default().code(), 302);
    }
}
