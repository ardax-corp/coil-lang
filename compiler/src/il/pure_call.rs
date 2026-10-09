//! Matching a callee name against a set of function names (the checker's
//! pure or length-stable functions), as HIR passes look calls up.

use std::collections::HashSet;

/// Exact bind name, `$mono$` clone of a listed bind, or a single `::` suffix
/// against the AST short name.
pub(crate) fn name_in(set: &HashSet<String>, name: &str) -> bool {
    let stem = name.split("$mono$").next().unwrap_or(name);
    if set.contains(stem) {
        return true;
    }
    match stem.rsplit_once("::") {
        Some((prefix, short)) if !prefix.contains("::") => set.contains(short),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_qualified_pure_name_matches_ast_short_name() {
        let set: HashSet<String> = ["sq".to_string()].into();
        assert!(name_in(&set, "util::sq"));
        assert!(!name_in(&set, "mod::Type::sq"));
        assert!(name_in(&set, "sq$mono$3$0"));
    }
}
