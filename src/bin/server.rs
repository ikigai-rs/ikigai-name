//! `ikigai-name-server` — the standalone resolver.
//!
//! A host, in the ikigai sense: it binds concrete endpoints into a kernel and
//! puts an HTTP face on it. The library knows nothing about files, ports or
//! HTTP; all of that is decided here, which is why the same module can be
//! embedded in a larger host without dragging any of it along.
//!
//! ## The Cargo feature list is the module manifest
//!
//! This binary links `ikigai-name`, `ikigai-fs`, `ikigai-sparql` and
//! `ikigai-web` — and nothing else. There is no calendar, no exec, no secrets
//! module compiled in, so a flaw in one cannot be reached from here. That is
//! linkage-gating rather than config-gating: what this process cannot do is a
//! property of how it was built, not of a file someone might edit.
//!
//! ## The public surface is two routes
//!
//! ```text
//! /{ns}          → urn:name:doc:{ns}
//! /{ns}/{doc}    → urn:name:doc:{ns}/{doc}
//! ```
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

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use ikigai_core::{
    AsyncFnEndpoint, Description, Error, Exact, Invocation, InvokeFuture, Iri, Kernel, UriTemplate,
    Verb,
};
use ikigai_name::REGISTRY_IRI;
use ikigai_web::{fixed_cap, EdgeConfig, Route, RouteTable};

/// The IRI whose trailing variable carries the curated path.
const DOC_TEMPLATE: &str = "urn:name:doc:{path}";

/// What this host was told to do. Flags and a config root only — no environment
/// variables, which are the banned third channel.
struct Options {
    listen: SocketAddr,
    root: PathBuf,
    registry: String,
    cache_files: bool,
}

fn usage() -> ! {
    eprintln!(
        "ikigai-name-server — resolve curated IRIs\n\n\
         USAGE:\n  \
           ikigai-name-server [--listen ADDR] [--root DIR] [--registry NAME]\n\n\
         OPTIONS:\n  \
           --listen ADDR    address to bind (default 127.0.0.1:8080)\n  \
           --root DIR       directory urn:file:* resolves within (default .)\n  \
           --registry NAME  the registry file within --root (default registry.json)\n  \
           --cache-files    cache file reads; correct only if every change to\n                   \
                            --root is followed by a restart (a deploy is; an\n                   \
                            editor is not)\n"
    );
    std::process::exit(2)
}

fn options() -> Options {
    let mut listen = "127.0.0.1:8080".to_string();
    let mut root = PathBuf::from(".");
    let mut registry = "registry.json".to_string();
    let mut cache_files = false;

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
    Options {
        listen,
        root,
        registry,
        cache_files,
    }
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

fn routes() -> RouteTable {
    let route = |pattern: &str, iri: &str| Route {
        pattern: pattern.to_string(),
        iri_template: iri.to_string(),
        cap: None,
        cors: None,
        csp: None,
    };
    RouteTable::new(vec![
        // Most specific first: first match wins.
        route("/{ns}/{doc}", "urn:name:doc:{ns}/{doc}"),
        route("/{ns}", "urn:name:doc:{ns}"),
    ])
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

    let names = ikigai_name::space()
        // The path-carrying face, bound alongside the plain `urn:name:document`
        // so both spellings resolve: an IRI whose tail IS the path, and the
        // argument form the REPL and tests use.
        .bind(
            UriTemplate::parse(DOC_TEMPLATE).expect("DOC_TEMPLATE is a valid template"),
            ikigai_name::document(),
        )
        .bind(Exact::new(REGISTRY_IRI), registry_alias(options.registry));

    // Order is the resolution order: the resolver's own names, then the engine
    // it queries through, then the filesystem both of them read from.
    let space = ikigai_core::Fallback::new(vec![
        Arc::new(names),
        Arc::new(ikigai_sparql::space()),
        // ★ File reads are uncacheable by DEFAULT, and that default is right.
        //
        // A golden thread is cut by a write THROUGH the kernel; a `git pull` or
        // an editor changes the bytes without one, so a cached entry would go
        // stale and stay stale. `--cache-files` opts in for the one deployment
        // shape where that cannot bite: a checkout that only changes as part of
        // a deploy, which restarts the process anyway. Without it every
        // documentation view re-reads the file and re-runs its SPARQL query —
        // correct, and considerably slower.
        Arc::new(if options.cache_files {
            ikigai_fs::cacheable_space(&root)
        } else {
            ikigai_fs::space(&root)
        }),
    ]);
    let kernel = Arc::new(Kernel::new(Arc::new(space)));

    let config = EdgeConfig {
        // A path not in the table 404s before it reaches a resource, so the
        // route file is the whole public surface rather than a convenience over
        // one.
        routes_only: true,
        routes: routes(),
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
    // There is deliberately no WRITE scope. `urn:name:claim` and
    // `urn:name:admin` are Sinks, so this process physically cannot perform
    // them however it is addressed — administration belongs on a local or
    // authenticated transport, not on the open internet.
    let ceiling = vec![format!("urn:cap:fs:read:{}", root.display())];

    eprintln!(
        "ikigai-name-server: {} → {} (read-only, jailed to {}, file cache {})",
        options.listen,
        REGISTRY_IRI,
        root.display(),
        if options.cache_files { "on" } else { "off" }
    );
    ikigai_web::serve_with(kernel, fixed_cap(ceiling), options.listen, config).await
}
