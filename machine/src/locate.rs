//! Source locations and function names for panic reports, from a
//! program's debug tables: what the interpreter and the native driver both print.


use common::ProgramDebug;

/// A program's debug tables, as panic reports read them.
pub(crate) struct Locator<'a> {
    pub(crate) debug: &'a ProgramDebug,
}

impl Locator<'_> {

    /// The name of the function whose body holds `ip`.
    pub(crate) fn fn_symbol_at(&self, ip: usize) -> Option<&str> {
        let syms = &self.debug.fn_symbols;
        let i = syms.partition_point(|s| s.body_id as usize <= ip);
        (i > 0).then(|| syms[i - 1].name.as_str())
    }

    /// `path:line:col` of a native panic site.
    pub(crate) fn native_site_location(&self, site: crate::native::NativeSite) -> Option<String> {
        let (file, line) = self.debug.line_of(site.loc)?;
        let path = self.debug.source_files.get(file as usize)?;
        Some(format!("{path}:{}:{}", line.line, line.column))
    }

    /// The backtrace of native `sites`, innermost first: `  in name at
    /// loc` for each. An AOT executable's `__start` has no line: it is no
    /// function of the program. Nor have the functions an auto-par loop
    /// is compiled to: its chunk worker is the loop's body, so that frame's
    /// location is shown in the function the loop is written in, in place
    /// of the call that function makes into the fork-join.
    pub(crate) fn backtrace(&self, code: &crate::native::NativeCode, sites: impl IntoIterator<Item = u32>) -> Vec<String> {
        let line = |name: &str, loc: Option<String>| match loc {
            Some(loc) => format!("  in {name} at {loc}"),
            None => format!("  in {name}"),
        };
        let mut lines = Vec::new();
        // The innermost chunk worker frame, until its loop's function.
        let mut in_loop: Option<(&str, Option<String>)> = None;
        for s in sites {
            let Some(site) = code.site(s) else { continue };
            let name = self.fn_symbol_at(site.body_id as usize).unwrap_or("<unknown>");
            if name == common::NATIVE_START_NAME {
                continue;
            }
            let loc = self.native_site_location(site);
            match common::par_loop_fn(name) {
                Some(common::ParLoopFn::Fork) => {}
                Some(common::ParLoopFn::Worker) => {
                    in_loop.get_or_insert((name, loc));
                }
                None => {
                    let loc = in_loop.take().map_or(loc, |(_, inner)| inner);
                    lines.push(line(name, loc));
                }
            }
        }
        // A worker with no caller in the trace: a chunk's own job.
        lines.extend(in_loop.map(|(name, loc)| line(name, loc)));
        lines
    }
}
