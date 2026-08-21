//! The documentation face, proved through the real SPARQL module.
//!
//! The unit tests parse a hand-written `sparql-results+json` fixture, which
//! proves the merge logic but not that the query is right or that the format is
//! what the engine actually emits. This test runs the real thing: a Turtle
//! graph bound as a resource, `ikigai-sparql` resolving it through the kernel,
//! and `urn:name:docs` rendering whatever comes back.

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Endpoint, Exact, FnEndpoint, Invocation, Iri, Kernel, ReprType,
    Representation, Request, Result, Verb,
};
use std::sync::Arc;

/// A vocabulary in the shape ResMUD's actually takes: a class hierarchy, and a
/// property whose domain is what makes the border rule work.
const VOCAB: &str = r#"
@prefix rm:   <https://iriref.org/resmud/core#> .
@prefix rdf:  <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd:  <http://www.w3.org/2001/XMLSchema#> .

rm:Thing  a rdfs:Class ; rdfs:comment "Anything that can exist in a world." .
rm:Item   a rdfs:Class ; rdfs:subClassOf rm:Thing .
rm:Weapon a rdfs:Class ; rdfs:subClassOf rm:Item ;
    rdfs:comment "The class a violence-free realm refuses at its border." .

rm:damage a rdf:Property ;
    rdfs:domain rm:Weapon ;
    rdfs:range xsd:integer ;
    rdfs:comment "Declaring damage entails rm:Weapon." .
"#;

const REGISTRY: &str = r#"{"namespaces":[{"prefix":"resmud",
    "owner":"urn:cap:name:admin:resmud",
    "strategy":"hosted","source":"urn:test:vocab"}]}"#;

fn serving(media: &'static str, body: &'static str) -> FnEndpoint {
    FnEndpoint::new("fixture", move |_: &Invocation<'_>| {
        Ok(Representation::new(
            ReprType::new(media),
            body.as_bytes().to_vec(),
        ))
    })
}

/// Bound explicitly rather than by combining module spaces, so the test states
/// exactly what the docs face depends on: the real SPARQL engine, a registry,
/// and a graph to read.
fn kernel() -> Kernel {
    let space = ikigai_sparql::space()
        .bind(Exact::new("urn:name:docs"), ikigai_name::docs())
        .bind(Exact::new("urn:test:vocab"), serving("text/turtle", VOCAB))
        .bind(
            Exact::new("urn:name:registry-source"),
            serving("application/json", REGISTRY),
        );
    Kernel::new(Arc::new(space))
}

fn get(args: &[(&str, &str)]) -> Result<String> {
    let mut request = Request::new(Verb::Source, Iri::parse("urn:name:docs").unwrap());
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    let repr = block_on(kernel().issue(request, &Capability::root()))?;
    Ok(String::from_utf8(repr.bytes.clone()).expect("utf-8"))
}

#[test]
fn the_page_documents_every_term_in_the_vocabulary() {
    let html = get(&[("prefix", "resmud")]).expect("renders");
    for term in ["Thing", "Item", "Weapon", "damage"] {
        assert!(html.contains(term), "{term} is missing from the page");
    }
    assert!(html.contains("4 terms"), "counts them: {html}");
    assert!(
        html.contains("Anything that can exist in a world."),
        "carries rdfs:comment through"
    );
}

/// The htmx path: one term, as a fragment rather than a page.
#[test]
fn a_term_request_returns_a_fragment_carrying_its_schema() {
    let html = get(&[("prefix", "resmud"), ("term", "damage")]).expect("renders");
    assert!(
        !html.contains("<!doctype"),
        "a fragment, not a page: {html}"
    );
    assert!(html.contains("id=\"detail\""), "swaps into the pane");
    assert!(html.contains("Weapon"), "shows the domain");
    assert!(html.contains("integer"), "shows the range");
    assert!(
        html.contains("Declaring damage entails"),
        "shows the comment"
    );
}

#[test]
fn a_subclass_shows_what_it_descends_from() {
    let html = get(&[("prefix", "resmud"), ("term", "Weapon")]).expect("renders");
    assert!(html.contains("Item"), "names its parent: {html}");
}

#[test]
fn an_unknown_term_is_not_found() {
    let err = get(&[("prefix", "resmud"), ("term", "Quokka")]).expect_err("no such term");
    assert!(
        matches!(err, ikigai_core::Error::NotFound(_)),
        "got: {err:?}"
    );
}

#[test]
fn an_unclaimed_namespace_is_not_found() {
    let err = get(&[("prefix", "nobody")]).expect_err("unclaimed");
    assert!(
        matches!(err, ikigai_core::Error::NotFound(_)),
        "got: {err:?}"
    );
}

/// The theme argument reaches the stylesheet, so a chooser selection actually
/// changes the page rather than only the form's state.
#[test]
fn the_theme_argument_changes_the_rendered_palette() {
    let auto = get(&[("prefix", "resmud")]).expect("renders");
    assert!(
        auto.contains("prefers-color-scheme"),
        "auto follows the system"
    );

    let contrast = get(&[("prefix", "resmud"), ("theme", "contrast")]).expect("renders");
    assert!(
        !contrast.contains("prefers-color-scheme:dark"),
        "an explicit choice is not overridden by the system"
    );
    assert!(
        contrast.contains("#ffe066"),
        "the high-contrast accent is present"
    );
}

/// Declared = enforced, and the manifold must not over-offer: docs reads
/// public vocabulary, so it declares no capability.
#[test]
fn docs_declares_its_contract() {
    let described = format!("{:?}", ikigai_name::docs().describe());
    assert!(described.contains("prefix"), "declares prefix: {described}");
    assert!(described.contains("theme"), "declares theme");
    assert!(described.contains("text/html"), "declares its output");
}
