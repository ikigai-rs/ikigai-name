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
| `urn:name:docs` | Source | the namespace as HTML; with `term=`, one htmx fragment |
| `urn:name:document` | Source | the namespace document, negotiated (`as=`) |

## One IRI, many representations

`rm:Weapon` **is** `https://iriref.org/resmud/core#Weapon`. A `.ttl` suffix for
RDF tools and a `.html` one for browsers would fork that single term into
several, so `urn:name:document` negotiates instead:

| `as=` | answer |
|---|---|
| *(absent)* or `text/turtle` | the document as stored — Turtle is the hub |
| `text/html` | the documentation face |
| anything else | reached by selecting a transreptor chain and driving it |

Only the first two are produced here. Every other format arrives through the
kernel's transreptor selection — the same mechanism the `Meta` path uses — so
this crate gains every format the host has a transreptor bound for and carries
conversion code for none of them. When nothing reaches the requested type, the
error names what *is* available and what would fix it.

Media-type parameters do not defeat negotiation: `text/turtle; charset=utf-8` is
Turtle.

**The ETag is not minted here.** A representation already carries a content
address, and the HTTP face turns that into an `ETag` and answers
`If-None-Match`; minting a second one at this layer would give the same bytes
two identities.

## The documentation face

A vocabulary only a parser can read will not be adopted. `urn:name:docs` renders
a hosted namespace as a page, **transrepted from the same Turtle the namespace
serves** — so the documentation cannot drift from the vocabulary, because there
is no second copy of it. Terms are extracted with SPARQL issued back through the
kernel, so this crate carries no RDF parser and a mirrored namespace is read
exactly like a local one.

htmx does the interaction: term buttons `hx-get` their own detail into a live
region, and the theme chooser is a real `<form method="get">` that htmx upgrades
rather than replaces — so it works with JavaScript disabled.

### Colour is a floor, not a preference

Palettes are checked against the WCAG contrast floor using
[`ikigai-a11y`](https://github.com/ikigai-rs/ikigai-a11y), and **the check is a
test**: a palette whose body text falls below 4.5:1 fails the build. Body text,
muted text, links, and non-text borders are each checked against the floor that
applies to them, and the high-contrast palette is held to AAA (7:1) rather than
AA — otherwise it is just a third theme.

Selection is offered three ways on purpose: `prefers-color-scheme` for people
who set it once system-wide, an explicit chooser for everyone else, and a
high-contrast palette for people AA does not serve. `prefers-reduced-motion` is
honoured unconditionally.

```sh
ikigai -c 'source urn:name:resolve path=resmud/core'
# resmud	hosted	urn:file:resmud-vocab
```

## Status

M1. Registry, claim rule, resolution, capability-scoped administration, the HTML
documentation face, and content negotiation. Still to come: as-of resolution backed
by the vocabulary's own git history, signed redirect provenance and succession,
and peer mirroring — the properties that make a permanence promise credible.
