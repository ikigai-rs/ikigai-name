//! Membership of a hosted namespace, checked before serving or counting a name.

use crate::history::{read_as_of, AsOf};
use crate::iri::{is_iri, name_key};
use crate::registry::{Names, Namespace, Strategy};
use crate::tenant::{iri_host, tenant_of};
use ikigai_core::{Error, Invocation, Iri, Representation, Result};
use oxrdf::NamedOrBlankNode;
use oxrdfio::{RdfFormat, RdfParser};

pub(crate) async fn check(
    inv: &Invocation<'_>,
    namespace: &Namespace,
    name: &str,
    at: Option<&AsOf>,
    ceiling: usize,
) -> Result<Option<Representation>> {
    let Strategy::Hosted {
        source,
        names,
        documents,
    } = &namespace.strategy
    else {
        return Ok(None);
    };
    if *names == Names::Any
        || name_key(name) == name_key(&namespace.prefix)
        || documents
            .iter()
            .any(|document| name_key(document) == name_key(name))
    {
        return Ok(None);
    }
    if *names == Names::Terms {
        let stored =
            match (at, &namespace.repository) {
                (Some(at), Some(repository)) => {
                    crate::git::read_past(inv, &namespace.prefix, repository, at).await?
                }
                (Some(at), None) => read_as_of(inv, source, at).await?,
                (None, _) => {
                    inv.source(&Iri::parse(source).map_err(|e| {
                        Error::Endpoint(format!("name: bad source {source:?}: {e}"))
                    })?)
                    .await?
                }
            };
        if stored.bytes.len() > ceiling {
            return Err(Error::Endpoint(format!(
                "name: {name:?} is {} bytes, over this host's {ceiling}-byte ceiling (raise limits.max_document_bytes in the registry)", stored.bytes.len()
            )));
        }
        let text = std::str::from_utf8(&stored.bytes).map_err(|e| {
            Error::Endpoint(format!(
                "name: the document for {:?} is not UTF-8: {e}",
                namespace.prefix
            ))
        })?;
        ikigai_rdf::check_turtle_nesting(text, "document")?;
        let parser = RdfParser::from_format(RdfFormat::Turtle)
            .with_base_iri(source)
            .map_err(|e| Error::Endpoint(format!("name: bad source {source:?}: {e}")))?;
        let mut held = false;
        for quad in parser.for_slice(&stored.bytes) {
            let quad = quad.map_err(|e| {
                Error::Endpoint(format!(
                    "name: the document for {:?} is not Turtle: {e}",
                    namespace.prefix
                ))
            })?;
            if let NamedOrBlankNode::NamedNode(subject) = quad.subject {
                held |= matches_name(subject.as_str(), name, tenant_of(inv.capability));
            }
        }
        if held {
            return Ok(Some(stored));
        }
    }
    Err(Error::NotFound(format!(
        "name: {name:?} is not held by the namespace {:?}",
        namespace.prefix
    )))
}

fn matches_name(iri: &str, name: &str, tenant: Option<&str>) -> bool {
    if is_iri(name) {
        return name_key(iri) == name_key(name);
    }
    // A web term's host is checked when the deployment is keyed by tenant.
    // Without tenants the single registry supplies the host's vocabulary.
    if let Some(host) = iri_host(iri) {
        if tenant.is_some_and(|tenant| tenant != host) {
            return false;
        }
        if let Some((_, tail)) = iri.split_once("://") {
            if let Some((_, path)) = tail.split_once('/') {
                return name_key(path) == name_key(name);
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::matches_name;

    #[test]
    fn web_terms_belong_to_the_tenant_that_hosts_them() {
        assert!(matches_name(
            "https://example.org/vocab/Thing",
            "vocab/Thing",
            Some("example.org")
        ));
        assert!(!matches_name(
            "https://elsewhere.org/vocab/Thing",
            "vocab/Thing",
            Some("example.org")
        ));
        assert!(!matches_name(
            "https://example.org/vocab/Thing",
            "vocab/thing",
            None
        ));
    }

    #[test]
    fn iri_terms_use_the_existing_canonical_comparison() {
        assert!(matches_name(
            "tag:urnid.org,2026:ACME:Thing",
            "urn:urnid:acme:Thing",
            None
        ));
        assert!(!matches_name(
            "urn:urnid:acme:Thing",
            "urn:urnid:acme:thing",
            None
        ));
        assert!(!matches_name("urn:urnid:acme:Thing", "vocab/Thing", None));
    }
}
