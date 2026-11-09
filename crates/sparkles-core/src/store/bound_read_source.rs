//! Genuine physical input ownership. Checked query/read integration must retain
//! this source before preparing work and through each final physical output.
use super::BoundEvidenceView;
use crate::access::PreparedView;

/// An opaque shallow owner minted only by a genuine bound or prepared view.
/// Clones keep the actual original view backing and its credit alive. This
/// carrier grants no current access, and does not pay newly allocated outputs.
///
/// It exposes no raw snapshot or arbitrary-owner constructor.
///
/// ```compile_fail
/// use sparkles_core::store::{BoundReadSource, Snapshot};
/// fn escape(source: &BoundReadSource) -> Snapshot {
///     source.snapshot().clone()
/// }
/// ```
#[derive(Clone)]
pub struct BoundReadSource {
    source: Source,
}

#[derive(Clone)]
enum Source {
    Evidence(BoundEvidenceView),
    Prepared(PreparedView),
}

impl BoundReadSource {
    pub(crate) fn evidence(view: BoundEvidenceView) -> Self {
        Self {
            source: Source::Evidence(view),
        }
    }

    pub(crate) fn prepared(view: PreparedView) -> Self {
        Self {
            source: Source::Prepared(view),
        }
    }
}

impl std::fmt::Debug for BoundReadSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.source {
            Source::Evidence(view) => f
                .debug_tuple("BoundReadSource::Evidence")
                .field(view)
                .finish(),
            Source::Prepared(view) => {
                // Touch the genuine owner without exposing an extractor or claiming
                // authorization from the retained physical source.
                let _ = view.access();
                f.debug_tuple("BoundReadSource::Prepared").finish()
            }
        }
    }
}
