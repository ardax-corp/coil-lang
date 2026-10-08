//! Decision trees for `match` (HIR phase 7): rows grouped by their
//! outermost constructor, so a nested match tests the scrutinee's tag once
//! and then only the sub-patterns of the rows with that tag.
//!
//! Rows with different outer tags are disjoint and tests have no effects
//! (Coil has no guards), so the order across groups is free; within a group
//! the rows keep their source order.

use super::{HirArm, HirPat, HirPatFields};

/// Grouping is on unless `COIL_HIR_MATCH_TREE=0` (or `false` / `off` / `no`).
pub(crate) fn tree_from_env() -> bool {
    !matches!(
        std::env::var("COIL_HIR_MATCH_TREE").as_deref(),
        Ok("0" | "false" | "off" | "no")
    )
}

/// The rows of one outer variant, in source order.
#[derive(Debug)]
pub struct Group<'a> {
    pub enum_name: &'a str,
    pub variant: &'a str,
    /// Each row's sub-patterns and its arm.
    pub rows: Vec<(&'a HirPatFields, &'a HirArm)>,
}

/// A match split by outer variant, plus the trailing catch-all arm.
#[derive(Debug)]
pub struct OuterTree<'a> {
    pub groups: Vec<Group<'a>>,
    pub catch_all: Option<&'a HirArm>,
}

/// Group `arms` (already cut after the first catch-all) by outer variant.
/// `None` when grouping saves nothing: a top-level pattern that is not a
/// variant, or no variant with two rows.
pub fn outer_groups(arms: &[HirArm]) -> Option<OuterTree<'_>> {
    let mut groups: Vec<Group<'_>> = Vec::new();
    let mut catch_all = None;
    for (i, arm) in arms.iter().enumerate() {
        match &arm.pat {
            HirPat::Variant {
                enum_name,
                variant,
                fields,
                ..
            } => {
                let row = (fields, arm);
                match groups.iter_mut().find(|g| g.variant == variant.as_str()) {
                    // Rows after one whose sub-patterns all match never run.
                    Some(g) if g.rows.iter().any(|(f, _)| irrefutable(f)) => {}
                    Some(g) => g.rows.push(row),
                    None => groups.push(Group {
                        enum_name,
                        variant,
                        rows: vec![row],
                    }),
                }
            }
            HirPat::Wild | HirPat::Bind(_) if i + 1 == arms.len() => catch_all = Some(arm),
            _ => return None,
        }
    }
    groups.iter().any(|g| g.rows.len() > 1).then_some(OuterTree { groups, catch_all })
}

/// Every sub-pattern matches any value.
pub fn irrefutable(fields: &HirPatFields) -> bool {
    let any = |p: &HirPat| matches!(p, HirPat::Wild | HirPat::Bind(_));
    match fields {
        HirPatFields::Unit => true,
        HirPatFields::Tuple(parts) => parts.iter().all(any),
        HirPatFields::Record(named) => named.iter().all(|(_, p)| any(p)),
    }
}

#[cfg(test)]
#[path = "match_tree.tests.rs"]
mod tests;
