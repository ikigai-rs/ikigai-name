# Accessibility

What `ikigai-name` emits that people read (the HTML documentation page for a namespace, its
error and refusal messages, plain-text answers, and the standalone server's terminal text), the
rules that output follows, how it is tested, and how to report a barrier. It is not a conformance
claim. Adapted from the [ACCESSIBILITY.md template](https://github.com/mgifford/ACCESSIBILITY.md)
(MIT).

| Field | Value |
|---|---|
| Target | WCAG 2.2 Level AA for the HTML this repository writes |
| Status | Evaluation in progress. No conformance claim. |
| Owner | the maintainers of this repository |
| Report a barrier | [a GitHub issue](https://github.com/ikigai-rs/ikigai-name/issues/new?labels=accessibility) with the `accessibility` label; anything security-sensitive through [private vulnerability reporting](https://github.com/ikigai-rs/ikigai-name/security/advisories/new) |
| Last reviewed | 2026-11-12 |

## What it emits that people read

- **The documentation page** (`src/docs.rs`, `urn:name:docs`, and `urn:name:document` with
  `as=text/html`): a hosted or mirrored namespace as a page, with a header (the namespace, the
  number of terms and where they are served from), a theme chooser, a list of its terms as
  links, and a detail region; with `term=`, one term's detail as an htmx fragment (its name,
  IRI, comment, and a table of what it descends from, its domain and its range), and with
  `term=` and `view=page` the page with that detail in the region. Its stylesheet is a
  separate resource, `urn:name:style`.
- **Errors and refusals** (`src/lib.rs`, `src/document.rs`, `src/docs.rs`, `src/admin.rs`): most
  start `name:` and name the namespace or path they are about (`name: no namespace claims "x"`,
  `name: "x" is retired: <reason>`, `name: "x" overlaps the claimed namespace "y"`,
  `name: administering "x" needs <capability>`, `name: this server does not answer for "x"`,
  `name: "x" is not a term of this host: it lives at <IRI>`). A refusal about the past names its
  date in UTC (`name: urn:file:x did not exist as of 2026-03-31T23:59:59Z`, `name: this host keeps
  no history, so it cannot answer as of <date>; leave out as-of for the current answer, or bind
  urn:name:history-source to add it`). A registry refused for a redirect's status or rule names
  the namespace, and the rule by its place and its match (`the namespace "x" cannot be claimed:
  its status 404 is not one of 301, 302, 303, 307 or 308`, `the namespace "x" cannot be claimed:
  in rule 2 ("old/{y}"), its target uses {z}, which its match does not capture`). A registry
  refused for a prefix says why it cannot be held (`the namespace "-x" cannot be claimed: a
  namespace prefix cannot begin with `-`: its administrative capability would be an exclusion,
  which no grant can satisfy`). An `owner` that could not administer anything is refused naming
  what would (`"x" cannot administer a namespace: an owner is a capability that begins
  `urn:cap:name:admin:` and goes on to name something, such as `urn:cap:name:admin:staff`; leave
  it out to use the namespace's own`). A refusal to administer names the capability the registry
  records as its owner.
- **Plain-text and JSON answers**: `urn:name:resolve` answers a strategy and its source,
  separated by a tab (`hosted`, then `urn:file:…`), and for a redirect its target, its status and
  the match of the rule that answered, if one did (`redirect`, `https://…`, `302`, `old/{x}`);
  `claim` and `admin` answer one line
  (`claimed <prefix>`, `updated <prefix>`, `retired <prefix>`, with the owner or the reason after
  a tab); `registry` and `health` answer JSON. `urn:name:resolution` (`src/resolution.rs`)
  answers the same resolution as labeled JSON (`claim`, `strategy`, `target`, `tenant`,
  `as-of`), as Turtle, or as that same tab-separated line. A name forwarded to an upstream
  resolver is answered in the upstream's own words. Its refusals start `name:` and name the
  name, the upstream and what is missing (`name: forwarding "x" to https://… needs
  urn:cap:net:<host>`, `name: this host has no HTTP client, so it forwards nothing to https://…;
  bind urn:http:get to add it`, `name: a resolution is answered as application/json,
  text/turtle or text/plain, not <type>`). A name in a namespace that is not public is refused in
  exactly the words an unknown name is (`name: no namespace claims "x"`). A registry refused
  for a delegate names the namespace and the upstream as written (`the namespace "x" cannot be
  claimed: its upstream "y" is not an https: or http: address with a host`, `its unclaimed can
  only be a delegate, and this is hosted`).
- **The active-name counts** (`src/stat.rs`): a count answers a number and a newline (`3`); a
  namespace's report answers one line per month, the month, a tab and the count (`2026-11`, then
  `3`), or the sentence `name: "x" has no names counted yet`; a flush answers one labeled line
  (`names stored: 3, dropped: 0`). Their refusals start `name:` and name what they are about
  (`name: reading the count for "x" needs <capability>`, `name: "2026-13" is not a month: write
  yyyy-mm, such as 2026-11`, `name: this host keeps no count of names resolved; bind <template> to
  add it`, `name: could not store the names resolved in "x" for 2026-11: <why>; they stay buffered,
  and 0 more were dropped since the last report`).
- **A namespace served from a repository** (`src/git.rs`): a refresh answers one line naming the
  namespace, and what it did (`"acme": now serving <id>`, `"acme": unchanged, serving <id>`,
  `"acme": refused <id>, and the last good version keeps serving: <why>`, `"acme": nothing
  accepted: <why>`, `"acme": nothing to do: this push is to refs/heads/x, and main is served`). The
  status answers one labeled line each (`repository: <url>, branch main, file core.ttl`, `serving:
  <id>, since <time>`, or `serving: nothing yet`; `refused: <id>, at <time>: <why>`; `not read`
  or `gone`, `at <time>: <why>`). Refusals start `name:` and name the namespace (`name: "acme" has
  no version to serve yet; its administrator can read why at urn:name:git-status:acme`,
  `name: the repository serving "acme" no longer has its file`, `name: reading what happened to
  "acme" needs <capability>`). A registry refused for a repository names the namespace and what is
  wrong (`the namespace "acme" cannot be claimed: its repository has the path "index.html", and only
  a Turtle file ending .ttl is served, so that repository HTML or JavaScript never reaches this
  origin`).
- **The standalone server's terminal text** (`src/bin/server.rs`, `src/bin/server/git.rs`): its usage
  text, a startup line saying where it listens, what it may read, which tenants it answers for,
  whether it counts names and how often it reads repositories, refusals starting
  `ikigai-name-server:`, with `--count-names` a line for each flush that failed or dropped names, and
  with `--git` a refresh's line whenever a namespace's answer changes. The webhook's refusals are
  sentences too (`name: this push has no signature that verifies for that namespace; ...`), and they
  are the same whether or not the namespace exists.

Not this repository's: the error page a browser gets when a request fails (the HTTP door's, from
`ikigai-web`), the Turtle and other representations of a namespace (the namespace owner's
documents, or a transreptor's output), the terms' names and comments, which come from the
vocabulary as written, and a validator's words inside a refusal, which come from `ikigai-shacl` and
the owner's shapes.

## The rules that output follows

- **The page** has `<html lang="en">`, a viewport meta, a `<title>`, one `<h1>` (the namespace) in
  a `<header>`, and one `<main>` holding an `<h2>Terms</h2>` that labels the list
  (`aria-labelledby`) and the detail region. Every name and comment from the vocabulary is
  escaped.
- **Every term is a link that works without JavaScript**: a relative URL to the page with that
  term's detail in the region (`#detail` takes the reader there), and the page's title names the
  term. With htmx loaded, the same URL is fetched and only the region's contents are swapped
  (`hx-select="#detail > *"`, `hx-swap="innerHTML"`), so the region is never replaced, and the
  address bar takes the term's URL.
- **A term on another host is a labeled link, not a term of this page**: when a server has
  tenants, those terms sit under a second `<h2>Defined elsewhere</h2>` that labels their list
  (`aria-labelledby`), each an ordinary link whose text is the term's whole IRI (long IRIs wrap, so
  the page does not scroll sideways). An IRI that is not `http:` or `https:` is text. The heading
  and list are absent when there are none, so a page with no such terms is unchanged. In a term's
  detail, a parent, domain or range on another host shows its whole IRI instead of its local name.
- **The detail region is a polite live region** (`aria-live="polite"`) that starts with an
  instruction (`Choose a term to see its definition.`), and a term's detail uses a table whose
  rows have `<th scope="row">` headers. Choosing a term through htmx leaves focus on the link
  and lets the region announce the change; nothing moves focus.
- **The theme chooser is a real form**: a `<select>` with a visible `<label for="theme">`, inside a
  plain `<form method="get">` with an Apply button that is always visible. It never submits on
  change (WCAG 3.2.2), and it does the same thing with JavaScript on or off.
- **Every URL the page names is served by the standalone server**: the stylesheet at `/-/style`,
  htmx at `/-/htmx`, and the term links and theme form relative to the page itself. Nothing is
  inline, so the door's default `Content-Security-Policy` (`default-src 'self'`) refuses nothing
  the page needs.
- **Color is checked.** Three palettes (light, dark, high contrast) plus `auto`, which follows
  `prefers-color-scheme`. In every palette body text, muted text and links clear 4.5:1 on the
  page, inline code text clears 4.5:1 on its own background, borders clear 3:1, and the
  high-contrast palette's text clears 7:1. An unknown theme name falls back to the light palette.
- **Focus is visible**: a term link, the theme select, the Apply button and the detail region show
  a 3-pixel outline in the link color on keyboard focus.
- **Motion**: `prefers-reduced-motion: reduce` turns off every transition and animation.
- **Errors are sentences that name their input**, and where this module can know the next step
  they say it (`raise limits.max_document_bytes in the registry`, `bind a transreptor for … to add
  it`). No error echoes the registry's body.
- **A rule is named as its author wrote it**: by its place in the list and its `match`, in a
  refusal, and by its `match` in an answer, since the first rule of any shape is the one that
  answers. A rule's text holds no tab or newline, or the registry is refused, so an answer stays
  one line of the same fields.
- **A repository's words never become markup**: a served namespace is Turtle or the generated page,
  which escapes everything from the vocabulary as it always has, so a repository cannot add a script,
  a heading or a landmark to the page, or serve a page of its own.
- **A refusal is one line**: repository-supplied text in a reason has its control characters replaced
  and is cut at 2,000 characters, so it cannot forge a second labeled line of the status, or bury the
  ones after it. It lists three violations, and counts the rest.
- **A status labels every line**, by a word at its start, so a person can find `refused:` and a screen
  reader announces what the value is before the value. What is absent is said (`serving: nothing
  yet`), or its line is left out, never an empty field.
- **A log is not a stream of the same sentence**: a timer's refresh reaches the terminal when a
  namespace's answer changes, and `unchanged` never does.
- **No color in the terminal**: the server writes plain lines.
- **An empty report says so**: a report with no months is a sentence, never an empty answer, which
  would read as a failure. A count of nothing is `0`.
- **A flush labels its numbers**, and a server logs only a flush worth reading: one that failed or
  dropped names. A quiet log means counting is working.

## How it is tested

Every named check has a result: Pass, Fail or Blocked. A Pass covers only the stated
assertions, not overall accessibility or WCAG conformance. The automated checks below were
run on 2026-11-12 against main commit
[`279b0d8`](https://github.com/ikigai-rs/ikigai-name/commit/279b0d8a7d6e1496d35d3f17213e14172737f1d4),
before this documentation-only update, using `cargo test --all-features`, Cargo 1.99.1 and
rustc 1.99.1. This includes `server` and `git`. CI repeats that command's feature coverage
through the [shared Rust workflow](.github/workflows/ci.yml) with `features: "*"`.
The tests inspect generated output and exercise HTTP behavior; they do not establish how a
browser renders it or how assistive technology presents it.

| Check | Result | Evidence |
|---|---|---|
| Palette text, code, edges and high-contrast ratios | Pass | `src/docs.rs`: `every_palette_clears_the_text_floor`, `every_palette_clears_the_ui_floor_for_edges`, `code_is_readable_on_its_own_background`, `the_high_contrast_palette_clears_the_enhanced_floor`, via the all-feature command above, using `ikigai-a11y` 0.4.0. Measures specified color pairs, not rendered contrast in every state. |
| Automatic and unknown themes; reduced-motion CSS | Pass | `src/docs.rs`: `auto_emits_both_schemes_behind_the_media_query`, `an_unknown_theme_degrades_to_a_readable_palette`, `reduced_motion_is_honored`. Checks emitted CSS, not browser preference behavior. |
| Theme chooser without JavaScript | Pass | `src/docs.rs`: `the_chooser_works_without_javascript` checks the GET form, visible label and Apply button outside `noscript`; `tests/server_http.rs`: `every_url_the_page_names_is_served` exercises submission over HTTP. No interactive JavaScript-off browser run. |
| Page language, term list, live-region markup, links, assets and selected detail | Pass | `src/docs.rs`: `the_page_lists_terms_and_targets_the_detail_pane`, `a_term_is_a_link_htmx_enhances`, `the_page_links_its_assets_and_inlines_none`, `a_page_with_a_term_fills_the_region`. Checks generated markup, not live announcements or focus behavior. |
| Vocabulary text and attribute escaping | Pass | `src/docs.rs`: `term_text_is_escaped`, `hostile_names_are_escaped_in_attributes_and_text`; `tests/git_source.rs`: `the_page_is_generated_and_what_the_repository_says_in_it_is_escaped`. No browser execution. |
| Tenant references, labels, web links, text-only IRIs and host refusals | Pass | `tests/tenancy_http.rs`, including `a_term_lives_in_one_tenant_and_is_referenced_from_the_others` and `only_a_web_iri_is_a_link_in_the_references`. Real server HTTP tests; no browser or assistive technology. |
| Page, fragment, selected-term page and theme through SPARQL | Pass | `tests/through_sparql.rs`: `the_page_documents_every_term_in_the_vocabulary`, `a_term_request_returns_a_fragment_carrying_its_schema`, `a_term_with_view_page_is_the_page_with_its_detail`, `the_theme_argument_changes_the_rendered_palette`. Exercises the real engine; does not render in a browser. |
| HTML negotiation, 406 wording and every page URL served | Pass | `tests/server_http.rs`, including `every_url_the_page_names_is_served`: stylesheet and script types, term links, htmx fetches and theme submission over HTTP. Does not execute htmx or evaluate CSP in a browser. |
| Errors omit registry bodies; historical-date refusals | Pass | `tests/conformance.rs`: `errors_carry_no_registry_body`; `tests/as_of.rs`, unit tests in `src/document.rs`, and `tests/server_http.rs`: `as_of_is_refused_here_rather_than_answered_with_today`. Checks words and status, not their usability with assistive technology. |
| Redirect status and rule refusals and answer fields | Pass | Unit tests in `src/rules.rs`, `src/registry.rs`, `src/lib.rs`, including `every_bad_rule_is_refused_with_its_reason`, `a_bad_status_refuses_the_whole_file_naming_the_namespace`, `a_bad_rule_refuses_the_whole_file_naming_the_namespace_and_the_rule`, `resolve_names_the_status_and_the_rule_that_answered`. Checks emitted words and fields. |
| Counts, reports, empty reports, flushes and log refusals | Pass | `tests/active_names.rs`, including `the_report_has_one_line_per_period_held`, `a_bad_period_is_refused_by_name`, `a_failed_write_of_the_seen_set_does_not_fail_the_resolution`; `tests/active_names_http.rs` exercises failed-flush server logging. No terminal assistive-technology evaluation. |
| Repository status labels, bounded reasons, refresh and webhook refusals | Pass | `src/git.rs`: `the_status_is_one_labeled_line_each`, `a_reason_is_one_bounded_line_with_no_token`; `tests/git_source.rs`, including `the_reason_is_readable_by_the_owner_and_nobody_else`, `a_reader_echoing_the_token_does_not_put_it_in_the_record`; `tests/git_http.rs` exercises the real binary. Checks output and access control, not spoken readability. |
| Browser rendering, CSP, htmx focus/region/address/title behavior and Apply | Blocked | No current browser session was run. Historical Chromium check on 2026-11-01 ([PR #29](https://github.com/ikigai-rs/ikigai-name/pull/29)) reported no console errors, a preserved live region, focus remaining on the link, updated address/title and working Apply. Browser version and invocation were not recorded here; CI does not repeat it. It predates tenant references ([PR #31](https://github.com/ikigai-rs/ikigai-name/pull/31)). |
| Automated browser accessibility checker (axe) | Blocked | No current axe run. Historical 2026-11-01 run in PR #29 reported no violations under WCAG 2.0/2.1/2.2 A and AA rules for the list, htmx detail, term page, light/dark/high-contrast themes and emulated `prefers-contrast: more`. axe and Chromium versions and invocation were not recorded here; CI does not repeat it. No JavaScript-off or tenant-reference evaluation. |
| Manual keyboard navigation and visible focus | Blocked | No person's keyboard pass was performed for this review; no interactive browser session was available. CSS outlines and link markup alone do not verify navigation or focus visibility. |
| Zoom, narrow-screen reflow and text spacing | Blocked | No current browser measurements or manual evaluation; no interactive browser session was used for this review. |
| Screen reader, including live-region announcements | Blocked | No screen reader has been used on this output; no screen-reader session was available for this review. |
| Screen magnifier | Blocked | No screen magnifier has been used on this output; no magnifier session was available for this review. |
| Speech input | Blocked | No speech-input software has been used on this output; no speech-input session was available for this review. |
| Switch access | Blocked | No switch-access software has been used on this output; no switch-access session was available for this review. |
| Braille display | Blocked | No braille display has been used on this output; no braille display was available for this review. |

## Known limitations

Found by reading the source (2026-10-31, 2026-11-01) and the browser check above, not by testing
with people or assistive technology:

- A host other than the standalone server that serves the page must route `/-/style` to
  `urn:name:style` and `/-/htmx` to a vendored htmx, or the page loses its palette (the term links
  and the theme form still work). Nothing checks that for it.
- Applying a theme reloads the page without the term that was shown, and the theme form does not
  follow a term chosen through htmx.
- A term's comment in the list is cut at 80 characters with no ellipsis, so a cut reads as the
  whole comment.
- The theme chooser sits between the `<header>` and the `<main>`, outside every landmark.
- `urn:name:resolve`, `claim` and `admin` answer tab-separated fields with no labels, and so does a
  namespace's report. A redirect's answer has four fields, or five when a rule answered, so a
  person reads the fifth as the rule only by knowing that.
- A percent-encoded target is hard to read aloud or by eye, and a captured `%2F` reads as `%252F`.
  That is what keeps it from being a separator, and it is the URL as a client must use it.
- A refusal of a rule that is the wrong shape (a missing or misspelled field, say) ends in the JSON
  parser's words (``missing field `target` ``), after naming the namespace and the rule.
- A count at its ceiling of a million names is a floor, and neither the number nor the report says
  so. A count short of names a full buffer dropped does not say so either: only the next flush does.
- The standalone server does not serve the counts. Someone reading them there reads JSON files.
- The documentation page cannot show a vocabulary as of a date, so a person reading the past reads
  Turtle. The refusal says so and names what to ask for instead.
- A date in a refusal is always in UTC, whatever offset was asked with.
- A refusal of a version quotes the validator, so where a shape has no `sh:message` the reader gets
  whole IRIs (`http://www.w3.org/ns/shacl#MinCountConstraintComponent`), which are long and hard to read
  aloud. An owner's shapes decide this, and with the subset validator a `sh:message` on a property
  shape may not reach the report at all.
- A version is named by a content ID, which is long and means nothing to a person until it is compared
  with another. A file read at a branch's head does not say which commit it came from.
- On the standalone server, the owner reads a refusal in the forge's webhook deliveries, which is that
  forge's page rather than this module's, and on GitLab, which this door does not accept, or for a
  refusal the timer found, nowhere. Only the operator can read `git/status.{prefix}.json` there.
- A status on a kernel with no clock holds no times, and the standalone server has none without
  `--count-names`.
- A refusal for a host that is not a tenant cannot say which host would work: it names the host it
  was given and no next step, because the list of tenants is not this refusal's to disclose.

## Contributing

Read this file before changing any output listed above: the page, a fragment, the palettes, an
error's words or the server's terminal text. A new color goes into every palette and passes the
contrast tests in `src/docs.rs`; a change to the page keeps its `lang`, headings, landmarks, live
region and labeled chooser, with a test that pins them. Say in the pull request what was tested,
with what, and what was not. In the pull request's Accessibility section, give each check a
row in a Check | Result | Evidence table with Pass, Fail or Blocked, the command or test,
tool and version, or the reason it could not run. Keep unrun checks as Blocked, including
assistive technology nobody used. A historical run CI does not repeat stays Blocked with
its date.

## Reporting a barrier

[Open an issue](https://github.com/ikigai-rs/ikigai-name/issues/new?labels=accessibility) with the
`accessibility` label (the link sets it when GitHub lets you add labels; otherwise a maintainer
adds it): what you were doing, what happened, and your browser and any assistive technology, if
you are comfortable sharing them. Anything security-sensitive goes through
[private vulnerability reporting](https://github.com/ikigai-rs/ikigai-name/security/advisories/new).
**We acknowledge a report within 7 days**, and promise nothing further.

## Change history

| Date | Change |
|---|---|
| 2026-10-31 | First version. |
| 2026-11-01 | Terms are links that work without JavaScript, htmx swaps the region's contents, the theme chooser is a plain form, and the stylesheet and htmx are served by the standalone server ([PR #29](https://github.com/ikigai-rs/ikigai-name/pull/29)). Checked in a browser and with axe. |
| 2026-11-03 | Tenants by host: the "Defined elsewhere" list, whole IRIs for references to another host, and the host refusals ([PR #31](https://github.com/ikigai-rs/ikigai-name/pull/31)). Not checked in a browser or with axe. |
| 2026-11-03 | The `as-of` refusals ([PR #32](https://github.com/ikigai-rs/ikigai-name/pull/32)). The page itself is unchanged. |
| 2026-11-04 | The active-name counts, report and flush, and the server's `--count-names` lines ([PR #34](https://github.com/ikigai-rs/ikigai-name/pull/34)). Plain text and refusals only; the page is unchanged. Tested as above, with no assistive technology. |
| 2026-11-05 | A namespace served from a git repository ([PR #36](https://github.com/ikigai-rs/ikigai-name/pull/36)): a refresh's line, a status, their refusals, the webhook's refusals and the server's `--git` lines. Plain text and refusals only; the page is unchanged, and now also renders a repository's vocabulary, escaped. Tested as above, with no assistive technology and no browser. |
| 2026-11-05 | A redirect's status and rewrite rules ([PR #35](https://github.com/ikigai-rs/ikigai-name/pull/35)): more fields in `urn:name:resolve`'s answer, and the refusals of a status or a rule. Plain text and refusals only; the page is unchanged. Tested as above, with no assistive technology. |
| 2026-11-12 | Record Pass / Fail / Blocked results with scope and evidence; rerun automated checks on main and mark historical browser runs and unrun assistive-technology checks Blocked. |
