//! Glue to attach a compiled program to a VM: standard HostInvoke table,
//! dload/FFI paths, C struct layouts, thread program.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{Byte, ProgramDebug};

/// Inputs the compiler can supply without depending on `machine`.
pub struct VmHostSpec<'a> {
    pub entry_path: Option<&'a Path>,
    pub project_root: &'a Path,
    pub ffi_search_paths: &'a [PathBuf],
    pub native_pins: &'a [(String, String)],
    pub trusted_stems: &'a [String],
    pub extra_dload_stems: &'a [String],
    pub extra_dload_grants: &'a [(String, PathBuf)],
    pub c_structs: &'a [common::CStructLayout],
}

/// Index [`common::ClassWordKinds`] rows by `type_id`, at
/// [`common::type_kinds_index`].
pub fn class_kind_table(rows: &[common::ClassWordKinds]) -> Arc<Vec<Box<[u8]>>> {
    let len = rows.iter().map(|r| common::type_kinds_index(r.type_id) + 1).max().unwrap_or(0);
    let mut table: Vec<Box<[u8]>> = vec![Box::default(); len];
    for row in rows {
        table[common::type_kinds_index(row.type_id)] = row.kinds.clone().into_boxed_slice();
    }
    Arc::new(table)
}
