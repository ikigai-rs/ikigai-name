//! Target-health jobs use registry targets, never names seen in requests.
#![cfg(feature = "git")]

use ikigai_core::{
    Capability, Exact, FnEndpoint, Invocation, Iri, Kernel, ReprType, Representation, Request, Verb,
};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const CHECK: &str = "urn:name:target-health-check:namespace:acme";
const STATUS: &str = "urn:name:target-health:namespace:acme";
const OWNER: &str = "urn:cap:name:admin:staff";

fn registry(target: &str, rules: Value) -> Value {
    json!({"namespaces": [{"prefix": "acme", "owner": OWNER,
        "strategy": "redirect", "target": target, "rules": rules}]})
}

fn mutable_kernel(value: Value) -> (Kernel, Arc<Mutex<Value>>) {
    let value = Arc::new(Mutex::new(value));
    let source_value = value.clone();
    let health = ikigai_name::target_health::space().bind(
        Exact::new(ikigai_name::REGISTRY_IRI),
        FnEndpoint::new("registry", move |_: &Invocation<'_>| {
            Ok(Representation::new(
                ReprType::new("application/json"),
                serde_json::to_vec(&*source_value.lock().unwrap()).unwrap(),
            ))
        }),
    );
    let space = ikigai_core::Fallback::new(vec![Arc::new(health), Arc::new(ikigai_name::space())]);
    (Kernel::new(Arc::new(space)), value)
}

struct Server {
    base: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    fn start(location: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_requests = requests.clone();
        let thread_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            // Keep bodies pending until shutdown. A probe that reads one cannot finish.
            let mut pending_bodies = Vec::new();
            while !thread_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut bytes = Vec::new();
                        while !bytes.windows(4).any(|window| window == b"\r\n\r\n")
                            && bytes.len() < 8192
                        {
                            let mut part = [0; 1024];
                            let size = stream.read(&mut part).unwrap_or(0);
                            if size == 0 {
                                break;
                            }
                            bytes.extend_from_slice(&part[..size]);
                        }
                        let request = String::from_utf8_lossy(&bytes).into_owned();
                        let status = if request.starts_with("GET /missing ") {
                            404
                        } else if request.starts_with("GET /move ") {
                            302
                        } else {
                            200
                        };
                        thread_requests.lock().unwrap().push(request);
                        // Advertise a body but never send it: probes must finish on headers.
                        let _ = write!(stream, "HTTP/1.1 {status} Test\r\nLocation: {location}\r\nSet-Cookie: visitor=private\r\nContent-Length: 1000000\r\nConnection: close\r\n\r\n");
                        pending_bodies.push(stream);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("{error}"),
                }
            }
        });
        Self {
            base,
            requests,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}

fn kernel(target: &str) -> Kernel {
    let registry = json!({"namespaces": [{
        "prefix": "acme", "owner": "urn:cap:name:admin:staff",
        "strategy": "redirect", "target": target
    }]})
    .to_string();
    Kernel::new(Arc::new(ikigai_name::target_health::space().bind(
        Exact::new(ikigai_name::REGISTRY_IRI),
        FnEndpoint::new("registry", move |_: &Invocation<'_>| {
            Ok(Representation::new(
                ReprType::new("application/json"),
                registry.as_bytes().to_vec(),
            ))
        }),
    )))
}

async fn issue(
    kernel: &Kernel,
    verb: Verb,
    iri: &str,
    cap: &Capability,
) -> ikigai_core::Result<Value> {
    let repr = kernel
        .issue(Request::new(verb, Iri::parse(iri).unwrap()), cap)
        .await?;
    Ok(serde_json::from_slice(&repr.bytes).unwrap())
}

#[tokio::test]
async fn the_host_job_records_a_failed_connection_for_the_administrator() {
    // A just-closed local port refuses a connection.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let kernel = kernel(&format!("http://127.0.0.1:{port}/"));
    issue(
        &kernel,
        Verb::Sink,
        "urn:name:target-health-check:namespace:acme",
        &Capability::root(),
    )
    .await
    .expect("the host can check targets");
    let report = issue(
        &kernel,
        Verb::Source,
        "urn:name:target-health:namespace:acme",
        &Capability::scoped(["urn:cap:name:admin:staff"]),
    )
    .await
    .expect("the recorded owner can read the result");
    assert_eq!(report["prefix"], "acme");
    assert!(report["checked_at_unix_ms"].is_u64());
    assert_eq!(report["targets"][0]["outcome"], "unreachable");
    assert!(report["targets"][0]["status"].is_null());
    assert!(report["targets"][0]["elapsed_ms"].is_u64());
}

#[tokio::test]
async fn literal_bases_are_deduplicated_and_redirects_and_bodies_are_not_followed() {
    let next = Server::start(String::new());
    let server = Server::start(next.base.replace("127.0.0.1", "localhost") + "/private");
    let (kernel, _) = mutable_kernel(registry(
        &format!("{}/fallback", server.base),
        json!([
            {"match":"old/{x}", "target":format!("{}/rule/{{x}}", server.base)},
            {"match":"docs/{rest*}", "target":format!("{}/rule/{{rest}}", server.base)},
            {"match":"missing", "target":format!("{}/missing", server.base)},
            {"match":"move", "target":format!("{}/move", server.base)}
        ]),
    ));
    // Resolution traffic must neither trigger probes nor influence what they probe.
    let request = Request::new(Verb::Source, Iri::parse("urn:name:resolve").unwrap()).with_arg(
        "path",
        ikigai_core::ArgRef::Inline(b"acme/old/private-visitor-path".to_vec()),
    );
    kernel.issue(request, &Capability::root()).await.unwrap();
    assert!(server.requests.lock().unwrap().is_empty());
    let cap = Capability::scoped([
        ikigai_name::target_health::CAP_CHECK,
        "urn:cap:net:127.0.0.1",
    ]);
    let report = issue(&kernel, Verb::Sink, CHECK, &cap).await.unwrap();
    let checks = report["targets"].as_array().unwrap();
    assert!(
        checks.iter().all(|check| check["status"].is_u64()),
        "{report}; requests: {:?}",
        server.requests.lock().unwrap()
    );
    assert_eq!(checks.len(), 4);
    assert_eq!(
        checks
            .iter()
            .map(|check| check["status"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [200, 404, 302, 200]
    );
    assert!(checks.iter().all(|check| check["outcome"] == "answered"));
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 4);
    assert!(requests
        .iter()
        .all(|r| !r.contains("private-visitor-path") && !r.to_lowercase().contains("cookie:")));
    assert!(next.requests.lock().unwrap().is_empty());
    let serialized = report.to_string();
    assert!(
        !serialized.contains("visitor")
            && !serialized.contains("Location")
            && !serialized.contains("private")
    );
    assert_eq!(
        issue(&kernel, Verb::Source, STATUS, &Capability::scoped([OWNER]))
            .await
            .unwrap(),
        report
    );
}

#[tokio::test]
async fn every_host_is_authorized_before_any_probe_leaves() {
    let server = Server::start(String::new());
    let (kernel, _) = mutable_kernel(registry(
        &format!("{}/first", server.base),
        json!([
            {"match":"{x}", "target":"http://localhost/forbidden/{x}"}
        ]),
    ));
    for cap in [
        Capability::scoped([ikigai_name::target_health::CAP_CHECK]),
        Capability::scoped(["urn:cap:net:127.0.0.1"]),
        Capability::scoped([
            ikigai_name::target_health::CAP_CHECK,
            "urn:cap:net:127.0.0.1",
        ]),
    ] {
        assert!(matches!(
            issue(&kernel, Verb::Sink, CHECK, &cap).await,
            Err(ikigai_core::Error::Denied(_))
        ));
    }
    assert!(server.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn results_need_the_current_recorded_owner_and_are_never_cached() {
    let (kernel, value) = mutable_kernel(registry("urn:example:unsupported", json!([])));
    issue(&kernel, Verb::Sink, CHECK, &Capability::root())
        .await
        .unwrap();
    for cap in [
        Capability::scoped(Vec::<String>::new()),
        Capability::scoped(["urn:cap:name:admin:acme"]),
    ] {
        assert!(matches!(
            issue(&kernel, Verb::Source, STATUS, &cap).await,
            Err(ikigai_core::Error::Denied(_))
        ));
    }
    let report = kernel
        .issue(
            Request::new(Verb::Source, Iri::parse(STATUS).unwrap()),
            &Capability::scoped([OWNER]),
        )
        .await
        .unwrap();
    assert_eq!(report.expiry, ikigai_core::Expiry::Always);
    value.lock().unwrap()["namespaces"][0]["owner"] = json!("urn:cap:name:admin:new");
    assert!(matches!(
        issue(&kernel, Verb::Source, STATUS, &Capability::scoped([OWNER])).await,
        Err(ikigai_core::Error::Denied(_))
    ));
    assert!(issue(
        &kernel,
        Verb::Source,
        STATUS,
        &Capability::scoped(["urn:cap:name:admin:new"])
    )
    .await
    .is_ok());
}

#[tokio::test]
async fn unsupported_targets_are_explicit_and_changed_targets_need_a_new_check() {
    let (kernel, value) = mutable_kernel(registry("urn:example:unsupported", json!([])));
    assert!(matches!(
        issue(&kernel, Verb::Source, STATUS, &Capability::root()).await,
        Err(ikigai_core::Error::NotFound(_))
    ));
    let report = issue(&kernel, Verb::Sink, CHECK, &Capability::root())
        .await
        .unwrap();
    assert_eq!(report["targets"][0]["outcome"], "unsupported");
    value.lock().unwrap()["namespaces"][0]["target"] = json!("mailto:owner@example.org");
    assert!(matches!(
        issue(&kernel, Verb::Source, STATUS, &Capability::root()).await,
        Err(ikigai_core::Error::NotFound(_))
    ));
    issue(&kernel, Verb::Sink, CHECK, &Capability::root())
        .await
        .unwrap();
    let report = issue(&kernel, Verb::Source, STATUS, &Capability::root())
        .await
        .unwrap();
    assert_eq!(report["targets"].as_array().unwrap().len(), 1);
    assert_eq!(report["targets"][0]["target"], "mailto:owner@example.org");
}

#[tokio::test]
async fn tenants_with_the_same_prefix_have_separate_results() {
    let kernel = kernel("urn:example:unsupported");
    let job = Capability::scoped([
        ikigai_name::target_health::CAP_CHECK,
        "urn:cap:net:localhost",
        "urn:cap:name:tenant:first.example",
    ]);
    issue(&kernel, Verb::Sink, CHECK, &job).await.unwrap();
    let first = Capability::scoped([OWNER, "urn:cap:name:tenant:first.example"]);
    let second = Capability::scoped([OWNER, "urn:cap:name:tenant:second.example"]);
    assert!(issue(&kernel, Verb::Source, STATUS, &first).await.is_ok());
    assert!(matches!(
        issue(&kernel, Verb::Source, STATUS, &second).await,
        Err(ikigai_core::Error::NotFound(_))
    ));
}

/// The real host starts its timer without enabling repository access or secrets.
#[test]
fn the_standalone_timer_checks_redirects_in_each_tenants_own_registry() {
    use std::process::{Command, Stdio};
    use std::time::Instant;
    struct Host {
        child: std::process::Child,
        root: std::path::PathBuf,
    }
    impl Drop for Host {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    let server = Server::start(String::new());
    let root =
        std::env::temp_dir().join(format!("ikigai-name-target-health-{}", std::process::id()));
    for tenant in ["first.example", "second.example"] {
        std::fs::create_dir_all(root.join(tenant)).unwrap();
        let value = registry(&format!("{}/{tenant}", server.base), json!([]));
        std::fs::write(root.join(tenant).join("registry.json"), value.to_string()).unwrap();
    }
    let host = Host {
        child: Command::new(env!("CARGO_BIN_EXE_ikigai-name-server"))
            .args(["--listen", "127.0.0.1:0", "--root"])
            .arg(&root)
            .args([
                "--tenant",
                "first.example",
                "--tenant",
                "second.example",
                "--target-health-every",
                "3600",
                "--target-health-net",
                "127.0.0.1",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
        root,
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if server.requests.lock().unwrap().len() >= 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the host timer did not check both tenants"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests
        .iter()
        .any(|r| r.starts_with("GET /first.example ")));
    assert!(requests
        .iter()
        .any(|r| r.starts_with("GET /second.example ")));
    for tenant in ["first.example", "second.example"] {
        assert_eq!(
            std::fs::read_dir(host.root.join(tenant)).unwrap().count(),
            1,
            "health checks write no request data or files"
        );
    }
}

#[test]
fn the_opt_in_faces_conform_to_their_declared_contracts() {
    use ikigai_conformance::{Fixture, Suite};
    // No HTTP URLs: the conformance walk can exercise the job without traffic.
    let health = Arc::new(ikigai_name::target_health::space());
    let backing = Arc::new(
        ikigai_core::EndpointSpace::new().bind(
            Exact::new(ikigai_name::REGISTRY_IRI),
            FnEndpoint::new("registry", |_: &Invocation<'_>| {
                Ok(Representation::new(
                    ReprType::new("application/json"),
                    registry("urn:example:unsupported", json!([]))
                        .to_string()
                        .into_bytes(),
                ))
            })
            .with_description(
                ikigai_core::Description::new("registry")
                    .verb(Verb::Source)
                    .output("application/json"),
            ),
        ),
    );
    let kernel = Kernel::new(Arc::new(ikigai_core::Fallback::new(vec![
        health.clone(),
        backing,
    ])));
    futures::executor::block_on(issue(&kernel, Verb::Sink, CHECK, &Capability::root())).unwrap();
    let report = Suite::new()
        .body_space(health)
        .fixture(Fixture::new("target-health", Verb::Source).binding("prefix", "acme"))
        .fixture(Fixture::new("target-health-check", Verb::Sink).binding("prefix", "acme"))
        .run_blocking(&kernel);
    assert!(report.is_clean(), "{report}");
}
