//! Per-instruction debug locations shipped in `.ccoil` archives.

use rkyv::{Archive, Deserialize, Serialize};

/// Sentinel `file` index: no source location (synthetic / unknown).
pub const DEBUG_FILE_UNKNOWN: u32 = u32::MAX;

/// One entry per code slot (same index as runtime `ip`).
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
    pub body_id: u32,
}

/// How much debug info a compiled program keeps (`-g0`, `-g1`, `-g2`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum DebugLevel {
    /// No line table, function names or source paths. A panic prints only its
    /// message and bare `in <unknown>` frames.
    None,
    /// Function names and the line table (file, line, column per run): a
    /// backtrace reads `in inner at x.coil:3:8`. Byte spans are kept too.
    Lines,
    /// The default: everything the compiler recorded, including the byte
    /// spans the debugger and coverage need.
    #[default]
    Full,
}

impl DebugLevel {
    /// `0`, `1` or `2`.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim() {
            "0" => Ok(Self::None),
            "1" => Ok(Self::Lines),
            "2" => Ok(Self::Full),
            other => Err(format!("invalid debug level `{other}` (expected 0, 1 or 2)")),
        }
    }

    /// The level's digit, as `-g` takes it.
    pub fn digit(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Lines => 1,
            Self::Full => 2,
        }
    }
}

/// Debug sections loaded with code (not the reporting `SourceMap`).
#[derive(Clone, PartialEq, Eq, Archive, Serialize, Deserialize, Debug, Default)]
#[rkyv(compare(PartialEq))]
pub struct ProgramDebug {
    pub source_files: Vec<String>,
    pub debug_locs: Vec<DebugLoc>,
    /// Sorted by `body_id` for binary search during panic backtraces.
    pub fn_symbols: Vec<FnDebugSym>,
    /// One per [`Self::debug_locs`] entry, or empty when not recorded
    /// (archives before minor 30): then lines come from the source files.
    pub debug_lines: Vec<DebugLine>,
}

impl ProgramDebug {

    /// Line and column of `loc`, in a table sorted by location (a compiled
    /// program's: each location its bodies use, once). A span that differs
    /// only in its end stands in for it.
    pub fn line_of(&self, loc: DebugLoc) -> Option<(u32, DebugLine)> {
        if !loc.is_known() {
            return None;
        }
        let key = |l: &DebugLoc| (l.file, l.start_byte, l.end_byte);
        let at = self.debug_locs.partition_point(|l| key(l) < (loc.file, loc.start_byte, 0));
        let near = self.debug_locs[at..].iter().take_while(|l| l.file == loc.file && l.start_byte == loc.start_byte);
        let i = near.clone().position(|l| l.end_byte == loc.end_byte).or_else(|| near.clone().next().map(|_| 0))?;
        self.debug_lines.get(at + i).map(|line| (loc.file, *line))
    }

    /// Keep what `level` records: `-g0` nothing, else everything.
    pub fn with_level(self, level: crate::DebugLevel) -> Self {
        match level {
            crate::DebugLevel::None => Self::default(),
            _ => self,
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
            source_files: vec!["a.coil".into(), "missing.coil".into()],
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
        debug.resolve_lines(|file| (file == "a.coil").then(|| "fn f() {\n  · x\n}".to_string()));
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
