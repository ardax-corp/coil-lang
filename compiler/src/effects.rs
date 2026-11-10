//! Effect vocabulary shared by every effect analysis: the AST walk
//! (`typechecking::purity`), the HIR pass (`hir::effects`) and the MIR edge
//! table (`mir::effects`).
//!
//! The bits themselves are [`common::EffectFlags`]. This module holds what
//! the analyses agree on beyond them: the effects of a host or
//! virtual-module callee by name, and the length facts the loop proofs read.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use common::EffectFlags;

use crate::typechecking::{BuiltinExport, PreludeFn, VirtualModules};

/// What the loop length proofs may assume about calls (see
/// `docs/internals/limitations.md`, impure calls in counted loops).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LengthStability {
    /// User functions that cannot change the length of any array, even one
    /// they can reach. Always a superset of the pure set.
    pub fns: HashSet<String>,
    /// No `fn drop()` can change an array length. Finalizers run at
    /// allocation safepoints, so when this is false only pure calls are
    /// length-stable and allocating ops stay barriers.
    pub alloc_stable: bool,
}

/// Whether an operator on an operand of type `ty` (`a < b`, `-a`, `a += b`)
/// can run a user's trait impl. Only the built-in scalars, and tuples,
/// records, arrays, `Vec`s, `Option`s and `Result`s of them, are known not to: a
/// class or an enum may have the impl, a type parameter stands for one that
/// may, and an unknown type is taken as either.
pub(crate) fn operator_runs_user_code(ty: Option<&crate::typechecking::Ty>) -> bool {
    use crate::typechecking::Ty;
    use crate::typechecking::ty::{BOOL, BYTE, FLOAT, INT, STRING, UNIT, strip_readonly};
    let Some(ty) = ty else { return true };
    let any = |tys: &[Ty]| tys.iter().any(|t| operator_runs_user_code(Some(t)));
    match strip_readonly(ty) {
        Ty::Con(n) => !matches!(n.as_str(), INT | BYTE | FLOAT | STRING | BOOL | UNIT),
        Ty::Tuple(items) => any(items),
        Ty::Record { fields } => fields.iter().any(|(_, t)| operator_runs_user_code(Some(t))),
        Ty::Array { element, .. } => operator_runs_user_code(Some(element)),
        Ty::App(head, args) => !matches!(head.as_ref(), Ty::Con(n) if matches!(n.as_str(), "Vec" | "Option" | "Result")) || any(args),
        // `Option::None` in `o == Option::None`: typed as the variant.
        Ty::Constructor { owner, .. } => operator_runs_user_code(Some(owner)),
        Ty::Never => false,
        _ => true,
    }
}

/// Effect bits for a host / virtual-module callee (I6), by registry name
/// (`math_sin`, `fs_exists`) or by the short name a virtual module exports
/// (`exists`).
///
/// Registry names read the `effects` column of [`common::HOST_NATIVES`]. A
/// short name takes the union of every row it is exported as (`close` is both
/// `io::close` and `thread::close`). Unknown names fail closed
/// (`UNKNOWN | HOST | RESIZE`) so they are never treated as hoistable.
pub fn classify_host_name(name: &str) -> EffectFlags {
    let short = name.rsplit("::").next().unwrap_or(name);
    host_effect_table()
        .get(short)
        .copied()
        .unwrap_or(UNKNOWN_CALLEE)
}

const UNKNOWN_CALLEE: EffectFlags = EffectFlags::from_bits(
    EffectFlags::UNKNOWN | EffectFlags::HOST | EffectFlags::RESIZE,
);

fn host_effect_table() -> &'static HashMap<&'static str, EffectFlags> {
    static TABLE: OnceLock<HashMap<&'static str, EffectFlags>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table: HashMap<&'static str, EffectFlags> = HashMap::new();
        for row in common::HOST_NATIVES {
            table.insert(row.name, row.effects);
        }
        for (_, export) in VirtualModules::new().all_exports() {
            let Some(flags) = export_effects(export) else {
                continue;
            };
            let entry = table.entry(export_surface(export)).or_default();
            *entry = entry.union(flags);
        }
        // Names that are neither a host row nor a virtual export but reach
        // this table unresolved (methods and stdlib helpers called by name).
        for (name, bits) in [
            ("len", EffectFlags::HOST | EffectFlags::UNKNOWN),
            ("write_all", EffectFlags::WRITE),
            ("fd", EffectFlags::ATTACH_PARK | EffectFlags::RESIZE),
        ] {
            table.entry(name).or_insert(EffectFlags::from_bits(bits));
        }
        table
    })
}

fn export_surface(export: &BuiltinExport) -> &'static str {
    match export {
        BuiltinExport::Enum { name }
        | BuiltinExport::TypeClass { name }
        | BuiltinExport::OpaqueType { name } => name,
        BuiltinExport::FfiTag { variant } => variant,
        BuiltinExport::FfiFn { kind } => kind.as_str(),
        BuiltinExport::Fn { kind } => kind.as_str(),
        BuiltinExport::IoFn { kind } => kind.as_str(),
        BuiltinExport::StringFn { kind } => kind.as_str(),
        BuiltinExport::ThreadFn { kind } => kind.as_str(),
        BuiltinExport::GcFn { kind } => kind.as_str(),
        BuiltinExport::HostFn { surface, .. } => surface,
    }
}

/// Effects of calling a virtual-module export; `None` for types.
fn export_effects(export: &BuiltinExport) -> Option<EffectFlags> {
    let row = |registry: &str| {
        common::HOST_NATIVES
            .iter()
            .find(|n| n.name == registry)
            .map(|n| n.effects)
            .unwrap_or(UNKNOWN_CALLEE)
    };
    Some(match export {
        BuiltinExport::Enum { .. }
        | BuiltinExport::TypeClass { .. }
        | BuiltinExport::OpaqueType { .. }
        | BuiltinExport::FfiTag { .. } => return None,
        BuiltinExport::FfiFn { .. } => {
            EffectFlags::from_bits(EffectFlags::FFI | EffectFlags::RESIZE)
        }
        BuiltinExport::Fn { kind: PreludeFn::BlackBox } => row(common::BLACK_BOX_NATIVE),
        // `fn_key()` is an `int` literal by the time anything runs.
        BuiltinExport::Fn { kind: PreludeFn::FnKey } => EffectFlags::empty(),
        // `ord`, `char`, matrix helpers: compiled inline or in userland,
        // not a host row.
        BuiltinExport::Fn { .. } => UNKNOWN_CALLEE,
        BuiltinExport::IoFn { kind } => row(kind.native_name()),
        BuiltinExport::StringFn { kind } => match kind.native_name() {
            Some(registry) => row(registry),
            // `format` lowers to the FORMAT opcode. Kept impure like the
            // other text helpers (see `TEXT` in `common::host`).
            None => EffectFlags::from_bits(EffectFlags::ALLOC),
        },
        BuiltinExport::ThreadFn { kind } => row(kind.native_name()),
        BuiltinExport::GcFn { kind } => row(kind.native_name()),
        BuiltinExport::HostFn { registry, .. } => row(registry),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_host_name_math_is_pure_clocks_and_io_are_not() {
        assert!(classify_host_name("math_sin").is_pure());
        assert!(classify_host_name("packed_dot").is_pure());
        assert!(classify_host_name("simd_axpy_reduce").is_pure());
        assert!(classify_host_name("mono_nanos").contains(EffectFlags::HOST));
        assert!(classify_host_name("clock_sleep_ms").contains(EffectFlags::HOST));
        assert!(classify_host_name("write").contains(EffectFlags::IO));
        assert!(classify_host_name("gc_collect").contains(EffectFlags::GC));
        assert!(classify_host_name("invoke").contains(EffectFlags::FFI));
        assert!(!classify_host_name("mystery").is_pure());
    }

    #[test]
    fn every_virtual_export_reaches_a_host_row() {
        // A new export whose registry name has no HOST_NATIVES row would fall
        // back to UNKNOWN: give it a row (with its effects) instead.
        for (module, export) in VirtualModules::new().all_exports() {
            let registry = match export {
                BuiltinExport::IoFn { kind } => Some(kind.native_name()),
                BuiltinExport::StringFn { kind } => kind.native_name(),
                BuiltinExport::ThreadFn { kind } => Some(kind.native_name()),
                BuiltinExport::GcFn { kind } => Some(kind.native_name()),
                BuiltinExport::HostFn { registry, .. } => Some(*registry),
                _ => None,
            };
            if let Some(registry) = registry {
                assert!(
                    common::HOST_NATIVES.iter().any(|n| n.name == registry),
                    "{module}::{} -> `{registry}` has no host row",
                    export_surface(export)
                );
            }
        }
    }

    #[test]
    fn host_effects_come_from_the_table_not_the_name() {
        // `close` is exported by both `io` and `thread`: union of both rows.
        let close = classify_host_name("close");
        assert!(close.contains(EffectFlags::IO) && close.contains(EffectFlags::THREAD));
        // `io::fs` and `env` short names used to fall through to UNKNOWN.
        assert!(classify_host_name("exists").contains(EffectFlags::IO));
        assert!(!classify_host_name("exists").contains(EffectFlags::UNKNOWN));
        assert!(classify_host_name("var").contains(EffectFlags::ENV));
        assert!(classify_host_name("exit").contains(EffectFlags::EXEC));
        assert!(classify_host_name("connect").contains(EffectFlags::NET));
        assert!(classify_host_name("wait_ready").contains(EffectFlags::SUSPEND));
        assert!(!classify_host_name("wait_ready").contains(EffectFlags::IO));
        assert!(classify_host_name("remove_file").contains(EffectFlags::WRITE));
        assert!(!classify_host_name("exists").contains(EffectFlags::WRITE));
        assert!(classify_host_name("get").contains(EffectFlags::GC));
        // Same row by registry name and by surface name.
        assert_eq!(classify_host_name("fs_exists"), classify_host_name("exists"));
        assert_eq!(classify_host_name("wait_readable"), classify_host_name("await_readable"));
        // An unresolved name is unknown, even if it looks like math.
        assert_eq!(classify_host_name("log"), UNKNOWN_CALLEE);
        assert!(classify_host_name("vec_push_like").contains(EffectFlags::UNKNOWN));
    }
}
