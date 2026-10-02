//! Jena's highlighting of `text:query` literals (`"highlight:…"`), which runs Lucene's
//! `Highlighter` with a `SimpleFragmenter` and a `SimpleHTMLFormatter`.
//!
//! The literal is cut into fragments of about `z:` characters at token boundaries. A new
//! fragment starts at the first token that ends at or after the next multiple of the
//! fragment size. A fragment scores the number of distinct query words it contains, and
//! the best `m:` fragments are kept, best first. With `jf:`, adjacent ones are merged and
//! those without a match dropped. Each matching token is wrapped in the `s:` and `e:`
//! marks (a phrase only where it occurs), and with `jh:` two marked tokens separated by
//! one space are marked as one. The fragments are joined with `f:`.

use super::HighlightOpts;
use super::lucene::Matchers;
use tantivy::tokenizer::TextAnalyzer;

/// The highlighted fragments of `text`, or `None` when no token matches.
pub(super) fn highlight(
    text: &str,
    opts: &HighlightOpts,
    m: &Matchers,
    analyzer: &mut TextAnalyzer,
) -> Option<String> {
    // tokens: byte range, and whether the query matches them
    let mut all = Vec::new();
    let mut stream = analyzer.token_stream(text);
    while let Some(t) = stream.next() {
        all.push((t.position, t.text.clone(), t.offset_from, t.offset_to));
    }
    let marks = m.marks(&all.iter().map(|t| (t.0, t.1.as_str())).collect::<Vec<_>>());
    let toks: Vec<(usize, usize, Option<String>)> = all
        .into_iter()
        .zip(marks)
        .map(|((_, t, from, to), hit)| (from, to, hit.then_some(t)))
        .collect();
    if toks.iter().all(|t| t.2.is_none()) {
        return None;
    }
    // fragments: the byte offset each starts at, and its tokens. As in Lucene, the first
    // token never starts one, and a fragment starts right after the token before its
    // first, so it begins with the space between them.
    let mut frags: Vec<(usize, std::ops::Range<usize>)> = vec![(0, 0..0)];
    let mut chars = 0usize;
    let mut at = 0usize;
    let mut n = 1usize;
    for (i, &(_, to, _)) in toks.iter().enumerate() {
        chars += text[at..to].chars().count();
        at = to;
        if i > 0 && chars >= opts.frag_size.saturating_mul(n) {
            n += 1;
            frags.push((toks[i - 1].1, i..i));
        }
        frags.last_mut().unwrap().1.end = i + 1;
    }
    let bounds = |f: usize| {
        let end = frags.get(f + 1).map_or(text.len(), |n| n.0);
        (frags[f].0, end)
    };
    let score = |r: &std::ops::Range<usize>| {
        let mut words: Vec<&str> = toks[r.clone()]
            .iter()
            .filter_map(|t| t.2.as_deref())
            .collect();
        words.sort_unstable();
        words.dedup();
        words.len()
    };
    // the best fragments, best first (an earlier one first on a tie)
    let mut best: Vec<(usize, usize, usize)> = (0..frags.len())
        .map(|f| (f, f, score(&frags[f].1)))
        .collect();
    best.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    best.truncate(opts.max_frags);
    let mut best: Vec<Option<(usize, usize, usize)>> = best.into_iter().map(Some).collect();
    if opts.join_frags {
        // merge kept fragments that follow each other in the text; the merged one takes
        // the place of the better of the two
        'merge: loop {
            for i in 0..best.len() {
                for j in 0..best.len() {
                    let (Some(a), Some(b)) = (best[i], best[j]) else {
                        continue;
                    };
                    if i != j && a.1 + 1 == b.0 {
                        let merged = (a.0, b.1, a.2.max(b.2));
                        let (keep, drop) = if a.2 > b.2 { (i, j) } else { (j, i) };
                        best[keep] = Some(merged);
                        best[drop] = None;
                        continue 'merge;
                    }
                }
            }
            break;
        }
    }
    // as in Lucene, only merging drops the fragments without a match
    let out: Vec<String> = best
        .into_iter()
        .flatten()
        .filter(|f| f.2 > 0 || !opts.join_frags)
        .map(|(first, last, _)| {
            let (start, _) = bounds(first);
            let (_, end) = bounds(last);
            let mut s = String::new();
            let mut pos = start;
            for &(from, to, ref hit) in &toks[frags[first].1.start..frags[last].1.end] {
                if hit.is_some() {
                    s.push_str(&text[pos..from]);
                    s.push_str(&opts.start);
                    s.push_str(&text[from..to]);
                    s.push_str(&opts.end);
                    pos = to;
                }
            }
            s.push_str(&text[pos..end]);
            if opts.join_hi {
                s = s.replace(&format!("{} {}", opts.end, opts.start), " ");
            }
            s
        })
        .collect();
    (!out.is_empty()).then(|| out.join(&opts.frag_sep))
}
