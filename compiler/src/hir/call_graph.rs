//! The call graph among a module's functions, from the calls HIR resolved.

use std::collections::{HashMap, HashSet};

use super::effects::{def_of, local_name};
use super::{BodyKind, Callee, HirKind, HirModule};
use crate::typechecking::infer::Checker;

impl HirModule {
    /// Short names of the functions on a call cycle, alone or with others:
    /// the only sibling tail calls codegen turns into jumps.
    ///
    /// A node is a `fn` body and an edge is a call by name from that body.
    /// Methods are not nodes, and a call inside a lambda is the lambda's.
    /// The overloads of one name are told apart only by their defs, so a
    /// call that names several counts for each.
    pub fn recursive_fns(&self, checker: &Checker, module_path: &str) -> HashSet<String> {
        let current = checker.current_module_name() == module_path;
        let mut by_def: HashMap<_, Vec<usize>> = HashMap::new();
        let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, body) in self.bodies.iter().enumerate() {
            if body.kind != BodyKind::Function {
                continue;
            }
            if let Some(def) = def_of(checker, module_path, &body.name) {
                by_def.entry(def).or_default().push(i);
            }
            by_name.entry(body.name.as_str()).or_default().push(i);
            if let Some(local) = local_name(module_path, &body.name)
                && local != body.name
            {
                by_name.entry(local).or_default().push(i);
            }
        }
        let mut edges: Vec<Vec<usize>> = vec![Vec::new(); self.bodies.len()];
        for (i, body) in self.bodies.iter().enumerate() {
            if body.kind != BodyKind::Function {
                continue;
            }
            for e in &body.exprs {
                let HirKind::Call { callee: Callee::Named { name, def, .. }, .. } = &e.kind else { continue };
                let def = def.or_else(|| current.then(|| checker.def_id_of(name)).flatten());
                let found = def.and_then(|d| by_def.get(&d)).or_else(|| by_name.get(name.as_str()));
                edges[i].extend(found.into_iter().flatten().copied());
            }
        }
        let mut out = HashSet::new();
        for scc in sccs(&edges) {
            if scc.len() > 1 || edges[scc[0]].contains(&scc[0]) {
                for i in scc {
                    let name = &self.bodies[i].name;
                    out.insert(name.rsplit("::").next().unwrap_or(name).to_string());
                }
            }
        }
        out
    }
}

/// The strongly connected components of a graph given as each node's
/// successors (Tarjan's algorithm, without recursion: a call chain can be
/// as long as the module).
fn sccs(edges: &[Vec<usize>]) -> Vec<Vec<usize>> {
    const UNSEEN: usize = usize::MAX;
    let n = edges.len();
    let mut index = vec![UNSEEN; n];
    let mut low = vec![0; n];
    let mut on_stack = vec![false; n];
    let mut stack = Vec::new();
    let mut next = 0;
    let mut out = Vec::new();
    for root in 0..n {
        if index[root] != UNSEEN {
            continue;
        }
        // (node, how many of its successors were looked at).
        let mut work = vec![(root, 0)];
        while let Some(&mut (v, ref mut at)) = work.last_mut() {
            if *at == 0 {
                index[v] = next;
                low[v] = next;
                next += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            if let Some(&w) = edges[v].get(*at) {
                *at += 1;
                if index[w] == UNSEEN {
                    work.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            work.pop();
            if let Some(&(parent, _)) = work.last() {
                low[parent] = low[parent].min(low[v]);
            }
            if low[v] == index[v] {
                let mut scc = Vec::new();
                while let Some(w) = stack.pop() {
                    on_stack[w] = false;
                    scc.push(w);
                    if w == v {
                        break;
                    }
                }
                out.push(scc);
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "call_graph.tests.rs"]
mod tests;
