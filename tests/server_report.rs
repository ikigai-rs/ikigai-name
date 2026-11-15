#![cfg(feature = "server")]
use std::process::Command;

#[test]
fn monthly_report_reads_stored_sets_without_listening_or_writing() {
    let root = std::env::temp_dir().join(format!("name-report-{}", std::process::id()));
    std::fs::create_dir_all(root.join("seen")).unwrap();
    std::fs::write(root.join("registry.json"), r#"{"namespaces":[{"prefix":"acme","owner":"urn:cap:name:admin:acme","strategy":"redirect","target":"https://example.com/","rules":[{"match":"old/{x}","target":"https://example.com/{x}"}]}]}"#).unwrap();
    let set = r#"{"period":"2026-11","names":["acme/a","acme/b"]}"#;
    std::fs::write(root.join("seen/acme.2026-11.json"), set).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ikigai-name-server"))
        .arg("--root")
        .arg(&root)
        .args(["--report", "2026-11", "--listen"])
        .arg(listener.local_addr().unwrap().to_string())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("active names: 2"), "{text}");
    assert!(text.contains("misses: 0"), "{text}");
    assert!(text.contains("dropped names: 0"), "{text}");
    assert!(text.contains("old/{x}"), "{text}");
    assert_eq!(
        std::fs::read_to_string(root.join("seen/acme.2026-11.json")).unwrap(),
        set
    );
    std::fs::remove_dir_all(root).unwrap();
}

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "name-monthly-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, file: &str, content: &str) {
        let path = self.0.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    fn report(&self, flags: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_ikigai-name-server"))
            .arg("--root")
            .arg(&self.0)
            .arg("--report")
            .arg("2026-11")
            .args(flags)
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
const REGISTRY: &str = r#"{"namespaces":[{"prefix":"acme","owner":"urn:cap:name:admin:acme","strategy":"redirect","target":"https://example.com/","rules":[{"match":"old/{x}","target":"https://example.com/{x}"}]},{"prefix":"empty","owner":"urn:cap:name:admin:empty","strategy":"retired","reason":"ended","public":false}]}"#;

#[test]
fn tenants_rules_misses_drops_and_turtle_are_reported_from_their_own_sets() {
    let f = Fixture::new();
    for (host, names, drops) in [
        ("a.example", r#"["acme/a","acme/b"]"#, 3),
        ("b.example", r#"["acme/c"]"#, 5),
    ] {
        f.write(&format!("{host}/custom.json"), REGISTRY);
        f.write(
            &format!("{host}/seen/acme.2026-11.json"),
            &format!(r#"{{"period":"2026-11","names":{names},"dropped":{drops}}}"#),
        );
        f.write(
            &format!("{host}/seen/acme.2026-12.json"),
            r#"{"period":"2026-12","names":["acme/not-november"]}"#,
        );
        f.write(
            &format!("{host}/seen/partitions/acme~3Arule~3Aold~252F~257Bx~257D.2026-11.json"),
            r#"{"period":"2026-11","names":["acme/a"],"dropped":2}"#,
        );
        f.write(
            &format!("{host}/seen/partitions/acme~3Amiss.2026-11.json"),
            r#"{"period":"2026-11","names":["acme/b"],"dropped":1}"#,
        );
    }
    let flags = [
        "--registry",
        "custom.json",
        "--tenant",
        "a.example",
        "--tenant",
        "b.example",
    ];
    let output = f.report(&flags);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(output.stdout).unwrap();
    for expected in [
        "tenant: \"a.example\"\tnamespace: \"acme\"\tactive names: 2",
        "tenant: \"b.example\"\tnamespace: \"acme\"\tactive names: 1",
        "tenant: \"a.example\"\tnamespace: \"acme\"\tdropped names: 6",
        "tenant: \"b.example\"\tnamespace: \"acme\"\tdropped names: 8",
        "names answered by rule: 1\trule: \"old/{x}\"",
        "misses: 1",
        "namespace: \"empty\"\tactive names: 0",
    ] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    let mut turtle_flags = flags.to_vec();
    turtle_flags.extend(["--as", "text/turtle"]);
    let output = f.report(&turtle_flags);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let ttl = String::from_utf8(output.stdout).unwrap();
    let triples = oxrdfio::RdfParser::from_format(oxrdfio::RdfFormat::Turtle)
        .for_reader(ttl.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let observations: std::collections::BTreeSet<_> = triples
        .iter()
        .filter(|q| q.predicate.as_str() == "https://schema.org/variableMeasured")
        .map(|q| q.subject.to_string())
        .collect();
    assert_eq!(
        observations.len(),
        14,
        "every metric and tenant has its own subject"
    );
    for expected in [
        "schema:value 6",
        "schema:value 8",
        "schema:spatialCoverage \"a.example\"",
        "schema:description \"old/{x}\"",
        "xsd:gYearMonth",
    ] {
        assert!(ttl.contains(expected), "missing {expected}: {ttl}");
    }
}

#[test]
fn missing_sets_are_zero_and_empty_registries_say_so_without_writes() {
    let f = Fixture::new();
    f.write("registry.json", REGISTRY);
    let out = f.report(&["--count-names"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!f.0.join("seen").exists());
    assert!(!f.0.join("git").exists());
    f.write("registry.json", r#"{"namespaces":[]}"#);
    for flags in [vec![], vec!["--as", "text/turtle"]] {
        let out = f.report(&flags);
        assert!(out.status.success());
        assert!(String::from_utf8(out.stdout)
            .unwrap()
            .contains("no namespaces to report for 2026-11"));
    }
}

#[test]
fn invalid_inputs_and_corrupt_sets_fail_without_partial_stdout() {
    let f = Fixture::new();
    f.write("registry.json", REGISTRY);
    for (flags, expected) in [
        (vec!["--report", "2026-13"], "is not a month"),
        (vec!["--as", "application/json"], "is not a report format"),
        (vec!["--check"], "separately from --check"),
    ] {
        let out = f.report(&flags);
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains(expected));
    }
    for content in ["not JSON", r#"{"period":"2026-12","names":[]}"#] {
        f.write("seen/acme.2026-11.json", content);
        let out = f.report(&[]);
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("acme"));
    }
}
