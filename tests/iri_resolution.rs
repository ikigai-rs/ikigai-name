//! URN and tag names: claims on an IRI prefix, resolution as data, the urnid
//! equivalence, the same answer for a private name as for an unknown one, and
//! delegation to an upstream resolver.

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Error, Exact, FnEndpoint, Invocation, Iri, Kernel, ReprType,
    Representation, Request, Result, Verb,
};
use ikigai_name::registry::{ClaimError, LoadError, Namespace, Registry, Strategy};
use ikigai_name::tenant::tenant_scope;
use ikigai_name::REGISTRY_IRI;
use std::sync::{Arc, Mutex};

const REGISTRY_JSON: &str = r#"{
  "namespaces": [
    {"prefix": "resmud", "owner": "urn:cap:name:admin:resmud",
     "strategy": "hosted", "source": "urn:file:resmud-vocab"},
    {"prefix": "urn:urnid:acme:", "owner": "urn:cap:name:admin:acme",
     "strategy": "redirect", "target": "https://acme.example/ns", "status": 303,
     "rules": [{"match": "item/{id}", "target": "https://acme.example/items/{id}"}]},
    {"prefix": "tag:urnid.org,2026:vocab:", "owner": "urn:cap:name:admin:vocab",
     "strategy": "hosted", "source": "urn:file:vocab"},
    {"prefix": "urn:urnid:secret:", "owner": "urn:cap:name:admin:secret",
     "strategy": "hosted", "source": "urn:file:secret", "public": false},
    {"prefix": "urn:urnid:elsewhere:", "owner": "urn:cap:name:admin:elsewhere",
     "strategy": "delegate", "upstream": "https://urnid.org/"}
  ]
}"#;

const VOCAB: &str = "@prefix ex: <urn:urnid:vocab:> .\nex:a ex:b ex:c .\n";

/// What the stand-in HTTP client was asked for.
type Asked = Arc<Mutex<Vec<String>>>;

/// A kernel over `json`, with a vocabulary file and, when `asked` is given, a stand-in
/// `urn:http:get` answering every URL with a line naming it.
fn kernel(json: &'static str, asked: Option<Asked>) -> Kernel {
    let source = FnEndpoint::new("registry-source", move |_: &Invocation<'_>| {
        Ok(Representation::new(
            ReprType::new("application/json"),
            json.as_bytes().to_vec(),
        ))
    });
    let vocab = FnEndpoint::new("vocab", |_: &Invocation<'_>| {
        Ok(Representation::new(
            ReprType::new("text/turtle"),
            VOCAB.as_bytes().to_vec(),
        ))
    });
    let mut space = ikigai_name::space()
        .bind(Exact::new(REGISTRY_IRI), source)
        .bind(Exact::new("urn:file:vocab"), vocab);
    if let Some(asked) = asked {
        let get = FnEndpoint::new("http-get", move |inv: &Invocation<'_>| {
            let url = inv.inline_str("url")?.to_string();
            asked.lock().unwrap().push(url.clone());
            if url.contains("unknown") {
                return Err(Error::NotFound(format!(
                    "upstream: no namespace claims {url}"
                )));
            }
            Ok(Representation::new(
                ReprType::new("text/plain"),
                format!("upstream answered {url}\n").into_bytes(),
            ))
        });
        space = space.bind(Exact::new("urn:http:get"), get);
    }
    Kernel::new(Arc::new(space))
}

fn source(kernel: &Kernel, iri: &str, args: &[(&str, &str)], cap: &Capability) -> Result<String> {
    let mut request = Request::new(Verb::Source, Iri::parse(iri).expect("an IRI"));
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    let repr = block_on(kernel.issue(request, cap))?;
    Ok(String::from_utf8(repr.bytes.clone()).expect("utf-8"))
}

fn resolution(kernel: &Kernel, iri: &str, args: &[(&str, &str)]) -> Result<String> {
    source(
        kernel,
        &format!("urn:name:resolution:iri:{iri}"),
        args,
        &Capability::root(),
    )
}

fn json(text: &str) -> serde_json::Value {
    serde_json::from_str(text).expect("JSON")
}

fn hosted(prefix: &str) -> Namespace {
    Namespace {
        prefix: prefix.into(),
        owner: "urn:cap:name:admin:x".into(),
        strategy: Strategy::Hosted {
            source: "urn:file:x".into(),
        },
        repository: None,
        public: true,
    }
}

// --- a claim on an IRI prefix ---------------------------------------------------

/// The claim rule, unchanged, over colon-separated segments: at claim time and at load.
#[test]
fn colon_segmented_prefixes_that_overlap_are_refused_at_claim_and_at_load() {
    for (held, wanted, overlapping) in [
        ("urn:urnid:acme:", "urn:urnid:acme:", true),
        ("urn:urnid:acme:", "urn:urnid:acme:sub:", true),
        ("urn:urnid:acme:sub:", "urn:urnid:acme:", true),
        // The equivalence: the tag form is the same claim, whatever the owner's case.
        ("urn:urnid:acme:", "tag:urnid.org,2026:ACME:", true),
        ("tag:urnid.org,2026:acme:x:", "urn:urnid:Acme:", true),
        // Segment-wise, never textual.
        ("urn:urnid:acme:", "urn:urnid:acmecorp:", false),
        ("urn:urnid:acme:", "tag:urnid.org,2027:acme:", false),
        ("urn:urnid:acme:", "urn:isbn:", false),
        // A path is never an IRI, whatever it shares with one.
        ("urn", "urn:urnid:acme:", false),
        ("urn:urnid:acme:", "urn/urnid/acme", false),
    ] {
        let registry = Registry {
            namespaces: vec![hosted(held)],
            ..Registry::default()
        };
        let by_claim = registry.may_claim(wanted);
        assert_eq!(
            by_claim.is_err(),
            overlapping,
            "{held} then {wanted}: {by_claim:?}"
        );
        if overlapping {
            assert_eq!(by_claim, Err(ClaimError::Overlaps(held.into())));
        }
        let file = serde_json::to_vec(&Registry {
            namespaces: vec![hosted(held), hosted(wanted)],
            ..Registry::default()
        })
        .unwrap();
        match Registry::from_json(&file) {
            Ok(_) => assert!(!overlapping, "{held} then {wanted} loaded"),
            Err(LoadError::Refused { prefix, error }) => {
                assert!(overlapping, "{held} then {wanted}: {error}");
                assert_eq!(prefix, wanted);
                assert_eq!(error, ClaimError::Overlaps(held.into()));
            }
            Err(other) => panic!("{other}"),
        }
    }
}

#[test]
fn lookup_finds_the_one_iri_claim_containing_a_name_in_either_form() {
    let registry = Registry::from_json(REGISTRY_JSON.as_bytes()).expect("loads");
    for (name, claim) in [
        ("urn:urnid:acme:thing", Some("urn:urnid:acme:")),
        ("tag:urnid.org,2026:ACME:thing", Some("urn:urnid:acme:")),
        ("urn:urnid:vocab:a", Some("tag:urnid.org,2026:vocab:")),
        ("urn:urnid:acmecorp:thing", None),
        ("urn:urnid:acme", Some("urn:urnid:acme:")),
        ("resmud/core", Some("resmud")),
        ("urn/urnid/acme", None),
    ] {
        assert_eq!(
            registry.lookup(name).map(|ns| ns.prefix.as_str()),
            claim,
            "{name}"
        );
    }
}

// --- resolution of one IRI ------------------------------------------------------

#[test]
fn a_resolution_answers_target_strategy_claim_and_tenant_as_json() {
    let kernel = kernel(REGISTRY_JSON, None);
    let got = json(&resolution(&kernel, "urn:urnid:acme:item/42", &[]).expect("resolves"));
    assert_eq!(got["name"], "urn:urnid:acme:item/42");
    assert_eq!(got["canonical"], "urn:urnid:acme:item/42");
    assert_eq!(got["claim"], "urn:urnid:acme:");
    assert_eq!(got["strategy"], "redirect");
    assert_eq!(got["target"], "https://acme.example/ns");
    assert_eq!(got["status"], 303);
    assert!(got.get("tenant").is_none(), "no tenant was asked of: {got}");

    // Colon segments are what a rule matches.
    let ruled = json(&resolution(&kernel, "urn:urnid:acme:item:42", &[]).expect("resolves"));
    assert_eq!(ruled["target"], "https://acme.example/items/42");
    assert_eq!(ruled["rule"], "item/{id}");

    let hosted = json(&resolution(&kernel, "urn:urnid:vocab:a", &[]).expect("resolves"));
    assert_eq!(hosted["strategy"], "hosted");
    assert_eq!(hosted["target"], "urn:file:vocab");
    assert_eq!(hosted["claim"], "tag:urnid.org,2026:vocab:");

    // The tenant is the one the capability names.
    let cap = Capability::scoped([tenant_scope("urnid.org").unwrap()]);
    let tenanted = json(
        &source(
            &kernel,
            "urn:name:resolution",
            &[("iri", "urn:urnid:vocab:a")],
            &cap,
        )
        .expect("resolves"),
    );
    assert_eq!(tenanted["tenant"], "urnid.org");
}

#[test]
fn the_urn_and_the_tag_resolve_to_one_answer() {
    let kernel = kernel(REGISTRY_JSON, None);
    let mut urn = json(&resolution(&kernel, "urn:urnid:ACME:item:7", &[]).unwrap());
    let mut tag = json(&resolution(&kernel, "tag:urnid.org,2026:acme:item:7", &[]).unwrap());
    assert_eq!(urn["canonical"], "urn:urnid:acme:item:7");
    for answer in [&mut urn, &mut tag] {
        answer.as_object_mut().unwrap().remove("name");
    }
    assert_eq!(urn, tag);
    // The local part is case-sensitive.
    let other = json(&resolution(&kernel, "urn:urnid:acme:Item:7", &[]).unwrap());
    assert_eq!(other["target"], "https://acme.example/ns");
}

#[test]
fn a_resolution_answers_turtle_and_the_line_resolve_prints() {
    let kernel = kernel(REGISTRY_JSON, None);
    let turtle =
        resolution(&kernel, "urn:urnid:acme:item:42", &[("as", "text/turtle")]).expect("resolves");
    for want in [
        "@prefix name: <urn:name:vocab#> .",
        "name:name \"urn:urnid:acme:item:42\"",
        "name:claim \"urn:urnid:acme:\"",
        "name:strategy \"redirect\"",
        "name:target <https://acme.example/items/42>",
        "name:status 303",
        "name:rule \"item/{id}\"",
    ] {
        assert!(turtle.contains(want), "{want}: {turtle}");
    }
    let line = resolution(&kernel, "urn:urnid:acme:item:42", &[("as", "text/plain")]).unwrap();
    assert_eq!(
        line,
        "urn:urnid:acme:\tredirect\thttps://acme.example/items/42\t303\titem/{id}\n"
    );
    let refused = resolution(&kernel, "urn:urnid:acme:x", &[("as", "text/html")]);
    assert!(
        matches!(refused, Err(Error::Unsupported { .. })),
        "{refused:?}"
    );
}

// --- path names -------------------------------------------------------------------

/// `urn:name:resolve path=…` is the same resolution, as its line: for a path and an IRI.
#[test]
fn resolve_is_the_resolutions_line_for_a_path_and_an_iri() {
    let kernel = kernel(REGISTRY_JSON, None);
    for name in [
        "resmud/core",
        "urn:urnid:acme:item:9",
        "tag:urnid.org,2026:vocab:a",
    ] {
        let line = source(
            &kernel,
            "urn:name:resolve",
            &[("path", name)],
            &Capability::root(),
        )
        .expect("resolves");
        let same = source(
            &kernel,
            "urn:name:resolution",
            &[("iri", name), ("as", "text/plain")],
            &Capability::root(),
        )
        .expect("resolves");
        assert_eq!(line, same, "{name}");
    }
    assert_eq!(
        source(
            &kernel,
            "urn:name:resolve",
            &[("path", "resmud/core")],
            &Capability::root()
        )
        .unwrap(),
        "resmud\thosted\turn:file:resmud-vocab\n"
    );
}

/// The document face answers an IRI name as it answers a path.
#[test]
fn the_document_of_an_iri_name_is_served() {
    let kernel = kernel(REGISTRY_JSON, None);
    let doc = source(
        &kernel,
        "urn:name:doc:tag:urnid.org,2026:vocab:a",
        &[],
        &Capability::root(),
    )
    .expect("served");
    assert_eq!(doc, VOCAB);
}

// --- the same answer --------------------------------------------------------------

/// A private name and an unknown one are refused alike, on every face, to anyone not
/// holding the private namespace's owner; its owner is answered.
#[test]
fn a_private_name_and_an_unknown_name_get_the_same_answer() {
    let kernel = kernel(REGISTRY_JSON, None);
    let stranger = Capability::scoped(["urn:cap:name:admin:acme"]);
    let refusal = |iri: &str, args: &[(&str, &str)], cap: &Capability| {
        let err = source(&kernel, iri, args, cap).expect_err("refused");
        // The name is the only difference, so put the other in its place.
        let text = err
            .to_string()
            .replace("urn:urnid:secret:x", "NAME")
            .replace("urn:urnid:nobody:x", "NAME");
        (std::mem::discriminant(&err), text)
    };
    for (iri, key) in [
        ("urn:name:resolve", "path"),
        ("urn:name:resolution", "iri"),
        ("urn:name:document", "path"),
    ] {
        let private = refusal(iri, &[(key, "urn:urnid:secret:x")], &stranger);
        let unknown = refusal(iri, &[(key, "urn:urnid:nobody:x")], &stranger);
        assert_eq!(private, unknown, "{iri}");
        assert!(
            private.1.contains("no namespace claims"),
            "{iri}: {}",
            private.1
        );
    }
    let owner = Capability::scoped(["urn:cap:name:admin:secret"]);
    let answered = source(
        &kernel,
        "urn:name:resolve",
        &[("path", "urn:urnid:secret:x")],
        &owner,
    )
    .expect("its owner sees it");
    assert_eq!(answered, "urn:urnid:secret:\thosted\turn:file:secret\n");
}

/// Where unclaimed names are delegated, a private one is delegated too: the upstream is
/// asked the same question, and its answer is passed back alike.
#[test]
fn a_private_name_is_delegated_like_an_unknown_one() {
    const WITH_UPSTREAM: &str = r#"{
      "unclaimed": {"strategy": "delegate", "upstream": "https://urnid.org/"},
      "namespaces": [
        {"prefix": "urn:urnid:secret:", "owner": "urn:cap:name:admin:secret",
         "strategy": "hosted", "source": "urn:file:secret", "public": false}
      ]
    }"#;
    let asked = Asked::default();
    let kernel = kernel(WITH_UPSTREAM, Some(asked.clone()));
    let cap = Capability::scoped(["urn:cap:net:urnid.org"]);
    let private = source(
        &kernel,
        "urn:name:resolve",
        &[("path", "urn:urnid:secret:x")],
        &cap,
    )
    .expect("forwarded");
    let unknown = source(
        &kernel,
        "urn:name:resolve",
        &[("path", "urn:urnid:other:x")],
        &cap,
    )
    .expect("forwarded");
    assert_eq!(
        private.replace("secret", "NAME"),
        unknown.replace("other", "NAME")
    );
    assert_eq!(
        *asked.lock().unwrap(),
        [
            "https://urnid.org/-/resolution/urn:urnid:secret:x?as=text%2Fplain%3Bcharset%3Dutf-8",
            "https://urnid.org/-/resolution/urn:urnid:other:x?as=text%2Fplain%3Bcharset%3Dutf-8",
        ]
    );
}

// --- delegate -----------------------------------------------------------------------

#[test]
fn a_delegate_claim_forwards_to_its_upstream() {
    let asked = Asked::default();
    let kernel = kernel(REGISTRY_JSON, Some(asked.clone()));
    let cap = Capability::scoped(["urn:cap:net:urnid.org"]);
    let answer = source(
        &kernel,
        "urn:name:resolution:iri:urn:urnid:elsewhere:a/b",
        &[],
        &cap,
    )
    .expect("forwarded");
    assert_eq!(
        answer,
        "upstream answered https://urnid.org/-/resolution/urn:urnid:elsewhere:a%2Fb\
         ?as=application%2Fjson\n"
    );
    let document = source(
        &kernel,
        "urn:name:document",
        &[("path", "urn:urnid:elsewhere:a")],
        &cap,
    )
    .expect("forwarded");
    assert_eq!(
        document,
        "upstream answered https://urnid.org/urn:urnid:elsewhere:a?as=text%2Fturtle\n"
    );
    // An upstream's refusal is passed back as it is.
    let missing = source(
        &kernel,
        "urn:name:resolve",
        &[("path", "urn:urnid:elsewhere:unknown")],
        &cap,
    );
    assert!(matches!(missing, Err(Error::NotFound(_))), "{missing:?}");
}

/// Declared, and enforced to the host: `urn:cap:net:*` at the door, the upstream's own
/// host before anything leaves.
#[test]
fn delegation_declares_and_enforces_the_net_capability() {
    let asked = Asked::default();
    let kernel = kernel(REGISTRY_JSON, Some(asked.clone()));
    let described = kernel.describe_pattern("urn:name:delegate").expect("bound");
    let action = described
        .action_specs()
        .into_iter()
        .find(|a| a.verb == Verb::Source)
        .expect("a Source");
    assert_eq!(action.requires, ["urn:cap:net:*"]);

    for cap in [
        Capability::scoped(Vec::<String>::new()),
        Capability::scoped(["urn:cap:net:evil.example"]),
        Capability::scoped(["urn:cap:net:urnid.org.evil.example"]),
    ] {
        let refused = source(
            &kernel,
            "urn:name:resolve",
            &[("path", "urn:urnid:elsewhere:a")],
            &cap,
        )
        .expect_err("no grant for urnid.org");
        assert!(matches!(refused, Error::Denied(_)), "{cap:?}: {refused:?}");
    }
    assert!(asked.lock().unwrap().is_empty(), "nothing left");
}

#[test]
fn a_host_with_no_http_client_forwards_nothing_and_says_so() {
    let kernel = kernel(REGISTRY_JSON, None);
    let refused = source(
        &kernel,
        "urn:name:resolve",
        &[("path", "urn:urnid:elsewhere:a")],
        &Capability::root(),
    )
    .expect_err("no client");
    assert!(
        matches!(&refused, Error::Unsupported { detail, .. } if detail.contains("urn:http:get")),
        "{refused:?}"
    );
}

#[test]
fn an_upstream_that_is_not_a_web_address_refuses_the_registry() {
    for upstream in ["urnid.org", "ftp://urnid.org/", "https://", "urn:x:y"] {
        let claimed = format!(
            r#"{{"namespaces":[{{"prefix":"urn:urnid:a:","owner":"o","strategy":"delegate",
               "upstream":"{upstream}"}}]}}"#
        );
        assert!(
            matches!(
                Registry::from_json(claimed.as_bytes()),
                Err(LoadError::Refused {
                    error: ClaimError::BadUpstream(_),
                    ..
                })
            ),
            "{upstream}"
        );
        let unclaimed =
            format!(r#"{{"unclaimed":{{"strategy":"delegate","upstream":"{upstream}"}}}}"#);
        assert!(
            matches!(
                Registry::from_json(unclaimed.as_bytes()),
                Err(LoadError::Unclaimed(_))
            ),
            "{upstream}"
        );
    }
    let not_a_delegate = br#"{"unclaimed":{"strategy":"hosted","source":"urn:file:x"}}"#;
    let refused = Registry::from_json(not_a_delegate).expect_err("only a delegate");
    assert!(refused.to_string().contains("delegate"), "{refused}");
}

/// A strategy is data: a registry with every new form round-trips through JSON, and a
/// public namespace writes no `public`.
#[test]
fn the_new_forms_round_trip_through_json() {
    let registry = Registry::from_json(REGISTRY_JSON.as_bytes()).expect("loads");
    let written = serde_json::to_string(&registry).unwrap();
    assert_eq!(written.matches("\"public\"").count(), 1, "{written}");
    assert_eq!(Registry::from_json(written.as_bytes()).unwrap(), registry);
}
