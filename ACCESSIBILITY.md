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
| Last reviewed | 2026-10-31 |

## What it emits that people read

- **The documentation page** (`src/docs.rs`, `urn:name:docs`, and `urn:name:document` with
  `as=text/html`): a hosted or mirrored namespace as a page, with a header (the namespace, the
  number of terms and where they are served from), a theme chooser, a list of its terms as
  buttons, and a detail region; with `term=`, one term's detail as an htmx fragment (its name,
  IRI, comment, and a table of what it descends from, its domain and its range).
- **Errors and refusals** (`src/lib.rs`, `src/document.rs`, `src/docs.rs`, `src/admin.rs`): most
  start `name:` and name the namespace or path they are about (`name: no namespace claims "x"`,
  `name: "x" is retired: <reason>`, `name: "x" overlaps the claimed namespace "y"`,
  `name: administering "x" needs <capability>`).
- **Plain-text and JSON answers**: `urn:name:resolve` answers a strategy and its source,
  separated by a tab (`hosted`, then `urn:file:…`); `claim` and `admin` answer one line
  (`claimed <prefix>`, `updated <prefix>`, `retired <prefix>`, with the owner or the reason after
  a tab); `registry` and `health` answer JSON.
- **The standalone server's terminal text** (`src/bin/server.rs`): its usage text, a startup line
  saying where it listens and what it may read, and refusals starting `ikigai-name-server:`.

Not this repository's: the error page a browser gets when a request fails (the HTTP door's, from
`ikigai-web`), the Turtle and other representations of a namespace (the namespace owner's
documents, or a transreptor's output), and the terms' names and comments, which come from the
vocabulary as written.

## The rules that output follows

- **The page** has `<html lang="en">`, a viewport meta, a `<title>`, one `<h1>` (the namespace) in
  a `<header>`, and one `<main>` holding an `<h2>Terms</h2>` that labels the list
  (`aria-labelledby`) and the detail region. Every name and comment from the vocabulary is
  escaped.
- **The detail region is a polite live region** (`aria-live="polite"`) that starts with an
  instruction (`Choose a term to see its definition.`), and a term's detail uses a table whose
  rows have `<th scope="row">` headers.
- **The theme chooser is a real form**: a `<select>` with a visible `<label for="theme">`, inside a
  `<form method="get">` with a `<noscript>` Apply button, so it works without JavaScript.
- **Color is checked.** Three palettes (light, dark, high contrast) plus `auto`, which follows
  `prefers-color-scheme`. In every palette body text, muted text and links clear 4.5:1 on the
  page, inline code text clears 4.5:1 on its own background, borders clear 3:1, and the
  high-contrast palette's text clears 7:1. An unknown theme name falls back to the light palette.
- **Focus is visible**: a term button shows a 3-pixel outline in the link color on keyboard focus.
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
  form, `method="get"`, a `<noscript>` button, the label); `the_page_lists_terms_and_targets_the_detail_pane`
  (`lang`, the live region); `term_text_is_escaped` and
  `hostile_names_are_escaped_in_attributes_and_text`.
- `tests/through_sparql.rs` renders the page and a term's fragment over the real SPARQL engine
  (`the_page_documents_every_term_in_the_vocabulary`,
  `a_term_request_returns_a_fragment_carrying_its_schema`,
  `the_theme_argument_changes_the_rendered_palette`).
- `tests/server_http.rs` (with the `server` feature) checks that the standalone server answers the
  page for `Accept: text/html` and `?as=text/html`, and a `406` naming both faces otherwise.
  `tests/conformance.rs` (`errors_carry_no_registry_body`) pins that no error echoes the registry.
- **No automated accessibility checker, browser or assistive technology has been used on this
  output.** The contrast tests measure the palettes' color pairs; no page has been opened in a
  browser, checked with axe or similar, or read by a screen reader.

## Known limitations

Found by reading the source on 2026-10-31, not by testing with people or assistive technology:

- **On the standalone server, a term's detail cannot be reached from the page.** The page loads
  htmx from `/static/htmx.min.js` and the term buttons request `/r/urn:name:docs?…`, but the
  server routes only `/{ns}` and `/{ns}/{doc}` (`src/bin/server.rs`), so, by reading, neither path
  is served there and the buttons do nothing. This has not been checked in a browser.
- The term buttons are `<button type="button">` that need htmx; with JavaScript off, or where htmx
  does not load, there is no link or form that reaches a term's detail.
- A term's fragment replaces the whole detail region (`hx-swap="outerHTML"`), the live region
  included; a screen reader announces a replaced live region unreliably or not at all. The region
  has `tabindex="-1"`, but nothing moves focus to it.
- A term's comment in the list is cut at 80 characters with no ellipsis, so a cut reads as the
  whole comment.
- The theme chooser sits between the `<header>` and the `<main>`, outside every landmark.
- Whether choosing a theme with htmx loaded restyles the page has not been checked in a browser:
  the chooser swaps the `<body>`, and the palette is a `<style>` in the `<head>`.
- `urn:name:resolve`, `claim` and `admin` answer tab-separated fields with no labels.

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
