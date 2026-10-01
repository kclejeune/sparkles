//! Opt-in sorting of Turtle and TriG (`sort`): statements by printed subject within the
//! runs between sort barriers (detached comment blocks and directives), the entries of a
//! block by printed predicate after the `a` entries, objects by printed form; TriG graph
//! blocks by printed name, the default graph first. A node moves with its leading
//! comments and its trailing comment.
//!
//! Not written yet: `sort = true` warns `option-not-implemented` for Turtle and TriG.

/// Whether `sort` acts on Turtle and TriG.
pub const IMPLEMENTED: bool = false;
