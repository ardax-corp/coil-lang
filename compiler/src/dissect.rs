//! In-memory program dissection for DX (`coil dissect`).
//!
//! What `coil dissect` and the debugger read of a compile, by function
//! name. Writes no archive.

use std::fmt::Write as _;

use common::ProgramDebug;

/// A body's symbol (name, entry and debug locals).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FnSym {
    pub name: String,
    pub body_id: u32,
    /// User-facing locals/params `(name, slot)`, sorted by slot.
    pub locals: Vec<(String, u32)>,
    /// Scoped, typed debug variables with location lists.
    pub vars: Vec<crate::debug_vars::DebugVar>,
}

/// Artifacts from an in-memory dissect compile.
#[derive(Clone)]
pub struct DissectArtifacts {
    pub strings: Vec<String>,
    pub functions: Vec<FnSym>,
    pub debug: ProgramDebug,
    /// Class fields and enum variants for rendering heap values.
    pub classes: crate::debug_vars::DebugClassTable,
    pub enums: crate::debug_vars::DebugEnumTable,
}

/// Case-insensitive FQN match: exact, substring, trailing segment, or `name#N` overload.
pub fn matches_fn_pat(name: &str, pat: &str) -> bool {
    let name_l = name.to_ascii_lowercase();
    let pat_l = pat.to_ascii_lowercase();
    if pat_l.is_empty() {
        return true;
    }
    if name_l == pat_l || name_l.contains(&pat_l) {
        return true;
    }
    let seg = name_l.rsplit("::").next().unwrap_or(&name_l);
    if seg == pat_l {
        return true;
    }
    if let Some(rest) = seg.strip_prefix(&pat_l) {
        return rest.starts_with('#');
    }
    false
}

/// Format the symbol index (name and body identity).
pub fn format_symbol_index(functions: &[FnSym]) -> String {
    let mut syms = functions.to_vec();
    syms.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out = String::from(";; symbols\n");
    for s in &syms {
        let _ = writeln!(out, "{:>6}  {}", s.body_id, s.name);
    }
    out
}
