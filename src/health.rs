//! `urn:name:health` — the gauge.
//!
//! ## What this is, and what it deliberately is not
//!
//! It is **not** cache eviction. Eviction belongs to the kernel, which owns the
//! cache; a module cannot implement it and should not pretend to. Nor is a
//! resolver the place to *design* it: the whole corpus here is a few hundred
//! kilobytes of Turtle, so this host will never feel the memory pressure that
//! an eviction policy exists to answer. A policy built against a workload that
//! cannot exercise it is a policy nobody has tested.
//!
//! What belongs here is the **instrumentation that a real policy would need**,
//! and that an operator needs long before one exists: how much this deployment
//! is holding, what it refuses to hold, and whether its own answers are
//! cacheable at all. Build the gauge where the data is; build the policy where
//! there is pressure.
//!
//! ## Cacheability is reported, not assumed
//!
//! The `cacheable` field answers a question that is otherwise invisible until it
//! is a performance incident: **a face that never marks itself cacheable can
//! never be cached, however cacheable its sources are.** Both faces here were
//! exactly that until measured — every documentation view re-ran its SPARQL
//! query. The reverse trap is better known (one volatile dependency makes a
//! composite volatile) and the kernel already handles it by taking the meet;
//! this is the direction with no automatic protection, so it is worth a
//! number an operator can look at.

use crate::load;
use ikigai_core::{
    ActionSpec, AsyncFnEndpoint, Description, Error, Invocation, InvokeFuture, ReprType,
    Representation, Verb,
};
use serde::{Deserialize, Serialize};

const APPLICATION_JSON: &str = "application/json";

/// What this deployment is holding and refusing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health<'a> {
    /// Namespaces claimed, including retired ones — a retired claim still
    /// occupies its prefix forever, so it still counts.
    pub namespaces: usize,
    /// Of those, how many are served from here.
    pub hosted: usize,
    /// How many point at their owner's own node.
    pub redirected: usize,
    /// How many are cached copies of someone else's origin.
    pub mirrored: usize,
    /// How many are tombstones. Never decreases.
    pub retired: usize,
    /// The document ceiling in force, in bytes.
    pub max_document_bytes: usize,
    /// Which faces mark their own results cacheable.
    #[serde(borrow)]
    pub cacheable: Vec<&'a str>,
}

/// Faces whose results are a pure function of their inputs, and which therefore
/// mark themselves cacheable. Sinks are absent by definition: they mutate.
const CACHEABLE_FACES: [&str; 4] = [
    "urn:name:registry",
    "urn:name:resolve",
    "urn:name:docs",
    "urn:name:document",
];

/// `urn:name:health` — counts, ceilings, and cacheability posture.
pub fn health() -> AsyncFnEndpoint {
    AsyncFnEndpoint::new("health", |inv: &Invocation<'_>| -> InvokeFuture<'_> {
        Box::pin(async move {
            let registry = load(inv).await?;
            let mut report = Health {
                namespaces: registry.namespaces.len(),
                hosted: 0,
                redirected: 0,
                mirrored: 0,
                retired: 0,
                max_document_bytes: registry.limits.max_document_bytes,
                cacheable: CACHEABLE_FACES.to_vec(),
            };
            for ns in &registry.namespaces {
                use crate::registry::Strategy::*;
                match ns.strategy {
                    Hosted { .. } => report.hosted += 1,
                    Redirect { .. } => report.redirected += 1,
                    Mirror { .. } => report.mirrored += 1,
                    Retired { .. } => report.retired += 1,
                }
            }
            let body = serde_json::to_vec_pretty(&report)
                .map_err(|e| Error::Endpoint(format!("name: {e}")))?;
            // A pure function of the registry, like every other read face here.
            Ok(Representation::new(ReprType::new(APPLICATION_JSON), body).cacheable())
        })
    })
    .with_description(
        Description::new("health")
            .title("What this deployment holds and refuses")
            .summary(
                "Namespace counts by strategy, the document ceiling in force, and which \
                 faces mark their results cacheable. The instrumentation a cache policy \
                 would need — not a cache policy, which belongs to the kernel.",
            )
            .action(
                ActionSpec::new(Verb::Source)
                    .summary("report this deployment's footprint and limits")
                    .output(APPLICATION_JSON),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space;
    use futures::executor::block_on;
    use ikigai_core::{Capability, Exact, FnEndpoint, Iri, Kernel, Request};
    use std::sync::Arc;

    const REGISTRY: &str = r#"{"namespaces":[
        {"prefix":"a","owner":"x","strategy":"hosted","source":"urn:file:a"},
        {"prefix":"b","owner":"x","strategy":"redirect","target":"https://b.example/"},
        {"prefix":"c","owner":"x","strategy":"mirror","origin":"https://c.example/"},
        {"prefix":"d","owner":"x","strategy":"retired","reason":"ended"}
    ],"limits":{"max_document_bytes":4096}}"#;

    fn report(json: &'static str) -> (Health<'static>, ikigai_core::Expiry) {
        let source = FnEndpoint::new("registry-source", move |_: &Invocation<'_>| {
            Ok(
                Representation::new(ReprType::new(APPLICATION_JSON), json.as_bytes().to_vec())
                    // Cacheable, so the meet does not mask what this face declares.
                    .cacheable(),
            )
        });
        let kernel = Kernel::new(Arc::new(
            space().bind(Exact::new(crate::REGISTRY_IRI), source),
        ));
        let repr = block_on(kernel.issue(
            Request::new(Verb::Source, Iri::parse("urn:name:health").unwrap()),
            &Capability::root(),
        ))
        .expect("reports");
        // Leaked so the borrowed `cacheable` strings outlive the call; a test
        // fixture, not a pattern for the library.
        let bytes: &'static [u8] = Box::leak(repr.bytes.clone().into_boxed_slice());
        let health: Health<'static> = serde_json::from_slice(bytes).expect("valid JSON");
        (health, repr.expiry)
    }

    #[test]
    fn every_strategy_is_counted_separately() {
        let (h, _) = report(REGISTRY);
        assert_eq!(h.namespaces, 4);
        assert_eq!(
            (h.hosted, h.redirected, h.mirrored, h.retired),
            (1, 1, 1, 1)
        );
    }

    /// A retired namespace still occupies its prefix forever, so it must keep
    /// counting — reporting only the live ones would understate what can never
    /// be claimed again.
    #[test]
    fn retired_namespaces_still_count_against_the_total() {
        let (h, _) = report(REGISTRY);
        assert_eq!(
            h.namespaces,
            h.hosted + h.redirected + h.mirrored + h.retired
        );
    }

    #[test]
    fn the_ceiling_in_force_is_reported() {
        let (h, _) = report(REGISTRY);
        assert_eq!(h.max_document_bytes, 4096);
    }

    #[test]
    fn the_default_ceiling_is_reported_when_none_is_configured() {
        let (h, _) = report(r#"{"namespaces":[]}"#);
        assert_eq!(h.max_document_bytes, 8 * 1024 * 1024);
    }

    /// The regression guard that matters: these faces were all uncacheable
    /// until it was measured, and nothing in the type system says otherwise.
    #[test]
    fn health_declares_itself_cacheable_when_its_source_is() {
        let (_, expiry) = report(REGISTRY);
        assert_eq!(
            expiry,
            ikigai_core::Expiry::Never,
            "a pure read over a cacheable registry must be cacheable"
        );
    }

    #[test]
    fn it_names_the_faces_it_claims_are_cacheable() {
        let (h, _) = report(REGISTRY);
        assert!(h.cacheable.contains(&"urn:name:docs"));
        assert!(
            !h.cacheable.contains(&"urn:name:claim"),
            "a Sink is never cacheable"
        );
    }
}
