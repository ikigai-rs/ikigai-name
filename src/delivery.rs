//! Verified delivery of a content name's bytes from a trustless HTTP gateway: opt-in.
//!
//! By default the resolver holds a CID and fetches nothing (see [`content`](crate::content)).
//! A host that binds [`DELIVERY_SOURCE_IRI`] to a [`DeliveryConfig`] turns on this mode for
//! public content names: `urn:name:document` then fetches the content from the configured
//! gateways and serves it only once every byte is verified against the name's CID. The
//! design is the section "Client and verification" of `docs/design/ipfs-resolution.md`.
//!
//! ## The boundary
//!
//! Every fetch goes through the host's `urn:http:get`, under the caller's capability, so
//! nothing here opens a network path of its own. A gateway is tried only when the
//! capability holds `urn:cap:net:<its host>`, and `urn:http:get` re-runs that check at every
//! redirect hop, so a gateway cannot send the request anywhere the grant does not reach.
//!
//! ## What is verified
//!
//! - A **raw block** (`raw`, `0x55`) is asked for as `application/vnd.ipld.raw`, and served
//!   only when the SHA-256 of the answer is the CID's digest.
//! - A **UnixFS file** (`dag-pb`, `0x70`) is asked for as a CARv1. Traversal starts at the
//!   requested CID, never at whatever the CAR claims; each block it consumes is looked up by
//!   the CID its verified parent links to, and its codec and multihash are checked before it
//!   is decoded. An iterative pre-scan bounds the depth, sizes every node and checks that
//!   every linked block is present, BEFORE any byte is assembled. The file is then built from
//!   the authenticated links, in order. Nothing is served or cached before all of that holds.
//!
//! Only SHA-256 (`0x12`) is verified; any other hash, any other codec, a CARv2, and a
//! UnixFS node that is not a file are refused as [`Error::Unsupported`], distinctly.
//!
//! ## Failures, kept apart
//!
//! - **Integrity**: a gateway answered bytes that do not prove the content (a wrong digest, a
//!   malformed or truncated CAR, the wrong root, an omitted block). [`Error::Endpoint`],
//!   prefixed so it reads as such.
//! - **Unavailability**: a gateway did not answer the content at all. Its own HTTP failure
//!   ([`Error::Unavailable`], [`Error::Timeout`], [`Error::NotFound`] and the rest) is kept.
//! - **Limits** ([`Limits`]): transfer size, decoded size, DAG depth, block count and time.
//!   [`Error::OverBudget`].
//!
//! A gateway's integrity failure, unavailability, or a limit its answer alone ran past
//! (transfer size, block count, time) moves on to the next configured gateway, which is an
//! independent source. A limit the verified content itself runs past (decoded size, depth),
//! an unsupported format, or content that is not a well-formed UnixFS file is the same from
//! every source, so it ends the search.
//!
//! ## Serving
//!
//! A media type is the publisher's assertion, and verified content can still carry active
//! code. By default ([`Serve::Download`]) the answer is an attachment, labeled with the
//! declared type only when that type is passive (plain text, common images, audio and video,
//! JSON, CSV, Turtle), and `application/octet-stream` otherwise: a declared `text/html` or
//! `image/svg+xml` is never handed to a browser to render on this origin. A host that serves
//! this face on an origin of its own, holding nothing else, may choose
//! [`Serve::IsolatedOrigin`] to pass the declared type through, inline.

use crate::content::{gateway_refusal, gateway_url};
use cid::Cid;
use ikigai_core::{ArgRef, Error, Invocation, Iri, ReprType, Representation, Request, Verb};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::collections::HashMap;

/// Where the delivery configuration is read from. Unbound, nothing is fetched: the
/// resolver answers a content name as it always has.
pub const DELIVERY_SOURCE_IRI: &str = "urn:name:delivery-source";

/// The outbound client every fetch goes through, as `ikigai-http` binds it.
const HTTP_GET_IRI: &str = "urn:http:get";

/// The multicodec of a raw block.
const RAW: u64 = 0x55;
/// The multicodec of a `dag-pb` node.
const DAG_PB: u64 = 0x70;
/// The multihash code of SHA-256, the only hash verified.
const SHA2_256: u64 = 0x12;

/// The media type of an `ikigai-web` entity envelope: header lines, an empty line, the content.
const HTTP_ENTITY: &str = "application/vnd.ikigai.http-entity";
const OCTET_STREAM: &str = "application/octet-stream";

/// The longest CAR header read, in bytes. A CARv1 header naming one root is about 60.
const MAX_CAR_HEADER: u64 = 1024;
/// The most roots or header fields a CAR header may carry.
const MAX_CAR_HEADER_ITEMS: u64 = 8;

/// How a verified file is served. See the [module documentation](self).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Serve {
    /// As an attachment, labeled with the declared type only when it is passive.
    #[default]
    Download,
    /// Inline, with the declared type, for a host serving this face on an origin of its own.
    IsolatedOrigin,
}

/// The bounds one delivery is held to. Each is checked, and each has its own refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Limits {
    /// The most bytes one gateway's answer may hold. The host's `urn:http:get` collection
    /// limit is what stops reading; set it no higher than this.
    pub transfer_bytes: u64,
    /// The largest file that may be assembled.
    pub decoded_bytes: u64,
    /// The most links between the root and any block.
    pub depth: u32,
    /// The most blocks one answer may hold.
    pub blocks: u32,
    /// The longest one gateway may take, in milliseconds, on the kernel's clock.
    pub millis: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            transfer_bytes: 8 * 1024 * 1024,
            decoded_bytes: 8 * 1024 * 1024,
            depth: 32,
            blocks: 4096,
            millis: 30_000,
        }
    }
}

/// What [`DELIVERY_SOURCE_IRI`] holds, as JSON:
///
/// ```json
/// {"gateways": ["https://trustless.example", "https://second.example"],
///  "serve": "download",
///  "limits": {"transfer-bytes": 8388608, "decoded-bytes": 8388608, "depth": 32,
///             "blocks": 4096, "millis": 30000}}
/// ```
///
/// `gateways` is required, tried in order; `serve` and each limit have the defaults of
/// [`Serve`] and [`Limits`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryConfig {
    /// Trustless HTTP gateways, each an independent source, in the order they are tried.
    pub gateways: Vec<String>,
    /// How a verified file is served.
    #[serde(default)]
    pub serve: Serve,
    /// The bounds.
    #[serde(default)]
    pub limits: Limits,
}

impl DeliveryConfig {
    /// Parse and check a configuration. `Err` says why it cannot be used.
    pub fn from_json(bytes: &[u8]) -> std::result::Result<Self, String> {
        let config: DeliveryConfig = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if config.gateways.is_empty() {
            return Err("it names no gateway".into());
        }
        for (i, gateway) in config.gateways.iter().enumerate() {
            if let Some(why) = gateway_refusal(gateway) {
                return Err(why);
            }
            if config.gateways[..i].contains(gateway) {
                return Err(format!("gateway {gateway:?} is named twice"));
            }
        }
        let l = &config.limits;
        for (name, value) in [
            ("transfer-bytes", l.transfer_bytes),
            ("decoded-bytes", l.decoded_bytes),
            ("depth", u64::from(l.depth)),
            ("blocks", u64::from(l.blocks)),
            ("millis", l.millis),
        ] {
            if value == 0 {
                return Err(format!(
                    "its limit {name} is 0, which nothing can be within"
                ));
            }
        }
        Ok(config)
    }
}

/// Why one answer was not served. See the [module documentation](self).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The answer does not prove the content: try another source.
    Integrity(String),
    /// A codec, hash, CAR version or UnixFS type this does not verify.
    Unsupported(String),
    /// The verified content is not a well-formed UnixFS file: the same from every source.
    Invalid(String),
    /// A bound was passed. `content` is whether the verified content itself passed it (the
    /// same from every source) rather than one gateway's answer.
    Limit {
        /// Which bound, and by how much.
        detail: String,
        /// Whether retrying elsewhere cannot help.
        content: bool,
    },
}

impl Failure {
    fn integrity(detail: impl Into<String>) -> Self {
        Failure::Integrity(detail.into())
    }
    fn response_limit(detail: impl Into<String>) -> Self {
        Failure::Limit {
            detail: detail.into(),
            content: false,
        }
    }
    fn content_limit(detail: impl Into<String>) -> Self {
        Failure::Limit {
            detail: detail.into(),
            content: true,
        }
    }
    /// Whether another source could answer differently.
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Failure::Integrity(_) | Failure::Limit { content: false, .. }
        )
    }
}

/// A block's identity for lookup: its codec and multihash, so a CIDv0 link finds the
/// CIDv1 a CAR names the same block by.
type Key = (u64, Vec<u8>);

fn key(cid: &Cid) -> Key {
    (cid.codec(), cid.hash().to_bytes())
}

fn show(cid: &Cid) -> String {
    match cid.into_v1() {
        Ok(v1) => v1
            .to_string_of_base(cid::multibase::Base::Base32Lower)
            .unwrap_or_else(|_| v1.to_string()),
        Err(_) => cid.to_string(),
    }
}

/// Check that `cid` is one this verifies, and that `data` is the block it names.
fn verify_block(cid: &Cid, data: &[u8]) -> std::result::Result<(), Failure> {
    if cid.codec() != RAW && cid.codec() != DAG_PB {
        return Err(Failure::Unsupported(format!(
            "the block {} has the codec 0x{:x}, which is neither raw (0x55) nor dag-pb (0x70)",
            show(cid),
            cid.codec()
        )));
    }
    let hash = cid.hash();
    if hash.code() != SHA2_256 || hash.size() != 32 {
        return Err(Failure::Unsupported(format!(
            "the block {} is named by the multihash 0x{:x} of {} bytes, and only sha2-256 \
             (0x12, 32 bytes) is verified",
            show(cid),
            hash.code(),
            hash.size()
        )));
    }
    if Sha256::digest(data).as_slice() != hash.digest() {
        return Err(Failure::integrity(format!(
            "the bytes answered for the block {} do not have its digest",
            show(cid)
        )));
    }
    Ok(())
}

/// Verify a raw block answered for `cid`, within `limits`.
pub fn verify_raw(
    cid: &Cid,
    bytes: &[u8],
    limits: &Limits,
) -> std::result::Result<Vec<u8>, Failure> {
    if bytes.len() as u64 > limits.transfer_bytes {
        return Err(Failure::response_limit(format!(
            "the answer is {} bytes, more than the transfer limit of {}",
            bytes.len(),
            limits.transfer_bytes
        )));
    }
    if cid.codec() != RAW {
        return Err(Failure::Unsupported(format!(
            "{} is not a raw block",
            show(cid)
        )));
    }
    verify_block(cid, bytes)?;
    if bytes.len() as u64 > limits.decoded_bytes {
        return Err(Failure::content_limit(format!(
            "the content is {} bytes, more than the decoded limit of {}",
            bytes.len(),
            limits.decoded_bytes
        )));
    }
    Ok(bytes.to_vec())
}

/// An unsigned LEB128 varint at `*at`, advancing past it. `None` when truncated or longer
/// than nine bytes (the multiformats bound).
fn varint(bytes: &[u8], at: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for i in 0..9 {
        let byte = *bytes.get(*at)?;
        *at += 1;
        value |= u64::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            // A minimal encoding only: a final zero byte after the first is overlong.
            if i > 0 && byte == 0 {
                return None;
            }
            return Some(value);
        }
    }
    None
}

/// `n` bytes at `*at`, advancing past them.
fn take<'a>(bytes: &'a [u8], at: &mut usize, n: u64) -> Option<&'a [u8]> {
    let n = usize::try_from(n).ok()?;
    let end = at.checked_add(n)?;
    let slice = bytes.get(*at..end)?;
    *at = end;
    Some(slice)
}

/// A CBOR item head: its major type and argument. Indefinite lengths are refused.
fn cbor_head(bytes: &[u8], at: &mut usize) -> Option<(u8, u64)> {
    let first = *bytes.get(*at)?;
    *at += 1;
    let major = first >> 5;
    let arg = match first & 0x1f {
        n @ 0..=23 => u64::from(n),
        24 => u64::from(*take(bytes, at, 1)?.first()?),
        25 => u64::from(u16::from_be_bytes(take(bytes, at, 2)?.try_into().ok()?)),
        26 => u64::from(u32::from_be_bytes(take(bytes, at, 4)?.try_into().ok()?)),
        27 => u64::from_be_bytes(take(bytes, at, 8)?.try_into().ok()?),
        _ => return None,
    };
    Some((major, arg))
}

/// A CAR header's version and roots, read as the fixed shape a CARv1 header has, without
/// a general (recursive) CBOR parser. `Err` is a clause saying what is wrong.
fn car_header(header: &[u8]) -> std::result::Result<(u64, Vec<Cid>), String> {
    let malformed = || "its header is not a CAR header".to_string();
    let mut at = 0;
    let (major, fields) = cbor_head(header, &mut at).ok_or_else(malformed)?;
    if major != 5 || fields > MAX_CAR_HEADER_ITEMS {
        return Err(malformed());
    }
    let (mut version, mut roots) = (None, Vec::new());
    for _ in 0..fields {
        let (major, len) = cbor_head(header, &mut at).ok_or_else(malformed)?;
        if major != 3 {
            return Err(malformed());
        }
        match take(header, &mut at, len).ok_or_else(malformed)? {
            b"version" => {
                let (major, value) = cbor_head(header, &mut at).ok_or_else(malformed)?;
                if major != 0 {
                    return Err(malformed());
                }
                version = Some(value);
            }
            b"roots" => {
                let (major, count) = cbor_head(header, &mut at).ok_or_else(malformed)?;
                if major != 4 || count > MAX_CAR_HEADER_ITEMS {
                    return Err(malformed());
                }
                for _ in 0..count {
                    // A CID in DAG-CBOR: tag 42 around a byte string led by a zero byte.
                    if cbor_head(header, &mut at) != Some((6, 42)) {
                        return Err(malformed());
                    }
                    let (major, len) = cbor_head(header, &mut at).ok_or_else(malformed)?;
                    let bytes = take(header, &mut at, len).ok_or_else(malformed)?;
                    let (2, [0, cid @ ..]) = (major, bytes) else {
                        return Err(malformed());
                    };
                    roots.push(Cid::try_from(cid).map_err(|_| malformed())?);
                }
            }
            _ => return Err(malformed()),
        }
    }
    if at != header.len() {
        return Err(malformed());
    }
    Ok((version.ok_or_else(malformed)?, roots))
}

/// A CARv1's blocks, keyed by codec and multihash, each as the CAR names it. Nothing is
/// verified here: a block is verified when traversal consumes it.
fn read_car<'a>(
    car: &'a [u8],
    root: &Cid,
    limits: &Limits,
) -> std::result::Result<HashMap<Key, (Cid, &'a [u8])>, Failure> {
    if car.len() as u64 > limits.transfer_bytes {
        return Err(Failure::response_limit(format!(
            "the answer is {} bytes, more than the transfer limit of {}",
            car.len(),
            limits.transfer_bytes
        )));
    }
    let malformed = |why: &str| Failure::integrity(format!("the answer is not a CAR: {why}"));
    let mut at = 0;
    let header_len = varint(car, &mut at).ok_or_else(|| malformed("it has no header"))?;
    if header_len == 0 || header_len > MAX_CAR_HEADER {
        return Err(malformed("its header length is out of range"));
    }
    let header = take(car, &mut at, header_len).ok_or_else(|| malformed("it is truncated"))?;
    let (version, roots) = car_header(header).map_err(|why| malformed(&why))?;
    if version != 1 {
        return Err(Failure::Unsupported(format!(
            "the answer is a CAR of version {version}, and only CARv1 is read"
        )));
    }
    // The header alone proves nothing, but a CAR for some other root is the wrong answer.
    if roots.len() != 1 || key(&roots[0]) != key(root) {
        let named: Vec<String> = roots.iter().map(show).collect();
        return Err(Failure::integrity(format!(
            "the answer is a CAR for the root {}, not {}",
            if named.is_empty() {
                "(none)".to_string()
            } else {
                named.join(", ")
            },
            show(root)
        )));
    }
    let mut blocks = HashMap::new();
    let mut count = 0u32;
    while at < car.len() {
        count += 1;
        if count > limits.blocks {
            return Err(Failure::response_limit(format!(
                "the answer holds more than the block limit of {}",
                limits.blocks
            )));
        }
        let len = varint(car, &mut at).ok_or_else(|| malformed("a block's length is truncated"))?;
        let mut section =
            take(car, &mut at, len).ok_or_else(|| malformed("a block is truncated"))?;
        let cid =
            Cid::read_bytes(&mut section).map_err(|_| malformed("a block's CID is not one"))?;
        // The first copy stands; a later one under the same CID is not consumed.
        blocks.entry(key(&cid)).or_insert((cid, section));
    }
    Ok(blocks)
}

/// A protobuf field: its number, and its value as a varint or bytes.
enum Field<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
}

/// The fields of a protobuf message, in order. Fixed-width fields are skipped; any other
/// wire type is malformed.
fn fields(message: &[u8]) -> std::result::Result<Vec<(u64, Field<'_>)>, ()> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < message.len() {
        let tag = varint(message, &mut at).ok_or(())?;
        let field = tag >> 3;
        match tag & 7 {
            0 => out.push((field, Field::Varint(varint(message, &mut at).ok_or(())?))),
            1 => {
                take(message, &mut at, 8).ok_or(())?;
            }
            2 => {
                let len = varint(message, &mut at).ok_or(())?;
                out.push((field, Field::Bytes(take(message, &mut at, len).ok_or(())?)));
            }
            5 => {
                take(message, &mut at, 4).ok_or(())?;
            }
            _ => return Err(()),
        }
    }
    Ok(out)
}

/// One node of a UnixFS file: its own bytes, then its children's content in order.
struct FileNode<'a> {
    data: &'a [u8],
    links: Vec<Cid>,
    blocksizes: Vec<u64>,
    filesize: Option<u64>,
}

/// Decode a verified block as a node of a UnixFS file.
fn file_node<'a>(cid: &Cid, block: &'a [u8]) -> std::result::Result<FileNode<'a>, Failure> {
    if cid.codec() == RAW {
        return Ok(FileNode {
            data: block,
            links: Vec::new(),
            blocksizes: Vec::new(),
            filesize: None,
        });
    }
    let invalid = |why: &str| Failure::Invalid(format!("the block {} {why}", show(cid)));
    let node = fields(block).map_err(|_| invalid("is not a dag-pb node"))?;
    let (mut data, mut links) = (None, Vec::new());
    for (number, value) in node {
        match (number, value) {
            (1, Field::Bytes(bytes)) if data.is_none() => data = Some(bytes),
            (2, Field::Bytes(link)) => {
                let mut hash = None;
                for (number, value) in fields(link).map_err(|_| invalid("has a malformed link"))? {
                    match (number, value) {
                        (1, Field::Bytes(bytes)) if hash.is_none() => hash = Some(bytes),
                        (2, Field::Bytes(_)) | (3, Field::Varint(_)) => {}
                        _ => return Err(invalid("has a malformed link")),
                    }
                }
                let hash = hash.ok_or_else(|| invalid("has a link with no CID"))?;
                links.push(Cid::try_from(hash).map_err(|_| invalid("links to what is not a CID"))?);
            }
            _ => return Err(invalid("is not a dag-pb node")),
        }
    }
    let data = data.ok_or_else(|| {
        Failure::Unsupported(format!(
            "the dag-pb node {} carries no UnixFS data",
            show(cid)
        ))
    })?;
    let (mut kind, mut bytes, mut filesize, mut blocksizes) = (None, &[][..], None, Vec::new());
    for (number, value) in fields(data).map_err(|_| invalid("has malformed UnixFS data"))? {
        match (number, value) {
            (1, Field::Varint(v)) => kind = Some(v),
            (2, Field::Bytes(b)) => bytes = b,
            (3, Field::Varint(v)) => filesize = Some(v),
            (4, Field::Varint(v)) => blocksizes.push(v),
            (4, Field::Bytes(mut packed)) => {
                while !packed.is_empty() {
                    let mut at = 0;
                    blocksizes.push(
                        varint(packed, &mut at)
                            .ok_or_else(|| invalid("has malformed blocksizes"))?,
                    );
                    packed = &packed[at..];
                }
            }
            _ => {}
        }
    }
    match kind {
        Some(0) | Some(2) => {}
        Some(other) => {
            let what = match other {
                1 => "a directory",
                3 => "metadata",
                4 => "a symbolic link",
                5 => "a sharded directory",
                _ => "of an unknown UnixFS type",
            };
            return Err(Failure::Unsupported(format!(
                "the UnixFS node {} is {what}, not a file",
                show(cid)
            )));
        }
        None => return Err(invalid("has UnixFS data with no type")),
    }
    if blocksizes.len() != links.len() {
        return Err(invalid(&format!(
            "has {} links but {} blocksizes",
            links.len(),
            blocksizes.len()
        )));
    }
    Ok(FileNode {
        data: bytes,
        links,
        blocksizes,
        filesize,
    })
}

/// The traversal's two steps for a node: enter it (look it up, verify it, queue its
/// children), then leave it once every child has been sized.
enum Step {
    Enter(Cid, u32),
    Leave(Key),
}

/// Verify a CAR answered for the UnixFS file `root`, within `limits`, and assemble it.
///
/// No recursion: the pre-scan walks an explicit stack, so a deep DAG is refused at the
/// depth limit rather than by the thread's stack.
pub fn verify_car(
    root: &Cid,
    car: &[u8],
    limits: &Limits,
) -> std::result::Result<Vec<u8>, Failure> {
    let blocks = read_car(car, root, limits)?;

    // The pre-scan: every reachable block present and verified, every node sized and
    // consistent with its parent's account of it, the depth and the size within bounds.
    let mut nodes: HashMap<Key, FileNode<'_>> = HashMap::new();
    // A sized node's content length and height (the most links below it).
    let mut sized: HashMap<Key, (u64, u32)> = HashMap::new();
    let mut stack = vec![Step::Enter(*root, 0)];
    while let Some(step) = stack.pop() {
        match step {
            Step::Enter(cid, depth) => {
                let k = key(&cid);
                if let Some(&(_, height)) = sized.get(&k) {
                    if depth.saturating_add(height) > limits.depth {
                        return Err(depth_exceeded(limits));
                    }
                    continue;
                }
                if nodes.contains_key(&k) {
                    return Err(Failure::Invalid(format!(
                        "the block {} links to itself",
                        show(&cid)
                    )));
                }
                if depth > limits.depth {
                    return Err(depth_exceeded(limits));
                }
                let Some((_, block)) = blocks.get(&k) else {
                    return Err(Failure::integrity(format!(
                        "the answer omits the block {}, so the file is incomplete",
                        show(&cid)
                    )));
                };
                verify_block(&cid, block)?;
                let node = file_node(&cid, block)?;
                stack.push(Step::Leave(k.clone()));
                for child in node.links.iter().rev() {
                    stack.push(Step::Enter(*child, depth + 1));
                }
                nodes.insert(k, node);
            }
            Step::Leave(k) => {
                let node = &nodes[&k];
                let mut size = node.data.len() as u64;
                let mut height = 0u32;
                for (child, declared) in node.links.iter().zip(&node.blocksizes) {
                    let (child_size, child_height) = sized[&key(child)];
                    if child_size != *declared {
                        return Err(Failure::Invalid(format!(
                            "the block {} declares {declared} bytes for a child that holds \
                             {child_size}",
                            show(&blocks[&k].0)
                        )));
                    }
                    size = size.saturating_add(child_size);
                    height = height.max(child_height + 1);
                }
                if let Some(filesize) = node.filesize {
                    if filesize != size {
                        return Err(Failure::Invalid(format!(
                            "the block {} declares a file of {filesize} bytes that holds {size}",
                            show(&blocks[&k].0)
                        )));
                    }
                }
                if size > limits.decoded_bytes {
                    return Err(Failure::content_limit(format!(
                        "the file holds more than the decoded limit of {} bytes",
                        limits.decoded_bytes
                    )));
                }
                sized.insert(k, (size, height));
            }
        }
    }

    // Assembly, in link order, from the verified nodes alone. A child that holds no bytes
    // is not visited, so every node visited holds at least one byte of the output: the work
    // is bounded by the size already checked, times the depth.
    let total = sized[&key(root)].0;
    let mut out = Vec::with_capacity(usize::try_from(total).unwrap_or(0));
    let mut stack = vec![key(root)];
    while let Some(k) = stack.pop() {
        let node = &nodes[&k];
        out.extend_from_slice(node.data);
        for child in node.links.iter().rev() {
            let k = key(child);
            if sized[&k].0 > 0 {
                stack.push(k);
            }
        }
    }
    if out.len() as u64 != total {
        return Err(Failure::Invalid(format!(
            "the file {} assembled to {} bytes, not the {total} it declares",
            show(root),
            out.len()
        )));
    }
    Ok(out)
}

fn depth_exceeded(limits: &Limits) -> Failure {
    Failure::content_limit(format!(
        "the file is more than {} links deep, the depth limit",
        limits.depth
    ))
}

/// Whether a declared media type is passive: one a browser shows, plays or saves without
/// running anything it carries.
fn passive(media_type: &str) -> bool {
    let bare = media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    matches!(
        bare.as_str(),
        "text/plain"
            | "text/csv"
            | "text/turtle"
            | "application/json"
            | "application/octet-stream"
            | "image/png"
            | "image/jpeg"
            | "image/gif"
            | "image/webp"
            | "image/avif"
            | "audio/mpeg"
            | "audio/ogg"
            | "video/mp4"
            | "video/webm"
    )
}

/// The verified `bytes` of `cid`, as served under `serve`: an entity envelope carrying the
/// content type and, for a download, `Content-Disposition: attachment`.
pub fn served(bytes: Vec<u8>, cid: &str, declared: Option<&str>, serve: Serve) -> Representation {
    let (content_type, disposition) = match serve {
        Serve::Download => (
            declared.filter(|t| passive(t)).unwrap_or(OCTET_STREAM),
            Some(format!("attachment; filename=\"{cid}\"")),
        ),
        Serve::IsolatedOrigin => (declared.unwrap_or(OCTET_STREAM), None),
    };
    // A declared type was checked as a media type when the registry loaded: tokens and
    // quoted strings only, so no line break reaches the header block.
    let mut head = format!("Content-Type: {content_type}\r\n");
    if let Some(disposition) = disposition {
        head.push_str(&format!("Content-Disposition: {disposition}\r\n"));
    }
    head.push_str("\r\n");
    let mut body = head.into_bytes();
    body.extend_from_slice(&bytes);
    Representation::new(ReprType::new(HTTP_ENTITY), body)
}

/// The delivery configuration, or `None` when the host bound none.
pub(crate) async fn config(inv: &Invocation<'_>) -> ikigai_core::Result<Option<DeliveryConfig>> {
    let iri = Iri::parse(DELIVERY_SOURCE_IRI).map_err(|e| Error::Endpoint(format!("name: {e}")))?;
    let repr = match inv.source(&iri).await {
        Ok(repr) => repr,
        Err(Error::Unresolved(missing)) if missing.as_str() == DELIVERY_SOURCE_IRI => {
            return Ok(None)
        }
        Err(other) => return Err(other),
    };
    DeliveryConfig::from_json(&repr.bytes)
        .map(Some)
        .map_err(|why| {
            Error::Endpoint(format!(
                "name: {DELIVERY_SOURCE_IRI} is not a valid delivery configuration: {why}"
            ))
        })
}

/// What one gateway's attempt came to, for the refusal that names them all.
enum Attempt {
    Failed(Failure),
    Unavailable(Error),
    Denied(String),
}

/// Fetch `cid` (canonical) from the configured gateways, in order, and serve the first
/// answer that verifies. See the [module documentation](self).
pub(crate) async fn deliver(
    inv: &Invocation<'_>,
    config: &DeliveryConfig,
    cid: &str,
    declared: Option<&str>,
) -> ikigai_core::Result<Representation> {
    let parsed = Cid::try_from(cid)
        .map_err(|e| Error::Endpoint(format!("name: {cid:?} is not a CID: {e}")))?;
    let limits = &config.limits;
    let raw = parsed.codec() == RAW;
    if inv.now().is_none() {
        return Err(Error::Unsupported {
            verb: None,
            detail: format!(
                "name: delivering ipfs://{cid} needs the kernel's clock, to hold each gateway \
                 to {} ms",
                limits.millis
            ),
        });
    }
    let mut attempts: Vec<(String, Attempt)> = Vec::new();
    for gateway in &config.gateways {
        let scope = crate::tenant::iri_host(gateway).map(|host| format!("urn:cap:net:{host}"));
        match scope {
            Some(scope) if inv.capability.allows(&scope) => {}
            Some(scope) => {
                attempts.push((gateway.clone(), Attempt::Denied(format!("needs {scope}"))));
                continue;
            }
            None => {
                attempts.push((gateway.clone(), Attempt::Denied("has no host".into())));
                continue;
            }
        }
        let start = inv.now().expect("checked above");
        let deadline = start.plus_millis(limits.millis);
        let (format, accept) = if raw {
            ("raw", "application/vnd.ipld.raw")
        } else {
            (
                "car&dag-scope=all",
                "application/vnd.ipld.car; version=1; order=dfs; dups=n",
            )
        };
        let url = format!("{}?format={format}", gateway_url(gateway, cid));
        let get = Iri::parse(HTTP_GET_IRI).map_err(|e| Error::Endpoint(format!("name: {e}")))?;
        let request = Request::new(Verb::Source, get)
            .with_arg("url", ArgRef::Inline(url.into_bytes()))
            // Never serve a gateway's answer from the cache, however long it says it may
            // be kept: an unverified answer is not the content. Only the verified file is.
            .with_arg("max_age", ArgRef::Inline(b"0".to_vec()))
            .with_arg(
                "headers",
                ArgRef::Inline(format!("Accept: {accept}\nUser-Agent: ikigai-name\n").into_bytes()),
            );
        let answered = inv.with_not_after(deadline).issue(request).await;
        let late = inv
            .now()
            .is_some_and(|now| now.as_millis() > deadline.as_millis());
        let answered = match answered {
            Err(Error::Unresolved(missing)) if missing.as_str() == HTTP_GET_IRI => {
                return Err(Error::Unsupported {
                    verb: None,
                    detail: format!(
                        "name: this host has no HTTP client, so it delivers nothing for \
                         ipfs://{cid}; bind {HTTP_GET_IRI} to add it"
                    ),
                });
            }
            _ if late => Err(Attempt::Failed(Failure::response_limit(format!(
                "it took more than the time limit of {} ms",
                limits.millis
            )))),
            Err(Error::OverBudget { detail }) => {
                Err(Attempt::Failed(Failure::response_limit(detail)))
            }
            Err(Error::Denied(why)) => Err(Attempt::Denied(why)),
            Err(other) => Err(Attempt::Unavailable(other)),
            Ok(repr) => Ok(repr),
        };
        let outcome = answered.and_then(|repr| {
            if raw {
                verify_raw(&parsed, &repr.bytes, limits)
            } else {
                verify_car(&parsed, &repr.bytes, limits)
            }
            .map_err(Attempt::Failed)
        });
        match outcome {
            Ok(bytes) => {
                // Verified, complete, and only now servable or cacheable.
                return Ok(served(bytes, cid, declared, config.serve).cacheable());
            }
            Err(Attempt::Failed(failure)) if !failure.retryable() => {
                return Err(refusal(cid, &[(gateway.clone(), Attempt::Failed(failure))]));
            }
            Err(attempt) => attempts.push((gateway.clone(), attempt)),
        }
    }
    Err(refusal(cid, &attempts))
}

/// The one refusal for a delivery that served nothing, naming what each gateway did. Its
/// kind is the most telling of theirs: an integrity failure, then a limit, then a
/// gateway's own unavailability, then a missing grant.
fn refusal(cid: &str, attempts: &[(String, Attempt)]) -> Error {
    let said: Vec<String> = attempts
        .iter()
        .map(|(gateway, attempt)| match attempt {
            Attempt::Failed(Failure::Integrity(why)) => {
                format!("{gateway} answered bytes that do not verify: {why}")
            }
            Attempt::Failed(Failure::Unsupported(why)) => format!("{gateway}: {why}"),
            Attempt::Failed(Failure::Invalid(why)) => {
                format!("{gateway}: the content is not a well-formed UnixFS file: {why}")
            }
            Attempt::Failed(Failure::Limit { detail, .. }) => format!("{gateway}: {detail}"),
            Attempt::Unavailable(e) => format!("{gateway} did not answer it: {e}"),
            Attempt::Denied(why) => format!("{gateway} was not asked: it {why}"),
        })
        .collect();
    let detail = format!(
        "name: no configured gateway delivered verified bytes for ipfs://{cid}: {}",
        said.join("; ")
    );
    let has = |f: fn(&Attempt) -> bool| attempts.iter().any(|(_, a)| f(a));
    if has(|a| matches!(a, Attempt::Failed(Failure::Unsupported(_)))) {
        return Error::Unsupported { verb: None, detail };
    }
    if has(|a| {
        matches!(
            a,
            Attempt::Failed(Failure::Integrity(_) | Failure::Invalid(_))
        )
    }) {
        return Error::Endpoint(detail);
    }
    if has(|a| matches!(a, Attempt::Failed(Failure::Limit { .. }))) {
        return Error::OverBudget { detail };
    }
    let unavailable = attempts.iter().find_map(|(_, a)| match a {
        Attempt::Unavailable(e) => Some(e),
        _ => None,
    });
    match unavailable {
        Some(Error::Timeout(_)) => Error::Timeout(detail),
        Some(Error::NotFound(_)) => Error::NotFound(detail),
        Some(Error::RetryAfter { delay, .. }) => Error::RetryAfter {
            delay: *delay,
            detail,
        },
        Some(_) => Error::Unavailable(detail),
        None => Error::Denied(detail),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cid::multihash::Multihash;

    fn raw_cid(bytes: &[u8]) -> Cid {
        Cid::new_v1(
            RAW,
            Multihash::wrap(SHA2_256, &Sha256::digest(bytes)).unwrap(),
        )
    }

    #[test]
    fn varints_are_minimal_and_bounded() {
        let mut at = 0;
        assert_eq!(varint(&[0xac, 0x02], &mut at), Some(300));
        assert_eq!(at, 2);
        assert_eq!(varint(&[0x80, 0x00], &mut 0), None, "overlong");
        assert_eq!(varint(&[0x80], &mut 0), None, "truncated");
        assert_eq!(varint(&[0xff; 10], &mut 0), None, "too long");
    }

    #[test]
    fn a_raw_block_verifies_against_its_cid_only() {
        let cid = raw_cid(b"hello");
        let limits = Limits::default();
        assert_eq!(verify_raw(&cid, b"hello", &limits).unwrap(), b"hello");
        assert!(matches!(
            verify_raw(&cid, b"hellO", &limits),
            Err(Failure::Integrity(_))
        ));
    }

    #[test]
    fn a_passive_type_is_kept_and_an_active_one_is_not() {
        assert!(passive("text/plain; charset=utf-8"));
        assert!(passive("IMAGE/PNG"));
        for active in [
            "text/html",
            "image/svg+xml",
            "application/javascript",
            "application/pdf",
        ] {
            assert!(!passive(active), "{active}");
        }
    }

    #[test]
    fn a_configuration_names_https_gateways_and_nonzero_limits() {
        let ok = DeliveryConfig::from_json(br#"{"gateways":["https://a.example"]}"#).unwrap();
        assert_eq!(ok.serve, Serve::Download);
        assert_eq!(ok.limits, Limits::default());
        for bad in [
            &br#"{"gateways":[]}"#[..],
            br#"{"gateways":["http://a.example"]}"#,
            br#"{"gateways":["https://a.example","https://a.example"]}"#,
            br#"{"gateways":["https://a.example"],"limits":{"depth":0}}"#,
            br#"{"gateways":["https://a.example"],"limit":{}}"#,
        ] {
            assert!(
                DeliveryConfig::from_json(bad).is_err(),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    }
}
