//! Matching a callee name against a set of function names (the pure or
//! length-stable functions of the module being compiled), as HIR passes
//! look calls up.

use std::collections::HashSet;

/// Whether a call of `name` runs a listed body: `name` as the set lists it
/// (`f`, `Owner::m`, `module::f`), or its `$mono$` clone. A name is never
/// matched by its last segment alone: `other::f` and `Other::m` are not
/// this module's `f` and `m` (#2436).
pub(crate) fn name_in(set: &HashSet<String>, name: &str) -> bool {
    set.contains(name.split("$mono$").next().unwrap_or(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_matches_itself_and_its_mono_clones_only() {
        let set: HashSet<String> = ["sq".to_string(), "util::cube".to_string()].into();
        assert!(name_in(&set, "sq"));
        assert!(name_in(&set, "sq$mono$3$0"));
        assert!(name_in(&set, "util::cube"));
        assert!(!name_in(&set, "util::sq"));
        assert!(!name_in(&set, "Type::sq"));
        assert!(!name_in(&set, "cube"));
    }
}
