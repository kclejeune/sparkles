//! Explicit compiler allocation bounds. This walk borrows syntax and allocates no
//! heap before admission, including for non-emitting nested parent frames.
use super::*;
use crate::budget::table_capacity;
use sparkles_budget::ParentRefused;

fn overflow() -> BudgetFailure {
    BudgetFailure::parent(ParentRefused::Overflow)
}
fn plus(a: usize, b: usize) -> Result<usize, BudgetFailure> {
    a.checked_add(b).ok_or_else(overflow)
}
fn times(a: usize, b: usize) -> Result<usize, BudgetFailure> {
    a.checked_mul(b).ok_or_else(overflow)
}
#[derive(Default, Clone, Copy)]
pub(super) struct Shape {
    pub clauses: usize,
    pub atoms: usize,
    pub builtins: usize,
    pub args: usize,
    pub vars: usize,
    pub var_bytes: usize,
    pub builtin_bytes: usize,
}
impl Shape {
    fn node(&mut self, node: &Node) -> Result<(), BudgetFailure> {
        match node {
            Node::Var(v) => {
                self.vars = plus(self.vars, 1)?;
                self.var_bytes = plus(self.var_bytes, v.len())?;
            }
            Node::Functor(_, args) => {
                for node in args {
                    self.node(node)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    pub fn clauses<'a>(
        clauses: impl IntoIterator<Item = &'a Clause>,
    ) -> Result<Self, BudgetFailure> {
        let mut shape = Self::default();
        for clause in clauses {
            match clause {
                Clause::Rule(_) => continue,
                Clause::Triple(t) => {
                    shape.atoms = plus(shape.atoms, 1)?;
                    for n in [&t.subject, &t.predicate, &t.object] {
                        shape.node(n)?;
                    }
                }
                Clause::Builtin(b) => {
                    shape.builtins = plus(shape.builtins, 1)?;
                    shape.args = plus(shape.args, b.args.len())?;
                    shape.builtin_bytes = plus(shape.builtin_bytes, b.name.len())?;
                    for n in &b.args {
                        shape.node(n)?;
                    }
                }
            }
            shape.clauses = plus(shape.clauses, 1)?;
        }
        Ok(shape)
    }
    fn merge(self, other: Self) -> Result<Self, BudgetFailure> {
        Ok(Self {
            clauses: plus(self.clauses, other.clauses)?,
            atoms: plus(self.atoms, other.atoms)?,
            builtins: plus(self.builtins, other.builtins)?,
            args: plus(self.args, other.args)?,
            vars: plus(self.vars, other.vars)?,
            var_bytes: plus(self.var_bytes, other.var_bytes)?,
            builtin_bytes: plus(self.builtin_bytes, other.builtin_bytes)?,
        })
    }
    /// The bytes one compiled rule keeps, which accumulate over the rules, and the
    /// scratch its compilation frees again before the next rule compiles.
    fn compilation_bytes(self, head: Self, name: usize) -> Result<(usize, usize), BudgetFailure> {
        let all_vars = plus(self.vars, head.vars)?;
        // Kept in the CRule and its RuleInfo, as rule_bytes and info_bytes count them.
        let mut kept = Bytes::default();
        kept.raw(name)?; // returned CRule name
        kept.vector(self.atoms, size_of::<Atom>())?;
        kept.vector(self.builtins, size_of::<CBuiltin>())?;
        kept.vector(head.clauses, size_of::<CHead>())?;
        kept.vector(self.args, size_of::<Slot>())?;
        kept.vector(self.args, size_of::<usize>())?; // returned inputs
        kept.vector(self.args, size_of::<usize>())?; // returned outputs
        kept.vector(head.args, size_of::<Slot>())?;
        kept.vector(all_vars, size_of::<String>())?; // returned variable names
        kept.raw(plus(self.var_bytes, head.var_bytes)?)?;
        kept.vector(self.builtins, size_of::<String>())?;
        kept.raw(self.builtin_bytes)?;
        // Freed when the rule's compilation ends.
        let mut b = Bytes::default();
        b.table(all_vars, size_of::<(&str, usize)>())?; // variable interning
        b.table(self.vars, size_of::<&str>())?; // borrowed textual-bound set
        b.vector(self.args, size_of::<usize>())?; // temporary input positions
        b.vector(self.args, size_of::<usize>())?; // temporary output positions
        b.vector(all_vars, size_of::<bool>())?; // head binding check
        b.vector(all_vars, size_of::<bool>())?; // plan binding scratch
        b.vector(self.atoms, size_of::<(u32, u32)>())?;
        b.vector(plus(self.atoms, self.builtins)?, size_of::<Step>())?;
        b.vector(self.atoms, size_of::<usize>())?; // remaining atoms
        b.vector(self.builtins, size_of::<usize>())?; // remaining calls
        // A rule that fails charges its message where the compiler formats it.
        Ok((kept.0, b.0))
    }
}
#[derive(Default)]
struct Bytes(usize);
impl Bytes {
    fn raw(&mut self, n: usize) -> Result<(), BudgetFailure> {
        self.0 = plus(self.0, n)?;
        Ok(())
    }
    fn vector(&mut self, n: usize, element: usize) -> Result<(), BudgetFailure> {
        self.raw(times(n, element)?)
    }
    fn table(&mut self, n: usize, element: usize) -> Result<(), BudgetFailure> {
        // Bound before using the common table estimator's saturating arithmetic.
        let buckets = n
            .checked_mul(8)
            .and_then(|n| n.checked_add(7))
            .ok_or_else(overflow)?;
        let _ = buckets
            .checked_next_power_of_two()
            .and_then(|n| n.checked_mul(element + 1))
            .and_then(|n| n.checked_add(16))
            .ok_or_else(overflow)?;
        self.raw(table_bytes(table_capacity(n), element))
    }
}
pub(super) fn digits(mut n: usize) -> usize {
    let mut d = 1;
    while n >= 10 {
        n /= 10;
        d += 1;
    }
    d
}
pub(super) fn label_len(r: &Rule) -> Result<usize, BudgetFailure> {
    r.name.as_ref().map_or_else(
        || plus("rule@line ".len(), digits(r.line)),
        |name| Ok(name.len()),
    )
}
pub(super) fn prewalk(rules: &[Rule], backward: bool) -> Result<(usize, usize), BudgetFailure> {
    fn walk(
        r: &Rule,
        prefix: Shape,
        name: usize,
        bytes: &mut Bytes,
        emitted: &mut usize,
        scratch: &mut usize,
    ) -> Result<(), BudgetFailure> {
        let body = prefix.merge(Shape::clauses(&r.body)?)?;
        let head = Shape::clauses(&r.head)?;
        bytes.raw(name)?;
        bytes.vector(body.clauses, size_of::<&Clause>())?;
        bytes.vector(head.clauses, size_of::<&Clause>())?;
        let mut nested = 0usize;
        for clause in &r.head {
            if let Clause::Rule(inner) = clause {
                nested = plus(nested, 1)?;
                let inner_name = inner
                    .name
                    .as_ref()
                    .map_or_else(|| plus(name, plus(1, digits(nested))?), |n| Ok(n.len()))?;
                walk(inner, body, inner_name, bytes, emitted, scratch)?;
            }
        }
        if head.clauses != 0 || nested == 0 {
            *emitted = plus(*emitted, 1)?;
            let (kept, freed) = body.compilation_bytes(head, name)?;
            bytes.raw(kept)?;
            *scratch = (*scratch).max(freed);
        }
        Ok(())
    }
    let mut bytes = Bytes::default();
    let mut emitted = 0;
    // Rules compile one at a time, so only the largest scratch is live at once.
    let mut scratch = 0;
    for rule in rules {
        let label = label_len(rule)?;
        if (rule.direction == Direction::Backward && !backward)
            || rule.body.iter().any(|c| matches!(c, Clause::Rule(_)))
        {
            bytes.raw(label)?;
            continue;
        }
        walk(
            rule,
            Shape::default(),
            label,
            &mut bytes,
            &mut emitted,
            &mut scratch,
        )?;
    }
    bytes.raw(scratch)?;
    bytes.vector(emitted, size_of::<FlatRule<'_>>())?;
    bytes.vector(emitted, size_of::<CRule>())?;
    bytes.vector(emitted, size_of::<RuleInfo>())?;
    Ok((bytes.0, emitted))
}

pub(super) fn rule_bytes(rule: &CRule) -> Result<usize, BudgetFailure> {
    let mut b = Bytes::default();
    b.raw(rule.name.capacity())?;
    b.vector(rule.atoms.capacity(), size_of::<Atom>())?;
    b.vector(rule.builtins.capacity(), size_of::<CBuiltin>())?;
    b.vector(rule.head.capacity(), size_of::<CHead>())?;
    for call in &rule.builtins {
        b.vector(call.args.capacity(), size_of::<Slot>())?;
        b.vector(call.inputs.capacity(), size_of::<usize>())?;
        b.vector(call.outputs.capacity(), size_of::<usize>())?;
    }
    for head in &rule.head {
        if let CHead::Action(_, args) = head {
            b.vector(args.capacity(), size_of::<Slot>())?;
        }
    }
    Ok(b.0)
}
pub(super) fn info_bytes(info: &RuleInfo) -> Result<usize, BudgetFailure> {
    let mut b = Bytes::default();
    for names in [&info.vars, &info.builtins] {
        b.vector(names.capacity(), size_of::<String>())?;
        for name in names {
            b.raw(name.capacity())?;
        }
    }
    Ok(b.0)
}
