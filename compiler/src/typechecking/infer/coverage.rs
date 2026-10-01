//! Match usefulness over [`CoverageTree`] rows (Maranget, "Warnings for
//! pattern matching"). An arm is reachable when it is useful against the
//! arms above it; a match without a catch-all is exhaustive when `_` is not
//! useful against all of its arms. Nested literals and constructors count:
//! `Some(200)` does not cover `Some(404)`.

use std::collections::BTreeMap;

/// One pattern's shape for usefulness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CoverageTree {
    /// Wildcard, binding, or anything that matches every value.
    Any,
    /// Integer literal.
    Int(i64),
    /// Enum variant `tag` of an enum with `variants` variants; payload
    /// sub-patterns in declaration order.
    Ctor {
        tag: u32,
        variants: u32,
        fields: Vec<CoverageTree>,
    },
}

static ANY: CoverageTree = CoverageTree::Any;

/// Whether some value matched by `q` is matched by none of `rows`.
pub(super) fn useful(rows: &[&CoverageTree], q: &CoverageTree) -> bool {
    let rows: Vec<Vec<&CoverageTree>> = rows.iter().map(|r| vec![*r]).collect();
    useful_vec(&rows, &[q])
}

fn useful_vec<'a>(rows: &[Vec<&'a CoverageTree>], q: &[&'a CoverageTree]) -> bool {
    let Some((head, rest)) = q.split_first() else {
        return rows.is_empty();
    };
    match head {
        CoverageTree::Ctor { tag, fields, .. } => {
            let spec = specialize(rows, *tag, fields.len());
            let q: Vec<&CoverageTree> = fields.iter().chain(rest.iter().copied()).collect();
            useful_vec(&spec, &q)
        }
        CoverageTree::Int(n) => {
            let spec: Vec<Vec<&CoverageTree>> = rows
                .iter()
                .filter(|r| match r[0] {
                    CoverageTree::Any => true,
                    CoverageTree::Int(m) => m == n,
                    CoverageTree::Ctor { .. } => false,
                })
                .map(|r| r[1..].to_vec())
                .collect();
            useful_vec(&spec, rest)
        }
        CoverageTree::Any => {
            // Head variants of the column, with their arities.
            let mut heads: BTreeMap<u32, usize> = BTreeMap::new();
            let mut variants = 0;
            for r in rows {
                if let CoverageTree::Ctor {
                    tag,
                    variants: n,
                    fields,
                } = r[0]
                {
                    heads.entry(*tag).or_insert(fields.len());
                    variants = *n;
                }
            }
            if !heads.is_empty() && heads.len() as u32 == variants {
                // Every variant heads some row: `_` is useful under one of them.
                heads.iter().any(|(&tag, &arity)| {
                    let spec = specialize(rows, tag, arity);
                    let q: Vec<&CoverageTree> = std::iter::repeat_n(&ANY, arity)
                        .chain(rest.iter().copied())
                        .collect();
                    useful_vec(&spec, &q)
                })
            } else {
                // A variant (or every other literal) is missing: only rows
                // with `_` in this column can cover it.
                let default: Vec<Vec<&CoverageTree>> = rows
                    .iter()
                    .filter(|r| matches!(r[0], CoverageTree::Any))
                    .map(|r| r[1..].to_vec())
                    .collect();
                useful_vec(&default, rest)
            }
        }
    }
}

/// Rows that match variant `tag`, its payload spliced in front.
fn specialize<'a>(
    rows: &[Vec<&'a CoverageTree>],
    tag: u32,
    arity: usize,
) -> Vec<Vec<&'a CoverageTree>> {
    rows.iter()
        .filter_map(|r| match r[0] {
            CoverageTree::Ctor { tag: t, fields, .. } if *t == tag => {
                Some(fields.iter().chain(r[1..].iter().copied()).collect())
            }
            CoverageTree::Any => Some(
                std::iter::repeat_n(&ANY, arity)
                    .chain(r[1..].iter().copied())
                    .collect(),
            ),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn some(p: CoverageTree) -> CoverageTree {
        CoverageTree::Ctor {
            tag: 1,
            variants: 2,
            fields: vec![p],
        }
    }

    const NONE: CoverageTree = CoverageTree::Ctor {
        tag: 0,
        variants: 2,
        fields: Vec::new(),
    };

    #[test]
    fn nested_literals_do_not_cover_the_payload() {
        let a = some(CoverageTree::Int(200));
        let b = some(CoverageTree::Int(404));
        assert!(useful(&[&a], &b));
        assert!(!useful(&[&a, &b], &some(CoverageTree::Int(200))));
        assert!(useful(&[&a, &b, &NONE], &CoverageTree::Any));
        let rest = some(CoverageTree::Any);
        assert!(!useful(&[&a, &b, &rest, &NONE], &CoverageTree::Any));
        assert!(!useful(&[&rest], &a), "Some(_) subsumes Some(200)");
    }
}
