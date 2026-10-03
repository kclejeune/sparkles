//! Lucene's sloppy phrase (`"a b"~n`): the words within `n` moves of the phrase, in any
//! order.
//!
//! Lucene gives each occurrence of a phrase word its position minus the word's offset in
//! the phrase, and a document matches when one occurrence of each word lies in a window
//! of these positions at most `n` wide. Tantivy's phrase slop differs: it keeps the words
//! in order, so `"a b"~2` does not match "b a" there, while it does in Lucene.

use tantivy::fieldnorm::FieldNormReader;
use tantivy::postings::{Postings, SegmentPostings};
use tantivy::query::{Bm25Weight, EmptyScorer, EnableScoring, Explanation, Query, Scorer, Weight};
use tantivy::schema::IndexRecordOption;
use tantivy::{DocId, DocSet, Score, SegmentReader, TERMINATED, Term};

#[derive(Clone, Debug)]
pub(super) struct SloppyPhraseQuery {
    /// each word with its offset in the phrase
    terms: Vec<(usize, Term)>,
    slop: u32,
}

impl SloppyPhraseQuery {
    /// A phrase of two or more distinct words.
    pub(super) fn new(terms: Vec<(usize, Term)>, slop: u32) -> Result<Self, String> {
        let mut distinct: Vec<&Term> = terms.iter().map(|(_, t)| t).collect();
        distinct.sort();
        distinct.dedup();
        if distinct.len() < terms.len() {
            return Err("a phrase with a slop cannot repeat a word".into());
        }
        Ok(SloppyPhraseQuery { terms, slop })
    }
}

impl Query for SloppyPhraseQuery {
    fn weight(&self, scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        let bm25 = match scoring {
            EnableScoring::Enabled {
                statistics_provider,
                ..
            } => {
                let terms: Vec<Term> = self.terms.iter().map(|(_, t)| t.clone()).collect();
                Some(Bm25Weight::for_terms(statistics_provider, &terms)?)
            }
            EnableScoring::Disabled { .. } => None,
        };
        Ok(Box::new(SloppyWeight {
            terms: self.terms.clone(),
            slop: self.slop,
            bm25,
        }))
    }

    fn query_terms<'a>(&'a self, visitor: &mut dyn FnMut(&'a Term, bool)) {
        for (_, t) in &self.terms {
            visitor(t, true);
        }
    }
}

struct SloppyWeight {
    terms: Vec<(usize, Term)>,
    slop: u32,
    bm25: Option<Bm25Weight>,
}

impl SloppyWeight {
    fn scorer_of(
        &self,
        reader: &SegmentReader,
        boost: Score,
    ) -> tantivy::Result<Option<SloppyScorer>> {
        let field = self.terms[0].1.field();
        let mut postings = Vec::with_capacity(self.terms.len());
        for (offset, term) in &self.terms {
            match reader
                .inverted_index(field)?
                .read_postings(term, IndexRecordOption::WithFreqsAndPositions)?
            {
                Some(p) => postings.push((*offset as i64, p)),
                None => return Ok(None),
            }
        }
        let fieldnorms = match (&self.bm25, reader.fieldnorms_readers().get_field(field)?) {
            (Some(_), Some(f)) => f,
            _ => FieldNormReader::constant(reader.max_doc(), 1),
        };
        let mut s = SloppyScorer {
            positions: vec![Vec::new(); postings.len()],
            postings,
            slop: i64::from(self.slop),
            doc: 0,
            bm25: self.bm25.as_ref().map(|w| w.boost_by(boost)),
            fieldnorms,
        };
        s.doc = s.settle(s.postings[0].1.doc());
        Ok(Some(s))
    }
}

impl Weight for SloppyWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        Ok(match self.scorer_of(reader, boost)? {
            Some(s) => Box::new(s),
            None => Box::new(EmptyScorer),
        })
    }

    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        let mut s = self.scorer_of(reader, 1.0)?.ok_or_else(|| {
            tantivy::TantivyError::InvalidArgument(format!("{doc} does not match"))
        })?;
        if s.seek(doc) != doc {
            return Err(tantivy::TantivyError::InvalidArgument(format!(
                "{doc} does not match"
            )));
        }
        Ok(Explanation::new("sloppy phrase", s.score()))
    }
}

struct SloppyScorer {
    /// each word's offset in the phrase and postings
    postings: Vec<(i64, SegmentPostings)>,
    positions: Vec<Vec<u32>>,
    slop: i64,
    doc: DocId,
    bm25: Option<Bm25Weight>,
    fieldnorms: FieldNormReader,
}

impl SloppyScorer {
    /// The first document at or after `target` that has every word, within the slop.
    fn settle(&mut self, mut target: DocId) -> DocId {
        loop {
            // the first document at or after target with every word
            'all: loop {
                if target == TERMINATED {
                    return TERMINATED;
                }
                for (_, p) in &mut self.postings {
                    let d = p.seek(target);
                    if d > target {
                        target = d;
                        continue 'all;
                    }
                }
                break;
            }
            if self.within_slop() {
                return target;
            }
            target = self.postings[0].1.advance();
        }
    }

    /// Whether one occurrence of each word lies in a window of phrase positions at most
    /// `slop` wide: the smallest such window, found by advancing the word whose position
    /// is the lowest.
    fn within_slop(&mut self) -> bool {
        for ((_, p), out) in self.postings.iter_mut().zip(&mut self.positions) {
            p.positions(out);
        }
        let pos = |i: usize, k: usize| i64::from(self.positions[i][k]) - self.postings[i].0;
        let mut at = vec![0usize; self.postings.len()];
        let mut max = (0..at.len()).map(|i| pos(i, 0)).max().unwrap_or(0);
        loop {
            let (low, min) = (0..at.len())
                .map(|i| (i, pos(i, at[i])))
                .min_by_key(|&(_, p)| p)
                .unwrap();
            if max - min <= self.slop {
                return true;
            }
            at[low] += 1;
            if at[low] == self.positions[low].len() {
                return false;
            }
            max = max.max(pos(low, at[low]));
        }
    }
}

impl DocSet for SloppyScorer {
    fn advance(&mut self) -> DocId {
        let next = self.postings[0].1.advance();
        self.doc = self.settle(next);
        self.doc
    }

    fn seek(&mut self, target: DocId) -> DocId {
        if target <= self.doc {
            return self.doc;
        }
        self.doc = self.settle(target);
        self.doc
    }

    fn doc(&self) -> DocId {
        self.doc
    }

    fn size_hint(&self) -> u32 {
        self.postings
            .iter()
            .map(|(_, p)| p.size_hint())
            .min()
            .unwrap_or(0)
    }
}

impl Scorer for SloppyScorer {
    fn score(&mut self) -> Score {
        match &self.bm25 {
            Some(w) => w.score(self.fieldnorms.fieldnorm_id(self.doc), 1),
            None => 1.0,
        }
    }
}
