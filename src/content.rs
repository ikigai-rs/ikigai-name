//! Content namespaces: names that resolve to IPFS content identifiers.
//!
//! A `content` namespace holds an explicit map from canonical full names to entries,
//! each a required CID and an optional declared media type. A name the map does not hold
//! is a miss within the claim: there is no suffix mapping, directory traversal, mutable
//! IPNS target or wildcard fallback. The design is `docs/design/ipfs-resolution.md`.
//!
//! A CID is parsed structurally by the [`cid`] crate (the multiformats project's own
//! implementation), and only one that names a raw block (`raw`, `0x55`) or a UnixFS root
//! (`dag-pb`, `0x70`) is held. CIDv0 input is accepted; every answer writes the canonical
//! lowercase base32 CIDv1. Nothing here performs network I/O: an optional gateway is a
//! retrieval hint, taken from the registry's own `gateway` and never fetched.

use crate::iri::{name_key, name_segments};
use cid::Cid;
use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The multicodec of a raw block.
const RAW: u64 = 0x55;
/// The multicodec of a `dag-pb` node, which a UnixFS file root is.
const DAG_PB: u64 = 0x70;

/// What one name of a content namespace resolves to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentEntry {
    /// The content identifier, as written: CIDv0 or CIDv1, in any multibase.
    pub cid: String,
    /// The media type the owner declares for the content. Metadata, never checked
    /// against the bytes.
    #[serde(
        rename = "media-type",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub media_type: Option<String>,
}

/// A content namespace's entries, in the order written. Kept as a list rather than a map
/// so that a name written twice, literally or only after canonicalization, is refused
/// rather than silently answered by whichever came last.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entries(pub Vec<(String, ContentEntry)>);

impl Serialize for Entries {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (name, entry) in &self.0 {
            map.serialize_entry(name, entry)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Entries {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Ordered;
        impl<'de> Visitor<'de> for Ordered {
            type Value = Entries;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a map from names to entries, each with a cid")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Entries, A::Error> {
                let mut entries = Vec::new();
                while let Some(pair) = map.next_entry::<String, ContentEntry>()? {
                    entries.push(pair);
                }
                Ok(Entries(entries))
            }
        }
        deserializer.deserialize_map(Ordered)
    }
}

impl Entries {
    /// The entry for `name`, compared in its canonical form.
    pub fn get(&self, name: &str) -> Option<&ContentEntry> {
        let wanted = name_key(name);
        self.0
            .iter()
            .find(|(key, _)| name_key(key) == wanted)
            .map(|(_, entry)| entry)
    }

    /// Whether a namespace claiming `prefix` may hold these entries: every name lies
    /// under the prefix, no two are one name once canonical, every CID parses as one
    /// this strategy serves, and every media type is one. `Err` is a clause saying why.
    pub fn check(&self, prefix: &str) -> Result<(), String> {
        let (claimed_iri, claimed) = name_segments(prefix);
        let mut seen: Vec<String> = Vec::with_capacity(self.0.len());
        for (name, entry) in &self.0 {
            let (iri, wanted) = name_segments(name);
            if iri != claimed_iri
                || wanted.len() < claimed.len()
                || wanted[..claimed.len()] != claimed[..]
            {
                return Err(format!("its entry {name:?} is not a name under {prefix:?}"));
            }
            let key = name_key(name);
            if let Some(earlier) = seen.iter().position(|k| *k == key) {
                return Err(format!(
                    "its entries {:?} and {name:?} are one name, {key:?}",
                    self.0[earlier].0
                ));
            }
            seen.push(key);
            canonical_cid(&entry.cid).map_err(|why| format!("its entry {name:?} {why}"))?;
            if let Some(media_type) = &entry.media_type {
                if !is_media_type(media_type) {
                    return Err(format!(
                        "its entry {name:?} declares the media type {media_type:?}, which \
                         is not a type/subtype such as text/plain"
                    ));
                }
            }
        }
        Ok(())
    }
}

/// The canonical form of `written`: lowercase base32 CIDv1. `Err` is a clause saying why
/// it is not a CID this strategy serves.
pub fn canonical_cid(written: &str) -> Result<String, String> {
    let cid = Cid::try_from(written.trim())
        .map_err(|e| format!("has the cid {written:?}, which is not a CID: {e}"))?;
    if cid.codec() != RAW && cid.codec() != DAG_PB {
        return Err(format!(
            "has the cid {written:?}, whose codec 0x{:x} is neither raw (0x55) nor a \
             UnixFS dag-pb root (0x70)",
            cid.codec()
        ));
    }
    // A CIDv0 is always dag-pb with sha2-256, so it always converts.
    let v1 = cid
        .into_v1()
        .map_err(|e| format!("has the cid {written:?}, which has no CIDv1: {e}"))?;
    v1.to_string_of_base(cid::multibase::Base::Base32Lower)
        .map_err(|e| format!("has the cid {written:?}, which cannot be written: {e}"))
}

/// Whether `text` is a media type: `type/subtype`, each an RFC 9110 token, with any
/// `;name=value` parameters, each value a token or a quoted string.
pub fn is_media_type(text: &str) -> bool {
    fn token(t: &str) -> bool {
        !t.is_empty()
            && t.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
    }
    let mut parts = text.split(';');
    let Some((kind, sub)) = parts.next().and_then(|head| head.trim().split_once('/')) else {
        return false;
    };
    token(kind)
        && token(sub)
        && parts.all(|param| {
            let Some((name, value)) = param.trim().split_once('=') else {
                return false;
            };
            let quoted = value.len() >= 2
                && value.starts_with('"')
                && value.ends_with('"')
                && !value[1..value.len() - 1].contains(['"', '\\'])
                && value
                    .bytes()
                    .all(|b| b == b'\t' || (b' '..=b'~').contains(&b));
            token(name) && (token(value) || quoted)
        })
}

/// Whether `gateway` may be a registry's IPFS gateway: an `https:` address with a host,
/// and no query or fragment, since a CID's path is appended to it.
pub fn gateway_refusal(gateway: &str) -> Option<String> {
    let https = gateway
        .get(..8)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"));
    if !https || crate::tenant::iri_host(gateway).is_none() || gateway.contains(['?', '#']) {
        return Some(format!(
            "gateway {gateway:?} is not an https: address with a host and no query or fragment"
        ));
    }
    None
}

/// Where `gateway` serves the content `cid` names (already canonical), by its path form.
pub fn gateway_url(gateway: &str, cid: &str) -> String {
    format!("{}/ipfs/{cid}", gateway.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CIDv0 and CIDv1 of the empty UnixFS directory are one CID.
    const V0: &str = "QmUNLLsPACCz1vLxQVkXqqLX5R1X345qqfHbsf67hvA3Nn";
    const V1: &str = "bafybeiczsscdsbs7ffqz55asqdf3smv6klcw3gofszvwlyarci47bgf354";
    const RAW_V1: &str = "bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy";

    #[test]
    fn a_cidv0_is_answered_as_its_base32_cidv1() {
        assert_eq!(canonical_cid(V0).as_deref(), Ok(V1));
        assert_eq!(canonical_cid(V1).as_deref(), Ok(V1));
        assert_eq!(canonical_cid(RAW_V1).as_deref(), Ok(RAW_V1));
        // The same raw CID in base58btc is written back in base32.
        let b58 = Cid::try_from(RAW_V1)
            .unwrap()
            .to_string_of_base(cid::multibase::Base::Base58Btc)
            .unwrap();
        assert_ne!(b58, RAW_V1);
        assert_eq!(canonical_cid(&b58).as_deref(), Ok(RAW_V1));
    }

    #[test]
    fn what_is_not_a_raw_or_unixfs_cid_is_refused() {
        assert!(canonical_cid("not-a-cid").is_err());
        assert!(canonical_cid("").is_err());
        // A truncated CID does not parse.
        assert!(canonical_cid(&RAW_V1[..20]).is_err());
        // dag-cbor (0x71) is a CID, but not one this strategy serves.
        let cbor = Cid::new_v1(0x71, *Cid::try_from(RAW_V1).unwrap().hash()).to_string();
        let why = canonical_cid(&cbor).unwrap_err();
        assert!(why.contains("0x71"), "{why}");
    }

    #[test]
    fn media_types_are_checked_as_metadata() {
        for good in [
            "text/plain",
            "image/png",
            "application/ld+json",
            "text/plain; charset=utf-8",
            "text/plain;format=\"flowed\"",
        ] {
            assert!(is_media_type(good), "{good}");
        }
        for bad in [
            "",
            "text",
            "text/",
            "/plain",
            "text/plain/x",
            "text plain",
            "text/plain;charset",
            "text/pl\u{e9}in",
        ] {
            assert!(!is_media_type(bad), "{bad}");
        }
    }

    #[test]
    fn a_gateway_is_an_https_address() {
        assert_eq!(gateway_refusal("https://dweb.link/"), None);
        for bad in [
            "http://dweb.link/",
            "ipfs://x",
            "https://",
            "https://dweb.link/?x",
            "dweb.link",
        ] {
            assert!(gateway_refusal(bad).is_some(), "{bad}");
        }
        assert_eq!(
            gateway_url("https://dweb.link/", V1),
            format!("https://dweb.link/ipfs/{V1}")
        );
    }
}
