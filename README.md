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

`space()` names itself `urn:iki:space:name` (`ikigai_name::SPACE_ID`), so
topology, `answered_by` and the space diagrams show it by that name.

```sh
ikigai -c 'source urn:name:resolve path=resmud/core'
# resmud	hosted	urn:file:resmud-vocab
```

## Resolution is not redirection

A redirect service can express exactly one strategy. This one names three, per
namespace:

| strategy | answer | why it matters |
|---|---|---|
| `hosted` | documents served from here, from a bound resource or [a file in someone's git repository](#a-namespace-served-from-a-git-repository) | the common case for a vocabulary you publish |
| `redirect` | the owner's own node, with a [status and rewrite rules](#redirects-statuses-and-rewrite-rules) | the only thing a PURL service can do |
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
kernel, so a mirrored namespace is read exactly like a local one. Hosted-name
membership in `terms` mode uses a separate Turtle reader.

The page works with JavaScript off, and htmx only improves it. Each term is a
link to the page with that term's detail filled in; with htmx loaded, the same
URL is fetched and only the detail region's contents are swapped, so its live
region announces the term and focus stays on the link. The theme chooser is a
plain `<form method="get">` with a visible Apply button. The stylesheet
(`urn:name:style`) and htmx are separate files at `/-/style` and `/-/htmx`, never
inline, because a door's default Content-Security-Policy refuses an inline
`<style>`; a host that serves the page routes those two paths.

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

### Tenants by host

The rule above is about prefixes inside one registry. One deployment can also
answer for several domains, each a **different set of hosted identifiers**:
multi-tenancy, not aliasing. `https://urnid.org/resmud/core#Weapon` and
`https://iriref.org/resmud/core#Weapon` are two terms, so each host claims its
prefixes independently, and a term lives in exactly one tenant: the one its IRI's
host names.

```sh
ikigai-name-server --root /srv/tenants \
  --tenant iriref.org --tenant urnid.org --tenant idref.org
```

```text
/srv/tenants/iriref.org/registry.json     the same layout a single --root holds
/srv/tenants/iriref.org/vocab/core.ttl    urn:file:vocab/core.ttl, for iriref.org
/srv/tenants/urnid.org/registry.json
/srv/tenants/urnid.org/vocab/core.ttl     urn:file:vocab/core.ttl, for urnid.org
```

| keyed on `Host` | how |
|---|---|
| storage | each tenant's `urn:file:*` is jailed to its own directory, and its capability reads only that directory |
| config | each tenant's `registry.json`: its claims, strategies, redirect targets and `limits` |
| caches | the door mints `urn:cap:name:tenant:<host>` into the request's capability, and the kernel's cache key holds the capability, at every depth of a call. An HTTP cache keys on the host already, as part of the URL |
| generated documents | the page and its links are relative or come from that tenant's vocabulary; a term on another host is listed under "Defined elsewhere", linked to where it lives, and `term=` refuses it with that address |

**Splitting a tenant off is a copy and a DNS change.** Its directory is already a
whole `--root`, so nothing in it is rewritten and it answers the same bytes alone
(`a_tenants_directory_splits_off_unchanged`).

Only `Host` picks the tenant. Not a query argument, and not `X-Forwarded-Host`,
which a client can write: a proxy in front must pass `Host` through (Apache:
`ProxyPreserveHost On`). A host that is not a tenant answers `421` before a route
is matched, and a request without exactly one `Host` naming a host answers `400`.
A bad `--tenant`, a missing directory, or two tenants resolving to shared files
stops the server at startup. With no `--tenant`, one set of namespaces in `--root`
answers for any host, as it always has.

A host embedding the module gets the same partition by minting
[`tenant_scope`](src/tenant.rs) into the capability it issues under, and binding
a `urn:name:registry-source` that keys on `tenant_of`.

**Not enforced: Turtle is served as stored.** A tenant's vocabulary may state
anything about any IRI. A file on urnid.org that declares an iriref.org term
still says so in its Turtle. The documentation face refuses to present that term
as urnid.org's, and hosted `names: "terms"` membership also checks the tenant.

### Two authorities, because an unclaimed prefix has no owner

A prefix nobody holds cannot be gated by its own administrative capability, so
claiming and administering are separate:

| capability | grants |
|---|---|
| `urn:cap:name:claim` | may become a tenant at all — the signup gate |
| the namespace's `owner` | owns that namespace: may change how it resolves, or retire it |

A namespace's `owner` is `urn:cap:name:admin:<prefix>` unless its claim says otherwise.
An operator provisioning a namespace on someone else's behalf can name another with
`owner=`, and that one must also start `urn:cap:name:admin:`, because that is the family
the kernel's floor admits. Whichever capability the registry records is the only one that
administers the namespace, reads its counts and reads what happened to its repository;
`urn:cap:name:admin:<prefix>` confers nothing over a namespace owned by something else.

Actions declare the **wildcard** (`urn:cap:name:admin:*`) — the coarse floor the
kernel enforces — and the exact owner is checked at invocation, since only that
code knows which namespace a request names and who owns it. Holding one namespace's
admin capability therefore confers nothing over anyone else's. A namespace that is
not public answers as one nobody claims to anyone not holding its owner, here as
everywhere.

**A prefix never begins with `-`.** The prefix sits where `ikigai-core` (0.1.86 and
later) reads a leading `-` as an *exclusion*: `urn:cap:name:admin:-acme` is a
deny-shaped scope that core keeps through every narrowing and never grants, so a
namespace called `-acme` could never be administered by anyone, its claimant
included. `urn:name:claim` and `urn:name:admin` therefore refuse such a prefix as
an invalid argument that says why, before any capability check, and a claim
refuses an explicit `owner` that is itself deny-shaped. A `-` anywhere else
(`acme-corp`, `acme/-draft`) is an ordinary character. A registry file is held to
the same rule: one with a `-acme` entry is refused whole, naming it.

### Retirement, never release

There is no way to free a prefix. Handing a used one to a new owner would let
them answer for IRIs the previous owner minted — an identifier hijack, and the
single failure a permanence service must not have. `Delete` writes a tombstone
instead: the claim survives, the prefix can never be re-issued, and dereferencing
an IRI under it reports the retirement and its reason rather than pretending the
namespace never existed.

### Conditional registry writes

Claim, update, and retirement read the registry through the kernel, retain the
Source representation's content ID, and send it as an inline `if-match` argument
alongside `content` on the Sink to `urn:name:registry-source`. The token identifies
the original representation (media type and bytes), including its JSON formatting;
it is not the hash of the parsed or reserialized registry.

A writable registry binding must compare that token with its current Source
content ID and replace the document **atomically**. A stale token must leave the
registry unchanged and return `Error::Conflict` naming the current token. The
module propagates that refusal without retrying: read again and reconsider the
edit against the new registry. Two writers from the same token can therefore
commit at most one different replacement, without a module-level lock.

This is a backing-store contract, not a generic kernel precondition: never bind
an unconditional Sink that ignores `if-match`. Read-only bindings still work for
resolution. The standalone server's registry alias is Source-only; this change
does not add file compare-and-set support or expose administration over HTTP.

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

## Redirects: statuses and rewrite rules

A `redirect` namespace can move an existing URL scheme, or reshape it, as data in
the registry. It holds a `status` and an ordered list of `rules`:

```json
{
  "prefix": "catalog",
  "owner": "urn:cap:name:admin:catalog",
  "strategy": "redirect",
  "target": "https://new.example/catalog",
  "status": 302,
  "rules": [
    { "match": "item/{year}/{id}", "target": "https://new.example/items/{id}?year={year}" },
    { "match": "docs/{rest*}", "target": "https://new.example/manual/{rest}", "status": 308 }
  ]
}
```

`urn:name:resolve` answers the prefix, the strategy, the target and the status,
separated by tabs, and then the `match` of the rule that answered, if one did:

```sh
ikigai -c 'source urn:name:resolve path=catalog/item/2019/42'
# catalog	redirect	https://new.example/items/42?year=2019	302	item/{year}/{id}
ikigai -c 'source urn:name:resolve path=catalog/docs/setup/linux'
# catalog	redirect	https://new.example/manual/setup/linux	308	docs/{rest*}
ikigai -c 'source urn:name:resolve path=catalog/anything-else'
# catalog	redirect	https://new.example/catalog	302
```

`urn:name:document` answers the same target and status as core's typed redirect, so
the standalone server answers `GET /catalog/anything-else` with `302 Found` and
`Location: https://new.example/catalog`, whatever `Accept` asks for.

### Which status

`status` is one of 301, 302, 303, 307 or 308, and 302 if absent. Any other value
refuses the whole registry, naming the namespace.

| status | for |
|---|---|
| 302, 307 | a logical name pointing at a document that will move. A permanent code makes clients remember the target and skip the name. 307 also keeps the request's method |
| 303 | a name for a **thing**, answered by a document about it |
| 301, 308 | an old URL moving permanently onto a logical name. 308 also keeps the request's method |

### How a rule matches

- Rules are tried in order, and the **first that matches answers**. With no match,
  the namespace's own `target` and `status` apply, and that target is answered as
  written: nothing is appended to it.
- A `match` is relative to the prefix, so the rule above answers
  `catalog/item/2019/42`, and renaming a prefix leaves its rules alone.
- A `match` is segments separated by `/`, each one literal text or one whole capture.
  `{name}` takes one segment. `{name*}`, last only, takes the rest of the path, at
  least one segment. `{id}.html` is refused, because a capture is a whole segment.
- A `target` writes either kind of capture as `{name}`, as often as it likes. A
  rule's optional `status` overrides the namespace's.
- `resmud/core` and `/resmud//core` are one path here as everywhere, so doubled
  separators never reach a capture.

A `target` is an RFC 6570 Level 1 template, parsed and encoded by `ikigai-core`'s
`UriTemplate`. A `match` is a small syntax of this crate's, because RFC 6570 defines
expansion and not matching. See [`src/rules.rs`](src/rules.rs).

### What a request cannot do

A captured value comes from whoever made the request, and it can never change the
host, the scheme or the number of segments a rule's target has:

| in the path | what happens |
|---|---|
| `%2F` | `%` is encoded too, so it arrives as the text `%252F`, never a separator |
| `..` or `.` | matches no capture, so that rule does not match. Encoding cannot help: `.` needs no encoding, and URL parsers read `%2E%2E` as `..` |
| `//evil.example` | the doubled separator collapses, so `evil.example` is an ordinary segment under the rule's host |
| `https://evil.example/x` | too many segments for `{name}`. Under `{name*}` it stays below the rule's host, as `https%3A/evil.example/x` |

The `/` between the segments of a `{name*}` stays a `/`, since that is what the
capture is for. Everything else outside `A-Z a-z 0-9 - . _ ~` is percent-encoded.

### What refuses the registry

A bad rule refuses the **whole** registry, by the same all-or-nothing rule as
overlapping prefixes, with a sentence naming the namespace and the rule:

```text
the namespace "catalog" cannot be claimed: in rule 2 ("docs/{rest*}"), its target uses {path}, which its match does not capture
```

| refused | because |
|---|---|
| a target using a capture its match does not define | there is nothing to fill it |
| a capture defined twice in one match | one of them would be lost |
| a template that does not parse, or `{x}{y}` in a target | RFC 6570 Level 1 here refuses adjacent captures as ambiguous |
| `{name*}` before the end of a match, or `{name*}` in a target | it takes the rest of the path; a target writes `{name}` |
| a target not beginning with a literal scheme and host, ended by `/`, `?` or `#` before its first capture | `https://{x}/` and `https://new.example{x}` would let a request choose the host |
| spaces or control characters in a target, or control characters in a match | they are not URLs, and a tab would break the tab-separated answer |
| a misspelled field in a rule | a typo in `status` must not silently mean the namespace's |

`urn:name:claim` and `urn:name:admin` take the same `status`, and `rules` as a JSON
list, and are held to the same checks. Setting a strategy replaces the old one whole,
so an update that names neither returns the namespace to 302 and no rules.

### A worked migration

An organization has `https://old.example/reports/2019/42`, wants it at
`https://archive.example/2019-reports/42`, and wants the next move to need no
further change for anyone who linked to it.

**1. Put a logical name in front of the old scheme.** Claim `reports` as a redirect,
still pointing at the old host, with the old shape kept by a rule. Publish
`https://iriref.org/reports/2019/42` as the name from now on. 302, because the
target is going to move:

```json
{
  "prefix": "reports",
  "owner": "urn:cap:name:admin:reports",
  "strategy": "redirect",
  "target": "https://old.example/reports",
  "rules": [
    { "match": "{year}/{id}", "target": "https://old.example/reports/{year}/{id}" }
  ]
}
```

**2. Point the old URLs at the names.** On `old.example`, answer a permanent 301 or
308 from `/reports/{year}/{id}` to `https://iriref.org/reports/{year}/{id}`. That
redirect is the old host's own. A client now remembers the name and not the old host.

**3. Move the documents, and change one rule.** The name stays and the reshape is an
edit to the registry, with no rebuild. `as-of` still reports the old answer for a date
before the edit:

```json
{ "match": "{year}/{id}", "target": "https://archive.example/{year}-reports/{id}" }
```

```sh
ikigai -c 'source urn:name:resolve path=reports/2019/42'
# reports	redirect	https://archive.example/2019-reports/42	302	{year}/{id}
ikigai -c 'source urn:name:resolve path=reports/2019/42 as-of=2026-03-31'
# reports	redirect	https://old.example/reports/2019/42	302	{year}/{id}
```

## URN and tag names

A claim can hold an IRI prefix as well as a path: a `urn:` or a `tag:`, whose segments are
separated by `:` rather than `/`. The claim rule is the one paths follow: no two claims
overlap, refused when a claim is made and when the registry is loaded, compared segment by
segment, so `urn:urnid:acme:` holds `urn:urnid:acme:item:42` and has nothing to do with
`urn:urnid:acmecorp:`. A path claim never overlaps an IRI claim.

```json
{
  "unclaimed": {"strategy": "delegate", "upstream": "https://urnid.org/"},
  "namespaces": [
    {"prefix": "urn:urnid:acme:", "owner": "urn:cap:name:admin:acme",
     "strategy": "redirect", "target": "https://acme.example/ns", "status": 303,
     "rules": [{"match": "item/{id}", "target": "https://acme.example/items/{id}"}]},
    {"prefix": "tag:urnid.org,2026:gonk:", "owner": "urn:cap:name:admin:gonk",
     "strategy": "hosted", "source": "urn:file:gonk.ttl"},
    {"prefix": "urn:urnid:drafts:", "owner": "urn:cap:name:admin:drafts",
     "strategy": "hosted", "source": "urn:file:drafts.ttl", "public": false},
    {"prefix": "urn:urnid:elsewhere:", "owner": "urn:cap:name:admin:elsewhere",
     "strategy": "delegate", "upstream": "https://urnid.org/"}
  ]
}
```

### The urnid equivalence

`urn:urnid:{owner}:{local}` and `tag:urnid.org,2026:{owner}:{local}` are one name, by one rule
that works both ways: each is compared in its canonical form, the `urn:` one. The scheme, the
URN namespace identifier and the tag's authority compare case-insensitively; the **owner label
compares case-insensitively** (it is lowercased); the **local part is case-sensitive**, apart
from percent-encoding normalization (`%7e`, `%7E` and `~` are one name, `%2f` is `%2F`). So
`tag:urnid.org,2026:ACME:Item` is `urn:urnid:acme:Item`, and it is not `urn:urnid:acme:item`.
A claim on either form holds both, and claiming the other form is an overlap.

### Resolution as data

```sh
ikigai -c 'source urn:name:resolution:iri:tag:urnid.org,2026:acme:item:42'
# {"name": "tag:urnid.org,2026:acme:item:42", "canonical": "urn:urnid:acme:item:42",
#  "claim": "urn:urnid:acme:", "strategy": "redirect",
#  "target": "https://acme.example/items/42", "status": 303, "rule": "item/{id}"}
ikigai -c 'source urn:name:resolve path=urn:urnid:acme:item:42'
# urn:urnid:acme:	redirect	https://acme.example/items/42	303	item/{id}
```

`urn:name:resolution:iri:{iri}` (or `urn:name:resolution` with `iri=`) finds the one claim
containing a name, a path or an IRI, and answers its claim, strategy and target, the tenant it
was asked of and, with `as-of`, the instant. `as=text/turtle` answers the same record as
Turtle, and `as=text/plain` the line `urn:name:resolve` prints, which is computed from the same
resolution: `urn:name:resolve path=…` keeps working, for paths and IRIs alike.

### The same answer

A namespace with `"public": false` answers only a capability holding its `owner`. To anyone
else, a name under it is answered exactly as a name nobody claims: the same refusal, in the
same words, or the same forwarding when unclaimed names are delegated. Asking cannot tell a
private name from an unknown one.

### Delegating to an upstream resolver

A host such as gonk runs a local resolver that answers for its own names and passes the rest
upward. Strategy `delegate` forwards a name to its `upstream` resolver, for one claim, or as the
registry's `unclaimed` strategy, for every name no claim holds. Strategies stay data: `unclaimed`
takes nothing but a `delegate`, and a later strategy is an addition to the list.

Forwarding goes through `urn:name:delegate`, which declares `urn:cap:net:*` and refuses a
capability not holding `urn:cap:net:<upstream's host>` exactly, before anything leaves. It reads
the upstream's resolution at `{upstream}/-/resolution/iri/{name}` and its document at
`{upstream}/{name}` through `urn:http:get`, and passes the answer back as it came, a refusal
included. With `as-of`, it forwards the normalized UTC instant as a query parameter on either
face, so the upstream answers for the same date. Historical answers are never marked cacheable.
A host that binds no HTTP client delegates nothing and says so; the standalone server is such
a host.

### Over HTTP

`/{iri}` resolves on the tenant the `Host` names: `/urn:urnid:acme:Weapon` and
`/tag:urnid.org,2026:acme:Weapon` serve a hosted or mirrored namespace's document, negotiated
like any path. `/-/resolution/iri/{iri}` answers the resolution as data, which is what a delegating
resolver reads. A `redirect` answers its status with a `Location`, and a retired
namespace answers `410 Gone` with its reason, where a name nobody claims answers `404`.

## As of a date

`urn:name:document`, `urn:name:resolve` and `urn:name:registry` take `as-of`:
what a name meant at a date, or what the namespace policy was in March. It is a
**resolution parameter, never part of the name**, so an IRI stays one IRI at
every date. `YYYY-MM-DD` means the end of that day in UTC; an RFC 3339 instant
means that instant.

```sh
ikigai -c 'source urn:name:document path=resmud/core as-of=2026-03-31'
ikigai -c 'source urn:name:registry as-of=2026-03-31'
```

The namespace comes from the registry as it stood then and the document from the
source as it stood then, so a namespace retired since still answers for a date
when it was in force, and one claimed since answers `NotFound` for a date before.
A redirect's status and rules are the ones the registry held at that date too.
Only today's `limits` apply, because they are what this deployment holds itself
to, whatever an old file allowed.

**History is the archive; the cache is not.** A past answer is read from the
history of the repository holding the vocabulary and is never marked cacheable
here, so its correctness cannot depend on eviction.

| what | needs |
|---|---|
| any past answer | a binding at `urn:name:history-source`, taking `source` and `as-of` |
| `repo_history(dir, registry_file)`, the binding over git | `urn:repo:log`, and `urn:repo:show` |

**This is not usable from a stock `ikigai-repo` yet.** `urn:repo:log` picks the
commit, but no release serves a file at a commit, so a host using `repo_history`
must bind `urn:repo:show` itself, to the contract in
[`src/history.rs`](src/history.rs).

**The standalone server binds no history**, so it answers `as-of` with `501` and a
sentence saying why, rather than today's document. Binding one means linking an
exec module into an internet-facing binary this crate keeps free of them, which
is a decision of its own.

**The documentation page does not answer as of a date**: `as-of` with `text/html`
is refused, and Turtle is what a past read serves.

`repo_history` chooses by `urn:repo:log`'s **author** date, the only one it
reports, so a commit rebased or merged long after it was written counts from when
it was written. It follows one file name, so a date before a rename finds nothing.
Git records no media type, so a past document's type comes from its file name
(`.ttl`, `.nt`, `.nq`, `.n3`, `.trig`, `.rdf`, `.xml`, `.json`, `.jsonld`, `.txt`,
`.md`), and any other name answers whatever `urn:repo:show` typed it as.

## A namespace served from a git repository

An owner can point a logical identifier at a file in their repository and change what it
answers by pushing, with no server of their own. A `hosted` or `mirror` namespace names
the repository in the registry, and its `source` or `origin` is `urn:name:git:{prefix}`:

```json
{
  "prefix": "acme",
  "owner": "urn:cap:name:admin:acme",
  "strategy": "mirror",
  "origin": "urn:name:git:acme",
  "repository": {
    "url": "https://github.com/acme/vocab",
    "branch": "main",
    "path": "vocab/core.ttl",
    "shape": "urn:file:shapes/acme.ttl",
    "private": true
  }
}
```

**A request never reaches the repository.** A read answers from the last good copy, in a
store. Only a refresh reads the repository, so a client cannot make this host fetch or slow
a resolution by way of a slow forge, and a request keeps the read-only authority it has
always had. A host issues a refresh when a push webhook arrives and on a timer.

| when | what happens |
|---|---|
| a push, webhook signature good | the file is read and checked. If it passes it is stored, and storing it cuts the golden thread, so the next read serves it |
| a push to another branch | nothing is read |
| nothing changed | nothing is written and nothing is cut |
| the new version fails its shape | it is refused, the last good one keeps serving, and the reason is recorded |
| the file is not Turtle, is empty, or is over `limits.max_document_bytes` | refused the same way |
| the check cannot run (shapes unreadable, no validator bound) | nothing is accepted, and that is recorded rather than blamed on the push. An unchecked version is never served |
| the repository, branch or file is gone (`NotFound` from the reader) | a `mirror` keeps serving its last good copy, and a `hosted` namespace answers `404`, because deleting the repository is how such an owner withdraws it. Either comes back when the file does |
| any other failed read, such as a forge being down or a rejected token | recorded, and both keep serving |
| no version has passed yet | a read answers `503`, naming where its administrator can read why |

**Validation is SHACL, through `urn:shacl:validate`.** `shape` names the shapes every new
version must conform to, and must be a `urn:file:` IRI.

**The owner reads what happened** at `urn:name:git-status:{prefix}`, under
the namespace's `owner` or the operator's root capability, as one labeled line each:

```text
repository: https://github.com/acme/vocab, branch main, file vocab/core.ttl
serving: b3:8d9c5c…, since 2026-11-05T12:00:00Z
refused: b3:41f07a…, at 2026-11-05T13:00:00Z: it does not conform to urn:file:shapes/acme.ttl: https://acme.example/ns#Thing, at http://www.w3.org/2000/01/rdf-schema#comment: http://www.w3.org/ns/shacl#MinCountConstraintComponent
```

A version is named by its content ID, because a file read at a branch's head does not say
which commit it came from. Times need a kernel with a clock.

### What a repository can supply

A repository's bytes reach the resolver's origin as Turtle or not at all:

- The path must end `.ttl`, so a registry cannot name `index.html` or `app.js`. RDF/XML is
  left out on purpose, as a browser runs a script inside XML.
- The media type served is this module's, never the repository's or the reader's. A version
  a reader labels as HTML, JavaScript or SVG is refused, and the validator's parse refuses
  anything else that is not Turtle.
- The only HTML is the [documentation page](#the-documentation-face), generated from the
  graph with every name and comment escaped.

### Authority

A registry is written by claimants, and a refresh holds authority a claimant does not, so:

| threat | what stops it |
|---|---|
| a token in the registry | there is no field for one. `token` refuses the whole file, and so does a URL with credentials in it, and neither refusal repeats the value |
| one namespace using another's token, by naming it and a host of its own | a token's secret name is derived from the namespace, `name-git-token.{prefix}`, with every character but letters, digits, `-` and `_` written `~XX`, and with tenants `name-git-token.{tenant}.{prefix}`. The registry says only `private` |
| a shape pointed at a secret, to read it back through a parse error | a shape must be `urn:file:`, and it and the validator run with only the read scopes of the refresh's capability. A refresh issued under the root capability cannot be narrowed this way, and has only the `urn:file:` rule |
| a token echoed by a failing reader | every reason is scrubbed of it before it is stored or answered |
| a registry sending a read anywhere it likes | `forge_reader` reads only the forges it is given, and `urn:http:get` checks `urn:cap:net:` at every redirect and drops the credential on a change of host |
| a forged webhook | a door checks `verify_push` first. A signed `POST` only ever causes a read, which is validated like any other |

### The seams

Each is a bound IRI, so the library holds no git, no HTTP client and no file. See
[`src/git.rs`](src/git.rs) for the contracts.

| IRI | what |
|---|---|
| `urn:name:git-read` | Source taking `url`, `branch`, `path`, and optionally `token` and `as-of`. It answers the file, or `NotFound`. [`forge_reader`](src/git.rs) binds it over `urn:http:get` for GitHub, GitLab and Codeberg |
| `urn:name:git-store:{key}` | `document:{prefix}` and `status:{prefix}`, with Source and a conditional Sink like the [counts' store](#active-names). Key it on `tenant_of` if there are tenants |
| `urn:name:git-refresh:{prefix}` | the Sink a host issues, under `urn:cap:name:git-refresh`, what it reads and what it stores to. It takes the push payload as `content` |
| `urn:shacl:validate`, `urn:secret:{name}` | from `ikigai-shacl` and `ikigai-secret`, bound by the host |

### As of a date

`as-of` on such a namespace reads the same repository's history through `urn:name:git-read`,
with the registry as it stood from `urn:name:history-source`. History records what was pushed
rather than what was served, so **a version refused at the time is answered as committed**.
`forge_reader` can do this on GitHub and GitLab. Codeberg's commit listing ignores a date
(checked 2026-11-05), so there it is refused rather than answered with the wrong commit.

### Redirect target health

A host can opt into link-rot checks in a build with the existing `git` feature,
which already links the outbound client. Repository access and secrets need not
be enabled. The plain `server` build cannot probe targets.

```sh
ikigai-name-server --root /srv/namespaces \
  --target-health-every 3600 --target-health-net example.org \
  --target-health-net docs.example.org
```

The timer runs at startup and then at the declared interval. Its capability holds
`urn:cap:name:target-health-check`, registry read access, and only
`urn:cap:net:<host>` for each explicitly allowed DNS host or IPv4 address. Use host
names without ports; a grant covers that host's ports. The complete namespace's
HTTP targets must be authorized before any of its probes leave. An unlisted host
is refused and the job continues with other namespaces. Tenants use their own
registries and separate results. Public HTTP requests receive no job authority.

Each run makes one HEAD per distinct literal target in a redirect namespace:
its fallback target and each rule's literal base before its first `{capture}`.
It never substitutes a resolved name. Probes issue `urn:http:head` through the
kernel under the job's capability and read no response body. The host supplies
the transport and its timeout (30 seconds in the standalone server).
`ikigai-http` follows redirects, checking the host, port, and path grant at every
hop. A denied hop is `refused`; connection, TLS, timeout, and redirect failures
are `unreachable`. Non-HTTP URLs and URLs with credentials are `unsupported`.
HEAD returns `gone` for a final 404 or 410, and `answered` for other responses,
including server errors. It does not expose exact status codes, so `status` is
null unless supplied in the endpoint's `http-status` metadata (such as 304).
These checks say nothing about individual names below a base.

For an embedded host, bind `ikigai_name::target_health::space()` alongside
`ikigai_name::space()` and a host-backed `ikigai_http::space(transport)`, inject
a kernel clock, and issue Sink to
`urn:name:target-health-check:namespace:{prefix}` under the job capability.
Read Source at `urn:name:target-health:namespace:{prefix}` under the namespace's
**recorded owner** capability. These two faces are opt-in; the regular name space
does not bind them. There is no public HTTP route for them in the standalone server.

The JSON answer labels `prefix`, `checked_at_unix_ms` (UTC Unix milliseconds, null without a kernel clock), and
`targets`. Each target has its literal `target`, nullable HTTP `status`, `outcome`,
and `elapsed_ms` until headers or failure. It retains only the latest completed
check in memory; a restart loses it. A namespace with no check, or whose target
bases changed since the check, answers NotFound until the host checks it. Results
are uncacheable. No visitor, resolved request path, cookie, response header, body,
or raw transport error is collected. Resolution never waits for a probe.

### Repositories in the standalone server

The plain server still links no secret store and no outbound HTTP. Build it with the `git`
feature, and start it with `--git`:

```sh
cargo build --release --features git
ikigai-name-server --root /srv/namespaces --git --secrets /etc/ikigai-name/secrets
```

The last good copies are in `git/` under `--root`, or under each tenant's directory, so
splitting a tenant off copies them, and a mirror's copy outlives a restart. A background timer
and a signed push hold the only write scopes, on that directory alone. It refreshes at startup
and every `--git-every` seconds. Secrets outside `--root` only: the server refuses to start
otherwise. Two processes must not serve from one directory.

**To serve a repository:**

1. Claim the namespace, as above, with its shapes under `--root`.
2. Set its webhook on the forge to `POST https://your.host/-/git/{prefix}`, content type
   `application/json`, the push event, and a random key. GitHub, Gitea and Forgejo sign with
   `X-Hub-Signature-256`. GitLab sends a plain token instead, which this door does not
   accept, so a GitLab repository is refreshed by the timer alone.
3. Store that key as `name-git-hook.{prefix}`, and for a private repository a read-only token
   as `name-git-token.{prefix}`, in the secret store.

The server links the validator without rudof, which covers SHACL Core. A shape outside it, such
as SHACL-SPARQL, is refused by name, and nothing is accepted until it changes.

**The owner reads a refusal in the forge's webhook deliveries**, as the response to the push
that caused it. This server has no authenticated transport, the same reason the counts are not
routed, so `urn:name:git-status:{prefix}` is not routed either; the operator reads it from
`git/status.{prefix}.json`. Without `--count-names` this server has no clock, so that file holds
no times, and its modified time is when it last changed. A kernel with a clock buffers names
for a flush, and nothing here should hold those for nobody.

## Active names

A namespace's volume is its **active names**: the distinct names that resolved at
least once in a month, not its registry entries, since one entry can answer for any
number of names.

| IRI | answer | |
|---|---|---|
| `urn:name:stat:namespace:{prefix}:period:{yyyy-mm}` | `3`, or with `as=text/turtle` an observation | a past month is cacheable under its stored set; the current month is live |
| `urn:name:stat:rule:namespace:{prefix}:match:{rule}:period:{yyyy-mm}` | distinct names answered by that redirect rule; number or Turtle observation | the first matching rule counts, keyed by its percent-encoded match |
| `urn:name:stat:miss:namespace:{prefix}:period:{yyyy-mm}` | distinct names using a redirect fallback or rejected by hosted membership; number or Turtle observation | the same monthly cache behavior |
| `urn:name:stat:namespace:{prefix}` | one line per month held: `2026-11`, a tab, the count | always live |

```sh
ikigai -c 'source urn:name:stat:namespace:resmud:period:2026-11'
# 3
```

Reading a count takes the namespace's `owner` or the operator's root capability.
All count faces declare the wildcard form, like administration, and check the exact grant
before reading the registry, so a refusal does not say whether a namespace exists.

**Aggregate only.** What is stored is the name and the month, and nothing about the
request: no address, no user agent, no cookie, no per-visitor identifier.

```json
{ "period": "2026-11", "names": ["resmud/core", "resmud/core/weapon"] }
```

**What counts as active.** A name `urn:name:resolve` or `urn:name:document` (or its template
`urn:name:doc:{path}`) answered for, including one answered with a redirect, however
its path was spelled: `resmud/core` and `/resmud//core` are one name. Not counted: an
unclaimed or retired name, anything refused (over the ceiling, no conversion), an
`as-of` answer, which is about the past rather than this month's traffic, and
`urn:name:docs` called directly, which takes a namespace rather than a name. Months
are in UTC.

**Rules and misses.** A rule counts a distinct name only when it is the first rule
that matches. Its key is its `match` text, rather than its position or target, so an
edit to a target keeps the same partition. Percent-encode the match in the IRI:
`old/{x}` is `old%2F%7Bx%7D`. For example:

```sh
ikigai -c 'source urn:name:stat:rule:namespace:acme:match:old%2F%7Bx%7D:period:2026-11'
ikigai -c 'source urn:name:stat:miss:namespace:acme:period:2026-11'
```

A redirect that uses the namespace's fallback target matches no rule, so it counts
as both active and a miss, even if the namespace has no rules. A hosted name rejected
by `exact` or `terms` membership counts only as a miss. Unknown or invisible
namespaces, retired names, delegation and document-loading errors create neither
rule counts nor misses. Repeated names and alternate canonical spellings count once
in each partition, tenant and UTC month. Historical reads count nothing.

**It cannot slow or fail a resolution**, because resolving only adds the name to a
buffer in memory. A request could not write the store anyway, as it holds no write
authority. A host stores the buffer by issuing `urn:name:seen-flush` on a timer, under a
capability holding `urn:cap:name:seen-flush` and whatever its store needs. A failed
write fails that flush, which the host logs, and the names stay buffered. A count of
the current month includes them.

| bound | |
|---|---|
| what a crash can lose | the names first seen since the last successful flush, at most 16,384. The standalone server flushes every 60 seconds by default |
| name memberships buffered | 16,384 shared across active, rule and miss partitions. Past it a new name is dropped, and the next flush reports how many |
| one name | 1,024 bytes; a longer one is answered and dropped the same way |
| one stored set | 1,000,000 names; a count at that ceiling is a floor |

These are ceilings because **a client picks the names**. By default, a hosted namespace
answers for any path under its prefix, so `/resmud/anything` counts as a name, and a
client can inflate a count that way, or put arbitrary text in a name. Set `names` to
`exact` or `terms` to count only names the namespace holds (see below). Only the
namespace's administrator and the operator can read the count.

A hosted namespace can opt into a smaller set of names:

```json
{
  "prefix": "vocab/",
  "owner": "urn:cap:name:admin:vocab",
  "strategy": "hosted",
  "source": "urn:file:vocab.ttl",
  "names": "terms",
  "documents": ["vocab/core"]
}
```

| `names` | Names answered |
|---|---|
| `any` (the default when omitted) | Every name under the prefix; existing registries keep their behavior |
| `exact` | The prefix itself and the names in `documents` |
| `terms` | The same explicit names, plus IRI subjects defined in the source Turtle document |

`documents` is optional and lists full paths or claimed IRIs, rather than suffixes.
Every listed name serves the namespace's same `source`. A mention as a predicate or
object alone does not define a term. For a web term, its path is matched; with
tenants, its host must also match the request's tenant. URN and tag terms use the
same canonical comparison as other resolution. Prefixes and explicit documents
use the usual path or IRI comparison too.

A name outside the selected set returns 404 from both the document and resolution
faces and is counted only in the miss partition. An invalid `names` value refuses
the whole registry, naming the namespace. `terms` reads the current served document (or its historical
version with `as-of`), under the document size and Turtle nesting limits. It needs
no SPARQL binding. The claim and admin endpoints also accept `names=` and
`documents=` (the latter as a JSON list).

**A kernel with no clock counts nothing**, because it cannot say which month it is,
and answers exactly as before. On a kernel with a clock, a counted answer is cacheable
only until its month ends. The kernel answers a cached resolution without running the
face, so a name resolved in November and cached forever would never reach December's
count. An HTTP door still projects such an answer as `no-cache`, because it hangs from
the registry's thread, so a remote cache keeps asking; a cache that does not ask (a CDN
told to hold it) hides those hits, which nothing here can count.

**The store is a bound IRI**, like the registry, so this crate never touches a file. A
host binds `SEEN_TEMPLATE`, `urn:name:seen-source:namespace:{key}`, and keys it on
`tenant_of` if it has tenants. A set lives at
`urn:name:seen-source:namespace:{prefix}:period:{yyyy-mm}`, and the months a namespace
holds at `urn:name:seen-source:namespace:{prefix}`. Source answers the document, or
`NotFound` if there is none yet. Sink takes `content` and `if-match`, the content ID
being replaced, with no `if-match` meaning none may exist yet, and compares and
replaces as one operation or refuses with `Conflict`. With nothing bound, a count is
refused rather than answered as zero.

Rule and miss sets use the same store. Their keys are
`-stat:{encoded-prefix}:rule:{encoded-match}:period:{yyyy-mm}` or
`-stat:{encoded-prefix}:miss:period:{yyyy-mm}`; omit the `:period:` suffix and month
for the period index. The reserved `-` keeps these separate from namespace keys.
Encode every byte except ASCII letters, digits and `-._~` as `%HH` in the prefix
and match. Each set holds `period`, `names` and the cumulative `dropped` membership count;
each index holds only `periods`. Older sets without `dropped` read as zero recorded
drops; drops logged before this field was introduced cannot be recovered. All
partitions use the same name-length, stored-set and shared buffer ceilings. The flush's stored and dropped numbers count memberships across partitions,
so one name can contribute to more than one.

`space()` holds the buffer, so the counted faces are the ones it binds, `doc` at
`urn:name:doc:{path}` now among them. `resolve()`, `document()` and `doc()` bound on
their own count nothing.

### In the standalone server

`--count-names` turns it on, with the sets in `seen/` under `--root`, or under each
tenant's directory, so that splitting a tenant off copies its counts. Rule and miss
sets are under `seen/partitions/`, using the same file-name escaping. A background task
holds the only write scope in the process, on that directory alone, and no request
holds it. Without the flag the server has no clock, makes no directory, and is exactly
the read-only server it was.

**The counts are not routed.** Their reader is an administrator, and this server has
no authenticated transport, the same reason administration is absent. On the server,
print a month's stored counts through the kernel:

```sh
ikigai-name-server --root /srv/namespaces --report 2026-11
ikigai-name-server --root /srv/namespaces --report 2026-11 --as text/turtle
ikigai-name-server --root /srv/namespaces --tenant iriref.org --tenant urnid.org --report 2026-11
```

`--report MONTH` exits after reading, opens no listener, starts no timers or git
clients, and grants no filesystem writes, even with `--count-names` or `--git`.
It reads the registry and stored sets through kernel Sources, without a clock or
buffered names. Use the same `--registry` and repeated `--tenant` flags as serving.
Do not combine it with `--check`.

Every configured tenant and namespace (including private and retired namespaces)
is labeled with its month, active names, misses, each currently configured redirect
rule's count and total recorded dropped memberships across those partitions. Missing
sets answer zero; an empty registry says it has no namespaces. A missing or corrupt
registry or unreadable/corrupt set fails the command without partial stdout. Rules
removed from the current registry are outside this report. Drops count rejected
membership attempts, not distinct names: a name may contribute to both the active
and rule partitions. Historical drops from older sets without `dropped` are unknown
and contribute zero recorded drops.

Text labels every metric and quotes namespace and rule text. Turtle uses the same
`schema:Observation`, `schema:observationAbout`, `schema:variableMeasured`,
`schema:value` and `dcterms:temporal` (`xsd:gYearMonth`) terms as the count faces,
with `schema:spatialCoverage` for a tenant and `schema:description` for the rule
match. Each tenant, namespace, month and metric has a distinct subject. Embedded
hosts can bind `stat::monthly_report()` at `stat::MONTHLY_REPORT_TEMPLATE`; callers
must administer every namespace in the tenant.

A prefix's `/` is written `~2F` in the file's name. Two processes must not count into
one directory: the file binding's compare and replace are two steps, safe only because
one task writes. Names go to disk as a whole file rewrite, once a flush has something
new.

## Endpoints

| IRI | verb | what |
|---|---|---|
| `urn:name:document` | Source | the namespace document, negotiated (`as=`) |
| `urn:name:docs` | Source | the namespace as HTML; with `term=`, one htmx fragment, or with `view=page` the page showing that term |
| `urn:name:style` | Source | the documentation page's stylesheet for `theme=` |
| `urn:name:resolve` | Source | how a path or an IRI resolves (for a redirect, its target, status and any rule that answered), or a `NotFound` naming it (`Gone` for a retired namespace) |
| `urn:name:resolution:iri:{iri}` | Source | the same resolution as data: JSON, Turtle, or that line |
| `urn:name:resolution` | Source | the same, for `iri=` |
| `urn:name:delegate` | Source | forward a name to an upstream resolver; needs `urn:cap:net:<host>` |
| `urn:name:registry` | Source | every claimed namespace, as JSON |
| `urn:name:health` | Source | what this deployment holds, and what it refuses |
| `urn:name:stat:namespace:{prefix}:period:{yyyy-mm}` | Source | how many distinct names a namespace resolved in a month |
| `urn:name:stat:rule:namespace:{prefix}:match:{rule}:period:{yyyy-mm}` | Source | distinct names answered by a rule, keyed by its percent-encoded match |
| `urn:name:stat:miss:namespace:{prefix}:period:{yyyy-mm}` | Source | distinct names using a redirect fallback or rejected by hosted membership |
| `urn:name:stat:namespace:{prefix}` | Source | that count for every month held |
| `urn:name:seen-flush` | Sink | store the names resolved since the last flush |
| `urn:name:git:{prefix}` | Source | the last good copy of a repository's file |
| `urn:name:git-refresh:{prefix}` | Sink | read the repository again, and accept or refuse what it finds |
| `urn:name:git-status:{prefix}` | Source | what those refreshes found, for the namespace's administrator |
| `urn:name:claim` | Sink | claim a prefix nobody holds |
| `urn:name:admin` | Sink · Delete | change how a namespace resolves · retire it |

Every read face takes its input by name or from piped `content` (`path` for
`resolve` and `document`, `prefix` for `claim` and `admin`), so each is a
pipeline citizen: `echo acme | sink urn:name:claim strategy=hosted source=…`.

### Conformance

The module passes [`ikigai-conformance`](https://github.com/ikigai-rs/ikigai-conformance)
— `tests/conformance.rs` walks every endpoint above and holds the seven read
faces to their `.cacheable()` marking over a threaded registry, then shows the
same walk over a live registry reporting the downgrade on all seven, and walks them
again on a clock, with the counting on. What the
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
build rather than of a config file someone might edit. A build with the `git`
feature adds the secret store, an HTTP client and the validator, and nothing else.

| flag | default | |
|---|---|---|
| `--listen ADDR` | `127.0.0.1:8080` | address to bind |
| `--root DIR` | `.` | directory `urn:file:*` resolves within, and the jail |
| `--registry NAME` | `registry.json` | the registry file within `--root` |
| `--report MONTH` | off | print stored counts for every configured tenant and namespace, then exit without listening or writing |
| `--as TYPE` | `text/plain` | with `--report`, output `text/plain` or `text/turtle` |
| `--check` | off | validate the registry and read every hosted or mirrored document under its size ceiling, then exit without listening or writing |
| `--cache-files` | off | cache file reads; see [Cacheability](#cacheability) |
| `--tenant HOST` | none | answer for `HOST` from `--root/HOST`, and refuse any host not named; repeatable. See [Tenants by host](#tenants-by-host) |
| `--count-names` | off | count the distinct names each namespace resolves per month. See [Active names](#in-the-standalone-server) |
| `--flush-every SECONDS` | `60` | with `--count-names`, how often to store them |
| `--git` | off | serve namespaces from git repositories; needs a build with the `git` feature. See [In the standalone server](#repositories-in-the-standalone-server) |
| `--git-every SECONDS` | `900` | with `--git`, how often to read them if no webhook says to |
| `--secrets DIR` | the system's store | with `--git`, a directory holding tokens and webhook keys, outside `--root` |
| `--git-forge HOST=KIND[@API]` | none | with `--git`, read repositories on `HOST` too; `KIND` is `github`, `gitlab` or `forgejo`; repeatable |

To gate a registry change in CI, run:

```sh
ikigai-name-server --check --root /srv/namespaces
# With tenants, check every named tenant's registry and documents:
ikigai-name-server --check --root /srv/namespaces --tenant iriref.org --tenant urnid.org
```

`--check` uses the serving loader's all-or-nothing validation, including overlap and
redirect rules. It also reads each hosted or mirrored document, including private
namespaces, and applies `limits.max_document_bytes`. It exits 0 silently on success
or non-zero with the loader's refusal or a sentence naming the unreadable or oversized
namespace; a tenant refusal also names the host. `--registry` selects the file as usual.
Only file read scope is granted. No listener, count flush, repository refresh or secret
store is started, even with `--count-names` or `--git`. It checks readability and byte
size, not Turtle syntax, generated pages or remote repository contents; a source that
cannot be read locally is refused.

The public surface is two routes, plus the documentation page's two assets, and with
`--git` a signed webhook, served `routes_only` so anything else 404s before it reaches a
resource:

```text
/-/style       → urn:name:style      the page's stylesheet
/-/htmx        → urn:iki:ui:htmx     htmx, vendored by ikigai-ui
/-/git/{ns}    → urn:name:git-refresh:{ns}   with --git: a signed POST and nothing else
/-/resolution/iri/{iri} → urn:name:resolution:iri:{iri}
/{ns}          → urn:name:doc:{ns}            a path, or a urn: or tag: IRI
/{ns}/{doc}    → urn:name:doc:{ns}/{doc}
```

The asset paths begin with `-`, which no namespace prefix can, so they never
shadow a document.

Each route serves two faces: Turtle by default, and the HTML documentation by
`Accept: text/html` or `?as=text/html`. Any other type answers `406` naming
those two, because this build binds no transreptor — a host that embeds the
module beside one reaches more formats through `as=` in the kernel, but an HTTP
face negotiates only over what the document *declares* (ledger #249 is the open
question of whether it should consult the transreptor graph). `?description`
answers the route's OpenAPI description.

Administration is deliberately absent from it. The server holds a **read-only**
capability jailed to `--root`, so `urn:name:claim` and `urn:name:admin` cannot be
performed by this process however it is addressed — they belong on a local or
authenticated transport. `--count-names` adds one write scope, held by a background
task and limited to `seen/`, which no request can reach. `--git` adds another, limited to
`git/`, held by a background task and by a `POST` whose signature verifies.

## Known limitations

- **The standalone server routes at most two path segments.** `/{ns}` and
  `/{ns}/{doc}` are its document routes, so `GET /catalog/item/2019/42` is `404`
  there, and a rule that matches more than one segment below a one-segment prefix
  is reachable through `urn:name:document` and `urn:name:resolve` but not over HTTP.

## 0.7.0 migration

Move all ikigai dependencies to core 0.3, including the standalone server on
`ikigai-web` 0.5. Consumers sharing core types must move their other modules to
core 0.3 as well.

A `redirect` namespace's document is now an answer, not an error: `urn:name:document`
returns core's typed redirect (`Representation::redirect`, read with `as_redirect`), which
`ikigai-web` sends as the namespace's status with `Location`. It used to fail with an
endpoint error, a `500` over HTTP. A retired namespace now fails with `Error::Gone`, which
is `410` over HTTP, from `urn:name:document`, `urn:name:resolve` and `urn:name:docs`, where
it used to be `Error::NotFound` and `404`. A caller matching `NotFound` to recognize a
retirement should match `Gone`.

Rust callers: `Namespace` gains `public` (`true` for everything that exists today), `Strategy`
gains `Delegate`, `Registry` gains `unclaimed`, `ClaimError` gains `BadUpstream`, `LoadError`
gains `Unclaimed`, and `Health` gains `delegated`; an exhaustive `match` on any of them needs an
arm. A prefix that begins `urn:` or `tag:` is now an IRI claim, compared by colon-separated
segments in its canonical form, where it used to be a one-segment path; a registry holding such
a prefix as a path should check what it meant. `space()` binds three more faces:
`urn:name:resolution`, `urn:name:resolution:iri:{iri}` and `urn:name:delegate`. The server's
document routes take a `:` in a segment, which they used to refuse with `400`.

## 0.6.0 migration

Rust callers: `Namespace` gains `repository`, which is `None` for everything that exists
today, and `ClaimError` gains `BadRepository`. `urn:name:claim` and `urn:name:admin` gain an
optional `repository`, and as setting a strategy replaces the old one whole, an update that
names none removes it. A registry file with no `repository` means what it did, reads and
writes the same bytes, and a build without the `git` feature links nothing new but `hmac` and
`sha2`, which `verify_push` uses. `space()` binds three more faces, under `urn:name:git`.

## 0.5.0 migration

`urn:name:resolve`'s answer for a `redirect` namespace gains a field: the status, always,
so `prefix`, `strategy`, `target`, `status`, and then the rule's `match` if one answered.
A reader that took everything after the second tab as the target must stop. Rust
callers: `Strategy::Redirect` gains `status` and `rules`, and `ClaimError` gains
`BadStatus`, `BadRule` and `NotARuleList`. A registry file with neither field means
what it did, at 302. `urn:name:registry` and anything stored by `urn:name:claim` or
`urn:name:admin` now write each redirect's `status` out.

## 0.4.0 migration

`space()` now binds `doc` at `urn:name:doc:{path}` itself, so a host should stop binding
it: the space's own is reached first, and it is the one that counts. A kernel with a
clock now caches `urn:name:resolve` and `urn:name:document` until the end of the month
rather than indefinitely, and holds up to 16,384 names in memory if nothing flushes
them. A kernel with no clock is unchanged.

## 0.3.0 migration

This release moves the public core types to the 0.2 streams release (ledger
#2261), so consumers must upgrade their other ikigai modules to core 0.2 as
well. The old 0.1 caret requirement excluded 0.2 under Cargo's 0.x rules.
The standalone server and conformance suite use the matching ecosystem releases;
conformance now checks DATATYPES and FAILURES by default.

## Status

**Built and walked clean by `ikigai-conformance`:** the registry, the claim rule,
resolution, capability-scoped administration, the HTML documentation face, content
negotiation, limits, cacheability, active names, and namespaces served from a git
repository.

**On crates.io** as `ikigai-name` (0.2.x): `cargo add ikigai-name` to embed it,
or `cargo install --locked ikigai-name --features server` for the standalone
`ikigai-name-server` binary. Running the `server` feature needs a directory holding `registry.json` and the documents it
points at; embedded, a host binds `ikigai_name::space()` and nothing else.

**Built but not yet servable as it stands:** [as-of resolution](#as-of-a-date),
which needs a file-at-a-commit resource from `ikigai-repo`. Over HTTP, the
[active-name counts](#active-names) and a repository's status, which need an authenticated
transport.

**Tested mostly against stand-ins.** The tests use a real git history, a canned transport and a
local forge. By hand, once (2026-11-05): the built server read a public file from GitHub over
HTTPS, validated it and served it and its page, and `forge_reader`'s file URLs answered 200 from
GitLab and Codeberg. Not run: a real webhook delivery, a private repository on a real forge, or
GitLab or Codeberg through the binary.

**Not built**, and a permanence promise is weaker without them: signed redirect
provenance and succession, and peer mirroring.

## Accessibility

[ACCESSIBILITY.md](ACCESSIBILITY.md) says what this module emits that people read (the
documentation page, its errors, the counts, the server's terminal text), the rules that output follows, what
is tested, the known limitations, and how to report a barrier.

## License

MIT or Apache-2.0, at your option.
