//! `ikigai-name-server` — the standalone resolver.
//!
//! A host, in the ikigai sense: it binds concrete endpoints into a kernel and
//! puts an HTTP face on it. The library knows nothing about files, ports or
//! HTTP; all of that is decided here, which is why the same module can be
//! embedded in a larger host without dragging any of it along.
//!
//! ## The Cargo feature list is the module manifest
//!
//! This binary links `ikigai-name`, `ikigai-fs`, `ikigai-sparql`, `ikigai-web`
//! and `ikigai-ui` (for its vendored htmx) — and nothing else. There is no calendar, no exec, no secrets
//! module compiled in, so a flaw in one cannot be reached from here. That is
//! linkage-gating rather than config-gating: what this process cannot do is a
//! property of how it was built, not of a file someone might edit. The `git`
//! feature is the one addition, and it is a different build (see below).
//!
//! ## The public surface is two routes, and the page's two assets
//!
//! ```text
//! /-/style       → urn:name:style      the documentation page's stylesheet
//! /-/htmx        → urn:iki:ui:htmx     the vendored htmx it loads
//! /{ns}          → urn:name:doc:{ns}
//! /{ns}/{doc}    → urn:name:doc:{ns}/{doc}
//! ```
//!
//! The asset routes come first and begin with `-`, which no namespace can hold
//! (a prefix cannot begin with `-`), so they never shadow a document. The
//! page's term links and theme form are relative to the page itself, so they
//! reach the namespace document it was served from and need no route of their
//! own (ledger #2737).
//!
//! Served `routes_only`, so a path not listed 404s before it can reach a
//! resource. Administration is deliberately absent from the HTTP surface: claim
//! and retire are Sinks reached over a local or authenticated transport, not
//! from the open internet.
//!
//! The trailing `{path}` in `urn:name:doc:{path}` captures the remainder of the
//! IRI including slashes, which is what lets an HTTP path become a resource
//! identity rather than a query parameter — `rm:Weapon` is
//! `https://iriref.org/resmud/core#Weapon`, and the resolver has to be reachable
//! at exactly that path.
//!
//! ## Counting names, if asked to
//!
//! With `--count-names`, each name resolved is counted per namespace and month
//! ([`ikigai_name::stat`]), in `seen/` beside the registry, or in each tenant's
//! `seen/` so that a split still copies its counts. A background task holds the only
//! write scope in this process, on that directory alone, and no request holds it. The
//! counts are not routed: their reader is an administrator, and this server has no
//! authenticated transport, the same reason administration is absent.
//!
//! ## Repositories, if asked to, and if built to
//!
//! With the `git` feature and `--git`, a namespace can be served from a file in a git
//! repository ([`ikigai_name::git`]): an HTTP client, the secret store and the validator
//! are then linked, the last good copies live in `git/` beside the registry, and a
//! background timer and a signed `POST` to `/-/git/{prefix}` are the only things holding
//! a write scope, on that directory alone. A request still reads, and nothing else. A
//! binary built without the feature cannot do any of this, whatever it is told.
//!
//! ## Tenants, by `Host`
//!
//! With `--tenant HOST` (repeated), each host is a separate set of identifiers,
//! held in `--root/HOST` exactly as a single-tenant `--root` holds them: its own
//! `registry.json`, and its own documents named relative to that directory.
//! Moving a tenant to its own machine is therefore a copy of the directory and a
//! DNS change, with nothing in it rewritten.
//!
//! The `Host` header picks the tenant and nothing else does: not a query argument,
//! and not `X-Forwarded-Host`, which a client can write. A proxy in front must
//! pass `Host` through. A host that is not a tenant is refused before a route is
//! matched. The tenant is minted into the request's capability
//! ([`ikigai_name::tenant`]), which is what partitions the kernel's cache, and the
//! capability's file scope covers that tenant's directory alone.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ikigai_core::{
    ActionSpec, ArgRef, ArgSpec, AsyncFnEndpoint, Body, Capability, Description, Endpoint,
    EndpointSpace, Error, Exact, Invocation, InvokeFuture, Iri, Kernel, Representation, Request,
    Result, SystemClock, UriTemplate, Verb,
};
use ikigai_fs::FileEndpoint;
use ikigai_name::docs::{HTMX_PATH, STYLE_IRI, STYLE_PATH};
use ikigai_name::tenant::{normalize_host, tenant_of, tenant_scope};
use ikigai_name::{CAP_FLUSH, FLUSH_IRI, REGISTRY_IRI, SEEN_TEMPLATE};
use ikigai_web::{fixed_cap, AdmitFn, CapFn, EdgeConfig, HttpRequest, Refusal, Route, RouteTable};

#[cfg(feature = "git")]
#[path = "server/git.rs"]
mod git;

/// What this host was told to do. Flags and a config root only — no environment
/// variables, which are the banned third channel.
struct Options {
    listen: SocketAddr,
    root: PathBuf,
    registry: String,
    cache_files: bool,
    tenants: Vec<String>,
    count_names: bool,
    flush_every: Option<u64>,
    git: bool,
    #[cfg_attr(not(feature = "git"), allow(dead_code))]
    git_every: Option<u64>,
    #[cfg_attr(not(feature = "git"), allow(dead_code))]
    secrets: Option<PathBuf>,
    #[cfg_attr(not(feature = "git"), allow(dead_code))]
    git_forges: Vec<String>,
}

/// How often repositories are read if no webhook says to, when `--git` is on.
#[cfg(feature = "git")]
const GIT_EVERY_SECONDS: u64 = 900;

/// How often names go to disk by default, which is also the most a crash can lose.
const FLUSH_EVERY_SECONDS: u64 = 60;

fn usage() -> ! {
    eprintln!(
        "ikigai-name-server — resolve curated IRIs\n\n\
         USAGE:\n  \
           ikigai-name-server [--listen ADDR] [--root DIR] [--registry NAME] [--tenant HOST]...\n\n\
         OPTIONS:\n  \
           --listen ADDR    address to bind (default 127.0.0.1:8080)\n  \
           --root DIR       directory urn:file:* resolves within (default .)\n  \
           --registry NAME  the registry file within --root (default registry.json)\n  \
           --cache-files    cache file reads; correct only if every change to\n                   \
                            --root is followed by a restart (a deploy is; an\n                   \
                            editor is not)\n  \
           --tenant HOST    answer for HOST from the directory --root/HOST, and\n                   \
                            refuse any host not named; repeat for each host\n                   \
                            (default: one set of namespaces in --root, for any host)\n  \
           --count-names    count the distinct names each namespace resolves per\n                   \
                            month, in seen/ under --root or each tenant's directory\n  \
           --flush-every SECONDS\n                   \
                            with --count-names, how often to store them (default 60)\n  \
           --git            serve namespaces from git repositories, with their last\n                   \
                            good copies in git/ under --root or each tenant's\n                   \
                            directory (needs a build with the git feature)\n  \
           --git-every SECONDS\n                   \
                            with --git, how often to read them if no webhook says\n                   \
                            to (default 900)\n  \
           --secrets DIR    with --git, a directory holding the tokens and webhook\n                   \
                            keys, outside --root (default: the system's secret store)\n  \
           --git-forge HOST=KIND[@API]\n                   \
                            with --git, read repositories on HOST too; KIND is github,\n                   \
                            gitlab or forgejo, and API is where its API is; repeatable\n"
    );
    std::process::exit(2)
}

fn options() -> Options {
    let mut listen = "127.0.0.1:8080".to_string();
    let mut root = PathBuf::from(".");
    let mut registry = "registry.json".to_string();
    let mut cache_files = false;
    let mut tenants = Vec::new();
    let mut count_names = false;
    let mut flush_every = None;
    let mut git = false;
    let mut git_every = None;
    let mut secrets = None;
    let mut git_forges = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        // A missing value is a hard stop rather than a silent default: an
        // operator who typed `--listen` meant to set it, and quietly binding
        // somewhere else is worse than refusing to start.
        let mut value = || args.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--listen" => listen = value(),
            "--root" => root = PathBuf::from(value()),
            "--registry" => registry = value(),
            "--cache-files" => cache_files = true,
            "--tenant" => tenants.push(value()),
            "--count-names" => count_names = true,
            "--flush-every" => {
                let seconds = value();
                match seconds.parse() {
                    Ok(seconds) if seconds > 0 => flush_every = Some(seconds),
                    _ => {
                        eprintln!(
                            "ikigai-name-server: --flush-every `{seconds}` is not a whole number \
                             of seconds above zero"
                        );
                        std::process::exit(2)
                    }
                }
            }
            "--git" => git = true,
            "--git-every" => {
                let seconds = value();
                match seconds.parse() {
                    Ok(seconds) if seconds > 0 => git_every = Some(seconds),
                    _ => {
                        eprintln!(
                            "ikigai-name-server: --git-every `{seconds}` is not a whole number \
                             of seconds above zero"
                        );
                        std::process::exit(2)
                    }
                }
            }
            "--secrets" => secrets = Some(PathBuf::from(value())),
            "--git-forge" => git_forges.push(value()),
            "-h" | "--help" => usage(),
            other => {
                eprintln!("ikigai-name-server: unknown option `{other}`");
                usage()
            }
        }
    }

    let listen = listen.parse().unwrap_or_else(|e| {
        eprintln!("ikigai-name-server: `{listen}` is not an address: {e}");
        std::process::exit(2)
    });
    if flush_every.is_some() && !count_names {
        eprintln!("ikigai-name-server: --flush-every needs --count-names");
        std::process::exit(2)
    }
    if !git && (git_every.is_some() || secrets.is_some() || !git_forges.is_empty()) {
        eprintln!("ikigai-name-server: --git-every, --secrets and --git-forge need --git");
        std::process::exit(2)
    }
    if git && !cfg!(feature = "git") {
        eprintln!(
            "ikigai-name-server: --git needs a build with the git feature, and this one links \
             no HTTP client and no secret store; rebuild with --features git"
        );
        std::process::exit(2)
    }
    Options {
        listen,
        root,
        registry,
        cache_files,
        tenants,
        count_names,
        flush_every,
        git,
        git_every,
        secrets,
        git_forges,
    }
}

/// `urn:file:*` for a server with tenants: the same path means a different file
/// for each, because each tenant's files are jailed to its own directory. A
/// request naming no tenant reads nothing.
struct TenantFiles {
    jails: HashMap<String, FileEndpoint>,
}

impl TenantFiles {
    fn jail(&self, inv: &Invocation<'_>) -> Result<&FileEndpoint> {
        tenant_of(inv.capability)
            .and_then(|tenant| self.jails.get(tenant))
            .ok_or_else(|| {
                Error::Denied(format!(
                    "name: {} is read only on behalf of a tenant",
                    inv.request.target.as_str()
                ))
            })
    }
}

#[async_trait]
impl Endpoint for TenantFiles {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        self.jail(inv)?.invoke(inv).await
    }

    async fn invoke_body(&self, inv: &Invocation<'_>) -> Result<Representation<Body>> {
        self.jail(inv)?.invoke_body(inv).await
    }

    fn name(&self) -> &str {
        "tenant-files"
    }

    fn describe(&self) -> Description {
        match self.jails.values().next() {
            Some(jail) => jail.describe(),
            None => Description::new("tenant-files"),
        }
    }
}

/// Each tenant's directory, canonicalized once for its jail and its ACL (see the
/// note in `main`). A bad name, a repeat, a missing directory, or two tenants
/// sharing files stops the server rather than serving something else: tenants
/// that can read each other's files are not separate.
fn tenant_dirs(root: &std::path::Path, names: &[String]) -> BTreeMap<String, PathBuf> {
    let refuse = |why: String| -> ! {
        eprintln!("ikigai-name-server: {why}");
        std::process::exit(2)
    };
    let mut dirs: BTreeMap<String, PathBuf> = BTreeMap::new();
    for name in names {
        let host = match normalize_host(name) {
            Some(host) if host == *name => host,
            _ => refuse(format!(
                "--tenant `{name}` is not a lowercase host name with no port, like iriref.org"
            )),
        };
        if dirs.contains_key(&host) {
            refuse(format!("--tenant {host} is named twice"));
        }
        let wanted = root.join(&host);
        let dir = wanted.canonicalize().unwrap_or_else(|e| {
            refuse(format!(
                "cannot resolve the directory for tenant {host}, {}: {e}",
                wanted.display()
            ))
        });
        if let Some((other, _)) = dirs
            .iter()
            .find(|(_, held)| dir.starts_with(held) || held.starts_with(&dir))
        {
            refuse(format!(
                "tenants {other} and {host} share files; give each its own directory"
            ));
        }
        dirs.insert(host, dir);
    }
    dirs
}

/// The tenant a request's `Host` names, if it carries exactly one that parses.
/// A request with two is a request two hosts could read differently.
fn request_host(req: &HttpRequest) -> Option<String> {
    let mut hosts = req.headers.iter().filter(|(name, _)| name == "host");
    let host = hosts.next()?;
    if hosts.next().is_some() {
        return None;
    }
    normalize_host(&host.1)
}

/// Refuse a request for a host this server does not answer for, before a route
/// is matched, so `OPTIONS` and `?description` disclose nothing to it either.
fn admit_tenants(hosts: Vec<String>) -> AdmitFn {
    Arc::new(move |req| match request_host(req) {
        Some(host) if hosts.contains(&host) => None,
        Some(host) => Some(Refusal {
            status: 421,
            reason: format!("name: this server does not answer for {host:?}"),
        }),
        None => Some(Refusal {
            status: 400,
            reason: "name: this request needs exactly one Host header naming a host".to_string(),
        }),
    })
}

/// How `--git` was told to read repositories, or a refusal to start.
#[cfg(feature = "git")]
fn git_host(options: &Options, root: &std::path::Path, tenanted: bool) -> git::GitHost {
    let refuse = |why: String| -> ! {
        eprintln!("ikigai-name-server: {why}");
        std::process::exit(2)
    };
    let mut forges = ikigai_name::git::known_forges();
    for text in &options.git_forges {
        match git::parse_forge(text) {
            Some(forge) => forges.push(forge),
            None => refuse(format!(
                "--git-forge `{text}` is not HOST=KIND or HOST=KIND@API, with KIND one of \
                 github, gitlab or forgejo"
            )),
        }
    }
    let secrets: Arc<dyn ikigai_secret::Backend> = match &options.secrets {
        Some(dir) => {
            if let Err(e) = std::fs::create_dir_all(dir) {
                refuse(format!("cannot make --secrets {}: {e}", dir.display()));
            }
            let dir = dir.canonicalize().unwrap_or_else(|e| {
                refuse(format!("cannot resolve --secrets {}: {e}", dir.display()))
            });
            if let Err(why) = git::check_secrets_outside(root, &dir) {
                refuse(why);
            }
            Arc::new(ikigai_secret::FileBackend::new(dir))
        }
        None => ikigai_secret::default_backend(),
    };
    git::GitHost {
        forges,
        secrets,
        tenanted,
    }
}

/// Admit what the tenants admit, and of the webhook's path only a signed push.
#[cfg(feature = "git")]
fn admit_pushes(host: Arc<git::GitHost>, tenants: Option<AdmitFn>) -> AdmitFn {
    Arc::new(move |req| {
        tenants
            .as_ref()
            .and_then(|admit| admit(req))
            .or_else(|| host.admit(req))
    })
}

/// The capability for a request: its tenant, and reading that tenant's directory.
/// Anything else gets no authority at all, should it ever pass admission.
fn tenant_caps(ceilings: HashMap<String, Vec<String>>) -> CapFn {
    Arc::new(move |req| {
        let scopes = request_host(req).and_then(|host| ceilings.get(&host).cloned());
        Capability::scoped(scopes.unwrap_or_default())
    })
}

/// Bind `urn:name:registry-source` onto a file, by resolving it through the
/// kernel rather than reading it here.
///
/// Going through `urn:file:*` is what makes an edit take effect without a
/// restart: the read carries the file's golden thread, so writing the registry
/// cuts it and every derived answer recomputes. Reading it with `std::fs` would
/// work exactly once per process lifetime.
fn registry_alias(name: String) -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("registry-source", move |inv: &Invocation<'_>| {
        let target = format!("urn:file:{name}");
        Box::pin(async move {
            let iri = Iri::parse(&target).map_err(|e| Error::Endpoint(format!("name: {e}")))?;
            inv.source(&iri).await
        }) as InvokeFuture<'_>
    })
    .with_description(
        Description::new("registry-source")
            .title("The registry file")
            .verb(Verb::Source)
            .output("application/json"),
    )
}

/// A prefix as part of a file's name, flat though it can hold `/`. Everything but letters,
/// digits, `-` and `_` is written `~XX`, so a prefix cannot climb out or reach another's file.
fn file_part(prefix: &str) -> String {
    let mut name = String::new();
    for byte in prefix.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' {
            name.push(char::from(byte));
        } else {
            name.push_str(&format!("~{byte:02X}"));
        }
    }
    name
}

/// The file behind a key of [`SEEN_TEMPLATE`]: flat under `seen/`.
fn seen_file(key: &str) -> Option<String> {
    let (prefix, period) = match key.split_once(":period:") {
        Some((prefix, period)) => (prefix, period),
        None => (key, "periods"),
    };
    if prefix.is_empty()
        || !period
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return None;
    }
    Some(format!(
        "urn:file:{SEEN_DIR}/{}.{period}.json",
        file_part(prefix)
    ))
}

const SEEN_DIR: &str = "seen";

/// Bind the seen sets onto files, through the kernel, as the registry is.
///
/// The compare and the replace are two steps, which the store's contract allows only
/// because this process has one writer: the flush task runs one flush at a time. Two
/// processes must not count into one directory.
fn seen_files() -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("seen-source", |inv: &Invocation<'_>| -> InvokeFuture<'_> {
        Box::pin(async move {
            let key = inv
                .bindings
                .get("key")
                .ok_or_else(|| Error::MissingArgument("key".into()))?;
            let file = seen_file(key).ok_or_else(|| Error::InvalidArgument {
                name: "key".into(),
                detail: format!("{key:?} names no set of names"),
            })?;
            let iri = Iri::parse(&file).map_err(|e| Error::Endpoint(format!("name: {e}")))?;
            if inv.request.verb != Verb::Sink {
                return inv.source(&iri).await;
            }
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
        })
    })
    .with_description(
        Description::new("seen-source")
            .title("The names a namespace resolved, in files")
            .action(
                ActionSpec::new(Verb::Source)
                    .summary("read a stored set")
                    .requires("urn:cap:fs:read:*")
                    .input(ArgSpec::new("key").summary("the set").binding())
                    .output("application/json"),
            )
            .action(
                ActionSpec::new(Verb::Sink)
                    .summary("conditionally replace a stored set")
                    .requires("urn:cap:fs:write:*")
                    .input(ArgSpec::new("key").summary("the set").binding())
                    .input(ArgSpec::new("content").summary("the whole set, as JSON"))
                    .input(
                        ArgSpec::new("if-match")
                            .summary("the content ID being replaced; absent means none exists")
                            .optional(),
                    ),
            ),
    )
}

/// The capability that flushes into `dir`'s `seen/`, which the first write makes: the
/// only write scope in this process.
fn flush_capability(dir: &std::path::Path, tenant: Option<&str>) -> Capability {
    let mut scopes = vec![
        CAP_FLUSH.to_string(),
        format!("urn:cap:fs:read:{}", dir.display()),
        format!("urn:cap:fs:write:{}", dir.join(SEEN_DIR).display()),
    ];
    scopes.extend(tenant.and_then(tenant_scope));
    Capability::scoped(scopes)
}

/// Store the buffered names on a timer, for as long as the server runs. A failure is
/// logged and the names stay buffered for the next tick; no request ever waits on this.
fn spawn_flushes(kernel: Arc<Kernel>, capabilities: Vec<Capability>, every: Duration) {
    let target = Iri::parse(FLUSH_IRI).expect("FLUSH_IRI is a valid IRI");
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(every);
        loop {
            ticks.tick().await;
            for capability in &capabilities {
                let request = Request::new(Verb::Sink, target.clone());
                match kernel.issue(request, capability).await {
                    Ok(answer) => {
                        if !answer.bytes.ends_with(b"dropped: 0\n") {
                            eprint!(
                                "ikigai-name-server: {}",
                                String::from_utf8_lossy(&answer.bytes)
                            );
                        }
                    }
                    Err(e) => eprintln!("ikigai-name-server: {e}"),
                }
            }
        }
    });
}

fn routes(with_git: bool) -> RouteTable {
    let route = |pattern: &str, iri: &str| Route {
        pattern: pattern.to_string(),
        iri_template: iri.to_string(),
        redirect: None,
        cap: None,
        cors: None,
        csp: None,
    };
    // Most specific first: first match wins. The page's assets, at paths no
    // namespace can claim (see the module docs).
    let mut table = vec![
        route(STYLE_PATH, STYLE_IRI),
        route(HTMX_PATH, "urn:iki:ui:htmx"),
    ];
    // A repository's push webhook, under the same `-`. Admission has refused anything
    // here but a signed POST by the time a route is matched.
    if with_git {
        table.push(route(
            "/-/git/{ns}/{doc}",
            "urn:name:git-refresh:{ns}/{doc}",
        ));
        table.push(route("/-/git/{ns}", "urn:name:git-refresh:{ns}"));
    }
    // A name's resolution, as data, for a resolver delegating to this one. Raw, like
    // the document routes below: the variable IS a name, `:` and all.
    table.push(route(
        "/-/resolution/iri/{+iri}/{+rest}",
        "urn:name:resolution:iri:{+iri}/{+rest}",
    ));
    table.push(route(
        "/-/resolution/iri/{+iri}",
        "urn:name:resolution:iri:{+iri}",
    ));
    // Raw, so `/urn:urnid:acme:thing` reaches the document face as the IRI it is:
    // a strict variable refuses `:`. Every value is only a name the registry is asked
    // about, and `%2F` decodes to the `/` the two-segment route would have joined.
    table.push(route("/{+ns}/{+doc}", "urn:name:doc:{+ns}/{+doc}"));
    table.push(route("/{+ns}", "urn:name:doc:{+ns}"));
    RouteTable::new(table)
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let options = options();

    // ★ Canonicalise ONCE and use the result for both the jail and the ACL.
    //
    // They are two independent bounds on the same directory, and they only
    // agree if they are derived from one value: on macOS `/tmp` is a symlink to
    // `/private/tmp`, so passing the raw path to the jail and the resolved path
    // to the ACL denies every read while looking perfectly correct in the log.
    let root = options.root.canonicalize().unwrap_or_else(|e| {
        eprintln!(
            "ikigai-name-server: cannot resolve --root {}: {e}",
            options.root.display()
        );
        std::process::exit(2)
    });

    let tenants = tenant_dirs(&root, &options.tenants);

    let mut names = ikigai_name::space().bind(
        Exact::new(REGISTRY_IRI),
        registry_alias(options.registry.clone()),
    );
    if options.count_names {
        names = names.bind(
            UriTemplate::parse(SEEN_TEMPLATE).expect("SEEN_TEMPLATE is a valid template"),
            seen_files(),
        );
    }
    #[cfg(feature = "git")]
    let git_host = options
        .git
        .then(|| git_host(&options, &root, !tenants.is_empty()));
    #[cfg(feature = "git")]
    if git_host.is_some() {
        names = names
            .bind(
                Exact::new(ikigai_name::git::READ_IRI),
                ikigai_name::git::forge_reader_for(
                    git_host.as_ref().expect("just checked").forges.clone(),
                ),
            )
            .bind(
                UriTemplate::parse(ikigai_name::git::STORE_TEMPLATE)
                    .expect("STORE_TEMPLATE is a valid template"),
                git::store_files(),
            );
    }

    // Order is the resolution order: the resolver's own names, then the engine
    // it queries through, the shared UI artifacts the page loads (compiled in:
    // nothing is bound at `urn:iki:ui:dir:`), then the filesystem the resolver
    // and the engine read from.
    let mut spaces: Vec<Arc<dyn ikigai_core::Space>> = vec![
        Arc::new(names),
        Arc::new(ikigai_sparql::space()),
        Arc::new(ikigai_ui::space()),
    ];
    #[cfg(feature = "git")]
    if let Some(host) = &git_host {
        let transport = git::Transport::new().unwrap_or_else(|e| {
            eprintln!("ikigai-name-server: cannot make an HTTP client: {e}");
            std::process::exit(2)
        });
        spaces.push(Arc::new(ikigai_http::space(Arc::new(transport))));
        spaces.push(Arc::new(ikigai_secret::space(host.secrets.clone())));
        spaces.push(Arc::new(ikigai_shacl::space()));
    }
    spaces.push(
        // ★ File reads are uncacheable by DEFAULT, and that default is right.
        //
        // A golden thread is cut by a write THROUGH the kernel; a `git pull` or
        // an editor changes the bytes without one, so a cached entry would go
        // stale and stay stale. `--cache-files` opts in for the one deployment
        // shape where that cannot bite: a checkout that only changes as part of
        // a deploy, which restarts the process anyway. Without it every
        // documentation view re-reads the file and re-runs its SPARQL query —
        // correct, and considerably slower.
        Arc::new(if !tenants.is_empty() {
            let jails = tenants.iter().map(|(host, dir)| {
                let jail = FileEndpoint::new(dir);
                let jail = if options.cache_files {
                    jail.cacheable()
                } else {
                    jail
                };
                (host.clone(), jail)
            });
            EndpointSpace::new().bind(
                UriTemplate::parse(ikigai_fs::FILE_TEMPLATE)
                    .expect("FILE_TEMPLATE is a valid template"),
                TenantFiles {
                    jails: jails.collect(),
                },
            )
        } else if options.cache_files {
            ikigai_fs::cacheable_space(&root)
        } else {
            ikigai_fs::space(&root)
        }),
    );
    let space = ikigai_core::Fallback::new(spaces);
    // A clock only when counting: it is how a name is put in its month, and with none
    // the module counts nothing, so a server not asked to count is exactly as it was.
    let kernel = Kernel::new(Arc::new(space));
    let kernel = Arc::new(if options.count_names {
        kernel.with_clock(Arc::new(SystemClock))
    } else {
        kernel
    });

    let mut config = EdgeConfig {
        // A path not in the table 404s before it reaches a resource, so the
        // route file is the whole public surface rather than a convenience over
        // one.
        routes_only: true,
        routes: routes(options.git),
        ..EdgeConfig::default()
    };

    // ★ Read, and only read, and only inside the root.
    //
    // A request arrives public, but resolving a namespace has to reach a file,
    // and a capability attenuates DOWN a call chain rather than growing — so
    // the ceiling has to be granted here. Two independent bounds keep that
    // honest: ikigai-fs jails every path under `root` whatever the capability
    // says, and this ACL grants read beneath that same root and nothing else.
    //
    // There is deliberately no WRITE scope on any request. `urn:name:claim` and
    // `urn:name:admin` are Sinks, so this process physically cannot perform
    // them however it is addressed — administration belongs on a local or
    // authenticated transport, not on the open internet. The one write scope
    // this process can hold is `--count-names`' flush task's, on `seen/` alone.
    //
    // With tenants, the same two bounds hold per request: each is jailed to its
    // own directory, and its capability reads that directory alone.
    let jailed = if tenants.is_empty() {
        root.display().to_string()
    } else {
        let hosts = tenants.keys().cloned().collect::<Vec<_>>();
        format!("{}/ for tenants {}", root.display(), hosts.join(", "))
    };
    let cap_fn = if tenants.is_empty() {
        fixed_cap(vec![format!("urn:cap:fs:read:{}", root.display())])
    } else {
        let ceilings = tenants.iter().map(|(host, dir)| {
            let scope = tenant_scope(host).expect("a normalized tenant is a host name");
            let read = format!("urn:cap:fs:read:{}", dir.display());
            (host.clone(), vec![scope, read])
        });
        config.admit_fn = Some(admit_tenants(tenants.keys().cloned().collect()));
        tenant_caps(ceilings.collect())
    };

    #[cfg(feature = "git")]
    let (cap_fn, repositories) = match git_host {
        None => (cap_fn, String::new()),
        Some(host) => {
            let host = Arc::new(host);
            config.admit_fn = Some(admit_pushes(host.clone(), config.admit_fn.take()));
            let sites = if tenants.is_empty() {
                vec![git::Site {
                    dir: root.clone(),
                    tenant: None,
                }]
            } else {
                tenants
                    .iter()
                    .map(|(tenant, dir)| git::Site {
                        dir: dir.clone(),
                        tenant: Some(tenant.clone()),
                    })
                    .collect()
            };
            let every = options.git_every.unwrap_or(GIT_EVERY_SECONDS);
            git::spawn_refreshes(
                Arc::clone(&kernel),
                host.clone(),
                sites,
                Duration::from_secs(every),
            );
            let dirs = tenants.clone();
            let root = root.clone();
            let ordinary = cap_fn;
            let with_pushes: CapFn = Arc::new(move |req| {
                let dir = if host.tenanted {
                    request_host(req).and_then(|tenant| dirs.get(&tenant))
                } else {
                    Some(&root)
                };
                dir.and_then(|dir| host.push_capability(req, dir))
                    .unwrap_or_else(|| ordinary(req))
            });
            (
                with_pushes,
                format!(", reading repositories every {every}s"),
            )
        }
    };
    #[cfg(not(feature = "git"))]
    let repositories = String::new();

    let access = match (options.count_names, options.git) {
        (true, true) => "read-only but for seen/ and git/",
        (true, false) => "read-only but for seen/",
        (false, true) => "read-only but for git/",
        (false, false) => "read-only",
    };
    let counting = if options.count_names {
        let every = options.flush_every.unwrap_or(FLUSH_EVERY_SECONDS);
        let capabilities = if tenants.is_empty() {
            vec![flush_capability(&root, None)]
        } else {
            tenants
                .iter()
                .map(|(host, dir)| flush_capability(dir, Some(host)))
                .collect()
        };
        spawn_flushes(
            Arc::clone(&kernel),
            capabilities,
            Duration::from_secs(every),
        );
        format!(", counting names every {every}s")
    } else {
        String::new()
    };

    eprintln!(
        "ikigai-name-server: {} → {} ({access}, jailed to {}, file cache {}{counting}{repositories})",
        options.listen,
        REGISTRY_IRI,
        jailed,
        if options.cache_files { "on" } else { "off" }
    );
    ikigai_web::serve_with(kernel, cap_fn, options.listen, config).await
}
