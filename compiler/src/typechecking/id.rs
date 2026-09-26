//! Pre-walk [`NodeId`] minting for span-indexed type lookup.
//!
//! The pre-walk records each node's id by address (checked against its span).
//! [`Checker::infer`](super::infer::Checker::infer) and codegen key facts by
//! that exact id; the pre-order counter they also advance only stands in for
//! clones of a recorded node ([`IdTable::walk_id`]), since rules that skip or
//! revisit children make it drift.

use std::collections::HashMap;

use parser::ast::{EnumConstructPayload, EnumVariantPayload, Output};

/// Stable identifier for an AST node (minted in pre-walk visit order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

impl NodeId {
    pub fn raw(self) -> u32 {
        self.0
    }
}

/// IDs minted in pre-walk order; consumed in lockstep by inference.
#[derive(Debug, Default, Clone)]
pub struct IdTable {
    ids: Vec<NodeId>,
    /// Address of each node's `Output` → (minted id, span). Parents that
    /// rebuild child vectors free and reuse addresses, so a lookup whose span
    /// differs is a stale hit on another node and yields `None`.
    by_expr_ptr: HashMap<usize, (NodeId, usize, usize)>,
    /// Span of each minted id (index = id), to vet pre-order fallbacks.
    spans: Vec<(usize, usize)>,
    /// Next id for nodes the pre-walk never saw; outside the minted range so
    /// it neither grows [`Self::len`] nor aliases a real node.
    next_synthetic: u32,
}

impl IdTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self) -> NodeId {
        let id = NodeId(self.ids.len() as u32);
        self.ids.push(id);
        id
    }

    pub fn record_output(&mut self, node: &Output<'_>, id: NodeId) {
        let idx = id.0 as usize;
        if self.spans.len() <= idx {
            self.spans.resize(idx + 1, (usize::MAX, usize::MAX));
        }
        self.spans[idx] = (node.0.start, node.0.end);
        self.by_expr_ptr.insert(
            std::ptr::from_ref(node) as *const Output<'_> as usize,
            (id, node.0.start, node.0.end),
        );
    }

    /// Id for `node` in a pre-order walk: its recorded id, else the pre-order
    /// `seq` id when that id was minted for the same span (a clone of that
    /// node). `None` means `seq` belongs to a different node.
    pub fn walk_id(&self, node: &Output<'_>, seq: Option<NodeId>) -> Option<NodeId> {
        self.id_of_output(node).or_else(|| {
            seq.filter(|s| self.spans.get(s.0 as usize) == Some(&(node.0.start, node.0.end)))
        })
    }

    /// [`Self::walk_id`], minting a synthetic id instead of `None` so facts
    /// about an unrecorded node never overwrite another node's.
    pub fn resolve_walk_id(&mut self, node: &Output<'_>, seq: NodeId) -> NodeId {
        if let Some(id) = self.walk_id(node, Some(seq)) {
            return id;
        }
        let id = NodeId((1 << 31) + self.next_synthetic);
        self.next_synthetic += 1;
        id
    }

    pub fn id_of_output(&self, node: &Output<'_>) -> Option<NodeId> {
        let ptr = std::ptr::from_ref(node) as *const Output<'_> as usize;
        match self.by_expr_ptr.get(&ptr) {
            Some(&(id, start, end)) if start == node.0.start && end == node.0.end => Some(id),
            _ => None,
        }
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn ids(&self) -> &[NodeId] {
        &self.ids
    }
}

/// Pre-order walk: mint one ID per node, then recurse into children.
pub fn pre_walk(node: &Output, table: &mut IdTable) {
    let id = table.push();
    table.record_output(node, id);
    walk_children(node, &mut |child| pre_walk(child, table));
}

/// Call `visit` on each direct child expression of `node`, in pre-order.
pub fn walk_children<'s>(node: &Output<'s>, visit: &mut dyn FnMut(&Output<'s>)) {
    use parser::ast::Expression;
    match node.1.as_ref() {
        Expression::Noop(_)
        | Expression::Comment(_)
        | Expression::Integer(_)
        | Expression::Float(_)
        | Expression::String(_)
        | Expression::Bool(_)
        | Expression::Identifier(_)
        | Expression::Type(_)
        | Expression::Default(_)
        | Expression::Break
        | Expression::Continue
        | Expression::Use { .. }
        | Expression::Module(_, _)
        | Expression::Variable(_, _)
        | Expression::Constant(_, _)
        | Expression::Field { .. }
        | Expression::QualifiedAccess { .. } => {}

        Expression::Argument { ty, .. } => {
            if let Some(t) = ty {
                visit(t);
            }
        }

        Expression::Spread(inner) => visit(inner),

        Expression::TypeFnSig { params, ret } => {
            visit(params);
            visit(ret);
        }

        Expression::AttrDecl {
            docs: _,
            args,
            returns,
            body,
            ..
        } => {
            visit(args);
            if let Some(ret) = returns {
                visit(ret);
            }
            visit(body);
        }

        Expression::LetDestructure { rhs, .. } => visit(rhs),

        Expression::NamedArg(_, value) => visit(value),

        Expression::TypeApp { args, .. } => {
            for a in args {
                visit(a);
            }
        }

        Expression::TypeFun(arg, ret) => {
            visit(arg);
            visit(ret);
        }

        Expression::Expr(e)
        | Expression::Group(e)
        | Expression::Statement(e)
        | Expression::ExprStatement(e)
        | Expression::Return(e)
        | Expression::ImplicitReturn(e)
        | Expression::Raise(e)
        | Expression::Panic(e)
        | Expression::TypeOf(e)
        | Expression::Try(e)
        | Expression::Yield(e)
        | Expression::YieldFrom(e)
        | Expression::Negate(e)
        | Expression::Not(e)
        | Expression::LogicalNot(e)
        | Expression::Positive(e)
        | Expression::Adjust { target: e, .. }
        | Expression::Member(e) => visit(e),
        Expression::Defer { body, .. } => visit(body),

        Expression::CompoundAssign(name, _, value) => {
            visit(name);
            visit(value);
        }

        Expression::Assignment(name, value) => {
            visit(name);
            visit(value);
        }

        Expression::Add(l, r)
        | Expression::Sub(l, r)
        | Expression::Mul(l, r)
        | Expression::Div(l, r)
        | Expression::Mod(l, r)
        | Expression::Pow(l, r)
        | Expression::Shl(l, r)
        | Expression::Shr(l, r)
        | Expression::Xor(l, r)
        | Expression::And(l, r)
        | Expression::Or(l, r)
        | Expression::BitAnd(l, r)
        | Expression::BitOr(l, r)
        | Expression::Eq(l, r)
        | Expression::Neq(l, r)
        | Expression::Le(l, r)
        | Expression::Gt(l, r)
        | Expression::Leq(l, r)
        | Expression::Geq(l, r)
        | Expression::Coalesce(l, r) => {
            visit(l);
            visit(r);
        }
        Expression::Cast(expr, ty) => {
            visit(expr);
            visit(ty);
        }
        Expression::Range { start, end, .. } => {
            visit(start);
            visit(end);
        }

        Expression::Resume(target, arg) => {
            visit(target);
            if let Some(a) = arg {
                visit(a);
            }
        }

        Expression::Block(cs)
        | Expression::Program(cs)
        | Expression::Fragment(cs)
        | Expression::List(cs)
        | Expression::Declare(cs)
        | Expression::Invoke(cs) => {
            for c in cs {
                visit(c);
            }
        }
        Expression::Dload(path) => visit(path),
        Expression::Done(handle) => visit(handle),
        Expression::Tuple(items) => {
            for c in items {
                visit(c);
            }
        }
        Expression::Array(items) => {
            for c in items {
                visit(c);
            }
        }
        Expression::Index(target, index) => {
            visit(target);
            if let Some(index) = index {
                visit(index);
            }
        }
        Expression::Readonly(inner) => visit(inner),
        Expression::StaticDecl { ty, init, .. } => {
            if let Some(ty) = ty {
                visit(ty);
            }
            visit(init);
        }
        Expression::Dict(fields) => {
            for f in fields {
                visit(&f.value);
            }
        }
        Expression::If(branches) => {
            for b in branches {
                visit(b);
            }
        }
        Expression::Implementation { methods, .. } => {
            for m in methods {
                visit(m);
            }
        }
        Expression::Class { fields, .. } => {
            for f in fields {
                visit(f);
            }
        }

        Expression::Function { args, body, .. } => {
            visit(args);
            if let Some(body) = body {
                visit(body);
            }
        }
        Expression::Lambda { args, body, .. } => {
            visit(args);
            visit(body);
        }
        Expression::TestCase { name, body } => {
            visit(name);
            visit(body);
        }

        Expression::Branch(cond, body) => {
            if let Some(c) = cond {
                visit(c);
            }
            visit(body);
        }

        Expression::Call { name, args } => {
            visit(name);
            if let Some(a) = args {
                for arg in a {
                    visit(arg);
                }
            }
        }

        Expression::Loop {
            iterable,
            body,
            identifier,
            pattern: _,
        } => {
            // For-in binds `identifier` before the body; visit order must
            // match infer (iterable → binding → body). Pattern for-in has
            // no Identifier node.
            visit(iterable);
            if let Some(i) = identifier {
                visit(i);
            }
            visit(body);
        }

        // Patterns have no NodeId; walk bodies only (lockstep with infer).
        Expression::Match { scrutinee, arms } => {
            visit(scrutinee);
            for arm in arms {
                visit(&arm.body);
            }
        }

        Expression::IfLet {
            scrutinee,
            then_arm,
            else_arm,
        } => {
            visit(scrutinee);
            visit(&then_arm.body);
            visit(&else_arm.body);
        }

        Expression::WhileLet {
            scrutinee,
            then_arm,
            on_miss,
        } => {
            visit(scrutinee);
            visit(&then_arm.body);
            visit(&on_miss.body);
        }

        Expression::EnumDecl { variants, .. } => {
            for v in variants {
                visit(v);
            }
        }
        Expression::TypeAlias { ty, .. } => {
            visit(ty);
        }
        Expression::ExternBlock { declarations, .. } => {
            for decl in declarations {
                visit(&decl.args);
                if let Some(ret) = &decl.returns {
                    visit(ret);
                }
            }
        }
        Expression::ExternStruct(decl) => {
            for (_, ty) in &decl.fields {
                visit(ty);
            }
        }
        Expression::EnumVariant { payload, .. } => match payload {
            EnumVariantPayload::Unit => {}
            EnumVariantPayload::Tuple(parts) => {
                for p in parts {
                    visit(p);
                }
            }
            EnumVariantPayload::Record(fields) => {
                for f in fields {
                    visit(&f.value);
                }
            }
        },
        Expression::Construct { fields, .. } => match fields {
            EnumConstructPayload::Unit => {}
            EnumConstructPayload::Tuple(args) => {
                for arg in args {
                    visit(arg);
                }
            }
            EnumConstructPayload::Record(parts) => {
                for p in parts {
                    visit(&p.value);
                }
            }
        },

        Expression::Method(_, body) => visit(body),

        Expression::Access(receiver, _) | Expression::OptionalAccess(receiver, _) => {
            visit(receiver)
        }

        Expression::Instantiate(class, args) => {
            visit(class);
            if let Some(a) = args {
                for arg in a {
                    visit(arg);
                }
            }
        }

        // New generic-system nodes — no ID-table children needed yet.
        Expression::Forall { ty, .. } => visit(ty),
        Expression::TypeClass { methods, .. } => {
            for m in methods {
                visit(m);
            }
        }
        Expression::TypeClassImpl { args, methods, .. } => {
            // Walk type-annotation args so NodeId counters match infer.rs's
            // `self.infer(a)` calls for each arg.
            for a in args {
                visit(a);
            }
            for m in methods {
                visit(m);
            }
        }
        Expression::AssocTypeDecl { .. } => {}
        Expression::AssocTypeDef { ty, .. } => {
            visit(ty);
        }
        Expression::TypeProjection { args, .. } => {
            for arg in args {
                visit(arg);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parser::Pratt;

    fn count_nodes(src: &str) -> usize {
        let ast = Pratt::default().parse(src).expect("parse failed");
        let mut table = IdTable::new();
        pre_walk(&ast, &mut table);
        table.len()
    }

    #[test]
    fn pre_walk_mints_one_id_per_node_for_simple_expr() {
        assert_eq!(count_nodes("1 + 2;"), 7);
    }

    #[test]
    fn pre_walk_assigns_unique_ids_in_visit_order() {
        let ast = Pratt::default().parse("42;").expect("parse failed");
        let mut table = IdTable::new();
        pre_walk(&ast, &mut table);
        let ids = table.ids();
        for pair in ids.windows(2) {
            assert_ne!(pair[0], pair[1]);
        }
        assert_eq!(ids[0], NodeId(0));
        assert_eq!(ids[ids.len() - 1], NodeId((ids.len() - 1) as u32));
    }

    #[test]
    fn pre_walk_handles_shared_spans_with_distinct_ids() {
        let ast = Pratt::default().parse("42;").expect("parse failed");
        let mut table = IdTable::new();
        pre_walk(&ast, &mut table);
        assert!(table.len() >= 3);
        let mut seen = std::collections::HashSet::new();
        for id in table.ids() {
            assert!(seen.insert(*id), "duplicate ID: {:?}", id);
        }
    }
}
