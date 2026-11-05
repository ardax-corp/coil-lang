//! Source locations and function names for panic reports, from a
//! program's debug tables: what the VM and the native driver both print.

use std::path::{Path, PathBuf};

use common::ProgramDebug;

/// A program's debug tables, as panic reports read them.
pub(crate) struct Locator<'a> {
    pub(crate) debug: &'a ProgramDebug,
    /// Directory of the entry script (relative source paths).
    pub(crate) base_dir: Option<&'a Path>,
}

impl Locator<'_> {

    /// The name of the function whose body holds `ip`.
    pub(crate) fn fn_symbol_at(&self, ip: usize) -> Option<&str> {
        let syms = &self.debug.fn_symbols;
        let i = syms.partition_point(|s| s.entry_pc as usize <= ip);
        (i > 0).then(|| syms[i - 1].name.as_str())
    }

    /// `path:line:col` of a native panic site.
    pub(crate) fn native_site_location(&self, site: crate::native::NativeSite) -> Option<String> {
        let (file, line) = self.debug.line_of(site.loc)?;
        let path = self.debug.source_files.get(file as usize)?;
        Some(format!("{path}:{}:{}", line.line, line.column))
    }

    /// The backtrace line of native site `s`: `  in name at loc`.
    pub(crate) fn native_site_line(&self, code: &crate::native::NativeCode, s: u32) -> Option<String> {
        let site = code.site(s)?;
        let name = self.fn_symbol_at(site.entry_pc as usize).unwrap_or("<unknown>");
        Some(match self.native_site_location(site) {
            Some(loc) => format!("  in {name} at {loc}"),
            None => format!("  in {name}"),
        })
    }
}
