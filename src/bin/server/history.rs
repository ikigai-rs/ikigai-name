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
//! - **A cap on reads in flight.** The timeout bounds the answer, not the work: a git
//!   still running at the timeout is not killed (`ikigai-repo` 0.6.0 waits on it
//!   synchronously), so its blocking thread and its child live on. At most
//!   [`IN_FLIGHT`] reads hold a thread at once, counted until the thread returns, not
//!   until the request is answered, and one past that is refused at once with
//!   `Retry-After` rather than queued. As-of questions are rare and public, so a few
//!   are plenty for real use, and a flood of them costs at most [`IN_FLIGHT`] threads
//!   and git processes instead of the runtime's 512 blocking threads.
//! - **The top of a repository.** `ikigai-repo` stops git's search for a repository at
//!   the directory's parent, and `repo_history` names files from the repository's top,
//!   so each site directory must be the top of its own repository: [`not_a_repository`]
//!   refuses to start otherwise.
//!
//! ## Windows
//!
//! [`fix_git`]'s fixed path is a Unix one, so `--history git` is refused at startup on
//! any other host ([`not_unix`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;

use async_trait::async_trait;
use ikigai_core::{
    ArgSpec, Capability, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Kernel,
    Representation, Request, Result, Verb,
};
use ikigai_name::tenant::{tenant_of, tenant_scope};
use ikigai_name::{repo_history, HISTORY_IRI};

/// The only directory `git` is looked up in.
pub const GIT_DIR: &str = "/usr/bin";

/// The longest one as-of read may take, log and file together.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// The most history reads that hold a blocking thread (and so a git) at once. Each read
/// is one `git log` then one `git show`, one after the other, so this is also the most
/// git processes this server runs at once.
pub const IN_FLIGHT: usize = 4;

/// How long a read refused for [`IN_FLIGHT`] is told to wait: the [`TIMEOUT`], by which
/// every read answered so far has either finished or been given up on.
pub const RETRY: Duration = TIMEOUT;

/// The exec scope the history kernel holds, and the only one in this process.
const EXEC_SCOPE: &str = "urn:cap:exec:read:git";

/// Make `git` mean [`GIT_DIR`]`/git`, and nothing else. Called before the runtime starts,
/// while this process has one thread, which is what makes changing its environment sound.
pub fn fix_git() {
    std::env::set_var("PATH", GIT_DIR);
}

/// The startup refusal for a site directory that is not the top of a git repository, or
/// `None` if it is. A `.git` file (a worktree's) counts, as git counts it.
pub fn not_a_repository(dir: &Path) -> Option<String> {
    (!dir.join(".git").exists()).then(|| {
        format!(
            "--history git reads the git repository whose top is each site's directory, and \
             {} holds no .git; run `git init` there and commit the vocabulary, or point \
             --root (or the tenant's directory) at the top of a repository",
            dir.display()
        )
    })
}

/// The startup refusal on a host where [`GIT_DIR`] is not where git lives, or `None`.
pub fn not_unix() -> Option<String> {
    (!cfg!(unix)).then(|| {
        format!(
            "--history git runs git only from {GIT_DIR}, which only a Unix host has; run \
             this server on Linux or another Unix, or leave out --history"
        )
    })
}

/// The refusal of a read past [`IN_FLIGHT`].
fn busy() -> Error {
    Error::RetryAfter {
        delay: Some(RETRY),
        detail: format!(
            "name: this server is already reading {IN_FLIGHT} answers from history, the most \
             it reads at once; try again in {} seconds, or leave out as-of for the current \
             answer",
            RETRY.as_secs()
        ),
    }
}

/// The refusal of a read past `timeout`.
fn too_slow(timeout: Duration) -> Error {
    Error::Timeout(format!(
        "name: git took longer than {} seconds to read the history, so this server cannot \
         answer as of that date now; try again, or leave out as-of for the current answer",
        timeout.as_secs()
    ))
}

/// Run `read` on a blocking thread holding one of `gate`'s permits until it returns, and
/// answer within `timeout`. No permit left is [`busy`], at once.
async fn bounded<F>(gate: &Arc<Semaphore>, timeout: Duration, read: F) -> Result<Representation>
where
    F: FnOnce() -> Result<Representation> + Send + 'static,
{
    let permit = Arc::clone(gate).try_acquire_owned().map_err(|_| busy())?;
    let read = tokio::task::spawn_blocking(move || {
        let _held = permit;
        read()
    });
    match tokio::time::timeout(timeout, read).await {
        Ok(Ok(answer)) => answer,
        Ok(Err(e)) => Err(Error::Endpoint(format!(
            "name: the history read failed: {e}"
        ))),
        Err(_) => Err(too_slow(timeout)),
    }
}

/// `urn:name:history-source` for every site: one repository each, keyed by tenant.
pub struct History {
    sites: HashMap<Option<String>, Site>,
    /// Shared by every site, since the threads and processes it bounds are this
    /// process's, whichever tenant asked.
    gate: Arc<Semaphore>,
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
        History {
            sites,
            gate: Arc::new(Semaphore::new(IN_FLIGHT)),
        }
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
        bounded(&self.gate, TIMEOUT, move || {
            runtime.block_on(kernel.issue(request, &capability))
        })
        .await
    }

    fn name(&self) -> &str {
        "history-source"
    }

    fn describe(&self) -> Description {
        Description::new("history-source")
            .title("A resource as it stood at a date")
            .summary(
                "The site's resource as it stood at as-of, read from the vocabulary's git \
                 repository, or NotFound if it did not exist then.",
            )
            .verb(Verb::Source)
            .input(
                ArgSpec::new("source")
                    .summary("the resource's IRI, as the registry names it")
                    .class(XSD_STRING),
            )
            .input(
                ArgSpec::new("as-of")
                    .summary("the date or RFC 3339 instant to read it at")
                    .class(XSD_STRING),
            )
    }
}

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

#[cfg(test)]
mod tests {
    use super::*;
    use ikigai_core::ReprType;
    use std::sync::mpsc;

    fn answer() -> Result<Representation> {
        Ok(Representation::new(
            ReprType::new("text/turtle"),
            Vec::new(),
        ))
    }

    /// Past [`IN_FLIGHT`] reads, the next is refused at once with `Retry-After`, and a
    /// permit comes back only when its thread returns, not when its request is answered.
    #[tokio::test]
    async fn a_read_past_the_cap_is_refused_with_retry_after() {
        let gate = Arc::new(Semaphore::new(IN_FLIGHT));
        let mut release = Vec::new();
        for _ in 0..IN_FLIGHT {
            let (tx, rx) = mpsc::channel::<()>();
            release.push(tx);
            let gate = Arc::clone(&gate);
            // Each answers by timing out, and its thread lives on, holding its permit.
            let refused = bounded(&gate, Duration::from_millis(20), move || {
                let _ = rx.recv();
                answer()
            })
            .await;
            assert!(matches!(refused, Err(Error::Timeout(_))), "{refused:?}");
        }
        let busy = bounded(&gate, TIMEOUT, answer).await.unwrap_err();
        assert_eq!(ikigai_web::error_status(&busy), 429, "{busy:?}");
        assert_eq!(ikigai_web::retry_after_seconds(&busy), Some(10));
        assert_eq!(
            busy.to_string(),
            "retry after 10000 ms: name: this server is already reading 4 answers from \
             history, the most it reads at once; try again in 10 seconds, or leave out as-of \
             for the current answer"
        );
        drop(release);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while gate.available_permits() < IN_FLIGHT {
            assert!(
                std::time::Instant::now() < deadline,
                "the permits never came back"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(bounded(&gate, TIMEOUT, answer).await.is_ok());
    }

    /// A read past the timeout is answered with the sentence, as a 504.
    #[tokio::test]
    async fn a_read_past_the_timeout_is_refused_with_its_sentence() {
        let gate = Arc::new(Semaphore::new(IN_FLIGHT));
        let slow = bounded(&gate, Duration::from_secs(1), || {
            std::thread::sleep(Duration::from_millis(1500));
            answer()
        })
        .await
        .unwrap_err();
        assert_eq!(ikigai_web::error_status(&slow), 504, "{slow:?}");
        assert!(
            slow.to_string().ends_with(
                "name: git took longer than 1 seconds to read the history, so this server \
                 cannot answer as of that date now; try again, or leave out as-of for the \
                 current answer"
            ),
            "{slow}"
        );
    }

    #[test]
    fn a_directory_without_git_is_refused_and_told_what_to_change() {
        let dir = std::env::temp_dir();
        let refused = not_a_repository(&dir.join("ikigai-name-no-such-site")).unwrap();
        assert!(
            refused.contains("holds no .git; run `git init` there"),
            "{refused}"
        );
        assert_eq!(not_unix(), None);
    }
}
