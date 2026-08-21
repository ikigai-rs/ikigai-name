# ikigai-name

`urn:name:*` — **persistent-identifier resolution** as an [ikigai](https://github.com/ikigai-rs) module.

A curated IRI is meant to outlive the machine, the domain, and the organisation
that first served it. This is the resolver behind such an IRI.

## Resolution is not redirection

A redirect service can express exactly one strategy. This one names three, per
namespace:

| strategy | answer | why it matters |
|---|---|---|
| `hosted` | documents served from here | the common case for a vocabulary you publish |
| `redirect` | the owner's own node | the only thing a PURL service can do |
| `mirror` | a copy, with `origin` still the source of truth | the namespace survives its origin going dark |

## Tenancy: conflicts are refused at claim time

Namespaces are owned — each carries the capability that administers it — and
prefixes **cannot overlap**. That could have been arbitrated per request with
precedence rules or longest-match; instead a claim is refused if any claimed
prefix contains it or it contains any claimed prefix. Afterwards overlap is not
merely forbidden, it is unrepresentable, so **resolution never arbitrates and
two tenants cannot answer for the same IRI**.

Comparison is segment-wise, never textual: `acme` contains `acme/vocab` and is
unrelated to `acmecorp`.

## The registry is data, not code

```json
{
  "namespaces": [
    {
      "prefix": "resmud",
      "owner": "urn:cap:name:admin:resmud",
      "strategy": "hosted",
      "source": "urn:file:resmud-vocab"
    },
    {
      "prefix": "acme",
      "owner": "urn:cap:name:admin:acme",
      "strategy": "redirect",
      "target": "https://acme.example/ns"
    }
  ]
}
```

It is read back **through the kernel** from `urn:name:registry-source`, never
off the filesystem. Three things follow: an operator edits a resolution rule
without a rebuild; the read is golden-threaded, so the edit takes effect on the
next request; and a deployment can re-bind that IRI to a file, a peer, or a
store without this crate knowing which — which is also the migration path when
files stop being enough.

## Two authorities, because an unclaimed prefix has no owner

A prefix nobody holds cannot be gated by its own administrative capability, so
claiming and administering are separate:

| capability | grants |
|---|---|
| `urn:cap:name:claim` | may become a tenant at all — the signup gate |
| `urn:cap:name:admin:<prefix>` | owns that namespace: may change how it resolves, or retire it |

Actions declare the **wildcard** (`urn:cap:name:admin:*`) — the coarse floor the
kernel enforces — and the exact per-prefix grant is checked at invocation, since
only that code knows which prefix a request names. Holding one namespace's admin
capability therefore confers nothing over anyone else's.

## Retirement, never release

There is no way to free a prefix. Handing a used one to a new owner would let
them answer for IRIs the previous owner minted — an identifier hijack, and the
single failure a permanence service must not have. `Delete` writes a tombstone
instead: the claim survives, the prefix can never be re-issued, and dereferencing
an IRI under it reports the retirement and its reason rather than pretending the
namespace never existed.

## Endpoints

| IRI | verb | what |
|---|---|---|
| `urn:name:registry` | Source | every claimed namespace, as JSON |
| `urn:name:resolve` | Source | how a path resolves, or a `NotFound` naming it |
| `urn:name:claim` | Sink | claim a prefix nobody holds |
| `urn:name:admin` | Sink · Delete | change how a namespace resolves · retire it |

```sh
ikigai -c 'source urn:name:resolve path=resmud/core'
# resmud	hosted	urn:file:resmud-vocab
```

## Status

M1. Registry, claim rule, resolution, and capability-scoped administration. Still to come: as-of resolution backed
by the vocabulary's own git history, signed redirect provenance and succession,
and peer mirroring — the properties that make a permanence promise credible.
