//! Predicates whose planner reads are not ordinary stored RDF edges.

use sparkles_core::geo::vocab::{Relation, SpatialPfKind};
use sparkles_core::sparql::{arqpf, georewrite, hybrid, patternplan, textpf};

/// Snapshot settings can enable some of these rewrites later. A dependency plan
/// without those read producers must not assume their written predicate is data.
pub(crate) fn computed(iri: &str) -> bool {
    arqpf::PfKind::of(iri).is_some()
        || iri == arqpf::RDFS_MEMBER
        || iri == patternplan::HAS_PREDICATE
        || iri == patternplan::QL_HAS_PREDICATE
        || iri == textpf::TEXT_QUERY
        || iri == sparkles_core::vector::VECTOR_SEARCH
        || iri == hybrid::HYBRID_SEARCH
        || iri == georewrite::SPATIAL_EQUALS
        || SpatialPfKind::from_iri(iri).is_some()
        || Relation::from_property(iri).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planner_property_features_are_not_classified_as_stored_predicates() {
        for iri in sparkles_core::sparql::catalog::property_functions() {
            assert!(computed(&iri), "planner property feature omitted: {iri}");
        }
        for iri in [
            arqpf::RDFS_MEMBER,
            patternplan::HAS_PREDICATE,
            patternplan::QL_HAS_PREDICATE,
            georewrite::SPATIAL_EQUALS,
        ] {
            assert!(computed(iri), "synthetic predicate omitted: {iri}");
        }
        for relation in Relation::ALL {
            assert!(computed(&format!(
                "{}{}",
                sparkles_core::geo::vocab::GEO,
                relation.local()
            )));
        }
        assert!(!computed("urn:ordinary:predicate"));
        assert!(!computed(
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#first"
        ));
    }
}
