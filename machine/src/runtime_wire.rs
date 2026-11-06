//! Glue to attach a compiled program to a runtime: standard HostInvoke table,
//! dload/FFI paths, C struct layouts, thread program.

use std::sync::Arc;

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
