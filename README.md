# ikigai-name

`urn:name:*` — **persistent-identifier resolution** as an [ikigai](https://github.com/ikigai-rs) module.

A curated IRI is meant to outlive the machine, the domain, and the organization
that first served it. This is the resolver behind such an IRI: it decides what a
name means, serves the document in whatever representation was asked for, and
refuses the things that would quietly break the promise.

## Quick start

```sh
cargo build --release --features server

# a directory holding registry.json and the documents it points at
ikigai-name-server --listen 127.0.0.1:8080 --root /srv/namespaces --cache-files
```

```sh
curl -H 'Accept: text/turtle' http://127.0.0.1:8080/resmud/core   # the graph
curl -H 'Accept: text/html'   http://127.0.0.1:8080/resmud/core   # the docs page
```

Embedded in a larger host instead, the module is just a space to bind:

```rust
let kernel = Kernel::new(Arc::new(ikigai_name::space()));
```

```sh
ikigai -c 'source urn:name:resolve path=resmud/core'
# resmud	hosted	urn:file:resmud-vocab
```

## Resolution is not redirection

A redirect service can express exactly one strategy. This one names three, per
namespace:

| strategy | answer | why it matters |
|---|---|---|
| `hosted` | documents served from here | the common case for a vocabulary you publish |
| `redirect` | the owner's own node | the only thing a PURL service can do |
| `mirror` | a copy, with `origin` still the source of truth | the namespace survives its origin going dark |

## One IRI, many representations

`rm:Weapon` **is** `https://iriref.org/resmud/core#Weapon`. A `.ttl` suffix for
RDF tools and a `.html` one for browsers would fork that single term into
several, and nothing puts it back together afterward. So `urn:name:document`
negotiates instead:

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

### Color is a floor, not a preference

Palettes are checked against the WCAG contrast floor using
[`ikigai-a11y`](https://github.com/ikigai-rs/ikigai-a11y), and **the check is a
test**: a palette whose body text falls below 4.5:1 fails the build. Body text,
muted text, links, and non-text borders are each held to the floor that applies
to them, inline code is checked against its own background rather than the
page's, and the high-contrast palette answers to AAA (7:1) — otherwise it is
just a third theme.

Selection is offered three ways on purpose: `prefers-color-scheme` for people
who set it once system-wide, an explicit chooser for everyone else, and a
high-contrast palette for people AA does not serve. `prefers-reduced-motion` is
honored unconditionally.

## Tenancy: conflicts are refused at claim time

Namespaces are owned — each carries the capability that administers it — and
prefixes **cannot overlap**. That could have been arbitrated per request with
precedence rules or longest-match; instead a claim is refused if any claimed
prefix contains it or it contains any claimed prefix. Afterward overlap is not
merely forbidden, it is unrepresentable, so **resolution never arbitrates and
two tenants cannot answer for the same IRI**.

Loading the file enforces the same rule. The registry is hand-edited, and a
hand can write `resmud` and `resmud/core` side by side, so the loader rebuilds
the registry by claiming each entry in turn — the one claim rule, not a second
copy of it — and refuses the **whole** file at the first entry that rule would
refuse, naming both prefixes. A bad registry is a configuration error of the
same shape as malformed JSON: nothing in it is served, rather than a partial
registry that would arbitrate by file order.

Comparison is segment-wise, never textual: `acme` contains `acme/vocab` and is
unrelated to `acmecorp`.

### Two authorities, because an unclaimed prefix has no owner

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

### Retirement, never release

There is no way to free a prefix. Handing a used one to a new owner would let
them answer for IRIs the previous owner minted — an identifier hijack, and the
single failure a permanence service must not have. `Delete` writes a tombstone
instead: the claim survives, the prefix can never be re-issued, and dereferencing
an IRI under it reports the retirement and its reason rather than pretending the
namespace never existed.

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
  ],
  "limits": { "max_document_bytes": 8388608 }
}
```

It is read back **through the kernel** from `urn:name:registry-source`, never
off the filesystem. Three things follow: an operator edits a resolution rule
without a rebuild; the read is golden-threaded, so the edit takes effect on the
next request; and a deployment can re-bind that IRI to a file, a peer, or a
store without this crate knowing which — which is also the migration path when
files stop being enough.

`limits` are the ceilings this deployment holds itself to, kept here because the
registry is already the operator's editing surface and a second config channel
is a second thing to keep in step. The resolver reads whole documents into
memory, so an unbounded one is the only way a single request can hurt the host;
exceeding the ceiling is refused with an error naming the setting that would
raise it.

## Endpoints

| IRI | verb | what |
|---|---|---|
| `urn:name:document` | Source | the namespace document, negotiated (`as=`) |
| `urn:name:docs` | Source | the namespace as HTML; with `term=`, one htmx fragment |
| `urn:name:resolve` | Source | how a path resolves, or a `NotFound` naming it |
| `urn:name:registry` | Source | every claimed namespace, as JSON |
| `urn:name:health` | Source | what this deployment holds, and what it refuses |
| `urn:name:claim` | Sink | claim a prefix nobody holds |
| `urn:name:admin` | Sink · Delete | change how a namespace resolves · retire it |

Every read face takes its input by name or from piped `content` (`path` for
`resolve` and `document`, `prefix` for `claim` and `admin`), so each is a
pipeline citizen: `echo acme | sink urn:name:claim strategy=hosted source=…`.

### Conformance

The module passes [`ikigai-conformance`](https://github.com/ikigai-rs/ikigai-conformance)
— `tests/conformance.rs` walks every endpoint above and holds the five read
faces to their `.cacheable()` marking over a threaded registry, then shows the
same walk over a live registry reporting the downgrade on all five. What the
suite cannot see is pinned by hand in the same file: a forced recomputation
returns the same bytes; a claim overlapping a held prefix is a typed, permanent
refusal that writes nothing; every declared output is the media type served;
required means required; and the per-prefix administrative rule is reached only
under a grant on some other prefix.

## Cacheability

**Every read face marks itself cacheable, and that is a claim about the
computation, not the inputs.** The kernel takes the meet of a result's own
expiry with its dependencies', so a volatile source still yields a volatile
answer — but a face that never marks itself cacheable can never be cached
*however* cacheable its sources are, and nothing in the type system says so.
Every read face here was exactly that until it was measured: a documentation
view re-ran its SPARQL query on every request. There are tests pinning both
directions.

That chain is only as good as its bottom. `ikigai-fs` reads are **uncacheable by
default**, correctly — a golden thread is cut by a write *through the kernel*,
and a `git pull` is not one. `--cache-files` opts in for the one deployment
shape where that cannot bite: a `--root` that changes only as part of a deploy,
which restarts the process anyway.

`urn:name:health` reports the counts, the ceiling in force, and which faces
claim cacheability. It is **not** cache eviction — that belongs to the kernel,
and a corpus of a few hundred kilobytes would never exercise a policy anyway.
It is the instrumentation such a policy would need, and that an operator wants
long before one exists.

## Running the server

The binary is a **host**: it binds `ikigai-name`, `ikigai-fs`, `ikigai-sparql`
and `ikigai-web` into a kernel and puts HTTP on it. The library knows nothing
about files, ports, or HTTP. Because the feature list is the module manifest,
what this process *cannot* do — calendars, exec, secrets — is a property of the
build rather than of a config file someone might edit.

| flag | default | |
|---|---|---|
| `--listen ADDR` | `127.0.0.1:8080` | address to bind |
| `--root DIR` | `.` | directory `urn:file:*` resolves within, and the jail |
| `--registry NAME` | `registry.json` | the registry file within `--root` |
| `--cache-files` | off | cache file reads; see [Cacheability](#cacheability) |

The public surface is two routes, served `routes_only` so anything else 404s
before it reaches a resource:

```text
/{ns}          → urn:name:doc:{ns}
/{ns}/{doc}    → urn:name:doc:{ns}/{doc}
```

Administration is deliberately absent from it. The server holds a **read-only**
capability jailed to `--root`, so `urn:name:claim` and `urn:name:admin` cannot be
performed by this process however it is addressed — they belong on a local or
authenticated transport.

## Known limitations

- **A `redirect` namespace answers HTTP 500.** The module reports the target,
  but the HTTP face has no mapping for "this is somewhere else", so it surfaces
  as a server error rather than a `303`. Until that lands in `ikigai-web`, only
  `hosted` and `mirror` namespaces are servable over HTTP.
- **A retired namespace answers `404`, not `410`.** The distinction — *gone* as
  against *never here* — is exactly what a tombstone exists to express, and it
  is lost at the HTTP boundary for the same reason.
- **Registry writes are read-modify-write with no compare-and-set**, so two
  racing administrative writes can lose one. Acceptable while writes are rare
  and operator-driven; the fix is a validity-token precondition on the Sink, not
  a lock.

## Status

M1 complete: registry, claim rule, resolution, capability-scoped administration,
the HTML documentation face, content negotiation, limits, cacheability, and
conformance. Not yet published to crates.io.

Next: as-of resolution backed by the vocabulary's own git history, signed
redirect provenance and succession, and peer mirroring — the properties that
make a permanence promise credible.

## License

MIT or Apache-2.0, at your option.
