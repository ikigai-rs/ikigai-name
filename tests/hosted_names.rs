use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Error, Exact, FnEndpoint, Invocation, Iri, Kernel, ReprType,
    Representation, Request, Verb,
};
use std::sync::Arc;

const VOCAB: &str =
    "<https://example.org/vocab/Thing> a <http://www.w3.org/2000/01/rdf-schema#Class> ; <https://example.org/vocab/predicate> <https://example.org/vocab/object> .";

fn fixture(media: &'static str, body: String) -> FnEndpoint {
    FnEndpoint::new("fixture", move |_: &Invocation<'_>| {
        Ok(Representation::new(
            ReprType::new(media),
            body.as_bytes().to_vec(),
        ))
    })
}

fn registry(mode: &str) -> String {
    format!(
        r#"{{"namespaces":[{{"prefix":"vocab/","owner":"urn:cap:name:admin:vocab","strategy":"hosted","source":"urn:test:vocab",{mode}"documents":["vocab/core"]}}]}}"#
    )
}

fn kernel(mode: &str) -> Kernel {
    Kernel::new(Arc::new(
        ikigai_name::space()
            .bind(
                Exact::new(ikigai_name::REGISTRY_IRI),
                fixture("application/json", registry(mode)),
            )
            .bind(
                Exact::new("urn:test:vocab"),
                fixture("text/turtle", VOCAB.into()),
            ),
    ))
}

fn get(kernel: &Kernel, endpoint: &str, path: &str) -> ikigai_core::Result<Representation> {
    block_on(
        kernel.issue(
            Request::new(Verb::Source, Iri::parse(endpoint).unwrap())
                .with_arg("path", ArgRef::Inline(path.as_bytes().to_vec())),
            &Capability::root(),
        ),
    )
}

#[test]
fn exact_refuses_a_made_up_name() {
    let kernel = kernel(r#""names":"exact","#);
    for endpoint in ["urn:name:document", "urn:name:resolve"] {
        assert!(matches!(
            get(&kernel, endpoint, "vocab/made-up"),
            Err(Error::NotFound(_))
        ));
    }
}

#[test]
fn exact_answers_only_the_prefix_and_listed_documents() {
    let kernel = kernel(r#""names":"exact","#);
    for endpoint in ["urn:name:document", "urn:name:resolve"] {
        for path in ["vocab", "/vocab/", "vocab/core"] {
            get(&kernel, endpoint, path).expect("an explicit name");
        }
        for path in ["vocab/Thing", "vocab/core/made-up", "vocab/Core"] {
            assert!(
                matches!(get(&kernel, endpoint, path), Err(Error::NotFound(_))),
                "{path}"
            );
        }
    }
}

#[test]
fn terms_answers_defined_subjects_but_not_mentions_or_made_up_paths() {
    let kernel = kernel(r#""names":"terms","#);
    for endpoint in ["urn:name:document", "urn:name:resolve"] {
        for path in ["vocab", "vocab/core", "vocab/Thing"] {
            get(&kernel, endpoint, path).expect("an explicit or defined name");
        }
        for path in [
            "vocab/made-up",
            "vocab/thing",
            "vocab/Thing/made-up",
            "vocab/predicate",
            "vocab/object",
        ] {
            assert!(
                matches!(get(&kernel, endpoint, path), Err(Error::NotFound(_))),
                "{path}"
            );
        }
    }
}

#[test]
fn any_and_the_omitted_default_keep_answering_every_path() {
    for mode in ["", r#""names":"any","#] {
        let kernel = kernel(mode);
        for endpoint in ["urn:name:document", "urn:name:resolve"] {
            let a = get(&kernel, endpoint, "vocab/core").unwrap();
            let b = get(&kernel, endpoint, "vocab/made-up/nested").unwrap();
            assert_eq!(a.bytes, b.bytes);
        }
    }
}

#[test]
fn a_bad_mode_refuses_the_registry_naming_the_namespace() {
    for value in [r#""all""#, r#""Exact""#, "null", "42", "[]", "{}"] {
        let registry = format!(
            r#"{{"namespaces":[{{"prefix":"vocab/","owner":"o","strategy":"hosted","source":"urn:test:vocab","names":{value}}}]}}"#
        );
        let error = ikigai_name::registry::Registry::from_json(registry.as_bytes())
            .unwrap_err()
            .to_string();
        assert!(error.contains("namespace \"vocab/\""), "{error}");
        assert!(error.contains("any, exact or terms"), "{error}");
    }
}

#[test]
fn terms_checks_the_historical_document_instead_of_todays() {
    let mode = r#""names":"terms","#;
    let kernel = Kernel::new(Arc::new(
        ikigai_name::space()
            .bind(
                Exact::new(ikigai_name::REGISTRY_IRI),
                fixture("application/json", registry(mode)),
            )
            .bind(
                Exact::new("urn:test:vocab"),
                fixture("text/turtle", String::new()),
            )
            .bind(
                Exact::new(ikigai_name::HISTORY_IRI),
                FnEndpoint::new("history", move |inv: &Invocation<'_>| {
                    let (media, body) = if inv.inline_str("source")? == ikigai_name::REGISTRY_IRI {
                        ("application/json", registry(mode))
                    } else {
                        ("text/turtle", VOCAB.into())
                    };
                    Ok(Representation::new(ReprType::new(media), body.into_bytes()))
                }),
            ),
    ));
    for endpoint in ["urn:name:document", "urn:name:resolve"] {
        assert!(matches!(
            get(&kernel, endpoint, "vocab/Thing"),
            Err(Error::NotFound(_))
        ));
        let request = Request::new(Verb::Source, Iri::parse(endpoint).unwrap())
            .with_arg("path", ArgRef::Inline(b"vocab/Thing".to_vec()))
            .with_arg("as-of", ArgRef::Inline(b"2020-01-01".to_vec()));
        block_on(kernel.issue(request, &Capability::root()))
            .expect("defined in the historical document");
    }
}

#[test]
fn terms_refuses_a_malformed_document_even_after_a_matching_subject() {
    let kernel = Kernel::new(Arc::new(
        ikigai_name::space()
            .bind(
                Exact::new(ikigai_name::REGISTRY_IRI),
                fixture("application/json", registry(r#""names":"terms","#)),
            )
            .bind(
                Exact::new("urn:test:vocab"),
                fixture("text/turtle", format!("{VOCAB} this is not Turtle")),
            ),
    ));
    for endpoint in ["urn:name:document", "urn:name:resolve"] {
        let error = get(&kernel, endpoint, "vocab/Thing").unwrap_err();
        assert!(matches!(error, Error::Endpoint(_)));
        assert!(error.to_string().contains("vocab/"));
    }
}
