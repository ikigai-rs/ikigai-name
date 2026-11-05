//! As-of resolution over a real git history.
//!
//! A scratch repository with three dated commits, read through the real
//! `urn:repo:log` from `ikigai-repo`. `urn:repo:show` is a stand-in that runs
//! `git show`, because no `ikigai-repo` release serves that resource yet; its
//! contract is in `ikigai_name::history`.

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Description, EndpointSpace, Error, Exact, FnEndpoint, Invocation, Iri,
    Kernel, ReprType, Representation, Request, Result, Verb,
};
use ikigai_name::history::SHOW_IRI;
use ikigai_name::{repo_history, HISTORY_IRI, REGISTRY_IRI};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

fn registry(namespaces: &[&str]) -> String {
    format!("{{\"namespaces\":[{}]}}", namespaces.join(","))
}

const RESMUD: &str = r#"{"prefix":"resmud","owner":"urn:cap:name:admin:resmud",
    "strategy":"hosted","source":"urn:file:vocab/core.ttl"}"#;
const LEGACY: &str = r#"{"prefix":"legacy","owner":"urn:cap:name:admin:legacy",
    "strategy":"hosted","source":"urn:file:vocab/legacy.ttl"}"#;
const LATER: &str = r#"{"prefix":"later","owner":"urn:cap:name:admin:later",
    "strategy":"redirect","target":"https://later.example/ns"}"#;
const RESMUD_RETIRED: &str = r#"{"prefix":"resmud","owner":"urn:cap:name:admin:resmud",
    "strategy":"retired","reason":"replaced by the realm charter"}"#;

/// A repository whose history is:
///
/// - 2026-01-10 12:00 UTC: `resmud` and `legacy`, each at their first version
/// - 2026-03-10 12:00 UTC: `later` claimed, `resmud`'s vocabulary at version 2
/// - 2026-05-10 12:00 UTC: `resmud` retired, `legacy`'s file removed
struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new() -> Repo {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "ikigai-name-as-of-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("vocab")).expect("scratch repository");
        let repo = Repo { root };
        repo.git(&["init", "--quiet"], None);

        repo.write("registry.json", &registry(&[RESMUD, LEGACY]));
        repo.write("vocab/core.ttl", "# version 1\n");
        repo.write("vocab/legacy.ttl", "# legacy\n");
        repo.commit("2026-01-10T12:00:00Z");

        repo.write("registry.json", &registry(&[RESMUD, LEGACY, LATER]));
        repo.write("vocab/core.ttl", "# version 2\n");
        repo.commit("2026-03-10T12:00:00Z");

        repo.write("registry.json", &registry(&[RESMUD_RETIRED, LEGACY, LATER]));
        std::fs::remove_file(repo.root.join("vocab/legacy.ttl")).expect("remove");
        repo.commit("2026-05-10T12:00:00Z");
        repo
    }

    fn write(&self, path: &str, content: &str) {
        std::fs::write(self.root.join(path), content).expect("write");
    }

    fn commit(&self, date: &str) {
        self.git(&["add", "-A"], None);
        self.git(&["commit", "--quiet", "-m", date], Some(date));
    }

    fn git(&self, args: &[&str], date: Option<&str>) {
        let mut command = Command::new("git");
        command
            .args(["-c", "user.name=test", "-c", "user.email=test@example.org"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(&self.root);
        if let Some(date) = date {
            command
                .env("GIT_AUTHOR_DATE", date)
                .env("GIT_COMMITTER_DATE", date);
        }
        let status = command.status().expect("run git");
        assert!(status.success(), "git {args:?}: {status}");
    }

    fn kernel(&self) -> (Kernel, Arc<AtomicUsize>) {
        let reads = Arc::new(AtomicUsize::new(0));
        let dir = self.root.to_str().expect("a UTF-8 path").to_string();
        // The real log, and only the log, from the real module.
        let log = ikigai_repo::space_with_roots([&self.root]);
        let space = EndpointSpace::new()
            .bind(
                Exact::new("urn:name:registry"),
                ikigai_name::registry_endpoint(),
            )
            .bind(Exact::new("urn:name:resolve"), ikigai_name::resolve())
            .bind(Exact::new("urn:name:document"), ikigai_name::document())
            .bind(Exact::new(HISTORY_IRI), repo_history(dir, "registry.json"))
            .bind(Exact::new(SHOW_IRI), stand_in_show(reads.clone()))
            .bind(Exact::new("urn:test:to-json-ld"), to_json_ld())
            .bind(
                Exact::new(REGISTRY_IRI),
                current(self.root.join("registry.json"), "application/json"),
            )
            .bind(
                Exact::new("urn:file:vocab/core.ttl"),
                current(self.root.join("vocab/core.ttl"), "text/turtle"),
            );
        let fallback = ikigai_core::Fallback::new(vec![Arc::new(space), Arc::new(log)]);
        (Kernel::new(Arc::new(fallback)), reads)
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// `urn:repo:show`, to its contract: a file's bytes at a commit, or `NotFound`.
fn stand_in_show(reads: Arc<AtomicUsize>) -> FnEndpoint {
    FnEndpoint::new("show", move |inv: &Invocation<'_>| {
        reads.fetch_add(1, Ordering::Relaxed);
        let (dir, rev, path) = (
            inv.inline_str("dir")?,
            inv.inline_str("rev")?,
            inv.inline_str("path")?,
        );
        let output = Command::new("git")
            .args(["show", &format!("{rev}:{path}")])
            .current_dir(Path::new(dir))
            .output()
            .map_err(|e| Error::Endpoint(e.to_string()))?;
        if !output.status.success() {
            return Err(Error::NotFound(format!("{path} is not in {rev}")));
        }
        Ok(Representation::new(
            ReprType::new("application/octet-stream"),
            output.stdout,
        ))
    })
}

/// A transreptor that passes its content on, cacheable as a real one would be.
fn to_json_ld() -> FnEndpoint {
    FnEndpoint::new("to-json-ld", |inv: &Invocation<'_>| {
        let content = inv.inline_arg("content")?.to_vec();
        Ok(Representation::new(ReprType::new("application/ld+json"), content).cacheable())
    })
    .with_description(
        Description::new("to-json-ld").transreptor(["text/turtle"], ["application/ld+json"]),
    )
}

/// The working tree: what a request with no `as-of` reads. Cacheable, so a cached
/// present is there to leak into a past answer if anything let it.
fn current(path: PathBuf, media: &'static str) -> FnEndpoint {
    FnEndpoint::new("current", move |_: &Invocation<'_>| {
        let bytes = std::fs::read(&path).map_err(|e| Error::NotFound(e.to_string()))?;
        Ok(Representation::new(ReprType::new(media), bytes).cacheable())
    })
}

fn get(kernel: &Kernel, target: &str, args: &[(&str, &str)]) -> Result<Representation> {
    let mut request = Request::new(Verb::Source, Iri::parse(target).unwrap());
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    block_on(kernel.issue(request, &Capability::root()))
}

fn text(repr: &Representation) -> String {
    String::from_utf8(repr.bytes.clone()).expect("utf-8")
}

fn document(kernel: &Kernel, path: &str, at: &str) -> Result<String> {
    let repr = get(
        kernel,
        "urn:name:document",
        &[("path", path), ("as-of", at)],
    )?;
    Ok(text(&repr))
}

#[test]
fn a_document_answers_as_it_stood_at_the_date() {
    let repo = Repo::new();
    let (kernel, _) = repo.kernel();
    for (at, want) in [
        // The day's end, in UTC, so the commit made that day counts.
        ("2026-01-10", "# version 1\n"),
        ("2026-03-09", "# version 1\n"),
        ("2026-03-10", "# version 2\n"),
        // Either side of the commit, to the second and through an offset.
        ("2026-03-10T11:59:59Z", "# version 1\n"),
        ("2026-03-10T12:00:00Z", "# version 2\n"),
        ("2026-03-10T13:00:00+02:00", "# version 1\n"),
        ("2026-05-09", "# version 2\n"),
    ] {
        assert_eq!(document(&kernel, "resmud/core", at).unwrap(), want, "{at}");
    }
}

/// What a past answer is typed as has to survive git, which records no type, or
/// negotiation would look for a conversion from `application/octet-stream`.
#[test]
fn a_past_document_is_served_as_turtle() {
    let repo = Repo::new();
    let (kernel, _) = repo.kernel();
    let repr = get(
        &kernel,
        "urn:name:document",
        &[
            ("path", "resmud/core"),
            ("as-of", "2026-02-01"),
            ("as", "text/turtle"),
        ],
    )
    .expect("served");
    assert_eq!(repr.repr_type.media_type, "text/turtle");
}

/// A past read is typed from the file's name, so every type a file module gives a
/// vocabulary has to come back, or `as=` works today and is refused for March.
#[test]
fn a_past_document_keeps_the_type_its_file_name_gives_it() {
    let repo = Repo::new();
    let types = [
        ("ttl", "text/turtle"),
        ("nt", "application/n-triples"),
        ("nq", "application/n-quads"),
        ("n3", "text/n3"),
        ("trig", "application/trig"),
        ("rdf", "application/rdf+xml"),
        ("xml", "text/xml"),
        ("json", "application/json"),
        ("jsonld", "application/ld+json"),
        ("txt", "text/plain"),
        ("md", "text/markdown"),
    ];
    let namespaces: Vec<String> = types
        .iter()
        .map(|(extension, _)| {
            repo.write(&format!("vocab/typed.{extension}"), "typed\n");
            format!(
                r#"{{"prefix":"{extension}","owner":"urn:cap:name:admin:{extension}",
                "strategy":"hosted","source":"urn:file:vocab/typed.{extension}"}}"#
            )
        })
        .collect();
    let namespaces: Vec<&str> = namespaces.iter().map(String::as_str).collect();
    repo.write("registry.json", &registry(&namespaces));
    repo.commit("2026-06-01T12:00:00Z");

    let (kernel, _) = repo.kernel();
    for (extension, media) in types {
        let repr = get(
            &kernel,
            "urn:name:document",
            &[("path", extension), ("as-of", "2026-07-01"), ("as", media)],
        )
        .unwrap_or_else(|e| panic!("{extension}: {e:?}"));
        assert_eq!(repr.repr_type.media_type, media, "{extension}");
    }

    // A name nothing recognizes stays what the read said, and is not served as Turtle.
    repo.write("vocab/typed.bin", "typed\n");
    let namespace = r#"{"prefix":"bin","owner":"urn:cap:name:admin:bin",
        "strategy":"hosted","source":"urn:file:vocab/typed.bin"}"#;
    repo.write("registry.json", &registry(&[namespace]));
    repo.commit("2026-06-02T12:00:00Z");
    let err = document(&kernel, "bin", "2026-07-01").unwrap_err();
    assert!(
        matches!(&err, Error::Endpoint(m) if m.contains("nothing converts application/octet-stream")),
        "{err:?}"
    );
}

/// A transreptor marks its own answer cacheable, which must not make a past answer
/// so: the date's answer still comes from history every time.
#[test]
fn a_converted_past_answer_comes_from_history_too() {
    let repo = Repo::new();
    let (kernel, reads) = repo.kernel();
    let args = [
        ("path", "resmud/core"),
        ("as-of", "2026-02-01"),
        ("as", "application/ld+json"),
    ];
    let before = reads.load(Ordering::Relaxed);
    for _ in 0..2 {
        let repr = get(&kernel, "urn:name:document", &args).expect("served");
        assert_eq!(repr.repr_type.media_type, "application/ld+json");
        assert_eq!(text(&repr), "# version 1\n");
    }
    // The registry and the document, each read twice.
    assert_eq!(reads.load(Ordering::Relaxed) - before, 4);
}

/// The tombstone is as dated as anything else: retired now, in force in March.
#[test]
fn a_namespace_retired_later_still_answers_for_the_date_it_stood() {
    let repo = Repo::new();
    let (kernel, _) = repo.kernel();

    let err = get(&kernel, "urn:name:document", &[("path", "resmud/core")]).unwrap_err();
    assert!(
        matches!(&err, Error::NotFound(m) if m.contains("retired")),
        "{err:?}"
    );

    assert_eq!(
        document(&kernel, "resmud/core", "2026-04-01").unwrap(),
        "# version 2\n"
    );

    let err = document(&kernel, "resmud/core", "2026-06-01").unwrap_err();
    assert!(
        matches!(&err, Error::NotFound(m)
            if m.contains("retired as of 2026-06-01T23:59:59Z") && m.contains("realm charter")),
        "{err:?}"
    );
}

/// Namespace policy in March is the same mechanism as a vocabulary in March.
#[test]
fn the_registry_answers_as_it_stood_at_the_date() {
    let repo = Repo::new();
    let (kernel, _) = repo.kernel();
    let prefixes = |args: &[(&str, &str)]| {
        let repr = get(&kernel, "urn:name:registry", args).expect("served");
        let parsed = ikigai_name::Registry::from_json(&repr.bytes).expect("a registry");
        parsed
            .namespaces
            .into_iter()
            .map(|n| n.prefix)
            .collect::<Vec<_>>()
    };
    assert_eq!(prefixes(&[("as-of", "2026-02-01")]), ["resmud", "legacy"]);
    assert_eq!(
        prefixes(&[("as-of", "2026-04-01")]),
        ["resmud", "legacy", "later"]
    );

    let resolve = |at: &str| {
        get(
            &kernel,
            "urn:name:resolve",
            &[("path", "later/x"), ("as-of", at)],
        )
        .map(|r| text(&r))
    };
    assert_eq!(
        resolve("2026-04-01").unwrap(),
        "later\tredirect\thttps://later.example/ns\t302\n"
    );
    let err = resolve("2026-02-01").unwrap_err();
    assert!(
        matches!(&err, Error::NotFound(m)
            if m.contains("\"later/x\"") && m.contains("as of 2026-02-01T23:59:59Z")),
        "{err:?}"
    );
}

/// The status and the rules are the registry's, so a past answer uses the ones it
/// held then: one commit with a 302 and no rules, one with a rule and 301, and today's
/// with the rule moved and its status changed.
#[test]
fn a_past_answer_uses_the_status_and_rules_held_then() {
    let repo = Repo::new();
    let moved = |fields: &str| {
        format!(
            r#"{{"prefix":"moved","owner":"urn:cap:name:admin:moved",
            "strategy":"redirect","target":"https://first.example/"{fields}}}"#
        )
    };
    let write = |entry: String, date: &str| {
        repo.write("registry.json", &registry(&[entry.as_str()]));
        repo.commit(date);
    };
    write(moved(""), "2026-06-10T12:00:00Z");
    write(
        moved(
            r#","status":301,"rules":[
            {"match":"old/{x}","target":"https://second.example/{x}"}]"#,
        ),
        "2026-07-10T12:00:00Z",
    );
    write(
        moved(
            r#","status":308,"rules":[
            {"match":"old/{x}","target":"https://third.example/{x}","status":307}]"#,
        ),
        "2026-08-10T12:00:00Z",
    );
    let (kernel, _) = repo.kernel();
    let resolve = |args: &[(&str, &str)]| text(&get(&kernel, "urn:name:resolve", args).unwrap());

    for (at, want) in [
        (
            "2026-06-20",
            "moved\tredirect\thttps://first.example/\t302\n",
        ),
        (
            "2026-07-20",
            "moved\tredirect\thttps://second.example/a\t301\told/{x}\n",
        ),
        (
            "2026-08-20",
            "moved\tredirect\thttps://third.example/a\t307\told/{x}\n",
        ),
    ] {
        assert_eq!(
            resolve(&[("path", "moved/old/a"), ("as-of", at)]),
            want,
            "{at}"
        );
    }
    assert_eq!(
        resolve(&[("path", "moved/other"), ("as-of", "2026-07-20")]),
        "moved\tredirect\thttps://first.example/\t301\n"
    );
    // No date is today's answer.
    assert_eq!(
        resolve(&[("path", "moved/old/a")]),
        "moved\tredirect\thttps://third.example/a\t307\told/{x}\n"
    );
}

#[test]
fn nothing_answers_for_a_date_before_it_existed() {
    let repo = Repo::new();
    let (kernel, _) = repo.kernel();
    let err = document(&kernel, "resmud/core", "2025-12-31").unwrap_err();
    assert!(
        matches!(&err, Error::NotFound(m)
            if m.contains(REGISTRY_IRI) && m.contains("did not exist as of 2025-12-31T23:59:59Z")),
        "{err:?}"
    );
}

/// The newest commit before the date is the one that removed the file.
#[test]
fn a_file_removed_before_the_date_did_not_exist_then() {
    let repo = Repo::new();
    let (kernel, _) = repo.kernel();
    assert_eq!(
        document(&kernel, "legacy/x", "2026-04-01").unwrap(),
        "# legacy\n"
    );
    let err = document(&kernel, "legacy/x", "2026-06-01").unwrap_err();
    assert!(
        matches!(&err, Error::NotFound(m)
            if m.contains("urn:file:vocab/legacy.ttl") && m.contains("did not exist")),
        "{err:?}"
    );
}

/// Cache is not archive. A past answer reads history every time, and a cached
/// present never stands in for it, in either order.
#[test]
fn a_past_answer_comes_from_history_and_never_the_cache() {
    let repo = Repo::new();
    let (kernel, reads) = repo.kernel();
    repo.write("registry.json", &registry(&[RESMUD]));
    let path = [("path", "resmud/core")];

    let now = text(&get(&kernel, "urn:name:document", &path).unwrap());
    assert_eq!(now, "# version 2\n");

    let before = reads.load(Ordering::Relaxed);
    for _ in 0..2 {
        assert_eq!(
            document(&kernel, "resmud/core", "2026-02-01").unwrap(),
            "# version 1\n"
        );
    }
    // The registry and the document, each read twice.
    assert_eq!(reads.load(Ordering::Relaxed) - before, 4);

    assert_eq!(
        text(&get(&kernel, "urn:name:document", &path).unwrap()),
        now
    );
}

/// `urn:repo:show` is the part `ikigai-repo` does not serve yet, so a host that
/// binds `repo_history` without it gets told, rather than an unexplained miss.
#[test]
fn a_missing_repo_resource_is_named() {
    let repo = Repo::new();
    let dir = repo.root.to_str().unwrap().to_string();
    let space = ikigai_repo::space_with_roots([&repo.root])
        .bind(
            Exact::new("urn:name:registry"),
            ikigai_name::registry_endpoint(),
        )
        .bind(Exact::new(HISTORY_IRI), repo_history(dir, "registry.json"));
    let kernel = Kernel::new(Arc::new(space));
    let err = get(&kernel, "urn:name:registry", &[("as-of", "2026-02-01")]).unwrap_err();
    assert!(
        matches!(&err, Error::Unsupported { detail, .. } if detail.contains(SHOW_IRI)),
        "{err:?}"
    );
}

/// The git facades are capability-gated, and a past read is no way around that.
#[test]
fn reading_history_needs_the_git_capability() {
    let repo = Repo::new();
    let (kernel, _) = repo.kernel();
    let request = Request::new(Verb::Source, Iri::parse("urn:name:registry").unwrap())
        .with_arg("as-of", ArgRef::Inline(b"2026-02-01".to_vec()));
    let err =
        block_on(kernel.issue(request, &Capability::scoped(Vec::<String>::new()))).unwrap_err();
    assert!(matches!(err, Error::Denied(_)), "{err:?}");
}
