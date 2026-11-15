//! Offline validation through the real binary, without a free listening port.
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ikigai-name-check-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, name: &str, content: &str) {
        let path = self.0.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    fn check(&self, flags: &[&str]) -> Output {
        // Success even with this address occupied proves check never binds it.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        Command::new(env!("CARGO_BIN_EXE_ikigai-name-server"))
            .args(["--check", "--root"])
            .arg(&self.0)
            .arg("--listen")
            .arg(listener.local_addr().unwrap().to_string())
            .args(flags)
            .output()
            .unwrap()
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
const GOOD: &str = r#"{"namespaces":[
{"prefix":"acme","owner":"urn:cap:name:admin:acme","strategy":"hosted","source":"urn:file:core.ttl","public":false},
{"prefix":"copy","owner":"urn:cap:name:admin:copy","strategy":"mirror","origin":"urn:file:core.ttl"},
{"prefix":"away","owner":"urn:cap:name:admin:away","strategy":"redirect","target":"https://example.com/"},
{"prefix":"gone","owner":"urn:cap:name:admin:gone","strategy":"retired","reason":"retired"}
]}"#;
fn refused(output: Output, words: &[&str]) {
    assert!(!output.status.success());
    let text = String::from_utf8(output.stderr).unwrap();
    for word in words {
        assert!(text.contains(word), "missing {word:?}: {text}");
    }
    assert!(!text.contains("USAGE"), "{text}");
}
#[test]
fn a_good_registry_checks_without_listening_or_writing() {
    let root = Root::new();
    root.write("registry.json", GOOD);
    root.write("core.ttl", "# vocabulary\n");
    let output = root.check(&["--count-names"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!root.0.join("seen").exists());
    assert!(!root.0.join("git").exists());
}
#[test]
fn an_overlap_is_the_loaders_refusal() {
    let root = Root::new();
    root.write("registry.json", &GOOD.replace("\"copy\"", "\"acme/sub\""));
    refused(
        root.check(&[]),
        &["not a valid registry", "acme/sub", "acme", "overlaps"],
    );
}
#[test]
fn a_bad_rule_is_the_loaders_refusal() {
    let root = Root::new();
    root.write("registry.json", &GOOD.replace("\"target\":\"https://example.com/\"", "\"target\":\"https://example.com/\",\"rules\":[{\"match\":\"{x}\",\"target\":\"https://example.com/{y}\"}]"));
    refused(
        root.check(&[]),
        &["not a valid registry", "away", "rule 1", "{y}"],
    );
}
#[test]
fn a_missing_document_names_its_namespace() {
    let root = Root::new();
    root.write("registry.json", GOOD);
    refused(root.check(&[]), &["acme", "urn:file:core.ttl"]);
}
#[test]
fn an_oversized_document_names_its_namespace_and_ceiling() {
    let root = Root::new();
    root.write(
        "registry.json",
        &GOOD.replace(
            "{\"namespaces\"",
            "{\"limits\":{\"max_document_bytes\":3},\"namespaces\"",
        ),
    );
    root.write("core.ttl", "four");
    refused(
        root.check(&[]),
        &[
            "acme",
            "4 bytes",
            "3-byte ceiling",
            "limits.max_document_bytes",
        ],
    );
}
#[test]
fn every_tenant_is_checked_with_its_own_registry_and_documents() {
    let root = Root::new();
    for host in ["a.example", "b.example"] {
        root.write(&format!("{host}/custom.json"), GOOD);
        root.write(&format!("{host}/core.ttl"), "# ok");
    }
    let flags = [
        "--registry",
        "custom.json",
        "--tenant",
        "a.example",
        "--tenant",
        "b.example",
    ];
    assert!(root.check(&flags).status.success());
    std::fs::remove_file(root.0.join("b.example/core.ttl")).unwrap();
    refused(
        root.check(&flags),
        &["b.example", "acme", "urn:file:core.ttl"],
    );
}

#[test]
fn the_size_ceiling_is_inclusive() {
    let root = Root::new();
    root.write(
        "registry.json",
        &GOOD.replace(
            "{\"namespaces\"",
            "{\"limits\":{\"max_document_bytes\":4},\"namespaces\"",
        ),
    );
    root.write("core.ttl", "four");
    assert!(root.check(&[]).status.success());
}

#[test]
fn a_later_mirrored_document_is_read_too() {
    let root = Root::new();
    root.write(
        "registry.json",
        &GOOD.replace(
            "\"origin\":\"urn:file:core.ttl\"",
            "\"origin\":\"urn:file:missing.ttl\"",
        ),
    );
    root.write("core.ttl", "# ok");
    refused(root.check(&[]), &["copy", "urn:file:missing.ttl"]);
}

#[test]
fn a_document_cannot_escape_the_file_jail() {
    let root = Root::new();
    root.write(
        "registry.json",
        &GOOD.replace("urn:file:core.ttl", "urn:file:../outside.ttl"),
    );
    refused(root.check(&[]), &["acme", "urn:file:../outside.ttl"]);
}

#[cfg(feature = "git")]
#[test]
fn check_does_not_initialize_git_or_secrets() {
    let root = Root::new();
    root.write("registry.json", GOOD);
    root.write("core.ttl", "# ok");
    let secrets = root.0.join("must-not-be-created");
    let output = root.check(&["--git", "--secrets", secrets.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!secrets.exists());
    assert!(!root.0.join("git").exists());
}

#[test]
fn target_health_options_require_a_timer_and_an_explicit_host_together() {
    let root = Root::new();
    root.write("registry.json", GOOD);
    refused(
        root.check(&["--target-health-every", "60"]),
        &["needed together"],
    );
    refused(
        root.check(&["--target-health-net", "example.com"]),
        &["needed together"],
    );
    refused(root.check(&["--target-health-every", "0"]), &["above zero"]);
    refused(
        root.check(&["--target-health-net", "example.com:8080"]),
        &["without a port"],
    );
}

#[cfg(not(feature = "git"))]
#[test]
fn the_plain_server_cannot_be_told_to_make_target_probes() {
    let root = Root::new();
    root.write("registry.json", GOOD);
    refused(
        root.check(&[
            "--target-health-every",
            "60",
            "--target-health-net",
            "example.com",
        ]),
        &["need a build with the git feature"],
    );
}
