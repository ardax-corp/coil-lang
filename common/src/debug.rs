//! Per-instruction debug locations shipped in `.hyc` archives.

use rkyv::{Archive, Deserialize, Serialize};

/// Sentinel `file` index: no source location (synthetic / unknown).
pub const DEBUG_FILE_UNKNOWN: u32 = u32::MAX;

/// One entry per bytecode slot (same index as VM `ip`).
#[derive(Clone, Copy, PartialEq, Eq, Archive, Serialize, Deserialize, Debug, Default)]
#[rkyv(compare(PartialEq))]
pub struct DebugLoc {
    /// Index into [`ProgramDebug::source_files`].
    pub file: u32,
    pub start_byte: u32,
    pub end_byte: u32,
}

impl DebugLoc {
    pub const fn unknown() -> Self {
        Self {
            file: DEBUG_FILE_UNKNOWN,
            start_byte: 0,
            end_byte: 0,
        }
    }

    pub fn is_known(self) -> bool {
        self.file != DEBUG_FILE_UNKNOWN && self.start_byte < self.end_byte
    }
}

/// Line and column of a [`DebugLoc`], resolved when the program is compiled
/// so a packaged binary never needs its source files. `line == 0` means
/// unknown.
#[derive(Clone, Copy, PartialEq, Eq, Archive, Serialize, Deserialize, Debug, Default)]
#[rkyv(compare(PartialEq))]
pub struct DebugLine {
    /// 1-based.
    pub line: u32,
    /// 0-based, in characters.
    pub column: u32,
}

impl DebugLine {
    pub const fn unknown() -> Self {
        Self { line: 0, column: 0 }
    }

    pub fn is_known(self) -> bool {
        self.line != 0
    }
}

/// Function entry symbol for panic backtraces and debug tooling.
#[derive(Clone, PartialEq, Eq, Archive, Serialize, Deserialize, Debug)]
#[rkyv(compare(PartialEq))]
pub struct FnDebugSym {
    pub name: String,
    pub entry_pc: u32,
}

/// A `defer` cleanup range: a frame whose pc is in `start_pc..end_pc` is
/// left through the cleanup pad at `pad_pc`, which runs the function's armed
/// `defer` thunks. Sorted by `start_pc`, non-overlapping.
#[derive(Clone, Copy, PartialEq, Eq, Archive, Serialize, Deserialize, Debug)]
#[rkyv(compare(PartialEq))]
pub struct CleanupRange {
    pub start_pc: u32,
    pub end_pc: u32,
    pub pad_pc: u32,
    /// Slots the frame uses; the pad's calls go above them.
    pub frame_words: u32,
}

/// The cleanup range holding `pc`, if any (`ranges` sorted by `start_pc`).
pub fn cleanup_range_at(ranges: &[CleanupRange], pc: usize) -> Option<&CleanupRange> {
    let i = ranges.partition_point(|r| (r.start_pc as usize) <= pc);
    let r = ranges.get(i.checked_sub(1)?)?;
    (pc < r.end_pc as usize).then_some(r)
}

/// Debug sections loaded with bytecode (not the reporting `SourceMap`).
#[derive(Clone, PartialEq, Eq, Archive, Serialize, Deserialize, Debug, Default)]
#[rkyv(compare(PartialEq))]
pub struct ProgramDebug {
    pub source_files: Vec<String>,
    pub debug_locs: Vec<DebugLoc>,
    /// Sorted by `entry_pc` for binary search during panic backtraces.
    pub fn_symbols: Vec<FnDebugSym>,
    /// One per [`Self::debug_locs`] entry, or empty when not recorded
    /// (archives before minor 30): then lines come from the source files.
    pub debug_lines: Vec<DebugLine>,
    /// `defer` cleanup ranges (minor 33+); empty means panics run no `defer`.
    pub cleanup: Vec<CleanupRange>,
}

impl ProgramDebug {
    pub fn empty_for_bytecode_len(len: usize) -> Self {
        Self {
            source_files: Vec::new(),
            debug_locs: vec![DebugLoc::unknown(); len],
            fn_symbols: Vec::new(),
            debug_lines: Vec::new(),
            cleanup: Vec::new(),
        }
    }

    /// Recorded line/column for the location at `pc`, if any.
    pub fn line_at(&self, pc: usize) -> Option<DebugLine> {
        if self.debug_lines.len() != self.debug_locs.len() {
            return None;
        }
        self.debug_lines.get(pc).copied().filter(|l| l.is_known())
    }

    /// Resolve every known location against its source text (`text_of`
    /// returns `None` for a file it cannot provide).
    pub fn resolve_lines(&mut self, mut text_of: impl FnMut(&str) -> Option<String>) {
        let texts: Vec<Option<String>> = self.source_files.iter().map(|f| text_of(f)).collect();
        let indexes: Vec<Option<crate::LineIndex<'_>>> = texts
            .iter()
            .map(|t| t.as_deref().map(crate::LineIndex::new))
            .collect();
        self.debug_lines = self
            .debug_locs
            .iter()
            .map(|loc| {
                if !loc.is_known() {
                    return DebugLine::unknown();
                }
                match indexes.get(loc.file as usize).and_then(Option::as_ref) {
                    Some(index) => {
                        let pos = index.position(loc.start_byte as usize);
                        DebugLine {
                            line: pos.line,
                            column: pos.column,
                        }
                    }
                    None => DebugLine::unknown(),
                }
            })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_lines_records_line_and_column_per_loc() {
        let mut debug = ProgramDebug {
            source_files: vec!["a.hy".into(), "missing.hy".into()],
            debug_locs: vec![
                // `x` on line 2: "fn f() {\n  · x"
                DebugLoc {
                    file: 0,
                    start_byte: 14,
                    end_byte: 15,
                },
                DebugLoc::unknown(),
                DebugLoc {
                    file: 1,
                    start_byte: 0,
                    end_byte: 1,
                },
            ],
            ..ProgramDebug::default()
        };
        debug.resolve_lines(|file| (file == "a.hy").then(|| "fn f() {\n  · x\n}".to_string()));
        assert_eq!(debug.line_at(0), Some(DebugLine { line: 2, column: 4 }));
        assert_eq!(debug.line_at(1), None, "unknown loc");
        assert_eq!(debug.line_at(2), None, "source not available");
    }

    #[test]
    fn line_at_needs_one_line_per_loc() {
        let debug = ProgramDebug {
            debug_locs: vec![DebugLoc::unknown(); 2],
            debug_lines: vec![DebugLine { line: 1, column: 0 }],
            ..ProgramDebug::default()
        };
        assert_eq!(debug.line_at(0), None);
    }
}
