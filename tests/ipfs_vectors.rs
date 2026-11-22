//! Offline vectors for the IPFS boundary described in docs/design/ipfs-resolution.md.

use cid::{multihash::Multihash, Cid, Version};
use sha1::{Digest as _, Sha1};
use sha2::{Digest as _, Sha256};

const GIT_RAW: u64 = 0x78;
const RAW: u64 = 0x55;
const SHA1: u64 = 0x11;
const SHA2_256: u64 = 0x12;
const BLAKE3: u64 = 0x1e;

fn check_cid(codec: u64, hash_code: u64, digest: &[u8], expected_digest: &str, expected_cid: &str) {
    assert_eq!(hex::encode(digest), expected_digest);

    let parsed: Cid = expected_cid.parse().unwrap();
    assert_eq!(parsed.version(), Version::V1);
    assert_eq!(parsed.codec(), codec);
    assert_eq!(parsed.hash().code(), hash_code);
    assert_eq!(parsed.hash().digest(), digest);

    let wrapped = Cid::new_v1(codec, Multihash::wrap(hash_code, digest).unwrap());
    assert_eq!(wrapped.to_string(), expected_cid);
}

#[test]
fn git_raw_wraps_the_repository_object_id() {
    // git hash-object --stdin in repositories initialized with --object-format=sha1
    // and --object-format=sha256 respectively, with stdin exactly b"hello".
    let object = b"blob 5\0hello";
    check_cid(
        GIT_RAW,
        SHA1,
        &Sha1::digest(object),
        "b6fc4c620b67d95f953a5c1c1230aaab5db5a1b0",
        "baf4bcffw7rggec3h3fpzkos4dqjdbkvllw22dma",
    );
    check_cid(
        GIT_RAW,
        SHA2_256,
        &Sha256::digest(object),
        "8aec4e4876f854f688d0ebfc8f37598f38e5fd6903cccc850ca36591175aeb60",
        "baf4beiek5rheq5xykt3iruhl7shtowmphds722idztgikdfdmwirowxlma",
    );
}

#[test]
fn blake3_raw_wraps_the_digest_of_the_exact_block() {
    check_cid(
        RAW,
        BLAKE3,
        blake3::hash(b"").as_bytes(),
        "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
        "bafkr4ifpcne3t5pzugtkaqcn5i3nzskjtpfslsnnyejlpte2spfoihzsmi",
    );
    check_cid(
        RAW,
        BLAKE3,
        blake3::hash(b"hello").as_bytes(),
        "ea8f163db38682925e4491c5e58d4bb3506ef8c14eb78a86e908c5624a67200f",
        "bafkr4ihkr4ld3m4gqkjf4reryxsy2s5tkbxprqkow6fin2iiyvreuzzab4",
    );
}
