//! Content namespaces: names that resolve to IPFS CIDs, in each face, across tenants,
//! visibility and history, and the registry entries a content claim refuses.

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Error, Exact, FnEndpoint, Invocation, Iri, Kernel, ReprType,
    Representation, Request, Result, Verb,
};
use ikigai_name::registry::{ClaimError, LoadError, Registry, Strategy};
use ikigai_name::tenant::{tenant_of, tenant_scope};
use ikigai_name::REGISTRY_IRI;
use std::sync::Arc;

/// A UnixFS root written as CIDv0, and the CIDv1 every answer writes for it.
const V0: &str = "QmUNLLsPACCz1vLxQVkXqqLX5R1X345qqfHbsf67hvA3Nn";
const V0_AS_V1: &str = "bafybeiczsscdsbs7ffqz55asqdf3smv6klcw3gofszvwlyarci47bgf354";
/// A raw block.
const RAW: &str = "bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy";

const REGISTRY_JSON: &str = r#"{
  "gateway": "https://gateway.example/",
  "namespaces": [
    {"prefix": "files", "owner": "urn:cap:name:admin:files", "strategy": "content",
     "entries": {
       "files/readme": {"cid": "bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy",
                        "media-type": "text/plain; charset=utf-8"},
       "/files//old/": {"cid": "QmUNLLsPACCz1vLxQVkXqqLX5R1X345qqfHbsf67hvA3Nn"}
     }},
    {"prefix": "urn:urnid:blobs:", "owner": "urn:cap:name:admin:blobs", "strategy": "content",
     "entries": {
       "urn:urnid:blobs:one": {"cid": "bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy"}
     }},
    {"prefix": "secret", "owner": "urn:cap:name:admin:secret", "strategy": "content",
     "public": false,
     "entries": {
       "secret/plan": {"cid": "bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy"}
     }}
  ]
}"#;

/// The registry as it stood before: `files/readme` named the UnixFS root then.
const PAST_JSON: &str = r#"{"namespaces":[
  {"prefix": "files", "owner": "urn:cap:name:admin:files", "strategy": "content",
   "entries": {"files/readme": {"cid": "QmUNLLsPACCz1vLxQVkXqqLX5R1X345qqfHbsf67hvA3Nn"}}}
]}"#;

fn kernel(json: &'static str, past: Option<&'static str>) -> Kernel {
    let source = FnEndpoint::new("registry-source", move |_: &Invocation<'_>| {
        Ok(Representation::new(
            ReprType::new("application/json"),
            json.as_bytes().to_vec(),
        ))
    });
    let mut space = ikigai_name::space().bind(Exact::new(REGISTRY_IRI), source);
    if let Some(past) = past {
        space = space.bind(
            Exact::new(ikigai_name::HISTORY_IRI),
            FnEndpoint::new("history", move |_: &Invocation<'_>| {
                Ok(Representation::new(
                    ReprType::new("application/json"),
                    past.as_bytes().to_vec(),
                ))
            }),
        );
    }
    Kernel::new(Arc::new(space))
}

fn issue(
    kernel: &Kernel,
    iri: &str,
    args: &[(&str, &str)],
    cap: &Capability,
) -> Result<Representation> {
    let mut request = Request::new(Verb::Source, Iri::parse(iri).expect("an IRI"));
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    block_on(kernel.issue(request, cap))
}

fn text(repr: &Representation) -> String {
    String::from_utf8(repr.bytes.clone()).expect("utf-8")
}

fn resolution(
    kernel: &Kernel,
    name: &str,
    args: &[(&str, &str)],
    cap: &Capability,
) -> Result<String> {
    let mut all = vec![("iri", name)];
    all.extend_from_slice(args);
    issue(kernel, "urn:name:resolution", &all, cap).map(|r| text(&r))
}

fn json(text: &str) -> serde_json::Value {
    serde_json::from_str(text).expect("JSON")
}

fn root() -> Capability {
    Capability::root()
}

// --- the registry ----------------------------------------------------------------

/// The reported defect: a content entry was an unknown variant, so `--check` refused it.
#[test]
fn a_content_namespace_loads_and_round_trips() {
    let registry = Registry::from_json(REGISTRY_JSON.as_bytes()).expect("loads");
    let files = registry.lookup("files/readme").expect("claimed");
    assert_eq!(files.strategy.name(), "content");
    let written = serde_json::to_vec(&registry).unwrap();
    assert_eq!(Registry::from_json(&written).expect("reloads"), registry);
    assert_eq!(
        registry.gateway.as_deref(),
        Some("https://gateway.example/")
    );
}

fn refused(json: &str) -> String {
    match Registry::from_json(json.as_bytes()) {
        Err(error @ LoadError::Refused { .. }) => error.to_string(),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn one(entries: &str) -> String {
    format!(
        r#"{{"namespaces":[{{"prefix":"files","owner":"o","strategy":"content","entries":{entries}}}]}}"#
    )
}

#[test]
fn a_name_outside_the_prefix_is_refused() {
    for outside in ["other/readme", "filesx/readme", "urn:urnid:files:readme"] {
        let why = refused(&one(&format!(r#"{{"{outside}":{{"cid":"{RAW}"}}}}"#)));
        assert!(why.contains("is not a name under \"files\""), "{why}");
        assert!(
            why.starts_with("the namespace \"files\" cannot be claimed"),
            "{why}"
        );
    }
}

/// Written twice, literally or only once canonical: either way one name, refused.
#[test]
fn a_name_written_twice_is_refused() {
    for (a, b) in [("files/a", "files/a"), ("files/a", "/files//a/")] {
        let why = refused(&one(&format!(
            r#"{{"{a}":{{"cid":"{RAW}"}},"{b}":{{"cid":"{V0}"}}}}"#
        )));
        assert!(why.contains("are one name, \"files/a\""), "{why}");
    }
    let iris = r#"{"namespaces":[{"prefix":"urn:urnid:b:","owner":"o","strategy":"content",
        "entries":{"urn:urnid:b:x":{"cid":"QmUNLLsPACCz1vLxQVkXqqLX5R1X345qqfHbsf67hvA3Nn"},
                   "tag:urnid.org,2026:B:x":{"cid":"QmUNLLsPACCz1vLxQVkXqqLX5R1X345qqfHbsf67hvA3Nn"}}}]}"#;
    assert!(refused(iris).contains("are one name"));
}

#[test]
fn a_bad_cid_or_media_type_is_refused_by_name() {
    let why = refused(&one(r#"{"files/a":{"cid":"not-a-cid"}}"#));
    assert!(
        why.contains("files/a") && why.contains("not a CID"),
        "{why}"
    );
    let why = refused(&one(&format!(
        r#"{{"files/a":{{"cid":"{RAW}","media-type":"plain"}}}}"#
    )));
    assert!(why.contains("not a type/subtype"), "{why}");
    let why = refused(&one(r#"{"files/a":{}}"#));
    assert!(
        why.contains("entries are not valid") && why.contains("cid"),
        "{why}"
    );
}

#[test]
fn a_gateway_that_is_not_https_is_refused() {
    let error = Registry::from_json(br#"{"gateway":"http://gateway.example/","namespaces":[]}"#)
        .expect_err("refused");
    assert!(matches!(error, LoadError::Gateway(_)), "{error}");
    assert!(
        error.to_string().contains("not an https: address"),
        "{error}"
    );
}

/// A claim is held to the same rule a file is.
#[test]
fn a_claim_checks_its_entries() {
    let mut registry = Registry::default();
    let entries = serde_json::from_str(&format!(r#"{{"elsewhere/a":{{"cid":"{RAW}"}}}}"#)).unwrap();
    let refusal = registry
        .claim(ikigai_name::registry::Namespace {
            prefix: "files".into(),
            owner: "urn:cap:name:admin:files".into(),
            strategy: Strategy::Content { entries },
            repository: None,
            public: true,
        })
        .expect_err("refused");
    assert!(matches!(refusal, ClaimError::BadContent(_)), "{refusal}");
}

// --- the three faces ---------------------------------------------------------------

#[test]
fn json_names_the_ipfs_target_cid_gateway_and_media_type() {
    let k = kernel(REGISTRY_JSON, None);
    let record = json(&resolution(&k, "files/readme", &[], &root()).expect("resolves"));
    assert_eq!(record["claim"], "files");
    assert_eq!(record["strategy"], "content");
    assert_eq!(record["target"], format!("ipfs://{RAW}"));
    assert_eq!(record["cid"], RAW);
    assert_eq!(
        record["gateway"],
        format!("https://gateway.example/ipfs/{RAW}")
    );
    assert_eq!(record["media-type"], "text/plain; charset=utf-8");
    assert!(record.get("status").is_none(), "{record}");
}

/// The CIDv0 written in the registry is answered as its CIDv1, and a name is compared
/// canonically, both in the registry's key and in the request.
#[test]
fn cidv0_input_is_answered_as_base32_cidv1() {
    let k = kernel(REGISTRY_JSON, None);
    let record = json(&resolution(&k, "files/old", &[], &root()).expect("resolves"));
    assert_eq!(record["cid"], V0_AS_V1);
    assert_eq!(record["target"], format!("ipfs://{V0_AS_V1}"));
    assert!(record.get("media-type").is_none(), "{record}");
    let line = resolution(&k, "/files/old/", &[("as", "text/plain")], &root()).unwrap();
    assert_eq!(line, format!("files\tcontent\tipfs://{V0_AS_V1}\n"));
}

#[test]
fn turtle_writes_the_target_and_gateway_as_iris() {
    let k = kernel(REGISTRY_JSON, None);
    let turtle = resolution(&k, "files/readme", &[("as", "text/turtle")], &root()).unwrap();
    assert!(
        turtle.contains(&format!("name:target <ipfs://{RAW}>")),
        "{turtle}"
    );
    assert!(
        turtle.contains(&format!(
            "name:gateway <https://gateway.example/ipfs/{RAW}>"
        )),
        "{turtle}"
    );
    assert!(turtle.contains(&format!("name:cid \"{RAW}\"")), "{turtle}");
    assert!(
        turtle.contains("name:mediaType \"text/plain; charset=utf-8\""),
        "{turtle}"
    );
    assert!(turtle.contains("name:strategy \"content\""), "{turtle}");
    // It parses as Turtle.
    oxrdfio::RdfParser::from_format(oxrdfio::RdfFormat::Turtle)
        .for_slice(turtle.as_bytes())
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("valid Turtle");
}

/// Plain text, and `urn:name:resolve`, keep the three tab-separated fields.
#[test]
fn plain_text_is_three_fields() {
    let k = kernel(REGISTRY_JSON, None);
    let line = resolution(&k, "urn:urnid:blobs:one", &[("as", "text/plain")], &root()).unwrap();
    assert_eq!(line, format!("urn:urnid:blobs:\tcontent\tipfs://{RAW}\n"));
    let resolved = issue(&k, "urn:name:resolve", &[("path", "files/readme")], &root()).unwrap();
    assert_eq!(text(&resolved), format!("files\tcontent\tipfs://{RAW}\n"));
}

// --- misses, visibility and tenants ---------------------------------------------------

#[test]
fn a_name_without_an_entry_is_a_miss_within_the_claim() {
    let k = kernel(REGISTRY_JSON, None);
    for name in ["files/missing", "files", "files/readme/more"] {
        match resolution(&k, name, &[], &root()) {
            Err(Error::NotFound(why)) => assert!(
                why.contains("is not a name of the content namespace \"files\""),
                "{why}"
            ),
            other => panic!("{name}: expected a miss, got {other:?}"),
        }
    }
}

/// To a caller not holding its owner, a private content claim answers exactly as a
/// name nobody claims; its owner gets the record, with no gateway.
#[test]
fn a_private_claim_keeps_the_indistinguishable_miss_and_no_gateway() {
    let k = kernel(REGISTRY_JSON, None);
    let stranger = Capability::scoped(["urn:cap:name:admin:files"]);
    let hidden = resolution(&k, "secret/plan", &[], &stranger).expect_err("hidden");
    let unknown = resolution(&k, "nobody/plan", &[], &stranger).expect_err("unknown");
    let shape = |e: &Error| match e {
        Error::NotFound(why) => why.replace("secret/plan", "X").replace("nobody/plan", "X"),
        other => panic!("{other:?}"),
    };
    assert_eq!(shape(&hidden), shape(&unknown));
    let missing = resolution(&k, "secret/absent", &[], &stranger).expect_err("hidden");
    assert_eq!(
        shape(&missing),
        shape(&unknown).replace("X", "secret/absent")
    );

    let owner = Capability::scoped(["urn:cap:name:admin:secret"]);
    let record = json(&resolution(&k, "secret/plan", &[], &owner).expect("its owner sees it"));
    assert_eq!(record["cid"], RAW);
    assert!(record.get("gateway").is_none(), "{record}");
    // The document route points a private claim's owner at the record, not a gateway.
    match issue(&k, "urn:name:document", &[("path", "secret/plan")], &owner) {
        Err(Error::Unsupported { detail, .. }) => {
            assert!(
                detail.contains("urn:name:resolution:iri:secret/plan"),
                "{detail}"
            )
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// Each tenant answers from its own registry: its own entries and its own gateway, and
/// none of another tenant's.
#[test]
fn each_tenant_answers_its_own_entries_and_gateway() {
    let source = FnEndpoint::new("registry-source", |inv: &Invocation<'_>| {
        let body = match tenant_of(inv.capability) {
            Some("a.example") => format!(
                r#"{{"gateway":"https://ipfs.a.example","namespaces":[{{"prefix":"files",
                   "owner":"o","strategy":"content","entries":{{"files/x":{{"cid":"{RAW}"}}}}}}]}}"#
            ),
            Some("b.example") => format!(
                r#"{{"namespaces":[{{"prefix":"files","owner":"o","strategy":"content",
                   "entries":{{"files/y":{{"cid":"{V0}"}}}}}}]}}"#
            ),
            _ => return Err(Error::Denied("read only for a tenant".into())),
        };
        Ok(Representation::new(ReprType::new("application/json"), body.into_bytes()).cacheable())
    });
    let k = Kernel::new(Arc::new(
        ikigai_name::space().bind(Exact::new(REGISTRY_IRI), source),
    ));
    let cap = |host| Capability::scoped([tenant_scope(host).unwrap()]);
    let a = json(&resolution(&k, "files/x", &[], &cap("a.example")).unwrap());
    assert_eq!(a["tenant"], "a.example");
    assert_eq!(a["gateway"], format!("https://ipfs.a.example/ipfs/{RAW}"));
    let b = json(&resolution(&k, "files/y", &[], &cap("b.example")).unwrap());
    assert_eq!(b["tenant"], "b.example");
    assert_eq!(b["cid"], V0_AS_V1);
    assert!(b.get("gateway").is_none(), "{b}");
    assert!(matches!(
        resolution(&k, "files/x", &[], &cap("b.example")),
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        resolution(&k, "files/y", &[], &cap("a.example")),
        Err(Error::NotFound(_))
    ));
    // Without a gateway, b's document is refused and pointed at the record.
    match issue(
        &k,
        "urn:name:document",
        &[("path", "files/y")],
        &cap("b.example"),
    ) {
        Err(Error::Unsupported { detail, .. }) => {
            assert!(detail.contains(&format!("ipfs://{V0_AS_V1}")), "{detail}");
            assert!(
                detail.contains("urn:name:resolution:iri:files/y"),
                "{detail}"
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

// --- the document route ------------------------------------------------------------------

#[test]
fn a_public_claim_with_a_gateway_redirects_temporarily() {
    let k = kernel(REGISTRY_JSON, None);
    let answer = issue(&k, "urn:name:document", &[("path", "files/old")], &root()).unwrap();
    let redirect = answer.as_redirect().expect("a redirect");
    assert_eq!(redirect.status.code(), 307);
    assert_eq!(
        redirect.location.as_str(),
        format!("https://gateway.example/ipfs/{V0_AS_V1}")
    );
    assert!(matches!(
        issue(&k, "urn:name:document", &[("path", "files/none")], &root()),
        Err(Error::NotFound(_))
    ));
}

// --- history -------------------------------------------------------------------------------

/// A past answer comes from the registry as it stood then, never the current CID.
#[test]
fn a_past_answer_comes_from_the_past_registry() {
    let k = kernel(REGISTRY_JSON, Some(PAST_JSON));
    let record = json(
        &resolution(&k, "files/readme", &[("as-of", "2026-01-01")], &root()).expect("resolves"),
    );
    assert_eq!(record["cid"], V0_AS_V1, "{record}");
    assert_eq!(record["as-of"], "2026-01-01T23:59:59Z");
    assert!(record.get("media-type").is_none(), "{record}");
    // A name the past registry did not hold was a miss then, whatever is current.
    match resolution(&k, "files/old", &[("as-of", "2026-01-01")], &root()) {
        Err(Error::NotFound(why)) => assert!(why.contains("as of 2026-01-01T23:59:59Z"), "{why}"),
        other => panic!("expected a past miss, got {other:?}"),
    }
}

#[test]
fn without_history_a_past_answer_is_refused() {
    let k = kernel(REGISTRY_JSON, None);
    match resolution(&k, "files/readme", &[("as-of", "2026-01-01")], &root()) {
        Err(Error::Unsupported { detail, .. }) => {
            assert!(detail.contains("keeps no history"), "{detail}")
        }
        other => panic!("expected the history refusal, got {other:?}"),
    }
    match issue(
        &k,
        "urn:name:document",
        &[("path", "files/readme"), ("as-of", "2026-01-01")],
        &root(),
    ) {
        Err(Error::Unsupported { detail, .. }) => {
            assert!(detail.contains("keeps no history"), "{detail}")
        }
        other => panic!("expected the history refusal, got {other:?}"),
    }
}
