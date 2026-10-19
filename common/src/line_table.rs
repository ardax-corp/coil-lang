//! Debug info levels and the compact run-length line table (archive minor 40+,
//! coil-lang#1491).
//!
//! A location is stored once per run of bytecode slots that share it, not
//! once per slot. Runs are varint-encoded with deltas against the previous
//! run, so a typical run costs 3–5 bytes against the 20 bytes per slot of
//! the dense `debug_locs` + `debug_lines` tables.

use rkyv::{Archive, Deserialize, Serialize};

use crate::debug::{DEBUG_FILE_UNKNOWN, DebugLine, DebugLoc};

/// How much debug info a compiled program keeps (`-g0`, `-g1`, `-g2`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum DebugLevel {
    /// No line table, function names or source paths. A panic prints only its
    /// message and bare `in <unknown>` frames.
    None,
    /// Function names and the line table (file, line, column per run): a
    /// backtrace reads `in inner at x.hy:3:8`. No byte spans.
    Lines,
    /// Lines plus byte spans, which the debugger and coverage need.
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

/// Line and column from `pc` up to the next run. `file ==`
/// [`DEBUG_FILE_UNKNOWN`] marks slots with no location.
#[derive(Clone, Copy, PartialEq, Eq, Archive, Serialize, Deserialize, Debug)]
#[rkyv(compare(PartialEq))]
pub struct LineRun {
    pub pc: u32,
    pub file: u32,
    pub line: u32,
    pub column: u32,
}

/// Byte span from `pc` up to the next run (`-g2` only).
#[derive(Clone, Copy, PartialEq, Eq, Archive, Serialize, Deserialize, Debug)]
#[rkyv(compare(PartialEq))]
pub struct SpanRun {
    pub pc: u32,
    pub loc: DebugLoc,
}

/// Run-length locations for `len` bytecode slots. Lookups binary-search the
/// runs, which are sorted by `pc`.
#[derive(Clone, PartialEq, Eq, Archive, Serialize, Deserialize, Debug, Default)]
#[rkyv(compare(PartialEq))]
pub struct LineTable {
    /// Bytecode slots covered.
    pub len: u32,
    pub lines: Vec<LineRun>,
    /// Empty below `-g2`.
    pub spans: Vec<SpanRun>,
}

/// Why an encoded table did not decode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineTableError(pub &'static str);

impl LineTable {
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.spans.is_empty()
    }

    /// Runs from the dense per-slot tables. `lines` is aligned with `locs`
    /// (or empty: no line is known). Spans are kept only when `spans` is set.
    pub fn from_dense(locs: &[DebugLoc], lines: &[DebugLine], spans: bool) -> Self {
        let aligned = lines.len() == locs.len();
        let mut table = Self {
            len: locs.len() as u32,
            ..Self::default()
        };
        let mut prev_line: Option<(u32, u32, u32)> = None;
        let mut prev_span: Option<DebugLoc> = None;
        for (pc, loc) in locs.iter().enumerate() {
            let line = (aligned && loc.is_known())
                .then(|| lines[pc])
                .filter(|l| l.is_known())
                .map(|l| (loc.file, l.line, l.column));
            if line != prev_line {
                let (file, line_no, column) = line.unwrap_or((DEBUG_FILE_UNKNOWN, 0, 0));
                table.lines.push(LineRun {
                    pc: pc as u32,
                    file,
                    line: line_no,
                    column,
                });
                prev_line = line;
            }
            let span = loc.is_known().then_some(*loc);
            if spans && span != prev_span {
                table.spans.push(SpanRun {
                    pc: pc as u32,
                    loc: span.unwrap_or(DebugLoc::unknown()),
                });
                prev_span = span;
            }
        }
        table
    }

    fn run_index<T>(&self, runs: &[T], pc: usize, run_pc: impl Fn(&T) -> u32) -> Option<usize> {
        if pc >= self.len as usize {
            return None;
        }
        runs.partition_point(|r| run_pc(r) as usize <= pc).checked_sub(1)
    }

    /// File index and line/column recorded for `pc`.
    pub fn line_at(&self, pc: usize) -> Option<(u32, DebugLine)> {
        let run = self.lines[self.run_index(&self.lines, pc, |r| r.pc)?];
        (run.file != DEBUG_FILE_UNKNOWN).then_some((
            run.file,
            DebugLine {
                line: run.line,
                column: run.column,
            },
        ))
    }

    /// Byte span recorded for `pc` (`-g2` tables only).
    pub fn span_at(&self, pc: usize) -> Option<DebugLoc> {
        let run = self.spans[self.run_index(&self.spans, pc, |r| r.pc)?];
        run.loc.is_known().then_some(run.loc)
    }

    /// Drop the byte spans (`-g1`).
    pub fn without_spans(mut self) -> Self {
        self.spans = Vec::new();
        self
    }

    /// Encoded line runs: `len`, run count, then per run `pc_delta`,
    /// `file + 1` (`0` = unknown) and, for a known file, the zigzag line
    /// delta and the column. Empty when there are no runs.
    pub fn encode_lines(&self) -> Vec<u8> {
        if self.lines.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(self.lines.len() * 4 + 8);
        put_varint(&mut out, self.len as u64);
        put_varint(&mut out, self.lines.len() as u64);
        let (mut pc, mut line) = (0u32, 0u32);
        for run in &self.lines {
            put_varint(&mut out, (run.pc - pc) as u64);
            pc = run.pc;
            put_file(&mut out, run.file);
            if run.file != DEBUG_FILE_UNKNOWN {
                put_varint(&mut out, zigzag(run.line as i64 - line as i64));
                put_varint(&mut out, run.column as u64);
                line = run.line;
            }
        }
        out
    }

    /// Encoded span runs: `len`, run count, then per run `pc_delta`,
    /// `file + 1` and, for a known file, the zigzag start delta and the span
    /// length. Empty when there are no runs.
    pub fn encode_spans(&self) -> Vec<u8> {
        if self.spans.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(self.spans.len() * 5 + 8);
        put_varint(&mut out, self.len as u64);
        put_varint(&mut out, self.spans.len() as u64);
        let (mut pc, mut start) = (0u32, 0u32);
        for run in &self.spans {
            put_varint(&mut out, (run.pc - pc) as u64);
            pc = run.pc;
            put_file(&mut out, run.loc.file);
            if run.loc.file != DEBUG_FILE_UNKNOWN {
                put_varint(&mut out, zigzag(run.loc.start_byte as i64 - start as i64));
                put_varint(&mut out, (run.loc.end_byte - run.loc.start_byte) as u64);
                start = run.loc.start_byte;
            }
        }
        out
    }

    /// Decode [`Self::encode_lines`] / [`Self::encode_spans`] output. Every
    /// run's file must index one of `files` source files, and run pcs must
    /// increase and stay below `len`.
    pub fn decode(lines: &[u8], spans: &[u8], files: usize) -> Result<Self, LineTableError> {
        let mut table = Self::default();
        let mut have_len = false;
        if !lines.is_empty() {
            let mut r = Reader::new(lines);
            table.len = r.u32()?;
            have_len = true;
            let n = r.count()?;
            table.lines.reserve(n);
            let (mut pc, mut line) = (0u32, 0u32);
            for i in 0..n {
                pc = r.next_pc(pc, i, table.len)?;
                let file = r.file(files)?;
                let (mut l, mut column) = (0, 0);
                if file != DEBUG_FILE_UNKNOWN {
                    l = apply_delta(line, r.zigzag()?)?;
                    column = r.u32()?;
                    line = l;
                }
                table.lines.push(LineRun {
                    pc,
                    file,
                    line: l,
                    column,
                });
            }
            r.finish()?;
        }
        if !spans.is_empty() {
            let mut r = Reader::new(spans);
            let len = r.u32()?;
            if have_len && len != table.len {
                return Err(LineTableError("line and span tables cover different lengths"));
            }
            table.len = len;
            let n = r.count()?;
            table.spans.reserve(n);
            let (mut pc, mut start) = (0u32, 0u32);
            for i in 0..n {
                pc = r.next_pc(pc, i, table.len)?;
                let file = r.file(files)?;
                let mut loc = DebugLoc::unknown();
                if file != DEBUG_FILE_UNKNOWN {
                    let s = apply_delta(start, r.zigzag()?)?;
                    let end = s
                        .checked_add(r.u32()?)
                        .ok_or(LineTableError("span end overflows"))?;
                    loc = DebugLoc {
                        file,
                        start_byte: s,
                        end_byte: end,
                    };
                    start = s;
                }
                table.spans.push(SpanRun { pc, loc });
            }
            r.finish()?;
        }
        Ok(table)
    }
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

fn apply_delta(base: u32, delta: i64) -> Result<u32, LineTableError> {
    u32::try_from(base as i64 + delta).map_err(|_| LineTableError("delta out of range"))
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_file(out: &mut Vec<u8>, file: u32) {
    let code = if file == DEBUG_FILE_UNKNOWN { 0 } else { file as u64 + 1 };
    put_varint(out, code);
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn varint(&mut self) -> Result<u64, LineTableError> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = *self
                .bytes
                .get(self.at)
                .ok_or(LineTableError("truncated varint"))?;
            self.at += 1;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(LineTableError("varint too long"))
    }

    fn u32(&mut self) -> Result<u32, LineTableError> {
        u32::try_from(self.varint()?).map_err(|_| LineTableError("value exceeds u32"))
    }

    fn zigzag(&mut self) -> Result<i64, LineTableError> {
        Ok(unzigzag(self.varint()?))
    }

    /// A run count: every run takes at least two bytes, so a count the
    /// remaining bytes cannot hold is corrupt (and never over-allocates).
    fn count(&mut self) -> Result<usize, LineTableError> {
        let n = self.varint()? as usize;
        if n > (self.bytes.len() - self.at) / 2 {
            return Err(LineTableError("run count exceeds table"));
        }
        Ok(n)
    }

    fn next_pc(&mut self, pc: u32, i: usize, len: u32) -> Result<u32, LineTableError> {
        let delta = self.u32()?;
        if i > 0 && delta == 0 {
            return Err(LineTableError("runs out of order"));
        }
        let next = pc
            .checked_add(delta)
            .filter(|&p| p < len)
            .ok_or(LineTableError("run past the end of the bytecode"))?;
        Ok(next)
    }

    fn file(&mut self, files: usize) -> Result<u32, LineTableError> {
        match self.varint()? {
            0 => Ok(DEBUG_FILE_UNKNOWN),
            code if (code - 1) < files as u64 => Ok((code - 1) as u32),
            _ => Err(LineTableError("file index out of range")),
        }
    }

    fn finish(&self) -> Result<(), LineTableError> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err(LineTableError("trailing bytes"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(file: u32, start: u32, end: u32) -> DebugLoc {
        DebugLoc {
            file,
            start_byte: start,
            end_byte: end,
        }
    }

    fn line(line: u32, column: u32) -> DebugLine {
        DebugLine { line, column }
    }

    /// Dense tables with unknown slots, repeats, a file switch and a line
    /// that goes back up.
    fn sample() -> (Vec<DebugLoc>, Vec<DebugLine>) {
        let locs = vec![
            DebugLoc::unknown(),
            loc(0, 10, 14),
            loc(0, 10, 14),
            loc(0, 16, 20),
            DebugLoc::unknown(),
            loc(1, 300, 340),
            loc(1, 300, 340),
            loc(0, 2, 5),
            loc(0, 2, 5),
        ];
        let lines = vec![
            DebugLine::unknown(),
            line(2, 4),
            line(2, 4),
            line(2, 10),
            DebugLine::unknown(),
            line(40, 0),
            line(40, 0),
            line(1, 2),
            line(1, 2),
        ];
        (locs, lines)
    }

    #[test]
    fn runs_answer_every_pc_like_the_dense_tables() {
        let (locs, lines) = sample();
        let table = LineTable::from_dense(&locs, &lines, true);
        assert_eq!(table.lines.len(), 5, "{:?}", table.lines);
        assert_eq!(table.spans.len(), 5, "{:?}", table.spans);
        for pc in 0..locs.len() {
            let want_line = locs[pc].is_known().then(|| (locs[pc].file, lines[pc]));
            assert_eq!(table.line_at(pc), want_line, "pc {pc}");
            assert_eq!(table.span_at(pc), locs[pc].is_known().then_some(locs[pc]), "pc {pc}");
        }
        assert_eq!(table.line_at(locs.len()), None, "past the end");
    }

    #[test]
    fn encoding_round_trips() {
        let (locs, lines) = sample();
        let table = LineTable::from_dense(&locs, &lines, true);
        let (l, s) = (table.encode_lines(), table.encode_spans());
        assert_eq!(LineTable::decode(&l, &s, 2), Ok(table.clone()));
        let lines_only = table.clone().without_spans();
        assert!(lines_only.encode_spans().is_empty());
        assert_eq!(LineTable::decode(&l, &[], 2), Ok(lines_only));
        assert_eq!(LineTable::decode(&[], &[], 0), Ok(LineTable::default()));
    }

    #[test]
    fn large_values_round_trip() {
        let locs = vec![loc(0, 3_000_000_000, 3_000_000_100), loc(0, 1, 2)];
        let lines = vec![line(4_000_000_000, 70_000), line(1, 0)];
        let table = LineTable::from_dense(&locs, &lines, true);
        let back = LineTable::decode(&table.encode_lines(), &table.encode_spans(), 1);
        assert_eq!(back, Ok(table));
    }

    #[test]
    fn a_run_costs_a_few_bytes() {
        // 1000 statements of 4 slots each, one per line.
        let mut locs = Vec::new();
        let mut lines = Vec::new();
        for i in 0..1000u32 {
            for _ in 0..4 {
                locs.push(loc(0, i * 30, i * 30 + 20));
                lines.push(line(i + 1, 4));
            }
        }
        let table = LineTable::from_dense(&locs, &lines, true);
        assert_eq!(table.lines.len(), 1000);
        assert!(table.encode_lines().len() <= 4 * 1000 + 8);
        assert!(table.encode_spans().len() <= 5 * 1000 + 8);
    }

    #[test]
    fn corrupt_tables_are_rejected() {
        let (locs, lines) = sample();
        let table = LineTable::from_dense(&locs, &lines, true);
        let l = table.encode_lines();
        assert!(LineTable::decode(&l, &[], 1).is_err(), "file 1 out of range");
        assert!(LineTable::decode(&l[..l.len() - 1], &[], 2).is_err(), "truncated");
        let mut trailing = l.clone();
        trailing.push(0);
        assert!(LineTable::decode(&trailing, &[], 2).is_err(), "trailing byte");
        // A huge run count must fail, not allocate.
        assert!(LineTable::decode(&[5, 0xff, 0xff, 0xff, 0xff, 0x0f], &[], 2).is_err());
        let mut short = LineTable::from_dense(&locs[..4], &lines[..4], true);
        short.len = 4;
        assert!(LineTable::decode(&l, &short.encode_spans(), 2).is_err(), "length mismatch");
    }

    #[test]
    fn debug_level_parses_digits() {
        assert_eq!(DebugLevel::parse("0"), Ok(DebugLevel::None));
        assert_eq!(DebugLevel::parse("1"), Ok(DebugLevel::Lines));
        assert_eq!(DebugLevel::parse("2"), Ok(DebugLevel::Full));
        assert!(DebugLevel::parse("3").is_err());
        assert_eq!(DebugLevel::default(), DebugLevel::Full);
    }
}
