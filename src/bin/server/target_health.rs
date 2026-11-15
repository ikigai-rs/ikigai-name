//! The opt-in timer holds network authority; ordinary HTTP requests never do.
use ikigai_core::{Capability, Iri, Kernel, Request, Verb};
use ikigai_name::target_health::CAP_CHECK;
use ikigai_name::tenant::tenant_scope;
use ikigai_name::{Registry, Strategy, REGISTRY_IRI};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub fn capability(dir: &Path, tenant: Option<&str>, hosts: &[String]) -> Capability {
    let mut scopes = vec![
        CAP_CHECK.to_string(),
        format!("urn:cap:fs:read:{}", dir.display()),
    ];
    scopes.extend(tenant.and_then(tenant_scope));
    scopes.extend(hosts.iter().map(|host| format!("urn:cap:net:{host}")));
    Capability::scoped(scopes)
}

pub fn spawn(kernel: Arc<Kernel>, capabilities: Vec<Capability>, every: Duration) {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(every);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            for capability in &capabilities {
                if let Err(error) = check_site(&kernel, capability).await {
                    eprintln!("ikigai-name-server: target health job: {error}");
                }
            }
        }
    });
}

async fn check_site(kernel: &Kernel, capability: &Capability) -> ikigai_core::Result<()> {
    let request = Request::new(Verb::Source, Iri::parse(REGISTRY_IRI).expect("valid IRI"));
    let repr = kernel.issue(request, capability).await?;
    let registry = Registry::from_json(&repr.bytes).map_err(|e| {
        ikigai_core::Error::Endpoint(format!("name: {REGISTRY_IRI} is not a valid registry: {e}"))
    })?;
    for namespace in registry.namespaces {
        if !matches!(namespace.strategy, Strategy::Redirect { .. }) {
            continue;
        }
        let target = Iri::parse(format!(
            "urn:name:target-health-check:namespace:{}",
            namespace.prefix
        ))
        .map_err(|e| ikigai_core::Error::Endpoint(format!("name: {e}")))?;
        if let Err(error) = kernel
            .issue(Request::new(Verb::Sink, target), capability)
            .await
        {
            eprintln!("ikigai-name-server: target health job: {error}");
        }
    }
    Ok(())
}
