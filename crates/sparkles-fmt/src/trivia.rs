//! Comment classification and attachment, and blank-line accounting: which node owns
//! each comment (header, leading, trailing, dangling, detached, displaced), and which
//! nodes had a blank line before them.

use crate::Warning;
use crate::doc::{DocArena, DocId};
use crate::lex::TokenKind;
use crate::syntax::NodeKind;
use crate::tree::{NodeId, TokenId, Tree};
use std::collections::HashMap;

/// What a language says about its tree, for attaching comments.
pub trait CommentRules {
    /// Nodes that own comments (prologue declarations, clauses, group elements, entries,
    /// objects, arguments, chain operands …).
    fn is_attachment(&self, kind: NodeKind) -> bool;
    /// Nodes whose closing bracket takes dangling comments (`{ }`, `[ ]`, `( )` …).
    fn is_container(&self, kind: NodeKind) -> bool;
    /// Tokens that count as part of the item before them (`,` `;` `.`, `&&` `||`).
    fn is_separator(&self, kind: TokenKind) -> bool;
    /// Closing brackets (`}` `]` `)` `|}` `>>` `)>>`).
    fn is_closer(&self, kind: TokenKind) -> bool;
}

/// The comments of a tree, by owner.
#[derive(Clone, Debug, Default)]
pub struct Comments {
    header: Vec<TokenId>,
    leading: HashMap<NodeId, Vec<TokenId>>,
    trailing: HashMap<NodeId, Vec<TokenId>>,
    dangling: HashMap<NodeId, Vec<TokenId>>,
    detached: HashMap<NodeId, Vec<Vec<TokenId>>>,
    blank_before: HashMap<NodeId, bool>,
    ignored: HashMap<NodeId, bool>,
    ignore_file: bool,
    warnings: Vec<Warning>,
}

impl Comments {
    /// Classify every comment of `tree`.
    ///
    /// TODO: attachment (§ trailing, leading, dangling, detached, displaced). Only the
    /// file header is collected so far; the other comments stay inside the `Verbatim`
    /// ranges of the nodes that are not formatted yet.
    pub fn attach(tree: &Tree<'_>, rules: &dyn CommentRules) -> Comments {
        let _ = rules;
        let header: Vec<TokenId> = tree
            .tokens
            .iter()
            .enumerate()
            .take_while(|(_, t)| t.kind.is_trivia())
            .filter(|(_, t)| t.kind == TokenKind::Comment)
            .map(|(i, _)| TokenId(i as u32))
            .collect();
        let ignore_file = header
            .iter()
            .any(|&c| crate::pragma::is_ignore_file(tree.token_text(c)));
        Comments {
            header,
            ignore_file,
            ..Comments::default()
        }
    }

    /// The comments before the first significant token, in order.
    pub fn header(&self) -> &[TokenId] {
        &self.header
    }

    /// The comment block directly before `n` (no blank line between).
    pub fn leading(&self, n: NodeId) -> &[TokenId] {
        self.leading.get(&n).map_or(&[], Vec::as_slice)
    }

    /// The comment on the line where `n` ends.
    pub fn trailing(&self, n: NodeId) -> &[TokenId] {
        self.trailing.get(&n).map_or(&[], Vec::as_slice)
    }

    /// Comments before the closing bracket of container `n`.
    pub fn dangling(&self, n: NodeId) -> &[TokenId] {
        self.dangling.get(&n).map_or(&[], Vec::as_slice)
    }

    /// Comment blocks with blank lines around them, printed as siblings before `n`.
    pub fn detached_before(&self, n: NodeId) -> &[Vec<TokenId>] {
        self.detached.get(&n).map_or(&[], Vec::as_slice)
    }

    /// Whether the source had a blank line before `n` (after its previous sibling).
    pub fn blank_before(&self, n: NodeId) -> bool {
        self.blank_before.get(&n).copied().unwrap_or(false)
    }

    /// Whether `n`'s leading block ends with `# sparkles-fmt: ignore`.
    pub fn ignored(&self, n: NodeId) -> bool {
        self.ignored.get(&n).copied().unwrap_or(false)
    }

    /// Whether the header holds `# sparkles-fmt: ignore-file`.
    pub fn ignore_file(&self) -> bool {
        self.ignore_file
    }

    /// `comment-moved` warnings of displaced comments.
    pub fn warnings(&self) -> &[Warning] {
        &self.warnings
    }
}

/// `doc` with `node`'s leading, trailing and detached comments around it.
///
/// TODO: print the comments (stub: the comments are inside the node's `Verbatim`).
pub fn wrap(arena: &mut DocArena<'_>, comments: &Comments, node: NodeId, doc: DocId) -> DocId {
    let _ = (arena, comments, node);
    doc
}
