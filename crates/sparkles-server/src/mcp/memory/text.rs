//! Text helpers of the memory tools: words, a small BM25 index, edit distance, the
//! normalized form of labels, and search strings that the full-text query syntax reads
//! as plain words.

use std::collections::HashMap;

/// The words of `s`: runs of letters and digits, lowercased, with camel case split
/// (`memberOf` → `member`, `of`).
pub fn words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev: Option<char> = None;
    for c in s.chars() {
        if c.is_alphanumeric() {
            // a lowercase letter or digit followed by an uppercase one starts a word
            if c.is_uppercase()
                && prev.is_some_and(|p| p.is_lowercase() || p.is_ascii_digit())
                && !cur.is_empty()
            {
                out.push(std::mem::take(&mut cur));
            }
            cur.extend(c.to_lowercase());
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        prev = Some(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A light stem: plural endings removed (`teams` → `team`, `queries` → `query`).
pub fn stem(w: &str) -> String {
    let n = w.chars().count();
    if n > 4 && w.ends_with("ies") {
        format!("{}y", &w[..w.len() - 3])
    } else if n > 3 && w.ends_with('s') && !w.ends_with("ss") && !w.ends_with("us") {
        w[..w.len() - 1].to_string()
    } else {
        w.to_string()
    }
}

/// English function words, which BM25 leaves out of documents and questions.
const STOP: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "do", "does", "for", "from", "has", "have",
    "how", "in", "is", "it", "its", "of", "on", "or", "that", "the", "their", "this", "to", "was",
    "what", "when", "where", "which", "who", "whom", "with",
];

/// The stemmed words of `s` without function words, for ranking.
pub fn terms(s: &str) -> Vec<String> {
    words(s)
        .iter()
        .filter(|w| !STOP.contains(&w.as_str()))
        .map(|w| stem(w))
        .collect()
}

/// A label's normalized form: case folded, with runs of white space collapsed to one
/// space and trimmed.
pub fn normalize(s: &str) -> String {
    s.split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// A full-text query that matches the words of `s` as plain terms, leaving out function
/// words: the query syntax's operators and quotes never reach the parser. `None` when `s` has no word.
pub fn plain_query(s: &str) -> Option<String> {
    let w = words(s);
    if w.is_empty() {
        return None;
    }
    // function words only add noise to the ranking, unless they are all there is
    let content: Vec<&String> = w.iter().filter(|w| !STOP.contains(&w.as_str())).collect();
    // the parser reads AND, OR and NOT as operators only in upper case
    if content.is_empty() {
        Some(w.join(" "))
    } else {
        Some(
            content
                .iter()
                .map(|w| w.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        )
    }
}

/// A phrase query of the words of `s`.
pub fn phrase_query(s: &str) -> Option<String> {
    let w = words(s);
    (!w.is_empty()).then(|| format!("\"{}\"", w.join(" ")))
}

/// The Damerau–Levenshtein distance (optimal string alignment) of two strings, in
/// characters.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=m {
        d[0][j] = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut v = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = v;
        }
    }
    d[n][m]
}

/// A BM25 index over a few documents, held in memory.
pub struct Bm25 {
    docs: Vec<HashMap<String, usize>>,
    lens: Vec<usize>,
    avg: f64,
    df: HashMap<String, usize>,
}

impl Bm25 {
    const K1: f64 = 1.2;
    const B: f64 = 0.75;

    pub fn new(docs: &[String]) -> Bm25 {
        let mut df: HashMap<String, usize> = HashMap::new();
        let mut out = Vec::with_capacity(docs.len());
        let mut lens = Vec::with_capacity(docs.len());
        for d in docs {
            let mut tf: HashMap<String, usize> = HashMap::new();
            let t = terms(d);
            lens.push(t.len());
            for w in t {
                *tf.entry(w).or_default() += 1;
            }
            for w in tf.keys() {
                *df.entry(w.clone()).or_default() += 1;
            }
            out.push(tf);
        }
        let avg = if lens.is_empty() {
            0.0
        } else {
            lens.iter().sum::<usize>() as f64 / lens.len() as f64
        };
        Bm25 {
            docs: out,
            lens,
            avg,
            df,
        }
    }

    /// The score of every document for `query`, in document order.
    pub fn scores(&self, query: &str) -> Vec<f64> {
        let mut q = terms(query);
        q.sort();
        q.dedup();
        let n = self.docs.len() as f64;
        self.docs
            .iter()
            .zip(&self.lens)
            .map(|(tf, &len)| {
                q.iter()
                    .filter_map(|w| {
                        let f = *tf.get(w)? as f64;
                        let df = *self.df.get(w)? as f64;
                        let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
                        let norm = 1.0 - Self::B + Self::B * len as f64 / self.avg.max(1.0);
                        Some(idf * f * (Self::K1 + 1.0) / (f + Self::K1 * norm))
                    })
                    .sum()
            })
            .collect()
    }
}

/// The cosine similarity of two vectors (0 when either is zero or they differ in
/// length).
pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0f64, 0f64, 0f64);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (*x as f64, *y as f64);
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// Ranks of a scored list, best first: one more than the number of strictly better
/// entries, so ties share a rank.
pub fn ranks(scores: &[f64]) -> Vec<Option<usize>> {
    scores
        .iter()
        .map(|&s| (s > 0.0).then(|| 1 + scores.iter().filter(|&&o| o > s).count()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_split_camel_case_and_punctuation() {
        assert_eq!(words("memberOf"), ["member", "of"]);
        assert_eq!(words("foaf:givenName"), ["foaf", "given", "name"]);
        assert_eq!(
            words("Who is on the payments team?"),
            ["who", "is", "on", "the", "payments", "team"]
        );
        assert_eq!(words("HTTPServer2go"), ["httpserver2go"]);
        assert_eq!(terms("teams queries class"), ["team", "query", "class"]);
    }

    #[test]
    fn edit_distance_counts_transpositions_once() {
        assert_eq!(edit_distance("organisation", "organization"), 1);
        assert_eq!(edit_distance("memberof", "memberof"), 0);
        assert_eq!(edit_distance("ab", "ba"), 1);
        assert_eq!(edit_distance("", "abc"), 3);
    }

    #[test]
    fn plain_queries_never_carry_operators() {
        assert_eq!(plain_query("Ana \"(Lima)\" -x +y").unwrap(), "ana lima x y");
        assert_eq!(plain_query("AND OR NOT").unwrap(), "not");
        assert_eq!(plain_query("AND OR").unwrap(), "and or");
        assert_eq!(
            plain_query("Who is on the payments team?").unwrap(),
            "payments team"
        );
        assert_eq!(phrase_query("Payments  team").unwrap(), "\"payments team\"");
        assert!(plain_query("?!").is_none());
    }

    #[test]
    fn bm25_ranks_the_matching_document_first() {
        let idx = Bm25::new(&[
            "Members of a team. Who is on the payments team?".to_string(),
            "Count the people in a city".to_string(),
        ]);
        let s = idx.scores("who works in payments");
        assert!(s[0] > 0.0 && s[1] == 0.0, "{s:?}");
        assert_eq!(
            ranks(&[0.5, 0.0, 0.9, 0.5]),
            [Some(2), None, Some(1), Some(2)]
        );
    }
}
