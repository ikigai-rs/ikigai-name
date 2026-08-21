//! The HTML documentation face: a namespace, rendered for people.
//!
//! A vocabulary only a parser can read will not be adopted. This face turns the
//! same Turtle that `urn:name:resolve` points at into a page someone can browse
//! — **by transreption, not by maintaining a second copy**, so the documentation
//! cannot drift from the vocabulary.
//!
//! Terms are extracted with SPARQL issued back through the kernel, so this crate
//! carries no RDF parser and gains federation for free: a namespace hosted here
//! and one mirrored from a peer are read the same way.
//!
//! ## Colour is a floor, not a preference
//!
//! Themes are checked against the WCAG contrast floor by
//! [`ikigai_a11y`](https://crates.io/crates/ikigai-a11y), and the check is a
//! **test** rather than a review note: a palette whose body text falls below
//! 4.5:1 fails the build. That matters here because a documentation page is the
//! one artifact a newcomer *must* read, and because the a11y survey found only
//! five of thirty-two common themes clear AA unaided — an unchecked palette is
//! more likely to be illegible than not.
//!
//! Selection is offered three ways, deliberately: `prefers-color-scheme` for
//! people who set it once system-wide, an explicit chooser for everyone else,
//! and a high-contrast palette above the AAA floor for people for whom AA is not
//! enough.

use crate::registry::Strategy;
use crate::{load, resolve_path};
use ikigai_core::{
    ActionSpec, ArgRef, ArgSpec, AsyncFnEndpoint, Description, Error, Invocation, InvokeFuture,
    Iri, ReprType, Representation, Request, Result, Verb,
};

const TEXT_HTML: &str = "text/html;charset=utf-8";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// The SPARQL endpoint terms are read through. A bound IRI, so a host may serve
/// it from wherever it likes.
const SPARQL_IRI: &str = "urn:sparql:select";

fn text_html() -> ReprType {
    ReprType::new("text/html").with_param("charset", "utf-8")
}

// --- palettes ---------------------------------------------------------------

/// A documentation palette. Every field that carries text is contrast-checked
/// against [`Palette::ground`] in the test below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// Machine name, as accepted by `theme=`.
    pub name: &'static str,
    /// Human label for the chooser.
    pub label: &'static str,
    /// Page background.
    pub ground: &'static str,
    /// Body text.
    pub ink: &'static str,
    /// De-emphasised text — the field most likely to fail a floor, because
    /// "muted" is usually achieved by moving it toward the background.
    pub muted: &'static str,
    /// Links and accents.
    pub accent: &'static str,
    /// Rules and borders. Non-text, so it answers to the 3:1 floor.
    pub edge: &'static str,
    /// Inline code background.
    pub code_ground: &'static str,
}

/// WCAG AA for body text.
pub const FLOOR_TEXT: f64 = 4.5;
/// WCAG AA for large text and non-text UI.
pub const FLOOR_UI: f64 = 3.0;
/// WCAG AAA, which the high-contrast palette holds itself to.
pub const FLOOR_ENHANCED: f64 = 7.0;

/// The light palette (also the `auto` default in a light environment).
pub const LIGHT: Palette = Palette {
    name: "light",
    label: "Light",
    ground: "#ffffff",
    ink: "#1a1a1a",
    muted: "#595959",
    accent: "#0b4fa8",
    edge: "#767676",
    code_ground: "#f2f2f2",
};

/// The dark palette.
pub const DARK: Palette = Palette {
    name: "dark",
    label: "Dark",
    ground: "#14161a",
    ink: "#eceff4",
    muted: "#a8b0bd",
    accent: "#8ab4f8",
    edge: "#7d8695",
    code_ground: "#22262e",
};

/// A palette above the AAA floor, for whom AA is not enough.
pub const HIGH_CONTRAST: Palette = Palette {
    name: "contrast",
    label: "High contrast",
    ground: "#000000",
    ink: "#ffffff",
    muted: "#e0e0e0",
    accent: "#ffe066",
    edge: "#ffffff",
    code_ground: "#1a1a1a",
};

/// Every offered palette, in chooser order.
pub const PALETTES: [Palette; 3] = [LIGHT, DARK, HIGH_CONTRAST];

/// The palette named by `theme=`, or [`LIGHT`] for `auto` and anything
/// unrecognised — an unknown name must degrade to a readable page, never to no
/// page.
pub fn palette(name: &str) -> Palette {
    PALETTES
        .iter()
        .find(|p| p.name == name)
        .copied()
        .unwrap_or(LIGHT)
}

/// The stylesheet.
///
/// `auto` emits both palettes behind `prefers-color-scheme` so a system-wide
/// preference is honoured without anyone choosing anything; an explicit theme
/// emits just that one.
fn stylesheet(theme: &str) -> String {
    let vars = |p: &Palette| {
        format!(
            "--ground:{};--ink:{};--muted:{};--accent:{};--edge:{};--code:{};",
            p.ground, p.ink, p.muted, p.accent, p.edge, p.code_ground
        )
    };
    let scheme = if theme == "auto" {
        format!(
            ":root{{color-scheme:light dark;{}}}\
             @media (prefers-color-scheme:dark){{:root{{{}}}}}",
            vars(&LIGHT),
            vars(&DARK)
        )
    } else {
        let chosen = palette(theme);
        let scheme_hint = if chosen.name == "light" {
            "light"
        } else {
            "dark"
        };
        format!(":root{{color-scheme:{scheme_hint};{}}}", vars(&chosen))
    };
    format!(
        "{scheme}\
         *{{box-sizing:border-box}}\
         body{{margin:0;padding:1.5rem;background:var(--ground);color:var(--ink);\
         font:16px/1.6 system-ui,-apple-system,Segoe UI,sans-serif;max-width:60rem}}\
         a{{color:var(--accent)}}\
         h1{{font-size:1.5rem;margin:0 0 .25rem}}\
         .muted{{color:var(--muted)}}\
         code,.iri{{background:var(--code);padding:.1em .35em;border-radius:3px;\
         font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:.9em}}\
         .terms{{list-style:none;padding:0;display:grid;gap:.5rem;\
         grid-template-columns:repeat(auto-fill,minmax(14rem,1fr))}}\
         .term{{display:block;width:100%;text-align:left;padding:.6rem .75rem;\
         border:1px solid var(--edge);border-radius:6px;background:var(--ground);\
         color:var(--ink);font:inherit;cursor:pointer}}\
         .term:hover,.term:focus-visible{{border-color:var(--accent);outline:none}}\
         .term:focus-visible{{outline:3px solid var(--accent);outline-offset:2px}}\
         .detail{{margin-top:1.5rem;padding:1rem;border:1px solid var(--edge);border-radius:6px}}\
         .chooser{{display:flex;gap:.5rem;align-items:center;margin:1rem 0}}\
         select{{background:var(--ground);color:var(--ink);border:1px solid var(--edge);\
         padding:.3rem;border-radius:4px;font:inherit}}\
         @media (prefers-reduced-motion:reduce){{*{{transition:none!important;\
         animation:none!important}}}}"
    )
}

// --- term extraction --------------------------------------------------------

/// One documented term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    /// The term's IRI.
    pub iri: String,
    /// Its local name, for display.
    pub local: String,
    /// `rdfs:comment`, if it has one.
    pub comment: Option<String>,
    /// `rdfs:subClassOf` / `rdfs:subPropertyOf` target, if any.
    pub parent: Option<String>,
    /// `rdfs:domain`, if any.
    pub domain: Option<String>,
    /// `rdfs:range`, if any.
    pub range: Option<String>,
}

/// The local name of an IRI — after the last `#` or `/`.
fn local_name(iri: &str) -> String {
    iri.rsplit(['#', '/']).next().unwrap_or(iri).to_string()
}

/// The query for one namespace's terms.
///
/// ★ **Scoped to the named graph, and it must stay that way.** `ikigai-sparql`
/// loads each `graph=` source under its own name *and always loads the ikigai
/// vocabulary alongside it*, with the default graph as the union of both. An
/// unscoped `?term a ?kind` therefore documents ikigai's ~84 vocabulary terms as
/// though they belonged to the namespace being viewed — which is what the first
/// version of this did, and what the integration test caught.
fn term_query(graph: &str) -> String {
    format!(
        "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>\n\
         PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>\n\
         PREFIX owl: <http://www.w3.org/2002/07/owl#>\n\
         SELECT ?term ?comment ?parent ?domain ?range WHERE {{\n\
           GRAPH <{graph}> {{\n\
             ?term a ?kind .\n\
             FILTER(?kind IN (rdfs:Class, rdf:Property, owl:Class, owl:ObjectProperty, \
                              owl:DatatypeProperty))\n\
             OPTIONAL {{ ?term rdfs:comment ?comment }}\n\
             OPTIONAL {{ ?term rdfs:subClassOf ?parent }}\n\
             OPTIONAL {{ ?term rdfs:subPropertyOf ?parent }}\n\
             OPTIONAL {{ ?term rdfs:domain ?domain }}\n\
             OPTIONAL {{ ?term rdfs:range ?range }}\n\
           }}\n\
         }}\n\
         ORDER BY ?term"
    )
}

/// Pull one variable out of a SPARQL JSON binding.
fn bound(row: &serde_json::Value, name: &str) -> Option<String> {
    row.get(name)?.get("value")?.as_str().map(|s| s.to_string())
}

/// Parse `application/sparql-results+json` into terms.
fn terms_from_results(json: &[u8]) -> Result<Vec<Term>> {
    let parsed: serde_json::Value = serde_json::from_slice(json)
        .map_err(|e| Error::Endpoint(format!("name: unreadable SPARQL results: {e}")))?;
    let rows = parsed
        .get("results")
        .and_then(|r| r.get("bindings"))
        .and_then(|b| b.as_array())
        .ok_or_else(|| Error::Endpoint("name: SPARQL results had no bindings".into()))?;

    let mut terms: Vec<Term> = Vec::new();
    for row in rows {
        let Some(iri) = bound(row, "term") else {
            continue;
        };
        // OPTIONAL clauses multiply rows, so a term with a domain AND a range
        // arrives as several rows. Merge rather than emitting duplicates.
        if let Some(existing) = terms.iter_mut().find(|t| t.iri == iri) {
            existing.comment = existing.comment.take().or_else(|| bound(row, "comment"));
            existing.parent = existing.parent.take().or_else(|| bound(row, "parent"));
            existing.domain = existing.domain.take().or_else(|| bound(row, "domain"));
            existing.range = existing.range.take().or_else(|| bound(row, "range"));
            continue;
        }
        terms.push(Term {
            local: local_name(&iri),
            comment: bound(row, "comment"),
            parent: bound(row, "parent"),
            domain: bound(row, "domain"),
            range: bound(row, "range"),
            iri,
        });
    }
    Ok(terms)
}

/// Read a namespace's terms by asking the SPARQL module, through the kernel.
async fn terms_of(inv: &Invocation<'_>, graph: &str) -> Result<Vec<Term>> {
    // The graph IRI is interpolated into the query, so it is validated first: a
    // registry is operator-edited, but "operator-supplied" is not "safe to
    // splice into a query language".
    Iri::parse(graph).map_err(|e| {
        Error::Endpoint(format!("name: {graph:?} is not a usable graph source: {e}"))
    })?;
    if graph.contains(['<', '>', '"', '\\']) || graph.chars().any(char::is_whitespace) {
        return Err(Error::Endpoint(format!(
            "name: {graph:?} cannot be used as a graph name"
        )));
    }

    let iri = Iri::parse(SPARQL_IRI).map_err(|e| Error::Endpoint(format!("name: {e}")))?;
    let repr = inv
        .issue(
            Request::new(Verb::Source, iri)
                .with_arg("query", ArgRef::Inline(term_query(graph).into_bytes()))
                .with_arg("graph", ArgRef::Inline(graph.as_bytes().to_vec())),
        )
        .await?;
    terms_from_results(&repr.bytes)
}

// --- rendering --------------------------------------------------------------

/// Escape text for HTML. Vocabulary comments are operator-supplied and may
/// contain anything.
fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// One term's detail — the htmx fragment.
fn detail_html(term: &Term) -> String {
    let mut out = format!(
        "<div class=\"detail\" id=\"detail\" tabindex=\"-1\" aria-live=\"polite\">\
         <h2>{}</h2><p class=\"iri\">{}</p>",
        esc(&term.local),
        esc(&term.iri)
    );
    if let Some(comment) = &term.comment {
        out.push_str(&format!("<p>{}</p>", esc(comment)));
    }
    let mut rows = String::new();
    for (label, value) in [
        ("Subclass/subproperty of", &term.parent),
        ("Domain", &term.domain),
        ("Range", &term.range),
    ] {
        if let Some(value) = value {
            rows.push_str(&format!(
                "<tr><th scope=\"row\">{label}</th><td><code>{}</code></td></tr>",
                esc(&local_name(value))
            ));
        }
    }
    if !rows.is_empty() {
        out.push_str(&format!("<table>{rows}</table>"));
    }
    out.push_str("</div>");
    out
}

/// The theme chooser. A real `<select>` inside a `<form>`, so it works without
/// JavaScript and is reachable by keyboard and screen reader; htmx upgrades it
/// to swap in place when available.
fn chooser_html(prefix: &str, theme: &str) -> String {
    let options = PALETTES
        .iter()
        .map(|p| {
            format!(
                "<option value=\"{}\"{}>{}</option>",
                p.name,
                if p.name == theme { " selected" } else { "" },
                esc(p.label)
            )
        })
        .collect::<String>();
    let auto_selected = if theme == "auto" { " selected" } else { "" };
    format!(
        "<form class=\"chooser\" method=\"get\" \
         hx-get=\"/r/urn:name:docs\" hx-target=\"body\" hx-trigger=\"change from:#theme\">\
         <input type=\"hidden\" name=\"prefix\" value=\"{}\">\
         <label for=\"theme\">Theme</label>\
         <select id=\"theme\" name=\"theme\">\
         <option value=\"auto\"{auto_selected}>Match system</option>{options}</select>\
         <noscript><button type=\"submit\">Apply</button></noscript></form>",
        esc(prefix)
    )
}

/// The whole page.
fn page_html(prefix: &str, theme: &str, source: &str, terms: &[Term]) -> String {
    let items = terms
        .iter()
        .map(|t| {
            format!(
                "<li><button class=\"term\" type=\"button\" \
                 hx-get=\"/r/urn:name:docs?prefix={}&amp;term={}\" \
                 hx-target=\"#detail\" hx-swap=\"outerHTML\">\
                 <strong>{}</strong>{}</button></li>",
                esc(prefix),
                esc(&t.local),
                esc(&t.local),
                t.comment
                    .as_deref()
                    .map(|c| format!(
                        "<br><span class=\"muted\">{}</span>",
                        esc(c.chars().take(80).collect::<String>().trim())
                    ))
                    .unwrap_or_default()
            )
        })
        .collect::<String>();

    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{prefix} — namespace documentation</title>\
         <style>{}</style>\
         <script src=\"/static/htmx.min.js\" defer></script></head>\
         <body><header><h1>{prefix}</h1>\
         <p class=\"muted\">{} term{} · served from <code>{}</code></p></header>\
         {}<main><h2 id=\"terms-heading\">Terms</h2>\
         <ul class=\"terms\" aria-labelledby=\"terms-heading\">{items}</ul>\
         <div class=\"detail\" id=\"detail\" tabindex=\"-1\" aria-live=\"polite\">\
         <p class=\"muted\">Choose a term to see its definition.</p></div></main></body></html>",
        stylesheet(theme),
        terms.len(),
        if terms.len() == 1 { "" } else { "s" },
        esc(source),
        chooser_html(prefix, theme),
        prefix = esc(prefix),
    )
}

// --- the endpoint -----------------------------------------------------------

/// `urn:name:docs` — a namespace, documented for people.
pub fn docs() -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("docs", |inv: &Invocation<'_>| -> InvokeFuture<'_> {
        Box::pin(async move {
            let prefix = inv
                .inline_str("prefix")
                .map_err(|_| Error::MissingArgument("prefix".into()))?
                .trim()
                .to_string();
            let theme = inv
                .inline_str("theme")
                .map(|t| t.trim().to_string())
                .unwrap_or_else(|_| "auto".to_string());

            let registry = load(inv).await?;
            let found = resolve_path(&registry, &prefix)?;
            // Only a hosted namespace has documents here to read. A redirect is
            // someone else's to document, and saying so is more useful than an
            // empty page that looks like a vocabulary with no terms.
            let graph = match &found.strategy {
                Strategy::Hosted { source } => source.clone(),
                Strategy::Mirror { origin } => origin.clone(),
                Strategy::Redirect { target } => {
                    return Err(Error::Endpoint(format!(
                        "name: {prefix:?} is served by its owner at {target} — documentation \
                         lives there"
                    )))
                }
                Strategy::Retired { reason } => {
                    return Err(Error::NotFound(format!(
                        "name: {prefix:?} is retired: {reason}"
                    )))
                }
            };

            let terms = terms_of(inv, &graph).await?;

            // A `term=` request is htmx asking for one fragment, not a page.
            let body = match inv.inline_str("term").ok().map(|t| t.trim().to_string()) {
                Some(wanted) if !wanted.is_empty() => {
                    let term = terms
                        .iter()
                        .find(|t| t.local == wanted)
                        .ok_or_else(|| Error::NotFound(format!("name: no term {wanted:?}")))?;
                    detail_html(term)
                }
                _ => page_html(&prefix, &theme, &graph, &terms),
            };
            // ★ `.cacheable()` is a claim that THIS computation is pure — the
            // same registry, graph and arguments always render the same page —
            // not a claim that the inputs are fresh. The kernel takes the meet
            // of this with every dependency's expiry (kernel.rs), so a volatile
            // source still yields a volatile page. Without it the page is
            // unconditionally uncacheable, and every view re-runs the SPARQL
            // query even when nothing has changed.
            Ok(Representation::new(text_html(), body.into_bytes()).cacheable())
        })
    })
    .with_description(
        Description::new("docs")
            .title("Namespace documentation")
            .summary(
                "A hosted namespace rendered as HTML for people, transrepted from the same \
                 Turtle the namespace serves so the two cannot drift. With `term=`, returns \
                 that term's detail as an htmx fragment.",
            )
            .action(
                ActionSpec::new(Verb::Source)
                    .summary("document a namespace, or one term of it")
                    .input(
                        ArgSpec::new("prefix")
                            .summary("the namespace to document, e.g. resmud")
                            .class(XSD_STRING),
                    )
                    .input(
                        ArgSpec::new("term")
                            .summary("optional: one term's local name, returned as a fragment")
                            .optional()
                            .class(XSD_STRING),
                    )
                    .input(
                        ArgSpec::new("theme")
                            .summary("colour theme; `auto` follows prefers-color-scheme")
                            .one_of(["auto", "light", "dark", "contrast"])
                            .default_value("auto")
                            .optional()
                            .class(XSD_STRING),
                    )
                    .output(TEXT_HTML),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ikigai_a11y::{ratio, Rgba};

    fn rgba(hex: &str) -> Rgba {
        Rgba::parse(hex).expect("a palette colour parses")
    }

    /// ★ The floor is a test, not a review note. Every palette's body text must
    /// clear WCAG AA against its own ground — including `muted`, which is the
    /// field most likely to fail, since "muted" is usually achieved by moving
    /// text toward the background until it stops being readable.
    #[test]
    fn every_palette_clears_the_text_floor() {
        for p in PALETTES {
            let ground = rgba(p.ground);
            for (field, colour) in [("ink", p.ink), ("muted", p.muted), ("accent", p.accent)] {
                let r = ratio(rgba(colour), ground);
                assert!(
                    r >= FLOOR_TEXT,
                    "{}: {field} is {r:.2}:1 against the ground, below the {FLOOR_TEXT}:1 floor",
                    p.name
                );
            }
        }
    }

    /// Borders and rules carry no text, so they answer to the 3:1 non-text
    /// floor — but they must still be visible, which an unchecked "subtle"
    /// border routinely is not.
    #[test]
    fn every_palette_clears_the_ui_floor_for_edges() {
        for p in PALETTES {
            let r = ratio(rgba(p.edge), rgba(p.ground));
            assert!(
                r >= FLOOR_UI,
                "{}: edge is {r:.2}:1, below the {FLOOR_UI}:1 non-text floor",
                p.name
            );
        }
    }

    /// Inline code sits on its own background, so the pair that matters is
    /// ink-on-code, not ink-on-ground.
    #[test]
    fn code_is_readable_on_its_own_background() {
        for p in PALETTES {
            let r = ratio(rgba(p.ink), rgba(p.code_ground));
            assert!(
                r >= FLOOR_TEXT,
                "{}: body text on the code background is {r:.2}:1",
                p.name
            );
        }
    }

    /// The high-contrast palette exists for people whom AA does not serve, so
    /// it is held to AAA — otherwise it is just a third theme.
    #[test]
    fn the_high_contrast_palette_clears_the_enhanced_floor() {
        let ground = rgba(HIGH_CONTRAST.ground);
        for (field, colour) in [
            ("ink", HIGH_CONTRAST.ink),
            ("muted", HIGH_CONTRAST.muted),
            ("accent", HIGH_CONTRAST.accent),
        ] {
            let r = ratio(rgba(colour), ground);
            assert!(
                r >= FLOOR_ENHANCED,
                "high contrast: {field} is {r:.2}:1, below the AAA {FLOOR_ENHANCED}:1 floor"
            );
        }
    }

    #[test]
    fn an_unknown_theme_degrades_to_a_readable_palette() {
        assert_eq!(palette("chartreuse"), LIGHT);
        assert_eq!(palette("dark"), DARK);
    }

    #[test]
    fn auto_emits_both_schemes_behind_the_media_query() {
        let css = stylesheet("auto");
        assert!(css.contains("prefers-color-scheme:dark"), "{css}");
        assert!(css.contains(LIGHT.ground), "carries the light ground");
        assert!(css.contains(DARK.ground), "carries the dark ground");
    }

    #[test]
    fn an_explicit_theme_emits_only_that_one() {
        let css = stylesheet("contrast");
        assert!(
            !css.contains("prefers-color-scheme"),
            "no media query: {css}"
        );
        assert!(css.contains(HIGH_CONTRAST.accent));
    }

    /// Reduced motion is honoured unconditionally: there is no animation worth
    /// overriding someone's stated medical preference for.
    #[test]
    fn reduced_motion_is_honoured() {
        assert!(stylesheet("auto").contains("prefers-reduced-motion:reduce"));
    }

    #[test]
    fn optional_clauses_do_not_produce_duplicate_terms() {
        let json = br#"{"head":{"vars":["term","domain","range"]},"results":{"bindings":[
          {"term":{"type":"uri","value":"https://x/ns#damage"},
           "domain":{"type":"uri","value":"https://x/ns#Weapon"}},
          {"term":{"type":"uri","value":"https://x/ns#damage"},
           "range":{"type":"uri","value":"http://www.w3.org/2001/XMLSchema#integer"}}
        ]}}"#;
        let terms = terms_from_results(json).expect("parses");
        assert_eq!(terms.len(), 1, "one term, merged: {terms:?}");
        assert_eq!(terms[0].local, "damage");
        assert!(terms[0].domain.is_some() && terms[0].range.is_some());
    }

    /// Vocabulary comments are operator-supplied text and reach the page
    /// verbatim, so they are escaped.
    #[test]
    fn term_text_is_escaped() {
        let term = Term {
            iri: "https://x/ns#Trouble".into(),
            local: "Trouble".into(),
            comment: Some("<script>alert('x')</script>".into()),
            parent: None,
            domain: None,
            range: None,
        };
        let html = detail_html(&term);
        assert!(!html.contains("<script>"), "escaped: {html}");
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn the_chooser_works_without_javascript() {
        let html = chooser_html("resmud", "dark");
        assert!(html.contains("<form"), "a real form: {html}");
        assert!(html.contains("method=\"get\""), "submits on its own");
        assert!(html.contains("<noscript>"), "offers a submit button");
        assert!(
            html.contains("value=\"dark\" selected"),
            "reflects the choice"
        );
        assert!(html.contains("for=\"theme\""), "the select is labelled");
    }

    #[test]
    fn the_page_lists_terms_and_targets_the_detail_pane() {
        let terms = vec![Term {
            iri: "https://x/ns#Weapon".into(),
            local: "Weapon".into(),
            comment: Some("Anything that can inflict harm.".into()),
            parent: None,
            domain: None,
            range: None,
        }];
        let html = page_html("resmud", "auto", "urn:file:x", &terms);
        assert!(
            html.contains("hx-target=\"#detail\""),
            "htmx targets the pane"
        );
        assert!(html.contains("lang=\"en\""), "declares a language");
        assert!(html.contains("aria-live=\"polite\""), "announces the swap");
        assert!(html.contains("1 term ·"), "singular is not '1 terms'");
    }

    /// The regression guard for the bug the integration test caught: without
    /// the GRAPH clause the page documents ikigai's own vocabulary too.
    #[test]
    fn the_term_query_is_scoped_to_one_named_graph() {
        let q = term_query("urn:file:vocab");
        assert!(q.contains("GRAPH <urn:file:vocab>"), "scoped: {q}");
    }

    #[test]
    fn local_names_come_off_either_separator() {
        assert_eq!(local_name("https://x/ns#Weapon"), "Weapon");
        assert_eq!(local_name("https://x/ns/Weapon"), "Weapon");
    }
}
