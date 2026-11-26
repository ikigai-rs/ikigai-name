//! The history contract ([`history_contract::run`]) over the git binding, through the
//! library: `repo_history` over the real `urn:repo:log` and `urn:repo:show`, one
//! repository per tenant, keyed on the tenant the capability names.

mod history_contract;

use async_trait::async_trait;
use futures::executor::block_on;
use history_contract::{git, Binding};
use ikigai_core::{
    ArgRef, Capability, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Iri,
    Kernel, Representation, Request, Result, Verb,
};
use ikigai_name::tenant::{tenant_of, tenant_scope};
use ikigai_name::{repo_history, HISTORY_IRI, REGISTRY_IRI};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// `repo_history` for each tenant, as a host with tenants keys it.
struct Keyed(HashMap<String, ikigai_core::AsyncFnEndpoint>);

#[async_trait]
impl Endpoint for Keyed {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let tenant = tenant_of(inv.capability).ok_or_else(|| Error::Denied("no tenant".into()))?;
        self.0
            .get(tenant)
            .ok_or_else(|| Error::Denied(format!("no tenant {tenant}")))?
            .invoke(inv)
            .await
    }

    fn name(&self) -> &str {
        "keyed-history"
    }

    fn describe(&self) -> Description {
        Description::new("keyed-history")
    }
}

struct Library {
    root: PathBuf,
    kernel: Kernel,
}

impl Library {
    fn new(tenants: &[&str]) -> Library {
        let root = git::scratch("history-source");
        let mut bindings = HashMap::new();
        for tenant in tenants {
            let dir = root.join(tenant);
            git::init(&dir);
            bindings.insert(
                tenant.to_string(),
                repo_history(dir.display().to_string(), "registry.json"),
            );
        }
        let space = EndpointSpace::new().bind(Exact::new(HISTORY_IRI), Keyed(bindings));
        let repo = ikigai_repo::space_with_roots([&root]);
        let kernel = Kernel::new(Arc::new(ikigai_core::Fallback::new(vec![
            Arc::new(space),
            Arc::new(repo),
        ])));
        Library { root, kernel }
    }

    fn source(&self, tenant: &str, source: &str, as_of: &str) -> Result<Representation> {
        let request = Request::new(Verb::Source, Iri::parse(HISTORY_IRI).unwrap())
            .with_arg("source", ArgRef::Inline(source.as_bytes().to_vec()))
            .with_arg("as-of", ArgRef::Inline(as_of.as_bytes().to_vec()));
        let capability = Capability::scoped([
            "urn:cap:exec:read:git".to_string(),
            tenant_scope(tenant).unwrap(),
        ]);
        block_on(self.kernel.issue(request, &capability))
    }

    fn found(answer: Result<Representation>) -> Option<String> {
        match answer {
            Ok(repr) => {
                assert!(
                    matches!(repr.expiry, ikigai_core::Expiry::Always),
                    "an as-of answer is never cacheable"
                );
                Some(String::from_utf8(repr.bytes.to_vec()).unwrap())
            }
            Err(Error::NotFound(_)) => None,
            Err(other) => panic!("neither an answer nor NotFound: {other:?}"),
        }
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Binding for Library {
    fn record(&mut self, tenant: Option<&str>, at: &str, files: &[(&str, &str)]) {
        git::commit(&self.root.join(tenant.unwrap()), at, files);
    }

    fn read(&self, tenant: Option<&str>, as_of: &str) -> Option<String> {
        Self::found(self.source(tenant.unwrap(), history_contract::SOURCE, as_of))
    }

    fn claims(&self, tenant: Option<&str>, prefix: &str, as_of: &str) -> bool {
        Self::found(self.source(tenant.unwrap(), REGISTRY_IRI, as_of))
            .is_some_and(|registry| registry.contains(&format!("\"prefix\":\"{prefix}\"")))
    }
}

#[test]
fn the_git_binding_keeps_the_history_contract() {
    let mut library = Library::new(&["one.example", "other.example"]);
    history_contract::run(&mut library, Some("one.example"), Some("other.example"));
}
