//! Pre-walk [`NodeId`] minting for span-indexed type lookup.
//!
//! The pre-walk and [`Checker::infer`](super::infer::Checker::infer) both
//! visit the AST in pre-order, so the n-th infer call consumes the n-th ID.

use std::collections::HashMap;

use parser::ast::{EnumConstructPayload, EnumVariantPayload, Expression, Output, Pattern, PatternPayload};

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
    /// Heap pointer of each node's `Expression` → minted id (stable for the
    /// AST lifetime). Lets emit look up sidecar facts without source spans.
    by_expr_ptr: HashMap<usize, NodeId>,
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
        self.by_expr_ptr
            .insert(std::ptr::from_ref(node) as *const Output<'_> as usize, id);
    }

    pub fn id_of_ptr(&self, ptr: usize) -> Option<NodeId> {
        self.by_expr_ptr.get(&ptr).copied()
    }

    pub fn id_of_expr(&self, expr: &Expression<'_>) -> Option<NodeId> {
        self.id_of_ptr(std::ptr::from_ref(expr) as *const Expression<'_> as usize)
    }

    pub fn id_of_output(&self, node: &Output<'_>) -> Option<NodeId> {
        self.id_of_ptr(std::ptr::from_ref(node) as *const Output<'_> as usize)
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
    pre_walk_children(node, table);
}

fn pre_walk_children(node: &Output, table: &mut IdTable) {
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
                pre_walk(t, table);
            }
        }

        Expression::Spread(inner) => pre_walk(inner, table),

        Expression::TypeFnSig { params, ret } => {
            pre_walk(params, table);
            pre_walk(ret, table);
        }

        Expression::AttrDecl {
            docs: _,
            args,
            returns,
            body,
            ..
        } => {
            pre_walk(args, table);
            if let Some(ret) = returns {
                pre_walk(ret, table);
            }
            pre_walk(body, table);
        }

        Expression::LetDestructure { rhs, .. } => pre_walk(rhs, table),

        Expression::NamedArg(_, value) => pre_walk(value, table),

        Expression::TypeApp { args, .. } => {
            for a in args {
                pre_walk(a, table);
            }
        }

        Expression::TypeFun(arg, ret) => {
            pre_walk(arg, table);
            pre_walk(ret, table);
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
        | Expression::Member(e) => pre_walk(e, table),
        Expression::Defer { body, .. } => pre_walk(body, table),

        Expression::CompoundAssign(name, _, value) => {
            pre_walk(name, table);
            pre_walk(value, table);
        }

        Expression::Assignment(name, value) => {
            pre_walk(name, table);
            pre_walk(value, table);
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
            pre_walk(l, table);
            pre_walk(r, table);
        }
        Expression::Cast(expr, ty) => {
            pre_walk(expr, table);
            pre_walk(ty, table);
        }
        Expression::Range { start, end, .. } => {
            pre_walk(start, table);
            pre_walk(end, table);
        }

        Expression::Resume(target, arg) => {
            pre_walk(target, table);
            if let Some(a) = arg {
                pre_walk(a, table);
            }
        }

        Expression::Block(cs)
        | Expression::Program(cs)
        | Expression::Fragment(cs)
        | Expression::List(cs)
        | Expression::Declare(cs)
        | Expression::Invoke(cs) => {
            for c in cs {
                pre_walk(c, table);
            }
        }
        Expression::Dload(path) => pre_walk(path, table),
        Expression::Done(handle) => pre_walk(handle, table),
        Expression::Tuple(items) => {
            for c in items {
                pre_walk(c, table);
            }
        }
        Expression::Array(items) => {
            for c in items {
                pre_walk(c, table);
            }
        }
        Expression::Index(target, index) => {
            pre_walk(target, table);
            if let Some(index) = index {
                pre_walk(index, table);
            }
        }
        Expression::Readonly(inner) => pre_walk(inner, table),
        Expression::StaticDecl { ty, init, .. } => {
            if let Some(ty) = ty {
                pre_walk(ty, table);
            }
            pre_walk(init, table);
        }
        Expression::Dict(fields) => {
            for f in fields {
                pre_walk(&f.value, table);
            }
        }
        Expression::If(branches) => {
            for b in branches {
                pre_walk(b, table);
            }
        }
        Expression::Implementation { methods, .. } => {
            for m in methods {
                pre_walk(m, table);
            }
        }
        Expression::Class { fields, .. } => {
            for f in fields {
                pre_walk(f, table);
            }
        }

        Expression::Function { args, body, .. } => {
            pre_walk(args, table);
            if let Some(body) = body {
                pre_walk(body, table);
            }
        }
        Expression::Lambda { args, body, .. } => {
            pre_walk(args, table);
            pre_walk(body, table);
        }
        Expression::TestCase { name, body } => {
            pre_walk(name, table);
            pre_walk(body, table);
        }

        Expression::Branch(cond, body) => {
            if let Some(c) = cond {
                pre_walk(c, table);
            }
            pre_walk(body, table);
        }

        Expression::Call { name, args } => {
            pre_walk(name, table);
            if let Some(a) = args {
                for arg in a {
                    pre_walk(arg, table);
                }
            }
        }

        Expression::Loop {
            iterable,
            body,
            identifier,
        } => {
            // For-in binds `identifier` before the body; visit order must
            // match infer (iterable → binding → body).
            pre_walk(iterable, table);
            if let Some(i) = identifier {
                pre_walk(i, table);
            }
            pre_walk(body, table);
        }

        // Patterns have no NodeId; walk bodies only (lockstep with infer).
        Expression::Match { scrutinee, arms } => {
            pre_walk(scrutinee, table);
            for arm in arms {
                pre_walk_pattern(&arm.pattern.1, table);
                pre_walk(&arm.body, table);
            }
        }

        Expression::EnumDecl { variants, .. } => {
            for v in variants {
                pre_walk(v, table);
            }
        }
        Expression::TypeAlias { ty, .. } => {
            pre_walk(ty, table);
        }
        Expression::ExternBlock { declarations, .. } => {
            for decl in declarations {
                pre_walk(&decl.args, table);
                if let Some(ret) = &decl.returns {
                    pre_walk(ret, table);
                }
            }
        }
        Expression::ExternStruct(decl) => {
            for (_, ty) in &decl.fields {
                pre_walk(ty, table);
            }
        }
        Expression::EnumVariant { payload, .. } => match payload {
            EnumVariantPayload::Unit => {}
            EnumVariantPayload::Tuple(parts) => {
                for p in parts {
                    pre_walk(p, table);
                }
            }
            EnumVariantPayload::Record(fields) => {
                for f in fields {
                    pre_walk(&f.value, table);
                }
            }
        },
        Expression::Construct { fields, .. } => match fields {
            EnumConstructPayload::Unit => {}
            EnumConstructPayload::Tuple(args) => {
                for arg in args {
                    pre_walk(arg, table);
                }
            }
            EnumConstructPayload::Record(parts) => {
                for p in parts {
                    pre_walk(&p.value, table);
                }
            }
        },

        Expression::Method(_, body) => pre_walk(body, table),

        Expression::Access(receiver, _) | Expression::OptionalAccess(receiver, _) => {
            pre_walk(receiver, table)
        }

        Expression::Instantiate(class, args) => {
            pre_walk(class, table);
            if let Some(a) = args {
                for arg in a {
                    pre_walk(arg, table);
                }
            }
        }

        // New generic-system nodes — no ID-table children needed yet.
        Expression::Forall { ty, .. } => pre_walk(ty, table),
        Expression::TypeClass { methods, .. } => {
            for m in methods {
                pre_walk(m, table);
            }
        }
        Expression::TypeClassImpl { args, methods, .. } => {
            // Walk type-annotation args so NodeId counters match infer.rs's
            // `self.infer(a)` calls for each arg.
            for a in args {
                pre_walk(a, table);
            }
            for m in methods {
                pre_walk(m, table);
            }
        }
        Expression::AssocTypeDecl { .. } => {}
        Expression::AssocTypeDef { ty, .. } => {
            pre_walk(ty, table);
        }
        Expression::TypeProjection { args, .. } => {
            for arg in args {
                pre_walk(arg, table);
            }
        }
    }
}

/// Structural walk over patterns (no NodeIds).
pub fn pre_walk_pattern(pattern: &Pattern, _table: &mut IdTable) {
    match pattern {
        Pattern::Wildcard | Pattern::Default | Pattern::Binding { .. } | Pattern::Integer(_) => {}
        Pattern::Constructor { payload, .. } => match payload {
            PatternPayload::Unit => {}
            PatternPayload::Tuple(parts) => {
                for p in parts {
                    pre_walk_pattern(&p.1, _table);
                }
            }
            PatternPayload::Record(fields) => {
                for pf in fields {
                    pre_walk_pattern(&pf.pattern.1, _table);
                }
            }
        },
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
