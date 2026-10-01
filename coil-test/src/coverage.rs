//! Line coverage: per-PC hit counts from test jobs, joined with each
//! program's debug locations, summed per source line across programs.
//!
//! A line is *coverable* when some emitted instruction carries it (its
//! statement's first line); lines with no code are neither covered nor not.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use common::ProgramDebug;
use compiler::KeepFnFilter;

/// What `--coverage` measures and where it writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageOptions {
    /// lcov tracefile (`--coverage-out`).
    pub lcov_out: PathBuf,
    /// `test → file → lines` JSON (`--coverage-per-test`).
    pub per_test_out: Option<PathBuf>,
    /// Sources under this directory count (the current directory).
    pub project_root: PathBuf,
}

/// Default lcov path under the current directory.
pub const DEFAULT_LCOV_OUT: &str = "target/coverage/lcov.info";

/// A source counts when it lives under `cwd` and not under a `.deps/`
/// dependency checkout. Virtual modules and stdlib roots outside the project
/// are left out.
pub fn is_project_source(path: &str, cwd: &Path) -> bool {
    let Some(abs) = resolve(path, cwd) else {
        return false;
    };
    abs.starts_with(cwd)
        && !abs
            .components()
            .any(|c| c == Component::Normal(".deps".as_ref()))
}

/// [`is_project_source`] as a compiler keep-filter (never-called project
/// functions stay emitted so they report as uncovered).
pub fn project_filter(cwd: PathBuf) -> KeepFnFilter {
    Arc::new(move |path: &str| is_project_source(path, &cwd))
}

fn resolve(path: &str, cwd: &Path) -> Option<PathBuf> {
    let p = Path::new(path);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };
    std::fs::canonicalize(joined).ok()
}

/// One program's PC → (file, line) map. `None` for PCs that do not count.
pub struct ProgramLines {
    lines: Vec<Option<(usize, u32)>>,
}

struct FileCov {
    /// Display path (relative to `cwd` when under it).
    path: String,
    /// Canonical path.
    abs: PathBuf,
    /// How programs' debug info spells this file (the compiler's own path
    /// keys, e.g. for `Pipeline::set_file_text`).
    spellings: BTreeSet<String>,
    /// Line → hits.
    lines: BTreeMap<u32, u64>,
}

/// One test case's covered lines, for `--coverage-per-test`.
struct TestLines {
    file: String,
    name: String,
    lines: BTreeMap<usize, Vec<u32>>,
}

/// Coverage summed over every test program in a run.
pub struct Coverage {
    cwd: PathBuf,
    files: Vec<FileCov>,
    by_path: HashMap<PathBuf, usize>,
    /// Byte offsets of line starts per file index.
    line_starts: HashMap<usize, Vec<u32>>,
    per_test: Option<Vec<TestLines>>,
}

impl Coverage {
    pub fn new(cwd: PathBuf, per_test: bool) -> Self {
        Self {
            cwd,
            files: Vec::new(),
            by_path: HashMap::new(),
            line_starts: HashMap::new(),
            per_test: per_test.then(Vec::new),
        }
    }

    fn file_index(&mut self, source: &str) -> Option<usize> {
        if !is_project_source(source, &self.cwd) {
            return None;
        }
        let abs = resolve(source, &self.cwd)?;
        if let Some(&i) = self.by_path.get(&abs) {
            self.files[i].spellings.insert(source.to_string());
            return Some(i);
        }
        let text = std::fs::read(&abs).ok()?;
        let mut starts = vec![0u32];
        starts.extend(
            text.iter()
                .enumerate()
                .filter(|(_, b)| **b == b'\n')
                .map(|(i, _)| (i + 1) as u32),
        );
        let display = abs
            .strip_prefix(&self.cwd)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| abs.to_string_lossy().into_owned());
        let i = self.files.len();
        self.files.push(FileCov {
            path: display,
            abs: abs.clone(),
            spellings: BTreeSet::from([source.to_string()]),
            lines: BTreeMap::new(),
        });
        self.by_path.insert(abs, i);
        self.line_starts.insert(i, starts);
        Some(i)
    }

    /// Record a program's coverable lines (hits 0) and return its PC map.
    ///
    /// A line whose every instruction sits in `exclude` (test case bodies) is
    /// test code and does not count. A line that also has instructions
    /// elsewhere counts, including its copies inlined into a test body.
    pub fn register_program(
        &mut self,
        debug: &ProgramDebug,
        exclude: &[Range<usize>],
    ) -> ProgramLines {
        let mut file_ids: HashMap<u32, Option<usize>> = HashMap::new();
        let mut lines = vec![None; debug.debug_locs.len()];
        // (file, line) → seen outside a test body.
        let mut outside: HashMap<(usize, u32), bool> = HashMap::new();
        for (pc, loc) in debug.debug_locs.iter().enumerate() {
            if !loc.is_known() {
                continue;
            }
            let id = *file_ids.entry(loc.file).or_insert_with(|| {
                debug
                    .source_files
                    .get(loc.file as usize)
                    .and_then(|s| self.file_index(s))
            });
            let Some(id) = id else { continue };
            let starts = &self.line_starts[&id];
            let line = starts.partition_point(|&s| s <= loc.start_byte) as u32;
            let in_test = exclude.iter().any(|r| r.contains(&pc));
            *outside.entry((id, line)).or_insert(false) |= !in_test;
            lines[pc] = Some((id, line));
        }
        for slot in &mut lines {
            if let Some(key) = *slot {
                if outside[&key] {
                    self.files[key.0].lines.entry(key.1).or_insert(0);
                } else {
                    *slot = None;
                }
            }
        }
        ProgramLines { lines }
    }

    /// Add one case's hit counts; returns its covered lines per canonical
    /// source path.
    pub fn record(
        &mut self,
        program: &ProgramLines,
        hits: &[u32],
        test_file: &str,
        test_name: &str,
    ) -> Vec<(PathBuf, Vec<u32>)> {
        let mut covered: BTreeMap<usize, Vec<u32>> = BTreeMap::new();
        for (pc, &n) in hits.iter().enumerate() {
            if n == 0 {
                continue;
            }
            if let Some(Some((id, line))) = program.lines.get(pc) {
                *self.files[*id].lines.entry(*line).or_insert(0) += u64::from(n);
                covered.entry(*id).or_default().push(*line);
            }
        }
        for lines in covered.values_mut() {
            lines.sort_unstable();
            lines.dedup();
        }
        let by_path = covered
            .iter()
            .map(|(id, lines)| (self.files[*id].abs.clone(), lines.clone()))
            .collect();
        if let Some(per_test) = &mut self.per_test {
            per_test.push(TestLines {
                file: test_file.to_string(),
                name: test_name.to_string(),
                lines: covered,
            });
        }
        by_path
    }

    /// Project sources with coverable lines: `(canonical path, display path,
    /// line → hits)`, sorted by display path.
    pub fn files(&self) -> Vec<(&Path, &str, &BTreeMap<u32, u64>)> {
        self.sorted_files()
            .into_iter()
            .map(|f| (f.abs.as_path(), f.path.as_str(), &f.lines))
            .collect()
    }

    /// Every spelling programs used for `abs` (see [`FileCov::spellings`]).
    pub fn spellings(&self, abs: &Path) -> Vec<String> {
        self.by_path
            .get(abs)
            .map(|&i| self.files[i].spellings.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn sorted_files(&self) -> Vec<&FileCov> {
        let mut files: Vec<&FileCov> = self.files.iter().filter(|f| !f.lines.is_empty()).collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files
    }

    /// `(display path, lines hit, lines total)` per covered file, sorted.
    pub fn file_totals(&self) -> Vec<(String, usize, usize)> {
        self.sorted_files()
            .into_iter()
            .map(|f| {
                let hit = f.lines.values().filter(|n| **n > 0).count();
                (f.path.clone(), hit, f.lines.len())
            })
            .collect()
    }

    /// lcov tracefile text (`SF` / `DA` / `LF` / `LH` per file).
    pub fn lcov(&self) -> String {
        let mut out = String::from("TN:\n");
        for f in self.sorted_files() {
            let _ = writeln!(out, "SF:{}", f.path);
            for (line, hits) in &f.lines {
                let _ = writeln!(out, "DA:{line},{hits}");
            }
            let hit = f.lines.values().filter(|h| **h > 0).count();
            let _ = writeln!(out, "LF:{}\nLH:{hit}\nend_of_record", f.lines.len());
        }
        out
    }

    /// Per-file and total line coverage, one line each.
    pub fn summary(&self) -> String {
        let mut out = String::new();
        let (mut total, mut hit) = (0usize, 0usize);
        for f in self.sorted_files() {
            let h = f.lines.values().filter(|n| **n > 0).count();
            total += f.lines.len();
            hit += h;
            let _ = writeln!(
                out,
                "{:>6}  {h:>5}/{:<5}  {}",
                percent(h, f.lines.len()),
                f.lines.len(),
                f.path
            );
        }
        let _ = writeln!(
            out,
            "{:>6}  {hit:>5}/{total:<5}  total",
            percent(hit, total)
        );
        out
    }

    /// `{"tests":[{"file":…,"name":…,"lines":{"src/a.hy":[3,4]}}]}`.
    pub fn per_test_json(&self) -> Option<String> {
        let tests = self.per_test.as_ref()?;
        let mut out = String::from("{\"tests\":[");
        for (i, t) in tests.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let _ = write!(
                out,
                "{{\"file\":{},\"name\":{},\"lines\":{{",
                json_str(&t.file),
                json_str(&t.name)
            );
            for (j, (id, lines)) in t.lines.iter().enumerate() {
                if j > 0 {
                    out.push(',');
                }
                let nums: Vec<String> = lines.iter().map(u32::to_string).collect();
                let _ = write!(
                    out,
                    "{}:[{}]",
                    json_str(&self.files[*id].path),
                    nums.join(",")
                );
            }
            out.push_str("}}");
        }
        out.push_str("]}\n");
        Some(out)
    }
}

fn percent(hit: usize, total: usize) -> String {
    if total == 0 {
        "-".to_string()
    } else {
        format!("{:.1}%", hit as f64 * 100.0 / total as f64)
    }
}

pub(crate) fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// PC ranges of the test case functions (entry to the next symbol).
pub fn test_fn_ranges(
    debug: &ProgramDebug,
    entries: impl IntoIterator<Item = u32>,
) -> Vec<Range<usize>> {
    let syms = &debug.fn_symbols;
    let len = debug.debug_locs.len();
    entries
        .into_iter()
        .map(|entry| {
            let next = syms
                .iter()
                .map(|s| s.entry_pc)
                .filter(|&pc| pc > entry)
                .min()
                .map_or(len, |pc| pc as usize);
            entry as usize..next
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{DebugLoc, FnDebugSym};

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("coil_cov_{label}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(dir).unwrap()
    }

    fn loc(file: u32, start: u32) -> DebugLoc {
        DebugLoc {
            file,
            start_byte: start,
            end_byte: start + 1,
        }
    }

    #[test]
    fn lines_sum_across_cases_and_exclude_tests_and_deps() {
        let cwd = scratch("sum");
        std::fs::write(cwd.join("a.hy"), "l1\nl2\nl3\nl4\n").unwrap();
        std::fs::create_dir_all(cwd.join(".deps")).unwrap();
        std::fs::write(cwd.join(".deps/b.hy"), "x\n").unwrap();
        let debug = ProgramDebug {
            source_files: vec!["a.hy".into(), ".deps/b.hy".into()],
            // pc0 line1, pc1 line2, pc2 line3 (test body), pc3 dep, pc4 unknown, pc5 line4
            debug_locs: vec![
                loc(0, 0),
                loc(0, 3),
                loc(0, 6),
                loc(1, 0),
                DebugLoc::unknown(),
                loc(0, 9),
            ],
            fn_symbols: vec![
                FnDebugSym {
                    name: "f".into(),
                    entry_pc: 0,
                },
                FnDebugSym {
                    name: "t".into(),
                    entry_pc: 2,
                },
                FnDebugSym {
                    name: "g".into(),
                    entry_pc: 3,
                },
            ],
            debug_lines: Vec::new(),
        };
        let mut cov = Coverage::new(cwd.clone(), true);
        let exclude = test_fn_ranges(&debug, [2]);
        assert_eq!(exclude, vec![2..3]);
        let prog = cov.register_program(&debug, &exclude);
        cov.record(&prog, &[1, 0, 5, 7, 1, 0], "t.hy", "one");
        cov.record(&prog, &[2, 0, 0, 0, 0, 0], "t.hy", "two");
        assert_eq!(
            cov.lcov(),
            "TN:\nSF:a.hy\nDA:1,3\nDA:2,0\nDA:4,0\nLF:3\nLH:1\nend_of_record\n"
        );
        assert!(cov.summary().contains("33.3%"));
        let json = cov.per_test_json().unwrap();
        assert_eq!(
            json,
            "{\"tests\":[{\"file\":\"t.hy\",\"name\":\"one\",\"lines\":{\"a.hy\":[1]}},{\"file\":\"t.hy\",\"name\":\"two\",\"lines\":{\"a.hy\":[1]}}]}\n"
        );
        let _ = std::fs::remove_dir_all(&cwd);
    }

    /// A helper inlined into a test body still counts its hits there.
    #[test]
    fn inlined_copies_in_test_bodies_count() {
        let cwd = scratch("inline");
        std::fs::write(cwd.join("a.hy"), "helper\ntest\n").unwrap();
        let debug = ProgramDebug {
            source_files: vec!["a.hy".into()],
            // pc0 helper (line1), pc1 test body (line2), pc2 inlined helper (line1)
            debug_locs: vec![loc(0, 0), loc(0, 7), loc(0, 0)],
            fn_symbols: vec![
                FnDebugSym {
                    name: "helper".into(),
                    entry_pc: 0,
                },
                FnDebugSym {
                    name: "t".into(),
                    entry_pc: 1,
                },
            ],
            debug_lines: Vec::new(),
        };
        let mut cov = Coverage::new(cwd.clone(), false);
        let prog = cov.register_program(&debug, &test_fn_ranges(&debug, [1]));
        cov.record(&prog, &[0, 1, 1], "t.hy", "t");
        assert_eq!(
            cov.lcov(),
            "TN:\nSF:a.hy\nDA:1,1\nLF:1\nLH:1\nend_of_record\n"
        );
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn project_filter_rejects_outside_and_deps() {
        let cwd = scratch("filter");
        std::fs::write(cwd.join("in.hy"), "").unwrap();
        std::fs::create_dir_all(cwd.join(".deps/pkg")).unwrap();
        std::fs::write(cwd.join(".deps/pkg/x.hy"), "").unwrap();
        assert!(is_project_source("in.hy", &cwd));
        assert!(!is_project_source(".deps/pkg/x.hy", &cwd));
        assert!(!is_project_source("missing.hy", &cwd));
        assert!(!is_project_source("/etc/hostname", &cwd));
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn json_strings_are_escaped() {
        assert_eq!(json_str("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
    }
}
