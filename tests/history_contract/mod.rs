//! The contract any binding of `urn:name:history-source` must pass.
//!
//! Written against the seam alone: it records a history through [`Binding::record`] and
//! asks questions through [`Binding::read`] and [`Binding::claims`], and it never names
//! git. The git binding runs it in `tests/history_source.rs` (the library) and
//! `tests/history_http.rs` (the standalone server); a store-backed binding is expected
//! to run the same [`run`] with its own [`Binding`].

#![allow(dead_code)]

/// The vocabulary file every timeline below records.
pub const VOCAB: &str = "vocab/core.ttl";
/// The same file as the registry names it, and as a history source.
pub const SOURCE: &str = "urn:file:vocab/core.ttl";

pub const RESMUD: &str = r#"{"prefix":"resmud","owner":"urn:cap:name:admin:resmud",
    "strategy":"hosted","source":"urn:file:vocab/core.ttl"}"#;
pub const LATER: &str = r#"{"prefix":"later","owner":"urn:cap:name:admin:later",
    "strategy":"redirect","target":"https://later.example/ns"}"#;

pub fn registry(namespaces: &[&str]) -> String {
    format!("{{\"namespaces\":[{}]}}", namespaces.join(","))
}

/// A version of the vocabulary, as Turtle a document face can serve.
pub fn version(tag: &str) -> String {
    format!("<urn:test:vocab> <urn:test:version> \"{tag}\" .\n")
}

/// A history source under test, with a way to give it a past.
pub trait Binding {
    /// Record `files` (paths relative to the vocabulary's home, the registry at
    /// `registry.json`) as they stood from instant `at`, for `tenant`.
    fn record(&mut self, tenant: Option<&str>, at: &str, files: &[(&str, &str)]);
    /// [`SOURCE`] as it stood at `as_of`, or `None` for NotFound.
    fn read(&self, tenant: Option<&str>, as_of: &str) -> Option<String>;
    /// Whether the registry, as it stood at `as_of`, claimed `prefix`: the registry
    /// itself read as a source.
    fn claims(&self, tenant: Option<&str>, prefix: &str, as_of: &str) -> bool;
}

/// Every rule, over two tenants (`None` may stand for the only one if a binding has no
/// tenants, but then `other` must still be a distinct tenant).
pub fn run(binding: &mut impl Binding, one: Option<&str>, other: Option<&str>) {
    let v1 = version("1");
    let v2 = version("2");
    let v3 = version("3");
    let first = registry(&[RESMUD]);
    let second = registry(&[RESMUD, LATER]);
    binding.record(
        one,
        "2026-01-10T12:00:00Z",
        &[("registry.json", &first), (VOCAB, &v1)],
    );
    binding.record(
        one,
        "2026-03-10T12:00:00Z",
        &[("registry.json", &second), (VOCAB, &v2)],
    );
    binding.record(one, "2026-05-10T12:00:00Z", &[(VOCAB, &v3)]);
    let theirs = version("theirs");
    binding.record(
        other,
        "2026-02-10T12:00:00Z",
        &[("registry.json", &first), (VOCAB, &theirs)],
    );

    // NotFound before the resource existed.
    assert_eq!(binding.read(one, "2026-01-09"), None, "before it existed");
    assert_eq!(
        binding.read(one, "2026-01-10T11:59:59Z"),
        None,
        "a second before"
    );

    // The newest version at or before the instant: at the instant itself, and after
    // the last one.
    assert_eq!(binding.read(one, "2026-01-10T12:00:00Z"), Some(v1.clone()));
    assert_eq!(binding.read(one, "2026-03-10T12:00:00Z"), Some(v2.clone()));
    assert_eq!(binding.read(one, "2027-01-01"), Some(v3.clone()));

    // An instant between two versions answers the older.
    assert_eq!(binding.read(one, "2026-04-01"), Some(v2.clone()));
    assert_eq!(binding.read(one, "2026-03-10T11:59:59Z"), Some(v1.clone()));

    // The registry is a source like any other.
    assert!(binding.claims(one, "resmud", "2026-02-01"));
    assert!(!binding.claims(one, "later", "2026-02-01"));
    assert!(binding.claims(one, "later", "2026-04-01"));

    // Keyed by tenant: the other tenant's past is its own.
    assert_eq!(binding.read(other, "2026-04-01"), Some(theirs.clone()));
    assert_eq!(binding.read(other, "2026-02-01"), None);
    assert!(!binding.claims(other, "later", "2027-01-01"));
    assert_eq!(binding.read(one, "2026-02-01"), Some(v1.clone()));

    // Never cached: a version recorded later, dated between two already asked about,
    // changes the answer to a question already answered.
    let backfilled = version("2.5");
    binding.record(one, "2026-03-20T12:00:00Z", &[(VOCAB, &backfilled)]);
    assert_eq!(
        binding.read(one, "2026-04-01"),
        Some(backfilled),
        "a cached answer"
    );
    assert_eq!(binding.read(one, "2026-03-15"), Some(v2));
}

/// A scratch git repository per tenant, which the git binding's runs share.
pub mod git {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A fresh directory under the system's temporary one, canonicalized.
    pub fn scratch(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ikigai-name-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("scratch directory");
        dir.canonicalize().expect("canonical scratch directory")
    }

    pub fn init(dir: &Path) {
        std::fs::create_dir_all(dir.join("vocab")).expect("vocab directory");
        git(dir, &["init", "--quiet"], None);
    }

    /// Commit `files` with author and committer dates of `at`.
    pub fn commit(dir: &Path, at: &str, files: &[(&str, &str)]) {
        for (path, content) in files {
            std::fs::write(dir.join(path), content).expect("write");
        }
        git(dir, &["add", "-A"], None);
        git(dir, &["commit", "--quiet", "-m", at], Some(at));
    }

    fn git(dir: &Path, args: &[&str], date: Option<&str>) {
        let mut command = Command::new("git");
        command
            .args(["-c", "user.name=test", "-c", "user.email=test@example.org"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir);
        if let Some(date) = date {
            command
                .env("GIT_AUTHOR_DATE", date)
                .env("GIT_COMMITTER_DATE", date);
        }
        let status = command.status().expect("run git");
        assert!(status.success(), "git {args:?}: {status}");
    }
}
