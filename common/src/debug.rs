//! Per-instruction debug locations shipped in `.ccoil` archives.

use rkyv::{Archive, Deserialize, Serialize};

/// Sentinel `file` index: no source location (synthetic / unknown).
pub const DEBUG_FILE_UNKNOWN: u32 = u32::MAX;

/// The source location of an op: a byte span of a source file, and the
/// inline scope the op was spliced in from, if any.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct DebugLoc {
    /// Index into [`ProgramDebug::source_files`].
    pub file: u32,
    pub start_byte: u32,
    pub end_byte: u32,
    /// `0`: the op is code of the function it is in. `k`: inlining spliced
    /// it in, [`ProgramDebug::inline_scopes`]`[k - 1]` names the callee and
    /// the call (#2578).
    pub scope: u32,
}

impl DebugLoc {
    pub const fn unknown() -> Self {
        Self {
            file: DEBUG_FILE_UNKNOWN,
            start_byte: 0,
            end_byte: 0,
            scope: 0,
        }
    }

    /// A span of `file`, in no inline scope.
    pub const fn span(file: u32, start_byte: u32, end_byte: u32) -> Self {
        Self { file, start_byte, end_byte, scope: 0 }
    }

    pub fn is_known(self) -> bool {
        self.file != DEBUG_FILE_UNKNOWN && self.start_byte < self.end_byte
    }

    /// The same span in no inline scope.
    pub fn unscoped(self) -> Self {
        Self { scope: 0, ..self }
    }
}

/// A [`DebugLoc`]'s span as an archive's location table stores it (the
/// scope is not part of a line lookup).
#[derive(Clone, Copy, PartialEq, Eq, Archive, Serialize, Deserialize, Debug, Default)]
#[rkyv(compare(PartialEq))]
pub struct SpanLoc {
    pub file: u32,
    pub start_byte: u32,
    pub end_byte: u32,
}

impl From<DebugLoc> for SpanLoc {
    fn from(l: DebugLoc) -> Self {
        Self { file: l.file, start_byte: l.start_byte, end_byte: l.end_byte }
    }
}

impl From<SpanLoc> for DebugLoc {
    fn from(l: SpanLoc) -> Self {
        Self::span(l.file, l.start_byte, l.end_byte)
    }
}

/// A callee that inlining spliced into a caller: the frame a panic in the
/// spliced code shows between the op's own location and the call.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct InlineScope {
    /// The callee's name, as its own frame would show it.
    pub callee: String,
    /// The call it replaced; its own scope, when the call was itself
    /// spliced in, is the enclosing one.
    pub call: DebugLoc,
    /// The spliced code has the call's location (another module's callee):
    /// the callee's frame shows no line.
    pub at_call: bool,
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
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ProgramDebug {
    pub source_files: Vec<String>,
    pub debug_locs: Vec<DebugLoc>,
    /// Sorted by `body_id` for binary search during panic backtraces.
    pub fn_symbols: Vec<FnDebugSym>,
    /// One per [`Self::debug_locs`] entry, or empty when not recorded
    /// (archives before minor 30): then lines come from the source files.
    pub debug_lines: Vec<DebugLine>,
    /// The inline scopes [`DebugLoc::scope`] names (archive minor 1 of
    /// major 8; an older archive has none).
    pub inline_scopes: Vec<InlineScope>,
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

    /// The frames at `loc`, innermost first: each inline scope's callee with
    /// its location in it (`None` for one whose code has the call's
    /// location), then the location in the function the code is in.
    pub fn inline_frames(&self, loc: DebugLoc) -> (Vec<(&str, Option<DebugLoc>)>, DebugLoc) {
        let mut frames = Vec::new();
        let mut at = loc;
        // A scope's call is in an enclosing scope, made before it: the chain
        // ends. The bound guards a table that is not well formed.
        while at.scope != 0 && frames.len() <= self.inline_scopes.len() {
            let Some(scope) = self.inline_scopes.get(at.scope as usize - 1) else { break };
            frames.push((scope.callee.as_str(), (!scope.at_call).then_some(at.unscoped())));
            at = scope.call;
        }
        (frames, at.unscoped())
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

/// What a helper partial inlining cuts out of a function's cold arm is
/// called: `{function}$cold{k}`.
pub const COLD_HELPER_SEP: &str = "$cold";

/// The function cold helper `name` was cut out of, if it is one.
pub fn cold_helper_of(name: &str) -> Option<&str> {
    let (base, k) = name.rsplit_once(COLD_HELPER_SEP)?;
    (!k.is_empty() && k.bytes().all(|b| b.is_ascii_digit())).then_some(base)
}

/// The magic a native MIR section (`ArchivedProgram::native_mir`) starts
/// with, before its format byte.
pub const MIR_SECTION_MAGIC: &[u8; 5] = b"CNMIR";

/// The first MIR section format that holds the inline scope table, right
/// after the format byte (#2578).
pub const MIR_SECTION_SCOPES_FORMAT: u8 = 17;

/// Append `scopes` as a MIR section stores them: a count, then per scope
/// the callee's name (length, UTF-8), the call's file, start, end and
/// scope and an `at_call` byte; little-endian `u32`s.
pub fn write_inline_scopes(out: &mut Vec<u8>, scopes: &[InlineScope]) {
    let u32 = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
    u32(out, scopes.len() as u32);
    for s in scopes {
        u32(out, s.callee.len() as u32);
        out.extend_from_slice(s.callee.as_bytes());
        for v in [s.call.file, s.call.start_byte, s.call.end_byte, s.call.scope] {
            u32(out, v);
        }
        out.push(u8::from(s.at_call));
    }
}

/// The table [`write_inline_scopes`] wrote at `bytes[*at..]`; `*at` moves
/// past it. `None` when it is cut short or a name is not UTF-8.
pub fn read_inline_scopes(bytes: &[u8], at: &mut usize) -> Option<Vec<InlineScope>> {
    fn word(bytes: &[u8], at: &mut usize) -> Option<u32> {
        let v = u32::from_le_bytes(bytes.get(*at..*at + 4)?.try_into().ok()?);
        *at += 4;
        Some(v)
    }
    let n = word(bytes, at)?;
    let mut scopes = Vec::new();
    for _ in 0..n {
        let len = word(bytes, at)? as usize;
        let callee = String::from_utf8(bytes.get(*at..*at + len)?.to_vec()).ok()?;
        *at += len;
        let call = DebugLoc {
            file: word(bytes, at)?,
            start_byte: word(bytes, at)?,
            end_byte: word(bytes, at)?,
            scope: word(bytes, at)?,
        };
        let at_call = *bytes.get(*at)? != 0;
        *at += 1;
        scopes.push(InlineScope { callee, call, at_call });
    }
    Some(scopes)
}

/// The inline scope table of native MIR section `section`: none in a
/// section of an older format.
pub fn section_inline_scopes(section: &[u8]) -> Vec<InlineScope> {
    let head = MIR_SECTION_MAGIC.len();
    if !section.starts_with(MIR_SECTION_MAGIC) || section.get(head).is_none_or(|&f| f < MIR_SECTION_SCOPES_FORMAT) {
        return Vec::new();
    }
    let mut at = head + 1;
    read_inline_scopes(section, &mut at).unwrap_or_default()
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
                DebugLoc::span(0, 14, 15),
                DebugLoc::unknown(),
                DebugLoc::span(1, 0, 1),
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
