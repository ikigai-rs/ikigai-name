//! `--history KIND`: where this server reads a resource's past from.
//!
//! Every past read goes through `urn:name:history-source`, and this file only decides
//! what is bound there. The option names a history SOURCE KIND, so a later kind (a
//! database-backed store, say) is a new value of the option with its own binding at the
//! same IRI, and resolution does not change.
//!
//! ## `git`
//!
//! [`ikigai_name::repo_history`] over the vocabulary's own directory (`--root`, or each
//! tenant's), composed from `urn:repo:log` and `urn:repo:show` from `ikigai-repo`. They
//! are bound in a kernel of their own, which nothing else reaches: a request's
//! capability never holds an exec scope, and the history kernel is issued with exactly
//! `urn:cap:exec:read:git` (and the tenant, for its key), so no other exec authority
//! exists anywhere in this process.
//!
//! - **A fixed binary.** `ikigai-repo` 0.6.0 runs `git` by name, so [`fix_git`] sets
//!   this process's `PATH` to [`GIT_DIR`] alone before any thread starts: the only
//!   `git` it can find is `/usr/bin/git`. The server spawns nothing else.
//! - **A timeout.** A read that takes longer than [`TIMEOUT`] is refused, and the
//!   request answers rather than waits.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ikigai_core::{
    Capability, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Kernel,
    Representation, Request, Result,
};
use ikigai_name::tenant::{tenant_of, tenant_scope};
use ikigai_name::{repo_history, HISTORY_IRI};

/// The only directory `git` is looked up in.
pub const GIT_DIR: &str = "/usr/bin";

/// The longest one as-of read may take, log and file together.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// The exec scope the history kernel holds, and the only one in this process.
const EXEC_SCOPE: &str = "urn:cap:exec:read:git";

/// Make `git` mean [`GIT_DIR`]`/git`, and nothing else. Called before the runtime starts,
/// while this process has one thread, which is what makes changing its environment sound.
pub fn fix_git() {
    std::env::set_var("PATH", GIT_DIR);
}

/// `urn:name:history-source` for every site: one repository each, keyed by tenant.
pub struct History {
    sites: HashMap<Option<String>, Site>,
}

struct Site {
    kernel: Arc<Kernel>,
    capability: Capability,
}

impl History {
    /// The git binding over each site's directory, with `registry` as the registry's file.
    pub fn git(sites: &[(Option<String>, PathBuf)], registry: &str) -> History {
        let sites = sites
            .iter()
            .map(|(tenant, dir)| (tenant.clone(), git_site(dir, tenant.as_deref(), registry)))
            .collect();
        History { sites }
    }
}

fn git_site(dir: &Path, tenant: Option<&str>, registry: &str) -> Site {
    // `repo_history` issues `urn:repo:log` and `urn:repo:show` and nothing else, and
    // this kernel holds nothing but the two, so no other facade is reachable here.
    let history = EndpointSpace::new().bind(
        Exact::new(HISTORY_IRI),
        repo_history(dir.display().to_string(), registry),
    );
    let repo = ikigai_repo::space_with_roots([dir]);
    let mut scopes = vec![EXEC_SCOPE.to_string()];
    scopes.extend(tenant.and_then(tenant_scope));
    Site {
        kernel: Arc::new(Kernel::new(Arc::new(ikigai_core::Fallback::new(vec![
            Arc::new(history),
            Arc::new(repo),
        ])))),
        capability: Capability::scoped(scopes),
    }
}

#[async_trait]
impl Endpoint for History {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let tenant = tenant_of(inv.capability).map(str::to_string);
        let site = self.sites.get(&tenant).ok_or_else(|| {
            Error::Denied(format!(
                "name: {HISTORY_IRI} is read only on behalf of a tenant this server answers for"
            ))
        })?;
        let mut request = Request::new(inv.request.verb, inv.request.target.clone());
        request.args = inv.request.args.clone();
        let kernel = Arc::clone(&site.kernel);
        let capability = site.capability.clone();
        // ikigai-repo waits on git synchronously, so the read runs on a blocking thread
        // and the request waits on it only so long.
        let runtime = tokio::runtime::Handle::current();
        let read = tokio::task::spawn_blocking(move || {
            runtime.block_on(kernel.issue(request, &capability))
        });
        match tokio::time::timeout(TIMEOUT, read).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(e)) => Err(Error::Endpoint(format!(
                "name: the history read failed: {e}"
            ))),
            Err(_) => Err(Error::Endpoint(format!(
                "name: git took longer than {} seconds to read the history, so this server \
                 cannot answer as of that date now; try again, or leave out as-of for the \
                 current answer",
                TIMEOUT.as_secs()
            ))),
        }
    }

    fn name(&self) -> &str {
        "history-source"
    }

    fn describe(&self) -> Description {
        Description::new("history-source").title("A resource as it stood at a date")
    }
}
