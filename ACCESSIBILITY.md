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
| Last reviewed | 2026-11-03 |

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
  `name: "x" is not a term of this host: it lives at <IRI>`).
- **Plain-text and JSON answers**: `urn:name:resolve` answers a strategy and its source,
  separated by a tab (`hosted`, then `urn:file:…`); `claim` and `admin` answer one line
  (`claimed <prefix>`, `updated <prefix>`, `retired <prefix>`, with the owner or the reason after
  a tab); `registry` and `health` answer JSON.
- **The standalone server's terminal text** (`src/bin/server.rs`): its usage text, a startup line
  saying where it listens, what it may read and which tenants it answers for, and refusals starting
  `ikigai-name-server:`.

Not this repository's: the error page a browser gets when a request fails (the HTTP door's, from
`ikigai-web`), the Turtle and other representations of a namespace (the namespace owner's
documents, or a transreptor's output), and the terms' names and comments, which come from the
vocabulary as written.

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
- **No color in the terminal**: the server writes plain lines.

## How it is tested

- The unit tests in `src/docs.rs`: `every_palette_clears_the_text_floor`,
  `every_palette_clears_the_ui_floor_for_edges`, `code_is_readable_on_its_own_background` and
  `the_high_contrast_palette_clears_the_enhanced_floor` measure every palette pair with
  `ikigai-a11y`; `auto_emits_both_schemes_behind_the_media_query`, `reduced_motion_is_honored`,
  `an_unknown_theme_degrades_to_a_readable_palette`; `the_chooser_works_without_javascript` (a
  form, `method="get"`, no `action`, an Apply button outside `<noscript>`, the label);
  `the_page_lists_terms_and_targets_the_detail_pane` (`lang`, the live region);
  `a_term_is_a_link_htmx_enhances`, `the_page_links_its_assets_and_inlines_none`,
  `a_page_with_a_term_fills_the_region`; `term_text_is_escaped` and
  `hostile_names_are_escaped_in_attributes_and_text`.
- `tests/tenancy_http.rs` (with the `server` feature) pins the "Defined elsewhere" list's label,
  its links and its text-only case (`a_term_lives_in_one_tenant_and_is_referenced_from_the_others`,
  `only_a_web_iri_is_a_link_in_the_references`), and the words of the host and configuration
  refusals.
- `tests/through_sparql.rs` renders the page, a term's fragment and a term's page over the real
  SPARQL engine (`the_page_documents_every_term_in_the_vocabulary`,
  `a_term_request_returns_a_fragment_carrying_its_schema`,
  `a_term_with_view_page_is_the_page_with_its_detail`,
  `the_theme_argument_changes_the_rendered_palette`).
- `tests/server_http.rs` (with the `server` feature) checks that the standalone server answers the
  page for `Accept: text/html` and `?as=text/html`, and a `406` naming both faces otherwise; and
  (`every_url_the_page_names_is_served`) that every URL the page names answers there as what the
  page uses it for: the script as `text/javascript`, the stylesheet as `text/css`, each term's
  link and htmx fetch as the page with that term, and the theme form's submission.
  `tests/conformance.rs` (`errors_carry_no_registry_body`) pins that no error echoes the registry.
- **Checked once by hand in a browser and with axe, not in CI** (2026-11-01, ledger #2737): the
  standalone server's page in Chromium, with axe's WCAG 2.0, 2.1 and 2.2 A and AA rules under
  light, dark and `prefers-contrast: more`, for the term list, a term chosen through htmx, a
  term's page, and the dark and high-contrast themes: no violations and no console errors. The
  same check confirmed that choosing a term keeps the live region element, swaps its contents,
  leaves focus on the link and updates the address and title, and that Apply restyles the page.
  No screen reader has read the page, and axe cannot run with JavaScript off; the JavaScript-off
  path is covered by the HTTP test above. The "Defined elsewhere" list (2026-11-03, ledger #3047)
  has not been through this check: it reuses the link color the palette tests measure, and is
  otherwise pinned only by the tests above.

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
- `urn:name:resolve`, `claim` and `admin` answer tab-separated fields with no labels.
- A refusal for a host that is not a tenant cannot say which host would work: it names the host it
  was given and no next step, because the list of tenants is not this refusal's to disclose.

## Contributing

Read this file before changing any output listed above: the page, a fragment, the palettes, an
error's words or the server's terminal text. A new color goes into every palette and passes the
contrast tests in `src/docs.rs`; a change to the page keeps its `lang`, headings, landmarks, live
region and labeled chooser, with a test that pins them. Say in the pull request what was tested,
with what, and what was not.

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
| 2026-11-01 | Terms are links that work without JavaScript, htmx swaps the region's contents, the theme chooser is a plain form, and the stylesheet and htmx are served by the standalone server (ledger #2737). Checked in a browser and with axe. |
| 2026-11-03 | Tenants by host: the "Defined elsewhere" list, whole IRIs for references to another host, and the host refusals (ledger #3047). Not checked in a browser or with axe. |
