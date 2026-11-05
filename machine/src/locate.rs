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
    /// Where source file `path` is: as given, or under the entry's
    /// directory or its parent.
    pub(crate) fn resolve_source_path(&self, path: &str) -> PathBuf {
        let p = PathBuf::from(path);
        if p.is_absolute() || std::fs::metadata(&p).is_ok() {
            return p;
        }
        if let Some(base) = self.base_dir {
            let root = base.parent().unwrap_or(base);
            let from_root = root.join(path);
            if std::fs::metadata(&from_root).is_ok() {
                return from_root;
            }
            let from_base = base.join(path);
            if std::fs::metadata(&from_base).is_ok() {
                return from_base;
            }
        }
        p
    }

    /// `(file, line, column)` of the op at `pc`.
    pub(crate) fn pc_file_line_column(&self, pc: usize) -> Option<(u32, u32, u32)> {
        if let Some((file, line)) = self.debug.file_line_at(pc) {
            return Some((file, line.line, line.column));
        }
        let loc = self.debug.loc_at(pc)?;
        let path = self.debug.source_files.get(loc.file as usize)?;
        let text = std::fs::read_to_string(self.resolve_source_path(path)).ok()?;
        let pos = common::byte_to_position(&text, loc.start_byte as usize);
        Some((loc.file, pos.line, pos.column))
    }

    /// `path:line:col` of the op at `pc`.
    pub(crate) fn location(&self, pc: usize) -> Option<String> {
        let (file, line, column) = self.pc_file_line_column(pc)?;
        let path = self.debug.source_files.get(file as usize)?;
        Some(format!("{path}:{line}:{column}"))
    }

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
