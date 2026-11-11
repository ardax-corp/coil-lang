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
        self.location(site.loc)
    }

    /// `path:line:col` of `loc`.
    fn location(&self, loc: common::DebugLoc) -> Option<String> {
        let (file, line) = self.debug.line_of(loc)?;
        let path = self.debug.source_files.get(file as usize)?;
        Some(format!("{path}:{}:{}", line.line, line.column))
    }

    /// The frames of a function `name` at `loc`, innermost first: one per
    /// inline scope `loc` is in (the callee inlining spliced in, at its
    /// line), then `name` at the call that scope replaced (#2578). A cold
    /// helper partial inlining cut out of a function shows as that
    /// function; `cold` is the function the frame inside this one was
    /// such a helper of, which this frame's innermost one then is too (the
    /// helper's call), and is not shown again. Returns what `cold` is for
    /// the next frame.
    fn frames_at<'n>(&self, name: &'n str, loc: Option<common::DebugLoc>, cold: Option<&str>, lines: &mut Vec<String>) -> Option<&'n str> {
        let line = |name: &str, loc: Option<String>| match loc {
            Some(loc) => format!("  in {name} at {loc}"),
            None => format!("  in {name}"),
        };
        let shown = common::cold_helper_of(name);
        let own = shown.unwrap_or(name);
        let (inlined, at) = match loc {
            Some(loc) => self.debug.inline_frames(loc),
            None => (Vec::new(), common::DebugLoc::unknown()),
        };
        let mut frames: Vec<(&str, Option<common::DebugLoc>)> = inlined;
        frames.push((own, loc.map(|_| at)));
        if cold.is_some_and(|c| frames.first().is_some_and(|(f, _)| *f == c)) {
            frames.remove(0);
        }
        for (f, at) in frames {
            lines.push(line(f, at.and_then(|l| self.location(l))));
        }
        shown
    }

    /// The backtrace of native `sites`, innermost first: `  in name at
    /// loc` for each. An AOT executable's `__start` has no line: it is no
    /// function of the program. Nor have the functions an auto-par loop
    /// is compiled to: its chunk worker is the loop's body, so that frame's
    /// location is shown in the function the loop is written in, in place
    /// of the call that function makes into the fork-join. A recursive
    /// fork-join's worker stands in for a call of its function. Where it
    /// calls an arm on its own thread, that arm is the function again, and
    /// the worker's frame is a frame of that function (the name of the
    /// frame under it); where it calls the function in its own place (no
    /// location) it is no frame.
    pub(crate) fn backtrace(&self, code: &crate::native::NativeCode, sites: impl IntoIterator<Item = u32>) -> Vec<String> {
        let mut lines = Vec::new();
        // The innermost chunk worker frame, until its loop's function.
        let mut in_loop: Option<(&str, Option<common::DebugLoc>)> = None;
        // The function of the last frame printed.
        let mut under: Option<&str> = None;
        // The function the last frame printed was a cold helper of.
        let mut cold: Option<&str> = None;
        for s in sites {
            let Some(site) = code.site(s) else { continue };
            let name = self.fn_symbol_at(site.body_id as usize).unwrap_or("<unknown>");
            if name == common::NATIVE_START_NAME {
                continue;
            }
            let loc = self.location(site.loc).map(|_| site.loc);
            if let Some(stands_for) = common::par_fork_fn(name) {
                if loc.is_some() {
                    cold = self.frames_at(under.unwrap_or(stands_for), loc, cold, &mut lines);
                }
                continue;
            }
            match common::par_loop_fn(name) {
                Some(common::ParLoopFn::Fork) => {}
                Some(common::ParLoopFn::Worker) => {
                    in_loop.get_or_insert((name, loc));
                }
                None => {
                    let loc = in_loop.take().map_or(loc, |(_, inner)| inner);
                    cold = self.frames_at(name, loc, cold, &mut lines);
                    under = Some(name);
                }
            }
        }
        // A worker with no caller in the trace: a chunk's own job.
        if let Some((name, loc)) = in_loop {
            self.frames_at(name, loc, cold, &mut lines);
        }
        lines
    }
}
