# IPFS resolution and a read-only registry off-ramp

Proposed design, 2026-11-21. Start with CID-bearing resolution records and an offline
snapshot format. Keep pinning in the owner's publishing workflow and retrieval in
the client's hands. Add resolver-side verification only after exercising the
format with independent readers and storage providers.

## Baseline and evidence

- At commit `99fdf2016faf6575305a3188f318d95a03ab197a`, the standalone server's
  `--check --root` accepted a scratch registry with a redirect to
  `https://gateway.example/ipfs/<cid>`. Changing the entry to
  `"strategy":"content","cid":"<cid>"` exited 1 with `unknown variant content`.
  The redirect can transport a CID in a URL. It gives clients no typed content
  target or verification contract. See [registry strategies](../../src/registry.rs)
  and [resolution faces](../../src/resolution.rs).
- For the five bytes `hello`, `git hash-object --stdin` returned
  `b6fc4c620b67d95f953a5c1c1230aaab5db5a1b0`. SHA-1 of the bytes prefixed by
  `blob 5` and a NUL matched. SHA-1 of `hello` alone differed. Wrapping the
  Git digest produced `baf4bcffw7rggec3h3fpzkos4dqjdbkvllw22dma`.
  [Git object hashing](https://git-scm.com/docs/hash-function-transition)
- In the UnixFS `multiblock.txt` fixture, an offline check verified SHA-256
  of the 245-byte root block and of each of the five leaf blocks against their
  CIDs. The root CID's digest began `c244a03f`. Concatenating the leaves in
  fixture order gave 1,026 bytes whose SHA-256 began `998785f1`.
  [UnixFS file test vectors](https://specs.ipfs.tech/unixfs/#file-test-vectors)

These checks used local data and a downloaded public fixture. Interoperability
with live gateways and pinning services remains a follow-up.

## Content claims and faces

A `content` namespace has an explicit map from canonical full names to entries
containing a required CID and optional declared media type. Start with raw blocks
and UnixFS file roots, with each CID identifying one entry. No implicit suffix
mapping, directory traversal, mutable IPNS target, or wildcard fallback in this
strategy. An absent entry is a miss within the claim. Reuse path and IRI
canonicalization, tenant selection, visibility, and claim administration.
Validate that each canonical key falls under its prefix, reject duplicates after
canonicalization, and parse CIDs structurally. Accept compatible CIDv0 input
and emit canonical lowercase base32 CIDv1. Validate media types as metadata.
Resolve historical requests against the selected historical registry or return
the existing history refusal. A current CID cannot establish a past answer.

| Face | Proposed answer |
| --- | --- |
| `urn:name:resolution` and standalone resolution route, JSON | Existing name, canonical name, claim, tenant and as-of context. Set `strategy` to `content` and `target` to `ipfs://<cid>`. Add `cid`, optional `gateway` and optional `media-type`. |
| Turtle | Same record with the IPFS target and optional gateway as IRI objects, and CID and media type as labeled literals. |
| Plain text and `urn:name:resolve` | Keep claim, strategy and target as the three tab-separated fields. The CID is in the IPFS target. |
| Document and standalone document route | With an explicit tenant gateway, optionally return a temporary redirect for public claims. Otherwise return an unsupported document response that directs the caller to the resolution record. A later opt-in mode can return verified bytes. |

Derive the optional gateway hint from a tenant-configured HTTPS gateway and the
validated CID. Omit it for non-public claims and when no gateway is configured.
Treat it as a retrieval hint, with no effect on identity. A browser following
the document redirect trusts the gateway's response. The resolution record
itself does no network I/O. Private claims retain the existing indistinguishable
miss for unauthorized callers. Owners must keep private material off public
IPFS, even if access to the resolver record is restricted.

## Identifiers at the IPFS boundary

Keep internal digests in their existing form, including `blake3:`. Wrap digest
bytes into a CID only where their meaning matches the block the CID names.
[CID specification](https://specs.ipfs.tech/cid/) ·
[Multicodec table](https://github.com/multiformats/multicodec/blob/master/table.csv)

| Input | Boundary representation and acceptance |
| --- | --- |
| Git object ID | CIDv1 with codec `git-raw` (`0x78`) and multihash matching the repository's object format. Wrap the SHA-1 or SHA-256 digest bytes directly. The block must be the uncompressed Git object with its `type size` and NUL header. Plain file bytes, compressed loose objects and pack entries have different encodings. Expose this as a later metadata adapter once a provider can serve the exact block. SHA-1 object IDs are legacy references and are excluded from verified delivery and snapshot roots. Test SHA-256 retrieval and decoding before enabling it. |
| BLAKE3 digest of one raw block | CIDv1 with `raw` (`0x55`) and `blake3` multihash (`0x1e`, use the 32-byte digest). Decode the native digest to bytes and wrap it. Require an exact raw-block import within the supported block size. Check reader and pinner support for the draft multihash code. |
| UnixFS import, including chunked files | Store the root CID returned by the importer or pinner. Chunking, DAG layout, wrapping and metadata affect the root. For a pin-by-CID service, the importer supplies the root and the pin response must confirm that same CID. Preserve a separate native file digest if needed. Never derive the UnixFS CID from it. Start the off-ramp with SHA-256 UnixFS roots for compatibility. |

A CID can be computed without making any bytes available. Publish a usable
reference only after a known provider can return the expected content.

### Adapter interoperability evidence (2026-11-22)

The [offline vectors](../../tests/ipfs_vectors.rs) check the exact digest bytes,
CIDv1 codec and multihash, and canonical base32 CID. Git 2.54.0 supplied the
object IDs for a `hello` blob in SHA-1 and SHA-256 repositories. The raw-block
vectors cover the empty block and the five bytes of `hello`. The empty digest
matches the [BLAKE3 reference vectors](https://github.com/BLAKE3-team/BLAKE3/blob/master/test_vectors/test_vectors.json).
The tests use
`cid` 0.11.3 and its `multihash` re-export for CID encoding and parsing.

| Form | Offline vector | IPFS read and pin | Recommendation |
| --- | --- | --- | --- |
| Git `git-raw` with SHA-1 | Pass | Unmeasured | Wait for an exact-block provider before exposing legacy metadata. Keep SHA-1 excluded from verified delivery and snapshot roots. |
| Git `git-raw` with SHA-256 | Pass | Unmeasured | Wait for exact-block retrieval, Git decoding, and pinning evidence. |
| `raw` with BLAKE3 | Pass | Unmeasured | Wait for the intended reader and pinner to handle the draft multihash code and for exact-block import limits to be confirmed. |

No IPFS reader or pinner was installed on the test host. The offline checks
establish the CID mapping. Both adapters remain disabled.

## Read-only off-ramp

Use a versioned public registry snapshot plus one immutable export per owner
and tenant. This snapshot is separate from the live `mirror` strategy, which
tracks an origin. It is also separate from the existing registry JSON face,
which serializes the loaded registry. Build the public snapshot with an
explicit field allowlist and owner approval. See the
[registry endpoint](../../src/lib.rs) and [strategy definitions](../../src/registry.rs).

1. **Export one consistent revision.** Each owner export has a manifest with
   schema version, registry identity, tenant, covered prefixes, and a canonical
   name index with resolution records and any bundled payloads. Use deterministic
   serialization and stable ordering. Enumerate the names the owner wants to
   preserve. Open-ended live claims cannot promise offline answers for names
   outside that index. Identify each bundled payload by CID
   and include it in the export DAG. Use IPLD links or UnixFS directory links
   for recursive pinning. CID strings in JSON alone do not link blocks.
   For a redirect, delegate or external mirror, preserve its public resolution
   metadata and declare any remaining live dependency. To promise offline
   document access, bundle an approved copy and verify that it is reachable
   from the export root. Include approved retirement tombstones so a stale
   record cannot revive a name within a newer snapshot. Keep external CID
   references as metadata unless the owner opted to bundle their bytes.
2. **Publish the registry snapshot.** Its allowlisted index maps tenant and
   claimed prefix to the export root CID, with a release sequence and timestamp.
   Add a registry-level root linking the index and each export DAG so a replica
   can retain the complete release. Owners can retain their own export
   independently. Freeze the bytes before pinning and verify the tenant,
   prefix coverage and expected registry revision before moving any pointer.
3. **Distribute an authenticated starting point.** Sign a release descriptor
   binding the registry identity, sequence, cutoff time, and registry snapshot
   CID. Distribute the descriptor and verification key independently of the
   resolver through owners, mirrors and offline backups. A reader with an
   authenticated checkpoint can start from its CID if the operator disappears.
   Validate the signature, registry identity and sequence before accepting a
   newer descriptor. Persist the highest accepted sequence to detect rollback
   for returning readers. A first-time or isolated reader can verify a known
   release but cannot prove it has the latest one. Key rotation or successor
   authority requires a separately authorized trust policy. The initial
   off-ramp freezes the last good release.
4. **Resolve from a verified snapshot.** Traverse the verified index using
   the same tenant and canonical-name rules, then open the selected verified
   owner export. Honor tombstones and distinguish a missing name from a missing
   block or unsupported snapshot format. Label answers with their snapshot CID
   and cutoff. Read-only clients expose no mutation or delegation by default.
   A frozen snapshot supports its stated cutoff, with no inferred history.
   Clients need the saved checkpoint or a surviving discovery channel. The
   original HTTP address itself requires continued control of its domain.

## Pinning, custody and payment

Have a dedicated export job hand a local node or an upload-capable provider the
DAG and record the root it reports. Keep a CAR (Content Addressable aRchive)
backup that can be imported with the same CIDs. An independent replica imports
the CAR or recursively pins the root by CID while the original can serve it.
For asynchronous pinning, wait for `pinned` and perform an independent full
retrieval and verification before advancing the release descriptor. A failed
new release leaves the previous checkpoint usable. Repeat retrieval checks and
restore a lost copy from a surviving replica or CAR backup.
[IPFS persistence](https://docs.ipfs.tech/concepts/persistence/) ·
[Pinning Service API](https://github.com/ipfs/pinning-services-api-spec)

The owner pays for and controls a copy of their export, using their own node
or an account they can renew and transfer. The registry operator funds a
registry snapshot and its linked exports during normal service. Require a
separately administered replica of the complete registry snapshot under a
designated custodian before promising operator-independent availability. Agree
who pays storage, serving and retrieval costs, how long historical releases
are retained, who renews each account, and who takes custody of CAR backups
before onboarding a namespace to the off-ramp. Shared billing or a shared
provider account can make two pins fail together. If funding ends, the
descriptor and CAR still enable restoration, while public retrieval depends on
someone actually serving the blocks.

## Client and verification

Start with no IPFS client inside the resolver. Clients can follow the IPFS
URI, use a gateway, or work from an offline CAR. Keep Kubo's administrative
HTTP RPC and pinning credentials in the external publishing environment.
For later verified delivery use an explicitly configured trustless HTTP gateway
through the existing HTTP capability boundary. A local node's read-only gateway
can implement that interface. Require host-scoped permission, enforce approved
destinations across redirects or disable them, and bound transfer size,
decoded size, DAG depth, block count and time.
[Kubo RPC reference](https://docs.ipfs.tech/reference/kubo/rpc/) ·
[Trustless Gateway specification](https://specs.ipfs.tech/http-gateways/trustless-gateway/)

- **Wrong bytes or incomplete DAG.** Verify raw responses against the requested
  block CID. For UnixFS, request CAR, anchor traversal to the expected root,
  verify each consumed block's codec and multihash, and reconstruct the file
  from authenticated links in order. Check completeness before serving or
  caching the result. A CAR header, gateway status, or signature on a release
  descriptor alone cannot prove the result is complete. Refuse unsupported
  codecs or hashes distinctly. Treat integrity failures separately from
  unavailability, and allow a retry from an independently configured source.
- **Lying gateway or publisher.** Verification protects against a gateway
  substituting bytes for a known CID. It cannot make a gateway answer or prove
  that a publisher selected appropriate content. Trust the live registry or
  authenticated snapshot for the name-to-CID binding. Treat media type as a
  publisher assertion and serve verified arbitrary files as downloads or on
  an isolated origin. Verified content can still contain active code.
- **Public exposure.** Public pins can be copied indefinitely, and gateway
  requests expose interest in a CID. Export only explicitly approved public
  records and bytes. Exclude private claims, administrative capabilities,
  credentials, logs and internal origins. Review the name index and transitive
  DAG for incidental disclosure. Retirement or unpinning affects future
  service and cannot recall copies. Private exports need a distinct storage
  and access design.
  [IPFS privacy and encryption](https://docs.ipfs.tech/concepts/privacy-and-encryption/)

## Proposed follow-up items

- **Add CID resolution metadata.** Validate content claims and emit the same
  identity in JSON, Turtle and plain text, with local tests for tenant and
  visibility boundaries, history and missing entries.
- **Check interoperability of Git and BLAKE3 CID adapters.** Use offline
  vectors for Git object formats and BLAKE3 raw blocks. Exercise real readers
  and pinners in an isolated setup before enabling either adapter.
- **Specify the export and authenticated checkpoint formats.** Define the
  public allowlist, DAG layout, signature envelope and retention of
  tombstones. Test a read-only resolver with a local CAR and the live resolver
  stopped, including missing blocks, wrong tenant and replay of older releases.
- **Exercise pin custody and recovery.** Test two independent copies of a
  synthetic registry DAG with no public announcement. Document cost ownership,
  renewals, cold retrieval, backup import and handoff of the checkpoint.
- **Prototype bounded verified document delivery.** Start with raw blocks and
  add UnixFS CAR traversal after testing malformed responses, wrong roots,
  omitted blocks, unsafe media types and resource limits.
  Prototyped as the opt-in `src/delivery.rs` (ledger #4045), off unless a host
  binds `urn:name:delivery-source`, and tested over canned gateway answers.
