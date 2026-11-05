//! Serving namespaces from git repositories, with `--git`.
//!
//! Compiled only with the `git` feature, which is what links an HTTP client, the
//! secret store and the validator into this binary. Without it the server cannot read
//! a repository or a secret, however it is configured.
//!
//! A request still holds read authority at most. What writes is a refresh, and a
//! refresh runs in two places: on a timer, which holds the authority from the start,
//! and for a `POST` to `/-/git/{prefix}` whose signature verifies against that
//! namespace's key, which is the only request that is ever given it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::TryStreamExt;
use ikigai_core::{
    ActionSpec, ArgRef, ArgSpec, AsyncFnEndpoint, Capability, Description, Error, Invocation,
    InvokeFuture, Iri, Kernel, Request, Verb,
};
use ikigai_http::{HttpBody, HttpRequest as OutboundRequest, HttpResponse, HttpTransport};
use ikigai_name::git::{hook_secret, token_secret, verify_push, Forge, ForgeKind, CAP_REFRESH};
use ikigai_name::tenant::tenant_scope;
use ikigai_name::Registry;
use ikigai_secret::Backend;
use ikigai_web::{HttpRequest, Refusal};

use super::{file_part, request_host};

/// The directory under `--root`, or each tenant's directory, holding the last good copies.
pub const GIT_DIR: &str = "git";

/// The path a push webhook is sent to, with the prefix after it.
pub const HOOK_PATH: &str = "/-/git/";

/// How long an outbound read may take, whatever a forge is doing.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// How the server was told to read repositories.
pub struct GitHost {
    pub forges: Vec<Forge>,
    pub secrets: Arc<dyn Backend>,
    pub tenanted: bool,
}

impl GitHost {
    /// The prefix a signed push is for, if this request is one that verifies. A key
    /// is one namespace's, so it cannot be used to refresh another's.
    fn verified_prefix(&self, req: &HttpRequest) -> Option<String> {
        let prefix = req.path.strip_prefix(HOOK_PATH)?;
        if req.method != "POST" {
            return None;
        }
        let signature = req
            .headers
            .iter()
            .find(|(name, _)| name == "x-hub-signature-256")?;
        let tenant = self.tenant(req);
        let key = self
            .secrets
            .get(&hook_secret(tenant.as_deref(), prefix))
            .ok()??;
        verify_push(&key, &req.body, &signature.1).then(|| prefix.to_string())
    }

    fn tenant(&self, req: &HttpRequest) -> Option<String> {
        if self.tenanted {
            request_host(req)
        } else {
            None
        }
    }

    /// Refuse a request for the webhook's path unless it is a signed push. The sentence
    /// is the same whether or not the namespace exists or has a key, which is not for a
    /// stranger to learn.
    pub fn admit(&self, req: &HttpRequest) -> Option<Refusal> {
        if !req.path.starts_with(HOOK_PATH) {
            return None;
        }
        if req.method != "POST" {
            return Some(Refusal {
                status: 403,
                reason: "name: this path takes only a signed POST from a repository's webhook"
                    .to_string(),
            });
        }
        if self.verified_prefix(req).is_some() {
            return None;
        }
        Some(Refusal {
            status: 403,
            reason: "name: this push has no signature that verifies for that namespace; its \
                     X-Hub-Signature-256 must be made with the namespace's webhook key"
                .to_string(),
        })
    }

    /// The authority of a verified push, or `None` for every other request.
    pub fn push_capability(&self, req: &HttpRequest, dir: &Path) -> Option<Capability> {
        let prefix = self.verified_prefix(req)?;
        let tenant = self.tenant(req);
        Some(self.refresh_capability(dir, tenant.as_deref(), &prefix))
    }

    /// What a refresh of one namespace holds: its own token and no other, reading the
    /// directory, writing `git/`, and the network to the forges and no further.
    pub fn refresh_capability(&self, dir: &Path, tenant: Option<&str>, prefix: &str) -> Capability {
        let mut scopes = vec![
            CAP_REFRESH.to_string(),
            format!("urn:cap:fs:read:{}", dir.display()),
            format!("urn:cap:fs:write:{}", dir.join(GIT_DIR).display()),
            format!("urn:cap:secret:read:{}", token_secret(tenant, prefix)),
        ];
        scopes.extend(tenant.and_then(tenant_scope));
        scopes.extend(self.forges.iter().filter_map(net_scope));
        Capability::scoped(scopes)
    }
}

/// `urn:cap:net:<host>` for a forge's API.
fn net_scope(forge: &Forge) -> Option<String> {
    let (_, rest) = forge.api.split_once("://")?;
    let host = rest.split('/').next()?;
    let host = host.split(':').next()?;
    Some(format!("urn:cap:net:{host}"))
}

/// `--git-forge HOST=KIND` or `HOST=KIND@API`: a host beyond the public ones the
/// reader knows. `KIND` is `github`, `gitlab` or `forgejo`, and without an `API` it is
/// where that kind's API is on that host.
pub fn parse_forge(text: &str) -> Option<Forge> {
    let (host, rest) = text.split_once('=')?;
    let (kind, api) = match rest.split_once('@') {
        Some((kind, api)) => (kind, Some(api.trim_end_matches('/').to_string())),
        None => (rest, None),
    };
    let (kind, default_api) = match kind {
        "github" => (ForgeKind::GitHub, format!("https://{host}/api/v3")),
        "gitlab" => (ForgeKind::GitLab, format!("https://{host}/api/v4")),
        "forgejo" => (ForgeKind::Forgejo, format!("https://{host}/api/v1")),
        _ => return None,
    };
    if host.is_empty() || host.contains(['/', ' ']) {
        return None;
    }
    Some(Forge {
        host: host.to_string(),
        kind,
        api: api.unwrap_or(default_api),
    })
}

/// The file behind a key of the store: `document:acme/core` is
/// `git/document.acme~2Fcore.ttl`, and a status is JSON beside it.
fn store_file(key: &str) -> Option<String> {
    let (kind, prefix) = key.split_once(':')?;
    let extension = match kind {
        "document" => "ttl",
        "status" => "json",
        _ => return None,
    };
    if prefix.is_empty() {
        return None;
    }
    Some(format!(
        "urn:file:{GIT_DIR}/{kind}.{}.{extension}",
        file_part(prefix)
    ))
}

/// Bind the last good copies onto files, through the kernel, as the registry is.
///
/// A webhook and the timer can refresh at once, so the compare and the replace happen
/// under one lock here, which is what keeps them one operation within this process. Two
/// processes must not serve from one directory.
pub fn store_files() -> AsyncFnEndpoint {
    let writing = Arc::new(tokio::sync::Mutex::new(()));
    AsyncFnEndpoint::new("git-store", move |inv: &Invocation<'_>| {
        let writing = writing.clone();
        Box::pin(async move {
            let key = inv
                .bindings
                .get("key")
                .ok_or_else(|| Error::MissingArgument("key".into()))?;
            let file = store_file(key).ok_or_else(|| Error::InvalidArgument {
                name: "key".into(),
                detail: format!("{key:?} names no copy of a repository's file"),
            })?;
            let iri = Iri::parse(&file).map_err(|e| Error::Endpoint(format!("name: {e}")))?;
            if inv.request.verb != Verb::Sink {
                return inv.source(&iri).await;
            }
            let _one_writer = writing.lock().await;
            let current = match inv.source(&iri).await {
                Ok(current) => Some(current.content_id().to_string()),
                Err(Error::NotFound(_)) => None,
                Err(e) => return Err(e),
            };
            let expected = inv.inline_str("if-match").ok().map(str::to_string);
            if current != expected {
                return Err(Error::Conflict(format!(
                    "name: {file} changed since it was read; read it again"
                )));
            }
            let content = inv.inline_arg("content")?.to_vec();
            inv.issue(Request::new(Verb::Sink, iri).with_arg("content", ArgRef::Inline(content)))
                .await
        }) as InvokeFuture<'_>
    })
    .with_description(
        Description::new("git-store")
            .title("The last good copies of repositories' files")
            .action(
                ActionSpec::new(Verb::Source)
                    .summary("read a stored copy")
                    .requires("urn:cap:fs:read:*")
                    .input(ArgSpec::new("key").summary("the copy").binding()),
            )
            .action(
                ActionSpec::new(Verb::Sink)
                    .summary("conditionally replace a stored copy")
                    .requires("urn:cap:fs:write:*")
                    .input(ArgSpec::new("key").summary("the copy").binding())
                    .input(ArgSpec::new("content").summary("the whole copy"))
                    .input(
                        ArgSpec::new("if-match")
                            .summary("the content ID being replaced; absent means none exists")
                            .optional(),
                    ),
            ),
    )
}

/// A directory the timer refreshes: a tenant's, or `--root`'s.
pub struct Site {
    pub dir: PathBuf,
    pub tenant: Option<String>,
}

/// Read every site's repositories on a timer, for as long as the server runs, starting
/// now: this is what fills the store, and what catches a webhook that never arrived. A
/// line reaches the log when a namespace's answer changes, and not on every tick.
pub fn spawn_refreshes(kernel: Arc<Kernel>, host: Arc<GitHost>, sites: Vec<Site>, every: Duration) {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(every);
        let mut last: HashMap<(Option<String>, String), String> = HashMap::new();
        loop {
            ticks.tick().await;
            for site in &sites {
                refresh_site(&kernel, &host, site, &mut last).await;
            }
        }
    });
}

async fn refresh_site(
    kernel: &Kernel,
    host: &GitHost,
    site: &Site,
    last: &mut HashMap<(Option<String>, String), String>,
) {
    let tenant = site.tenant.as_deref();
    let mut reading = vec![format!("urn:cap:fs:read:{}", site.dir.display())];
    reading.extend(tenant.and_then(tenant_scope));
    let registry = Iri::parse("urn:name:registry").expect("a valid IRI");
    let listed = kernel
        .issue(
            Request::new(Verb::Source, registry),
            &Capability::scoped(reading),
        )
        .await
        .and_then(|repr| {
            Registry::from_json(&repr.bytes).map_err(|e| Error::Endpoint(e.to_string()))
        });
    let registry = match listed {
        Ok(registry) => registry,
        Err(e) => {
            eprintln!("ikigai-name-server: {e}");
            return;
        }
    };
    for namespace in registry.namespaces {
        if namespace.repository.is_none() {
            continue;
        }
        let prefix = namespace.prefix;
        let target = format!("urn:name:git-refresh:{prefix}");
        let Ok(target) = Iri::parse(&target) else {
            continue;
        };
        let capability = host.refresh_capability(&site.dir, tenant, &prefix);
        let line = match kernel
            .issue(Request::new(Verb::Sink, target), &capability)
            .await
        {
            Ok(answer) => String::from_utf8_lossy(&answer.bytes)
                .trim_end()
                .to_string(),
            Err(e) => format!("{prefix:?}: {e}"),
        };
        let quiet = line.contains(": unchanged, serving ");
        let key = (site.tenant.clone(), prefix);
        let new = last.get(&key) != Some(&line);
        if new && !quiet {
            eprintln!("ikigai-name-server: {line}");
        }
        last.insert(key, line);
    }
}

/// HTTPS for `urn:http:*`. It follows no redirect, as that contract requires, because only
/// the endpoint can check a redirect's target against the capability.
pub struct Transport {
    client: reqwest::Client,
}

impl Transport {
    pub fn new() -> std::result::Result<Transport, String> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(READ_TIMEOUT)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Transport { client })
    }
}

#[async_trait]
impl HttpTransport for Transport {
    async fn send(&self, request: OutboundRequest) -> std::result::Result<HttpResponse, String> {
        let method = reqwest::Method::from_bytes(request.method.as_str().as_bytes())
            .map_err(|e| e.to_string())?;
        let mut outbound = self.client.request(method, &request.url);
        for (name, value) in &request.headers {
            outbound = outbound.header(name, value);
        }
        if !request.body.is_empty() {
            outbound = outbound.body(request.body);
        }
        // Without its URL, which is not for a log: it can be a tenant's private path.
        let response = outbound
            .send()
            .await
            .map_err(|e| e.without_url().to_string())?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                Some((name.as_str().to_string(), value.to_str().ok()?.to_string()))
            })
            .collect();
        let reader = response
            .bytes_stream()
            .map_err(std::io::Error::other)
            .into_async_read();
        Ok(HttpResponse {
            status,
            headers,
            body: HttpBody::from_reader(reader),
        })
    }
}

/// A short error for what `--git` was told, or `Ok`.
pub fn check_secrets_outside(root: &Path, secrets: &Path) -> std::result::Result<(), String> {
    if secrets.starts_with(root) {
        return Err(format!(
            "--secrets {} is inside --root, which this server serves files from; move it out",
            secrets.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forge_is_a_host_a_kind_and_where_its_api_is() {
        let forge = parse_forge("git.acme.example=gitlab").unwrap();
        assert_eq!(forge.kind, ForgeKind::GitLab);
        assert_eq!(forge.api, "https://git.acme.example/api/v4");
        let forge = parse_forge("git.acme.example=github@https://api.acme.example/").unwrap();
        assert_eq!(forge.kind, ForgeKind::GitHub);
        assert_eq!(forge.api, "https://api.acme.example");
        for bad in ["", "a", "a=", "=github", "a=svn", "a/b=github"] {
            assert_eq!(parse_forge(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_stored_key_stays_flat_under_the_directory() {
        assert_eq!(
            store_file("document:acme/core").as_deref(),
            Some("urn:file:git/document.acme~2Fcore.ttl")
        );
        assert_eq!(
            store_file("status:../x").as_deref(),
            Some("urn:file:git/status.~2E~2E~2Fx.json")
        );
        for bad in ["", "document", "document:", "other:x"] {
            assert_eq!(store_file(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_net_scope_is_the_api_s_host() {
        let forge = parse_forge("f.example=github@http://127.0.0.1:9/x").unwrap();
        assert_eq!(net_scope(&forge).as_deref(), Some("urn:cap:net:127.0.0.1"));
    }

    #[test]
    fn secrets_under_the_root_are_refused() {
        assert!(check_secrets_outside(Path::new("/srv"), Path::new("/srv/secrets")).is_err());
        assert!(check_secrets_outside(Path::new("/srv"), Path::new("/etc/ikigai")).is_ok());
    }
}
