//! Host natives native code runs as Coil twins.
//!
//! The VM keeps these natives in the runtime. A native build calls their
//! twin instead, a Coil function in the embedded `native_twins` module
//! (`prelude/twins.coil`), so the native need not be in `libcoil_rt`.
//! While a native capture is open the compiler builds that module and
//! keeps its functions live; [`apply`] then turns each captured
//! `HostInvoke` of a native below into a `Call` of its twin. A twin returns
//! the word its native returns (no host enum layout: only natives whose
//! result is a plain word are listed).
//!
//! A `HostInvoke` of a native with neither a twin nor a direct host stays
//! one, and coil-native refuses it, so a gap shows in the native inventory.

use std::collections::HashMap;

use super::{MirFunc, MirInst};
use crate::bodies::Label;

/// `(host native, twin)`: the twin is `native_twins::<twin>`.
pub const NATIVE_TWINS: &[(&str, &str)] = &[
    ("hash_string", "hash_string"),
    (common::STRING_COMPARE_NATIVE, "string_compare"),
];

/// The compiler's function name of twin `twin`.
pub fn twin_fn_name(twin: &str) -> String {
    format!("{}::{twin}", crate::macros::TWINS_MODULE)
}

/// Every twin's function name (the compile keeps them live while a
/// native capture is open: no bytecode calls them).
pub fn twin_fn_names() -> impl Iterator<Item = String> {
    NATIVE_TWINS.iter().map(|&(_, twin)| twin_fn_name(twin))
}

/// Host id → twin function name, for the natives above.
fn twin_targets() -> HashMap<u16, String> {
    NATIVE_TWINS
        .iter()
        .map(|&(native, twin)| {
            let id = common::host_native_id(native).unwrap_or_else(|| panic!("native twin of unknown host native `{native}`"));
            (id as u16, twin_fn_name(twin))
        })
        .collect()
}

/// Rewrite each `HostInvoke` of a twinned native in `bodies` to a `Call`
/// of its twin, whose entry `entries` names. A twin's own body is left as
/// written; a twin the compile did not build leaves the call a
/// `HostInvoke`.
pub fn apply<'a>(bodies: impl Iterator<Item = (&'a str, &'a mut MirFunc)>, entries: &HashMap<String, Label>) {
    let targets: HashMap<u16, Label> = twin_targets()
        .into_iter()
        .filter_map(|(id, name)| entries.get(&name).map(|&label| (id, label)))
        .collect();
    if targets.is_empty() {
        return;
    }
    let prefix = format!("{}::", crate::macros::TWINS_MODULE);
    for (name, func) in bodies {
        if name.starts_with(&prefix) {
            continue;
        }
        for block in &mut func.blocks {
            for inst in &mut block.insts {
                if let MirInst::HostInvoke { dest, native_id, args, .. } = inst
                    && let Some(&target) = targets.get(native_id)
                {
                    *inst = MirInst::Call { dest: *dest, dest_hi: None, target, args: std::mem::take(args) };
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_twin_names_a_host_native() {
        assert_eq!(twin_targets().len(), NATIVE_TWINS.len());
    }

    #[test]
    fn every_twin_is_defined_in_the_twins_module() {
        for &(_, twin) in NATIVE_TWINS {
            assert!(
                crate::macros::TWINS_SOURCE.contains(&format!("fn {twin}(")),
                "`{twin}` is not a `fn` of prelude/twins.coil"
            );
        }
    }
}
