//! A namespace served from a git repository, over a real git history, the real
//! `ikigai-shacl` validator and the real `ikigai-secret` file store.
//!
//! `urn:name:git-read` is a stand-in that runs `git show` and `git log` on a scratch
//! repository, because a reader's HTTP is another module's: its contract is in
//! `ikigai_name::git`. `forge_reader` has its own tests below, over `ikigai-http`
//! and a canned transport. The store is a compare-and-replace map, to its contract.

use async_trait::async_trait;
use futures::executor::block_on;
use ikigai_core::{
    ActionSpec, ArgRef, ArgSpec, Capability, Description, EndpointSpace, Error, Exact, Fallback,
    FnEndpoint, Invocation, Iri, Kernel, ReprType, Representation, Request, Result, UriTemplate,
    Verb,
};
use ikigai_http::{HttpBody, HttpRequest, HttpResponse, HttpTransport};
use ikigai_name::git::{
    forge_reader_for, hook_secret, token_secret, verify_push, Forge, ForgeKind, CAP_REFRESH,
    READ_IRI, STORE_TEMPLATE,
};
use ikigai_name::{Registry, HISTORY_IRI, REGISTRY_IRI};
use ikigai_secret::{Backend, FileBackend};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const SHAPES: &str = r#"
@prefix sh:   <http://www.w3.org/ns/shacl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix ex:   <https://acme.example/ns#> .

ex:ClassShape a sh:NodeShape ;
    sh:targetClass rdfs:Class ;
    sh:property [ sh:path rdfs:comment ; sh:minCount 1 ;
                  sh:message "a class needs a comment" ] .
"#;

fn vocabulary(comment: &str) -> String {
    format!(
        "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
         @prefix ex: <https://acme.example/ns#> .\n\
         ex:Thing a rdfs:Class ; rdfs:comment \"{comment}\" .\n"
    )
}

/// Conforms to nothing in [`SHAPES`]: a class with no comment.
const NONCONFORMING: &str = "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
                             @prefix ex: <https://acme.example/ns#> .\n\
                             ex:Thing a rdfs:Class .\n";

fn entry(strategy: &str, private: bool) -> String {
    let field = if strategy == "hosted" {
        "source"
    } else {
        "origin"
    };
    format!(
        r#"{{"namespaces":[{{"prefix":"acme","owner":"urn:cap:name:admin:acme",
          "strategy":"{strategy}","{field}":"urn:name:git:acme",
          "repository":{{"url":"https://forge.example/acme/vocab","branch":"main",
            "path":"vocab/core.ttl","shape":"urn:file:shapes/acme.ttl","private":{private}}}}}]}}"#
    )
}

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Scratch {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "ikigai-name-git-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("repo/vocab")).expect("scratch");
        let scratch = Scratch { root };
        scratch.git(&["init", "--quiet", "--initial-branch=main"], None);
        scratch
    }

    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    fn git(&self, args: &[&str], date: Option<&str>) {
        let mut command = Command::new("git");
        command
            .args(["-c", "user.name=test", "-c", "user.email=test@example.org"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(self.repo());
        if let Some(date) = date {
            command
                .env("GIT_AUTHOR_DATE", date)
                .env("GIT_COMMITTER_DATE", date);
        }
        let status = command.status().expect("run git");
        assert!(status.success(), "git {args:?}: {status}");
    }

    /// Commit a new version of the served file: a push, as far as a reader can tell.
    fn push(&self, content: &str) {
        self.push_at(content, "2026-11-05T12:00:00Z");
    }

    fn push_at(&self, content: &str, date: &str) {
        std::fs::write(self.repo().join("vocab/core.ttl"), content).expect("write");
        self.git(&["add", "-A"], None);
        self.git(
            &["commit", "--quiet", "--allow-empty", "-m", date],
            Some(date),
        );
    }

    fn vanish(&self) {
        std::fs::remove_dir_all(self.repo()).expect("remove");
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// What the stand-in reader has been asked, and how it has been told to misbehave.
#[derive(Default)]
struct ReaderLog {
    reads: AtomicUsize,
    tokens: Mutex<Vec<String>>,
    answer_as: Mutex<Option<&'static str>>,
    fail_with: Mutex<Option<String>>,
}

/// `urn:name:git-read`, to its contract, over the scratch repository's files.
fn stand_in_reader(repo: PathBuf, log: Arc<ReaderLog>) -> FnEndpoint {
    FnEndpoint::new("git-read", move |inv: &Invocation<'_>| {
        log.reads.fetch_add(1, Ordering::Relaxed);
        if let Ok(token) = inv.inline_str("token") {
            log.tokens.lock().unwrap().push(token.to_string());
        }
        if let Some(why) = log.fail_with.lock().unwrap().clone() {
            return Err(Error::Unavailable(why));
        }
        let (branch, path) = (inv.inline_str("branch")?, inv.inline_str("path")?);
        if !repo.exists() {
            return Err(Error::NotFound("no such repository".into()));
        }
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .map_err(|e| Error::Endpoint(e.to_string()))
        };
        let rev = match inv.inline_str("as-of") {
            Ok(at) => {
                let until = format!("--until={at}");
                let listed = git(&["log", "-1", "--format=%H", &until, branch, "--", path])?;
                let rev = String::from_utf8_lossy(&listed.stdout).trim().to_string();
                if rev.is_empty() {
                    return Err(Error::NotFound(format!("{path} did not exist then")));
                }
                rev
            }
            Err(_) => branch.to_string(),
        };
        let shown = git(&["show", &format!("{rev}:{path}")])?;
        if !shown.status.success() {
            return Err(Error::NotFound(format!("{path} is not on {rev}")));
        }
        let media = log
            .answer_as
            .lock()
            .unwrap()
            .unwrap_or("application/octet-stream");
        Ok(Representation::new(ReprType::new(media), shown.stdout))
    })
}

/// The store, to its contract: compare and replace as one, with a write needing a scope
/// a request does not hold. Cacheable, so a stale answer would show. Every read counts.
fn store(writes: Arc<AtomicUsize>, reads: Arc<AtomicUsize>) -> FnEndpoint {
    let held: Mutex<BTreeMap<String, Vec<u8>>> = Mutex::default();
    let repr = |bytes: &[u8]| Representation::new(ReprType::new("text/turtle"), bytes.to_vec());
    FnEndpoint::new("git-store", move |inv: &Invocation<'_>| {
        let key = inv.bindings.get("key").expect("a key").to_string();
        let mut held = held.lock().unwrap();
        if inv.request.verb == Verb::Sink {
            let current = held
                .get(&key)
                .map(|bytes| repr(bytes).content_id().to_string());
            if current != inv.inline_str("if-match").ok().map(str::to_string) {
                return Err(Error::Conflict(format!("{key} changed")));
            }
            let content = inv.inline_arg("content")?.to_vec();
            writes.fetch_add(1, Ordering::Relaxed);
            held.insert(key, content.clone());
            return Ok(repr(&content));
        }
        reads.fetch_add(1, Ordering::Relaxed);
        match held.get(&key) {
            Some(bytes) => Ok(repr(bytes).cacheable()),
            None => Err(Error::NotFound(format!("nothing at {key}"))),
        }
    })
    .with_description(
        Description::new("git-store")
            .action(
                ActionSpec::new(Verb::Source).input(ArgSpec::new("key").summary("key").binding()),
            )
            .action(
                ActionSpec::new(Verb::Sink)
                    .requires(STORE_WRITE)
                    .input(ArgSpec::new("key").summary("key").binding())
                    .input(ArgSpec::new("content").summary("the bytes"))
                    .input(ArgSpec::new("if-match").summary("the token").optional()),
            ),
    )
}

const STORE_WRITE: &str = "urn:cap:test:store-write";

struct World {
    scratch: Scratch,
    kernel: Kernel,
    log: Arc<ReaderLog>,
    secrets: Arc<FileBackend>,
    store_writes: Arc<AtomicUsize>,
    store_reads: Arc<AtomicUsize>,
    /// The scopes each read of the shapes held.
    shape_caps: Arc<Mutex<Vec<Option<Vec<String>>>>>,
    registry: Arc<Mutex<String>>,
}

impl World {
    fn new(strategy: &str, private: bool) -> World {
        Self::with(strategy, private, true)
    }

    fn with(strategy: &str, private: bool, bind_validator: bool) -> World {
        let scratch = Scratch::new();
        let log = Arc::new(ReaderLog::default());
        let secrets_dir = scratch.root.join("secrets");
        let secrets = Arc::new(FileBackend::new(&secrets_dir));
        let store_writes = Arc::new(AtomicUsize::new(0));
        let store_reads = Arc::new(AtomicUsize::new(0));
        let shape_caps = Arc::new(Mutex::new(Vec::new()));
        let registry = Arc::new(Mutex::new(entry(strategy, private)));

        let held = registry.clone();
        let registry_source = FnEndpoint::new("registry-source", move |_: &Invocation<'_>| {
            Ok(Representation::new(
                ReprType::new("application/json"),
                held.lock().unwrap().clone().into_bytes(),
            )
            .cacheable())
        });
        let seen_caps = shape_caps.clone();
        let shapes = FnEndpoint::new("shapes", move |inv: &Invocation<'_>| {
            seen_caps.lock().unwrap().push(
                inv.capability
                    .scopes()
                    .map(|held| held.iter().cloned().collect()),
            );
            Ok(Representation::new(
                ReprType::new("text/turtle"),
                SHAPES.as_bytes().to_vec(),
            ))
        });
        // The registry's past, which is the only past this seam is asked for here.
        let held = registry.clone();
        let history = FnEndpoint::new("history", move |inv: &Invocation<'_>| {
            assert_eq!(inv.inline_str("source")?, REGISTRY_IRI, "only the registry");
            Ok(Representation::new(
                ReprType::new("application/json"),
                held.lock().unwrap().clone().into_bytes(),
            ))
        });
        let names = ikigai_name::space()
            .bind(Exact::new(REGISTRY_IRI), registry_source)
            .bind(Exact::new(HISTORY_IRI), history)
            .bind(
                UriTemplate::parse(STORE_TEMPLATE).unwrap(),
                store(store_writes.clone(), store_reads.clone()),
            )
            .bind(
                Exact::new(READ_IRI),
                stand_in_reader(scratch.repo(), log.clone()),
            )
            .bind(Exact::new("urn:file:shapes/acme.ttl"), shapes);
        let backend: Arc<dyn Backend> = secrets.clone();
        let mut spaces: Vec<Arc<dyn ikigai_core::Space>> = vec![
            Arc::new(names),
            Arc::new(ikigai_secret::space(backend)),
            Arc::new(ikigai_sparql::space()),
        ];
        if bind_validator {
            spaces.push(Arc::new(ikigai_shacl::space()));
        }
        World {
            scratch,
            kernel: Kernel::new(Arc::new(Fallback::new(spaces))),
            log,
            secrets,
            store_writes,
            store_reads,
            shape_caps,
            registry,
        }
    }

    /// What the webhook's door and the timer hold.
    fn refresh_capability(&self) -> Capability {
        Capability::scoped([
            CAP_REFRESH.to_string(),
            STORE_WRITE.to_string(),
            format!("urn:cap:secret:read:{}", token_secret(None, "acme")),
            "urn:cap:fs:read:/srv".to_string(),
        ])
    }

    fn refresh_with(&self, body: Option<&[u8]>) -> Result<String> {
        let mut request =
            Request::new(Verb::Sink, Iri::parse("urn:name:git-refresh:acme").unwrap());
        if let Some(body) = body {
            request = request.with_arg("content", ArgRef::Inline(body.to_vec()));
        }
        let answer = block_on(self.kernel.issue(request, &self.refresh_capability()))?;
        Ok(String::from_utf8(answer.bytes.clone()).unwrap())
    }

    fn refresh(&self) -> String {
        self.refresh_with(None).expect("a refresh runs")
    }

    /// A delivery from the forge: signed with the namespace's key, checked as a door
    /// would, and only then a refresh.
    fn webhook(&self, signed_with: &[u8]) -> Result<String> {
        let body = br#"{"ref":"refs/heads/main"}"#;
        let key = self
            .secrets
            .get(&hook_secret(None, "acme"))?
            .unwrap_or_default();
        if !verify_push(&key, body, &signature(signed_with, body)) {
            return Err(Error::Denied("bad signature".into()));
        }
        self.refresh_with(Some(body))
    }

    fn read_request(&self) -> Request {
        Request::new(Verb::Source, Iri::parse("urn:name:document").unwrap())
            .with_arg("path", ArgRef::Inline(b"acme/core".to_vec()))
    }

    /// A public request: reads, and nothing else.
    fn read(&self) -> Result<String> {
        let answer = block_on(self.kernel.issue(
            self.read_request(),
            &Capability::scoped(Vec::<String>::new()),
        ))?;
        assert_eq!(answer.repr_type.media_type, "text/turtle");
        Ok(String::from_utf8(answer.bytes.clone()).unwrap())
    }

    fn is_read_cached(&self) -> bool {
        self.kernel.is_cached(
            &self.read_request(),
            &Capability::scoped(Vec::<String>::new()),
        )
    }

    fn status(&self) -> String {
        let capability = Capability::scoped(["urn:cap:name:admin:acme"]);
        let request = Request::new(
            Verb::Source,
            Iri::parse("urn:name:git-status:acme").unwrap(),
        );
        let answer = block_on(self.kernel.issue(request, &capability)).expect("the owner reads");
        String::from_utf8(answer.bytes.clone()).unwrap()
    }

    fn reads(&self) -> usize {
        self.log.reads.load(Ordering::Relaxed)
    }
}

fn signature(key: &[u8], body: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(key).unwrap();
    mac.update(body);
    let hex: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("sha256={hex}")
}

// --- a push -----------------------------------------------------------------

#[test]
fn a_push_invalidates_and_the_next_read_serves_the_new_version() {
    let world = World::new("mirror", false);
    world
        .secrets
        .set(&hook_secret(None, "acme"), b"hook-key")
        .unwrap();
    world.scratch.push(&vocabulary("one"));
    assert!(world.refresh().contains("now serving"));
    assert!(world.read().unwrap().contains("one"));

    // Held under its thread: a second read touches neither the store nor the repository.
    assert!(world.is_read_cached(), "the answer is cached");
    let reads = world.store_reads.load(Ordering::Relaxed);
    assert!(world.read().unwrap().contains("one"));
    assert_eq!(world.store_reads.load(Ordering::Relaxed), reads);

    // Pushed, with the webhook not yet arrived: nothing has told this host.
    world.scratch.push(&vocabulary("two"));
    assert!(world.read().unwrap().contains("one"));

    // A delivery someone else signed does nothing at all.
    let forged = world.webhook(b"wrong-key").expect_err("not the forge");
    assert!(matches!(forged, Error::Denied(_)));
    assert!(world.is_read_cached());

    assert!(world.webhook(b"hook-key").unwrap().contains("now serving"));
    assert!(!world.is_read_cached(), "the webhook cut the thread");
    assert!(world.read().unwrap().contains("two"));
    assert!(world.status().contains("serving: "));
}

/// A client cannot make this host fetch, however it asks.
#[test]
fn a_read_never_reaches_the_repository() {
    let world = World::new("hosted", false);
    world.scratch.push(&vocabulary("one"));
    let err = world.read().expect_err("nothing fetched yet");
    assert!(matches!(err, Error::Unavailable(_)), "{err:?}");
    assert!(
        err.to_string().contains("urn:name:git-status:acme"),
        "{err}"
    );
    assert_eq!(world.reads(), 0);

    world.refresh();
    assert_eq!(world.reads(), 1);
    for _ in 0..3 {
        world.read().unwrap();
    }
    assert_eq!(world.reads(), 1);
}

/// A request holds no write authority, so it cannot become a refresh.
#[test]
fn only_the_refresh_capability_refreshes() {
    let world = World::new("hosted", false);
    world.scratch.push(&vocabulary("one"));
    let request = Request::new(Verb::Sink, Iri::parse("urn:name:git-refresh:acme").unwrap());
    let public = Capability::scoped(Vec::<String>::new());
    let err = block_on(world.kernel.issue(request, &public)).expect_err("not a door's");
    assert!(matches!(err, Error::Denied(_)), "{err:?}");
    assert_eq!(world.reads(), 0);
}

#[test]
fn a_push_to_another_branch_reads_nothing_and_cuts_nothing() {
    let world = World::new("hosted", false);
    world.scratch.push(&vocabulary("one"));
    world.refresh();
    world.read().unwrap();
    let reads = world.reads();
    let answer = world
        .refresh_with(Some(br#"{"ref":"refs/heads/draft"}"#))
        .unwrap();
    assert!(answer.contains("refs/heads/draft"), "{answer}");
    assert_eq!(world.reads(), reads);
    assert!(world.is_read_cached());
}

/// A timer finding nothing new writes nothing, so it cuts nothing.
#[test]
fn a_refresh_that_changes_nothing_cuts_nothing() {
    let world = World::new("hosted", false);
    world.scratch.push(&vocabulary("one"));
    world.refresh();
    world.read().unwrap();
    let writes = world.store_writes.load(Ordering::Relaxed);
    assert!(world.refresh().contains("unchanged"));
    assert_eq!(world.store_writes.load(Ordering::Relaxed), writes);
    assert!(world.is_read_cached());
}

// --- validation on change ---------------------------------------------------

#[test]
fn a_version_failing_its_shape_is_refused_and_the_last_good_keeps_serving() {
    let world = World::new("hosted", false);
    world.scratch.push(&vocabulary("good"));
    world.refresh();
    let good = world.read().unwrap();

    world.scratch.push(NONCONFORMING);
    let answer = world.refresh();
    assert!(answer.contains("refused"), "{answer}");
    assert!(answer.contains("rdf-schema#comment"), "{answer}");

    assert_eq!(world.read().unwrap(), good);
    let status = world.status();
    assert!(status.contains("serving: "), "{status}");
    assert!(status.contains("refused: "), "{status}");
    assert!(status.contains("https://acme.example/ns#Thing"), "{status}");
    assert!(status.contains("rdf-schema#comment"), "{status}");
    assert!(status.contains("urn:file:shapes/acme.ttl"), "{status}");

    // Fixed: served, and the refusal is no longer news.
    world.scratch.push(&vocabulary("fixed"));
    assert!(world.refresh().contains("now serving"));
    assert!(world.read().unwrap().contains("fixed"));
    assert!(!world.status().contains("refused"));
}

/// The reason is the owner's to read: it can hold what they have not published.
#[test]
fn the_reason_is_readable_by_the_owner_and_nobody_else() {
    let world = World::new("hosted", false);
    world.scratch.push(NONCONFORMING);
    world.refresh();
    let request = || {
        Request::new(
            Verb::Source,
            Iri::parse("urn:name:git-status:acme").unwrap(),
        )
    };
    for capability in [
        Capability::scoped(Vec::<String>::new()),
        Capability::scoped(["urn:cap:name:admin:other"]),
    ] {
        let err = block_on(world.kernel.issue(request(), &capability)).expect_err("not the owner");
        assert!(matches!(err, Error::Denied(_)), "{err:?}");
        assert!(!err.to_string().contains("rdf-schema#comment"), "{err}");
    }
    // Public, a refused first version says only that there is nothing yet.
    let err = world.read().expect_err("nothing good yet");
    assert!(matches!(err, Error::Unavailable(_)), "{err:?}");
    assert!(!err.to_string().contains("rdf-schema#comment"), "{err}");
}

/// A refused version is recorded once, so a timer does not rewrite the same news.
#[test]
fn a_refused_version_is_recorded_once() {
    let world = World::new("hosted", false);
    world.scratch.push(NONCONFORMING);
    world.refresh();
    let writes = world.store_writes.load(Ordering::Relaxed);
    assert!(world.refresh().contains("refused"));
    assert_eq!(world.store_writes.load(Ordering::Relaxed), writes);
}

#[test]
fn whatever_is_not_turtle_is_refused_and_the_last_good_keeps_serving() {
    let world = World::new("hosted", false);
    world.scratch.push(&vocabulary("good"));
    world.refresh();
    let good = world.read().unwrap();
    for (bad, why) in [
        ("<html><script>alert(1)</script></html>", "cannot be read"),
        ("function evil() { return 1; }", "cannot be read"),
        ("not turtle at all", "cannot be read"),
        ("", "empty"),
        ("   \n  ", "empty"),
    ] {
        world.scratch.push(bad);
        let answer = world.refresh();
        assert!(
            answer.contains("refused") && answer.contains(why),
            "{bad:?}: {answer}"
        );
        assert_eq!(world.read().unwrap(), good, "{bad:?}");
    }
    assert!(world.status().contains("refused: "));
}

/// Over the ceiling is refused before it is parsed, by the sentence naming the setting.
#[test]
fn a_version_over_the_ceiling_is_refused() {
    let world = World::new("hosted", false);
    {
        let mut registry: serde_json::Value =
            serde_json::from_str(&world.registry.lock().unwrap()).unwrap();
        registry["limits"] = serde_json::json!({"max_document_bytes": 20});
        *world.registry.lock().unwrap() = registry.to_string();
    }
    world.scratch.push(&vocabulary("too long for this host"));
    let answer = world.refresh();
    assert!(answer.contains("max_document_bytes"), "{answer}");
}

/// A version that could not be checked is not a version that passed.
#[test]
fn nothing_unchecked_is_ever_served() {
    let world = World::with("hosted", false, false);
    world.scratch.push(&vocabulary("one"));
    let answer = world.refresh();
    assert!(answer.contains("could not be checked"), "{answer}");
    assert!(world.read().is_err());
    assert!(world.status().contains("not read"));
    assert!(world.status().contains("serving: nothing yet"));
}

/// A bad shape is not the owner's push having failed, and it must not look like one.
#[test]
fn a_shape_that_cannot_be_read_blames_the_shape() {
    let world = World::new("hosted", false);
    world.scratch.push(&vocabulary("one"));
    world.refresh();
    {
        let mut registry = world.registry.lock().unwrap();
        *registry = registry.replace("urn:file:shapes/acme.ttl", "urn:file:shapes/missing.ttl");
    }
    world.kernel.cut(REGISTRY_IRI);
    world.scratch.push(&vocabulary("two"));
    let answer = world.refresh();
    assert!(answer.contains("could not be checked"), "{answer}");
    assert!(answer.contains("urn:file:shapes/missing.ttl"), "{answer}");
    assert!(!world.status().contains("refused: "));
    assert!(world.read().unwrap().contains("one"));
}

/// The shapes are named by a claimant and read under a refresh's authority, so they are
/// read with only its read scopes: never the secret, the write or the refresh.
#[test]
fn the_shapes_are_read_with_less_authority_than_the_refresh_holds() {
    let world = World::new("hosted", true);
    world
        .secrets
        .set(&token_secret(None, "acme"), b"tok-123")
        .unwrap();
    world.scratch.push(&vocabulary("one"));
    world.refresh();
    let seen = world.shape_caps.lock().unwrap().clone();
    assert!(!seen.is_empty());
    for scopes in seen {
        assert_eq!(scopes, Some(vec!["urn:cap:fs:read:/srv".to_string()]));
    }
}

// --- when the repository disappears -----------------------------------------

#[test]
fn a_vanished_repository_still_serves_the_last_good_copy_from_a_mirror() {
    let world = World::new("mirror", false);
    world.scratch.push(&vocabulary("kept"));
    world.refresh();
    world.read().unwrap();

    world.scratch.vanish();
    let answer = world.refresh();
    assert!(answer.contains("nothing accepted"), "{answer}");
    assert!(world.read().unwrap().contains("kept"));
    let status = world.status();
    assert!(status.contains("serving: "), "{status}");
    assert!(status.contains("gone"), "{status}");
    assert!(
        status.contains("https://forge.example/acme/vocab"),
        "{status}"
    );
}

/// A hosted namespace has made no promise to outlive its repository.
#[test]
fn a_vanished_repository_stops_a_hosted_namespace_and_it_comes_back() {
    let world = World::new("hosted", false);
    world.scratch.push(&vocabulary("one"));
    world.refresh();
    world.read().unwrap();

    let saved = world.scratch.root.join("saved");
    std::fs::rename(world.scratch.repo(), &saved).unwrap();
    world.refresh();
    let err = world.read().expect_err("withdrawn");
    assert!(matches!(err, Error::NotFound(_)), "{err:?}");
    assert!(err.to_string().contains("acme"), "{err}");

    std::fs::rename(&saved, world.scratch.repo()).unwrap();
    world.refresh();
    assert!(world.read().unwrap().contains("one"));
    assert!(!world.status().contains("gone"));
}

/// A forge having a bad minute is not a repository having been deleted.
#[test]
fn a_failed_read_keeps_a_hosted_namespace_serving() {
    let world = World::new("hosted", false);
    world.scratch.push(&vocabulary("one"));
    world.refresh();
    *world.log.fail_with.lock().unwrap() = Some("the forge is down".into());
    let answer = world.refresh();
    assert!(answer.contains("the forge is down"), "{answer}");
    assert!(world.read().unwrap().contains("one"));
    assert!(world.status().contains("not read"));

    *world.log.fail_with.lock().unwrap() = None;
    world.refresh();
    assert!(!world.status().contains("not read"));
}

// --- a private repository ---------------------------------------------------

#[test]
fn a_private_repository_s_token_comes_from_the_secret_store() {
    let world = World::new("hosted", true);
    world
        .secrets
        .set(&token_secret(None, "acme"), b"tok-123\n")
        .unwrap();
    world.scratch.push(&vocabulary("one"));
    assert!(world.refresh().contains("now serving"));
    assert_eq!(*world.log.tokens.lock().unwrap(), ["tok-123"]);
    assert!(world.read().unwrap().contains("one"));

    // Nowhere a claimant, a client or an owner reads.
    let registry = Request::new(Verb::Source, Iri::parse("urn:name:registry").unwrap());
    let served = block_on(world.kernel.issue(registry, &Capability::root())).unwrap();
    assert!(!String::from_utf8_lossy(&served.bytes).contains("tok-123"));
    assert!(!world.registry.lock().unwrap().contains("tok-123"));
    assert!(!world.status().contains("tok-123"));
}

#[test]
fn a_public_repository_is_read_with_no_token() {
    let world = World::new("hosted", false);
    world.scratch.push(&vocabulary("one"));
    world.refresh();
    assert!(world.log.tokens.lock().unwrap().is_empty());
}

/// Whatever a reader says, the token is not in what is recorded.
#[test]
fn a_reader_echoing_the_token_does_not_put_it_in_the_record() {
    let world = World::new("hosted", true);
    world
        .secrets
        .set(&token_secret(None, "acme"), b"tok-123")
        .unwrap();
    *world.log.fail_with.lock().unwrap() = Some("401 for Bearer tok-123".into());
    world.scratch.push(&vocabulary("one"));
    let answer = world.refresh();
    assert!(answer.contains("401 for Bearer [token]"), "{answer}");
    assert!(!answer.contains("tok-123"), "{answer}");
    assert!(!world.status().contains("tok-123"));
}

#[test]
fn a_missing_token_says_which_secret_and_reads_nothing() {
    let world = World::new("hosted", true);
    world.scratch.push(&vocabulary("one"));
    let answer = world.refresh();
    assert!(answer.contains(&token_secret(None, "acme")), "{answer}");
    assert_eq!(world.reads(), 0);
    assert!(world.status().contains("not read"));
}

/// With the scope for one namespace's token, another's cannot be read: and as there is
/// no field naming a secret, the registry cannot ask.
#[test]
fn a_namespace_cannot_reach_another_s_token() {
    let world = World::new("hosted", true);
    world
        .secrets
        .set("name-git-token.other", b"theirs")
        .unwrap();
    world.scratch.push(&vocabulary("one"));
    let answer = world.refresh();
    assert!(answer.contains("nothing accepted"), "{answer}");
    assert!(world.log.tokens.lock().unwrap().is_empty());
}

fn with_repository(fields: &str) -> String {
    format!(
        r#"{{"namespaces":[{{"prefix":"acme","owner":"o","strategy":"hosted",
          "source":"urn:name:git:acme","repository":{{{fields}}}}}]}}"#
    )
}

const FIELDS: &str = r#""url":"https://f.example/a/b","branch":"main","path":"c.ttl",
    "shape":"urn:file:s.ttl""#;

#[test]
fn a_token_in_the_registry_refuses_the_whole_file_and_is_not_repeated() {
    for bad in [
        format!(r#"{FIELDS},"token":"ghp_secret""#),
        r#""url":"https://ghp_secret@f.example/a/b","branch":"main","path":"c.ttl",
           "shape":"urn:file:s.ttl""#
            .to_string(),
    ] {
        let err = Registry::from_json(with_repository(&bad).as_bytes()).expect_err("refused");
        let message = err.to_string();
        assert!(message.contains("\"acme\""), "{message}");
        assert!(message.contains("registry"), "{message}");
        assert!(!message.contains("ghp_secret"), "{message}");
    }
}

#[test]
fn the_registry_holds_the_binding_and_the_strategy_that_serves_it() {
    assert!(Registry::from_json(with_repository(FIELDS).as_bytes()).is_ok());

    for bad in [
        // It must name what it is served at, or the binding would be read by nothing.
        with_repository(FIELDS).replace("urn:name:git:acme", "urn:file:other"),
        with_repository(FIELDS).replace("urn:name:git:acme", "urn:name:git:other"),
        // A redirect is the owner's to serve, and a tombstone serves nothing.
        format!(
            r#"{{"namespaces":[{{"prefix":"acme","owner":"o","strategy":"redirect",
              "target":"https://a.example/","repository":{{{FIELDS}}}}}]}}"#
        ),
        format!(
            r#"{{"namespaces":[{{"prefix":"acme","owner":"o","strategy":"retired",
              "reason":"r","repository":{{{FIELDS}}}}}]}}"#
        ),
        // A misspelled field must not silently mean a public repository.
        with_repository(&format!(r#"{FIELDS},"privat":true"#)),
        with_repository(r#""url":"https://f.example/a/b""#),
    ] {
        let err = Registry::from_json(bad.as_bytes()).expect_err(&bad);
        let message = err.to_string();
        assert!(
            message.contains("\"acme\"") && message.contains("repository"),
            "{bad}: {message}"
        );
    }
}

/// An entry with no repository reads and writes as it always has.
#[test]
fn a_namespace_with_no_repository_is_unchanged() {
    let json = r#"{"namespaces":[{"prefix":"a","owner":"o","strategy":"hosted",
        "source":"urn:file:a.ttl"}]}"#;
    let registry = Registry::from_json(json.as_bytes()).unwrap();
    let written = serde_json::to_string(&registry).unwrap();
    assert!(!written.contains("repository"), "{written}");
}

// --- what is served ---------------------------------------------------------

/// The registry refuses to name it, so it cannot be fetched at all.
#[test]
fn repository_html_or_javascript_cannot_be_bound() {
    for path in ["index.html", "app.js", "x.xhtml", "x.svg", "x.rdf", "x.htm"] {
        let bad = with_repository(FIELDS).replace("c.ttl", path);
        let message = Registry::from_json(bad.as_bytes())
            .expect_err(path)
            .to_string();
        assert!(message.contains(".ttl"), "{path}: {message}");
    }
}

/// A reader's word for what it read is not taken, and one that says HTML is refused.
#[test]
fn a_version_labeled_html_or_javascript_is_never_served() {
    for label in [
        "text/html",
        "text/html; charset=utf-8",
        "application/xhtml+xml",
        "text/javascript",
        "application/javascript",
        "image/svg+xml",
    ] {
        let world = World::new("hosted", false);
        world.scratch.push(&vocabulary("good"));
        world.refresh();
        let good = world.read().unwrap();

        *world.log.answer_as.lock().unwrap() = Some(label);
        world.scratch.push(&vocabulary("labeled"));
        let answer = world.refresh();
        assert!(
            answer.contains("only Turtle is served"),
            "{label}: {answer}"
        );
        assert_eq!(world.read().unwrap(), good, "{label}");
    }
}

/// The type served is this module's, whatever the reader called the bytes it accepted.
#[test]
fn what_is_served_is_turtle_whatever_the_reader_called_it() {
    let world = World::new("hosted", false);
    *world.log.answer_as.lock().unwrap() = Some("application/octet-stream");
    world.scratch.push(&vocabulary("one"));
    world.refresh();
    // `read` asserts the type.
    assert!(world.read().unwrap().contains("one"));
}

/// HTML reaches this origin only as a page generated from the graph, escaped.
#[test]
fn the_page_is_generated_and_what_the_repository_says_in_it_is_escaped() {
    let world = World::new("hosted", false);
    world.scratch.push(&vocabulary("<script>alert(1)</script>"));
    assert!(world.refresh().contains("now serving"));

    let page = block_on(
        world.kernel.issue(
            world
                .read_request()
                .with_arg("as", ArgRef::Inline(b"text/html".to_vec())),
            &Capability::root(),
        ),
    )
    .expect("a page");
    assert!(page.repr_type.media_type.starts_with("text/html"));
    let page = String::from_utf8(page.bytes.clone()).unwrap();
    assert!(page.contains("<h1"), "{page}");
    assert!(!page.contains("<script>alert(1)"), "{page}");
    assert!(page.contains("&lt;script&gt;alert(1)"), "{page}");
}

// --- administration ---------------------------------------------------------

fn claim_args(repository: &str) -> Request {
    Request::new(Verb::Sink, Iri::parse("urn:name:claim").unwrap())
        .with_arg("prefix", ArgRef::Inline(b"fresh".to_vec()))
        .with_arg("strategy", ArgRef::Inline(b"hosted".to_vec()))
        .with_arg("source", ArgRef::Inline(b"urn:name:git:fresh".to_vec()))
        .with_arg("repository", ArgRef::Inline(repository.as_bytes().to_vec()))
}

/// A claim is held to what a file is, so it cannot smuggle in what a file cannot.
#[test]
fn a_claim_is_refused_what_a_file_is_refused() {
    let world = World::new("hosted", false);
    for bad in [
        format!(r#"{{{FIELDS},"token":"ghp_secret"}}"#),
        format!(r#"{{{}}}"#, FIELDS.replace("c.ttl", "index.html")),
    ] {
        let err =
            block_on(world.kernel.issue(claim_args(&bad), &Capability::root())).expect_err(&bad);
        assert!(
            matches!(&err, Error::InvalidArgument { name, .. } if name == "repository"),
            "{bad}: {err:?}"
        );
        assert!(!err.to_string().contains("ghp_secret"), "{err}");
    }
}

// --- as of a date -----------------------------------------------------------

/// The registry's past through the history seam, and the file's through the repository.
#[test]
fn as_of_reads_the_same_repository_s_history() {
    let world = World::new("hosted", false);
    world
        .scratch
        .push_at(&vocabulary("january"), "2026-01-10T12:00:00Z");
    world
        .scratch
        .push_at(&vocabulary("march"), "2026-03-10T12:00:00Z");
    world
        .scratch
        .push_at(&vocabulary("may"), "2026-05-10T12:00:00Z");
    let past = |at: &str| {
        block_on(
            world.kernel.issue(
                world
                    .read_request()
                    .with_arg("as-of", ArgRef::Inline(at.as_bytes().to_vec())),
                &Capability::root(),
            ),
        )
    };
    for (at, want) in [
        ("2026-01-10", "january"),
        ("2026-03-09", "january"),
        ("2026-03-10", "march"),
        ("2026-06-01", "may"),
    ] {
        let answer = past(at).expect(at);
        assert_eq!(answer.repr_type.media_type, "text/turtle", "{at}");
        assert!(
            String::from_utf8_lossy(&answer.bytes).contains(want),
            "{at}"
        );
    }
    let err = past("2025-12-31").expect_err("before the first commit");
    assert!(matches!(err, Error::NotFound(_)), "{err:?}");
    assert!(err.to_string().contains("urn:name:git:acme"), "{err}");
    // Never from the store: nothing was ever refreshed.
    assert_eq!(world.store_writes.load(Ordering::Relaxed), 0);
    // And never held: the cache is not the archive.
    assert!(!world.kernel.is_cached(
        &world
            .read_request()
            .with_arg("as-of", ArgRef::Inline(b"2026-03-10".to_vec())),
        &Capability::root()
    ));
}

/// With no reader bound, a refresh says what would add one instead of recording blame.
#[test]
fn a_host_with_no_reader_says_what_to_bind() {
    let space = ikigai_name::space().bind(
        Exact::new(REGISTRY_IRI),
        FnEndpoint::new("registry-source", |_: &Invocation<'_>| {
            Ok(Representation::new(
                ReprType::new("application/json"),
                entry("hosted", false).into_bytes(),
            ))
        }),
    );
    let kernel = Kernel::new(Arc::new(space));
    let err = block_on(kernel.issue(
        Request::new(Verb::Sink, Iri::parse("urn:name:git-refresh:acme").unwrap()),
        &Capability::root(),
    ))
    .expect_err("no reader");
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
    assert!(err.to_string().contains(READ_IRI), "{err}");
}

// --- forge_reader, over ikigai-http ------------------------------------------

/// A canned forge: the answers by URL, and what it was sent.
#[derive(Default)]
struct CannedForge {
    answers: Mutex<BTreeMap<String, (u16, Vec<u8>)>>,
    sent: Mutex<Vec<HttpRequest>>,
}

#[async_trait]
impl HttpTransport for CannedForge {
    async fn send(&self, request: HttpRequest) -> std::result::Result<HttpResponse, String> {
        let (status, body) = self
            .answers
            .lock()
            .unwrap()
            .get(&request.url)
            .cloned()
            .unwrap_or((404, b"not found".to_vec()));
        self.sent.lock().unwrap().push(request);
        Ok(HttpResponse {
            status,
            headers: vec![
                ("content-type".to_string(), "text/plain".to_string()),
                // What GitHub's raw files say. It must not hold a read past a push.
                ("cache-control".to_string(), "max-age=300".to_string()),
            ],
            body: HttpBody::from(body),
        })
    }
}

const FORGE_NET: &str = "urn:cap:net:api.forge.example";

fn forge_world() -> (Kernel, Arc<CannedForge>) {
    let forge = Arc::new(CannedForge::default());
    let reader = forge_reader_for(vec![Forge {
        host: "forge.example".into(),
        kind: ForgeKind::GitHub,
        api: "https://api.forge.example".into(),
    }]);
    let space = EndpointSpace::new().bind(Exact::new(READ_IRI), reader);
    let kernel = Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(space),
        Arc::new(ikigai_http::space(forge.clone())),
    ])))
    .with_clock(Arc::new(ikigai_core::SystemClock));
    (kernel, forge)
}

fn forge_read(url: &str, extra: &[(&str, &str)]) -> Request {
    let mut request = Request::new(Verb::Source, Iri::parse(READ_IRI).unwrap())
        .with_arg("url", ArgRef::Inline(url.as_bytes().to_vec()))
        .with_arg("branch", ArgRef::Inline(b"main".to_vec()))
        .with_arg("path", ArgRef::Inline(b"vocab/core.ttl".to_vec()));
    for (name, value) in extra {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    request
}

const FILE_URL: &str =
    "https://api.forge.example/repos/acme/vocab/contents/vocab/core.ttl?ref=main";

#[test]
fn the_forge_reader_sends_the_token_and_reads_afresh_after_a_push() {
    let (kernel, forge) = forge_world();
    let capability = Capability::scoped([FORGE_NET]);
    forge
        .answers
        .lock()
        .unwrap()
        .insert(FILE_URL.to_string(), (200, b"# one\n".to_vec()));
    let read = || {
        block_on(kernel.issue(
            forge_read(
                "https://forge.example/acme/vocab.git",
                &[("token", "tok-123")],
            ),
            &capability,
        ))
    };
    assert_eq!(read().unwrap().bytes, b"# one\n");
    let sent = forge.sent.lock().unwrap()[0].clone();
    assert!(
        sent.headers
            .iter()
            .any(|(name, value)| name == "Authorization" && value == "Bearer tok-123"),
        "{:?}",
        sent.headers
    );

    forge
        .answers
        .lock()
        .unwrap()
        .insert(FILE_URL.to_string(), (200, b"# two\n".to_vec()));
    assert_eq!(read().unwrap().bytes, b"# two\n", "max-age=300 held this");
}

#[test]
fn the_forge_reader_says_not_found_for_a_404_and_keeps_other_failures_apart() {
    let (kernel, forge) = forge_world();
    let capability = Capability::scoped([FORGE_NET]);
    let read = || {
        block_on(kernel.issue(
            forge_read("https://forge.example/acme/vocab", &[]),
            &capability,
        ))
    };
    assert!(matches!(read().unwrap_err(), Error::NotFound(_)));
    for status in [401, 403, 500, 502] {
        forge
            .answers
            .lock()
            .unwrap()
            .insert(FILE_URL.to_string(), (status, b"no".to_vec()));
        let err = read().expect_err("a failure");
        assert!(!matches!(err, Error::NotFound(_)), "{status}: {err:?}");
    }
}

/// A registry is a claimant's writing, so where a read may go is not up to it.
#[test]
fn the_forge_reader_goes_only_to_its_own_forges() {
    let (kernel, forge) = forge_world();
    for url in [
        "https://evil.example/acme/vocab",
        "https://169.254.169.254/latest/meta",
        "https://forge.example.evil.example/acme/vocab",
        "https://forge.example/../vocab",
    ] {
        let err = block_on(kernel.issue(forge_read(url, &[]), &Capability::root())).expect_err(url);
        assert!(matches!(err, Error::Unsupported { .. }), "{url}: {err:?}");
    }
    assert!(forge.sent.lock().unwrap().is_empty());
}

#[test]
fn the_forge_reader_needs_the_net_scope_for_the_forge() {
    let (kernel, forge) = forge_world();
    let err = block_on(kernel.issue(
        forge_read("https://forge.example/acme/vocab", &[]),
        &Capability::scoped(["urn:cap:net:other.example"]),
    ))
    .expect_err("no grant");
    assert!(matches!(err, Error::Denied(_)), "{err:?}");
    assert!(forge.sent.lock().unwrap().is_empty());
}

#[test]
fn the_forge_reader_reads_as_of_through_the_commit_it_finds() {
    let (kernel, forge) = forge_world();
    let list = "https://api.forge.example/repos/acme/vocab/commits?sha=main&path=vocab%2Fcore.ttl\
                &until=2026-03-31T23%3A59%3A59Z&per_page=1";
    {
        let mut answers = forge.answers.lock().unwrap();
        answers.insert(list.to_string(), (200, br#"[{"sha":"abc123"}]"#.to_vec()));
        answers.insert(
            "https://api.forge.example/repos/acme/vocab/contents/vocab/core.ttl?ref=abc123"
                .to_string(),
            (200, b"# march\n".to_vec()),
        );
    }
    let answer = block_on(kernel.issue(
        forge_read(
            "https://forge.example/acme/vocab",
            &[("as-of", "2026-03-31")],
        ),
        &Capability::root(),
    ))
    .unwrap();
    assert_eq!(answer.bytes, b"# march\n");

    forge
        .answers
        .lock()
        .unwrap()
        .insert(list.to_string(), (200, b"[]".to_vec()));
    let err = block_on(kernel.issue(
        forge_read(
            "https://forge.example/acme/vocab",
            &[("as-of", "2026-03-31")],
        ),
        &Capability::root(),
    ))
    .expect_err("nothing by then");
    assert!(matches!(err, Error::NotFound(_)), "{err:?}");
}
