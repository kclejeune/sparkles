//! Turtle and TriG printing: directives in the `directive-style` family, subject blocks in
//! the diff-friendly expanded form (or the conventional form of `turtle-layout`), object
//! lists, blank node property lists, collections, RDF 1.2 terms and TriG graph blocks.
//!
//! Not written yet: [`print`] refuses every document.

use crate::doc::Printed;
use crate::lex::TokenKind;
use crate::syntax::NodeKind;
use crate::tree::Tree;
use crate::trivia::{CommentRules, Comments};
use crate::{FormatError, Language, Options};

/// Whether `turtle-layout = "conventional"` is implemented (until it is, it warns
/// `option-not-implemented` and prints the default layout).
pub const CONVENTIONAL_IMPLEMENTED: bool = false;

/// Print a Turtle (`trig: false`) or TriG tree.
pub fn print(
    tree: &Tree<'_>,
    comments: &Comments,
    opts: &Options,
    trig: bool,
) -> Result<Printed, FormatError> {
    let _ = (tree, comments, opts);
    Err(FormatError::unsupported_language(match trig {
        false => Language::Turtle,
        true => Language::TriG,
    }))
}

/// Turtle's and TriG's comment attachment rules.
pub struct TurtleRules;

pub static RULES: TurtleRules = TurtleRules;

impl CommentRules for TurtleRules {
    fn is_attachment(&self, kind: NodeKind) -> bool {
        use NodeKind as K;
        matches!(
            kind,
            K::BaseDecl
                | K::PrefixDecl
                | K::VersionDecl
                | K::TriplesStmt
                | K::GraphBlock
                | K::PropertyListEntry
                | K::Object
                | K::CollectionItem
        )
    }

    fn is_container(&self, kind: NodeKind) -> bool {
        use NodeKind as K;
        matches!(
            kind,
            K::GraphBlock | K::BNodePropertyList | K::Collection | K::AnnotationBlock
        )
    }

    fn is_separator(&self, kind: TokenKind) -> bool {
        use TokenKind as T;
        matches!(kind, T::Comma | T::Semicolon | T::Dot)
    }

    fn is_closer(&self, kind: TokenKind) -> bool {
        crate::sparql::parse::is_closer(kind)
    }
}
