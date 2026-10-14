//! Test coverage from the lcov tracefile `coil test --coverage` writes
//! (`target/coverage/lcov.info`). The server only reads it: tests are never
//! run from here. Lines show as hint diagnostics, functions as code lenses
//! and in hover. A file edited after the tracefile was written shows nothing,
//! since its line numbers no longer match.

use std::{
    collections::{BTreeMap, HashMap},
    ops::Range,
    path::{Path, PathBuf},
    time::SystemTime,
};

use parser::{
    Pratt,
    ast::{Expression, Output},
};

/// Default tracefile, relative to the workspace.
pub const DEFAULT_TRACEFILE: &str = "target/coverage/lcov.info";

/// A function below this share of covered lines gets a hint.
pub const DEFAULT_THRESHOLD: f64 = 0.8;

/// `initializationOptions.coverage`: `false` turns coverage off; an object
/// may set `enabled`, `threshold` (0..=1) and `path` (the tracefile).
#[derive(Debug, Clone, PartialEq)]
pub struct CoverageOptions {
    pub enabled: bool,
    pub threshold: f64,
    pub path: Option<PathBuf>,
}

impl Default for CoverageOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: DEFAULT_THRESHOLD,
            path: None,
        }
    }
}

impl CoverageOptions {
    pub fn from_init(options: Option<&serde_json::Value>) -> Self {
        let mut out = Self::default();
        match options.and_then(|o| o.get("coverage")) {
            Some(serde_json::Value::Bool(enabled)) => out.enabled = *enabled,
            Some(serde_json::Value::Object(map)) => {
                if let Some(enabled) = map.get("enabled").and_then(serde_json::Value::as_bool) {
                    out.enabled = enabled;
                }
                if let Some(threshold) = map.get("threshold").and_then(serde_json::Value::as_f64) {
                    out.threshold = threshold.clamp(0.0, 1.0);
                }
                if let Some(path) = map.get("path").and_then(serde_json::Value::as_str) {
                    out.path = Some(PathBuf::from(path));
                }
            }
            _ => {}
        }
        out
    }
}

/// The loaded tracefile: line hits per canonical source path.
#[derive(Default)]
pub struct Coverage {
    pub options: CoverageOptions,
    tracefile: Option<PathBuf>,
    /// Where `coil test` ran: `SF:` paths are relative to it.
    base: PathBuf,
    /// Modification time of the loaded tracefile (`None`: none loaded).
    stamp: Option<SystemTime>,
    files: HashMap<PathBuf, BTreeMap<u32, u64>>,
}

impl Coverage {
    pub fn new(options: CoverageOptions, workspace: Option<&Path>) -> Self {
        let tracefile = options.enabled.then(|| {
            let path = options.path.clone().unwrap_or_else(|| PathBuf::from(DEFAULT_TRACEFILE));
            match workspace {
                Some(root) if path.is_relative() => root.join(path),
                _ => path,
            }
        });
        Self {
            options,
            tracefile,
            base: workspace.map_or_else(|| PathBuf::from("."), Path::to_path_buf),
            ..Self::default()
        }
    }

    pub fn enabled(&self) -> bool {
        self.tracefile.is_some()
    }

    /// Reload when the tracefile appeared, changed or went away; `true`
    /// when what is shown changes.
    pub fn refresh(&mut self) -> bool {
        let Some(tracefile) = &self.tracefile else {
            return false;
        };
        let stamp = std::fs::metadata(tracefile).and_then(|m| m.modified()).ok();
        if stamp == self.stamp {
            return false;
        }
        self.files = match (stamp, std::fs::read_to_string(tracefile)) {
            (Some(_), Ok(text)) => parse_lcov(&text, &self.base),
            _ => HashMap::new(),
        };
        self.stamp = stamp;
        true
    }

    /// Line → hits for `path` when `text` (its current contents) is what the
    /// tracefile measured: unchanged on disk since and not edited unsaved.
    pub fn lines_for(&self, path: &Path, text: &str) -> Option<&BTreeMap<u32, u64>> {
        let stamp = self.stamp?;
        let lines = self.files.get(&canonical(path))?;
        let meta = std::fs::metadata(path).ok()?;
        if meta.modified().ok()? > stamp {
            return None;
        }
        // An unsaved buffer: only when it still matches the file.
        if meta.len() != text.len() as u64 || std::fs::read_to_string(path).ok()? != text {
            return None;
        }
        Some(lines)
    }
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// `SF:` paths are relative to the directory `coil test` ran in (`base`).
pub fn parse_lcov(text: &str, base: &Path) -> HashMap<PathBuf, BTreeMap<u32, u64>> {
    let mut files: HashMap<PathBuf, BTreeMap<u32, u64>> = HashMap::new();
    let mut current: Option<PathBuf> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(path) = line.strip_prefix("SF:") {
            let path = Path::new(path);
            current = Some(canonical(&if path.is_relative() { base.join(path) } else { path.to_path_buf() }));
        } else if let Some(data) = line.strip_prefix("DA:") {
            let mut parts = data.split(',');
            let (Some(file), Some(Ok(number)), Some(Ok(hits))) = (
                current.as_ref(),
                parts.next().map(str::parse::<u32>),
                parts.next().map(str::parse::<u64>),
            ) else {
                continue;
            };
            *files.entry(file.clone()).or_default().entry(number).or_insert(0) += hits;
        } else if line == "end_of_record" {
            current = None;
        }
    }
    files
}

/// A function or method declared in a file.
#[derive(Debug, Clone, PartialEq)]
pub struct FnSpan {
    pub name: String,
    /// Byte range of the name.
    pub name_range: Range<usize>,
    /// 1-based first and last line of the declaration.
    pub lines: (u32, u32),
}

/// Lines of a function the tracefile knows about.
#[derive(Debug, Clone, PartialEq)]
pub struct FnCoverage {
    pub function: FnSpan,
    pub hit: usize,
    pub total: usize,
}

impl FnCoverage {
    pub fn ratio(&self) -> f64 {
        self.hit as f64 / self.total as f64
    }

    /// `7/9 lines (78%)`.
    pub fn summary(&self) -> String {
        format!("{}/{} lines ({:.0}%)", self.hit, self.total, self.ratio() * 100.0)
    }
}

/// Top-level functions and the methods of `impl` / trait blocks, outermost
/// first (a nested function's lines also count for its enclosing one).
pub fn functions(source: &str) -> Vec<FnSpan> {
    let Ok(ast) = Pratt::default().parse(source) else {
        return Vec::new();
    };
    let Expression::Program(items) = ast.1.as_ref() else {
        return Vec::new();
    };
    let starts = line_starts(source);
    let mut out = Vec::new();
    for item in items {
        collect(source, &starts, item, &mut out);
    }
    out
}

fn collect(source: &str, starts: &[usize], item: &Output<'_>, out: &mut Vec<FnSpan>) {
    match item.1.as_ref() {
        Expression::Function { name, .. } => {
            // Spans may run on over trailing whitespace.
            let text = source.get(item.0.start..item.0.end).unwrap_or("");
            let span = item.0.start..item.0.start + text.trim_end().len();
            let name_range = name_range(source, &span, name);
            out.push(FnSpan {
                name: (*name).to_owned(),
                name_range,
                lines: (line_of(starts, span.start), line_of(starts, span.end.saturating_sub(1))),
            });
        }
        Expression::Method(_, inner) => collect(source, starts, inner, out),
        Expression::Implementation { methods, .. }
        | Expression::TypeClass { methods, .. }
        | Expression::TypeClassImpl { methods, .. } => {
            for method in methods {
                collect(source, starts, method, out);
            }
        }
        _ => {}
    }
}

/// The `name` after `fn` in the declaration (the span may start at docs).
fn name_range(source: &str, span: &Range<usize>, name: &str) -> Range<usize> {
    let text = source.get(span.clone()).unwrap_or("");
    let mut from = 0;
    while let Some(at) = text[from..].find("fn") {
        let after = from + at + 2;
        let rest = &text[after..];
        let skipped = rest.len() - rest.trim_start().len();
        if skipped > 0 && rest[skipped..].starts_with(name) {
            let start = span.start + after + skipped;
            return start..start + name.len();
        }
        from = after;
    }
    span.start..span.start
}

pub fn line_starts(source: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(source.match_indices('\n').map(|(i, _)| i + 1))
        .collect()
}

/// 1-based line of `byte`.
pub fn line_of(starts: &[usize], byte: usize) -> u32 {
    starts.partition_point(|&s| s <= byte) as u32
}

/// The tracefile's lines that hold source text (generated code can carry
/// a blank line's number, such as the one past the end).
pub fn source_lines(lines: &BTreeMap<u32, u64>, source: &str) -> BTreeMap<u32, u64> {
    let text: Vec<&str> = source.split('\n').collect();
    lines
        .iter()
        .filter(|(line, _)| {
            (**line as usize)
                .checked_sub(1)
                .and_then(|i| text.get(i))
                .is_some_and(|t| !t.trim().is_empty())
        })
        .map(|(line, hits)| (*line, *hits))
        .collect()
}

/// Coverage of each function with at least one coverable line.
pub fn function_coverage(functions: &[FnSpan], lines: &BTreeMap<u32, u64>) -> Vec<FnCoverage> {
    functions
        .iter()
        .filter_map(|function| {
            let (first, last) = function.lines;
            let (hit, total) = lines
                .range(first..=last)
                .fold((0, 0), |(hit, total), (_, hits)| (hit + usize::from(*hits > 0), total + 1));
            (total > 0).then(|| FnCoverage {
                function: function.clone(),
                hit,
                total,
            })
        })
        .collect()
}

/// Runs of consecutive uncovered lines (1-based, inclusive), leaving out
/// functions nothing covered (their own hint says so).
pub fn uncovered_runs(lines: &BTreeMap<u32, u64>, skip: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    let mut previous: Option<u32> = None;
    for (&line, &hits) in lines {
        let skipped = skip.iter().any(|&(first, last)| (first..=last).contains(&line));
        if hits > 0 || skipped {
            previous = Some(line);
            continue;
        }
        match runs.last_mut() {
            // Only coverable lines count: blank lines between keep a run.
            Some(run) if previous == Some(run.1) => run.1 = line,
            _ => runs.push((line, line)),
        }
        previous = Some(line);
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lcov_sums_records_and_resolves_paths() {
        let base = Path::new("/nonexistent/proj");
        let text = "TN:\nSF:src/a.hy\nDA:1,3\nDA:2,0\nLF:2\nLH:1\nend_of_record\nSF:src/a.hy\nDA:2,1\nend_of_record\n";
        let files = parse_lcov(text, base);
        let lines = &files[&base.join("src/a.hy")];
        assert_eq!(lines.iter().map(|(l, h)| (*l, *h)).collect::<Vec<_>>(), [(1, 3), (2, 1)]);
    }

    #[test]
    fn functions_and_methods_with_line_ranges() {
        let source = "/// Doc.\nfn add(int a) -> int {\n    return a + 1;\n}\n\nclass P {\n    pub x: int,\n}\n\nimpl P {\n    pub fn get() -> int {\n        return self.x;\n    }\n}\n";
        let found = functions(source);
        let names: Vec<_> = found.iter().map(|f| (f.name.as_str(), f.lines)).collect();
        assert_eq!(names, [("add", (1, 4)), ("get", (11, 13))]);
        assert_eq!(&source[found[0].name_range.clone()], "add");
        assert_eq!(&source[found[1].name_range.clone()], "get");
    }

    #[test]
    fn function_totals_and_uncovered_runs() {
        let functions = vec![
            FnSpan { name: "a".into(), name_range: 0..0, lines: (1, 5) },
            FnSpan { name: "b".into(), name_range: 0..0, lines: (7, 9) },
            FnSpan { name: "generic".into(), name_range: 0..0, lines: (11, 12) },
        ];
        let lines: BTreeMap<u32, u64> = [(2, 1), (3, 0), (5, 0), (8, 0), (9, 0)].into();
        let cov = function_coverage(&functions, &lines);
        assert_eq!(cov.iter().map(|c| (c.hit, c.total)).collect::<Vec<_>>(), [(1, 3), (0, 2)]);
        assert_eq!(cov[0].summary(), "1/3 lines (33%)");
        // 3 and 5 are one run (4 is not coverable); `b` is skipped.
        assert_eq!(uncovered_runs(&lines, &[(7, 9)]), [(3, 5)]);
        let blank: BTreeMap<u32, u64> = [(1, 0), (2, 0), (3, 0)].into();
        assert_eq!(source_lines(&blank, "x\n\n").keys().collect::<Vec<_>>(), [&1]);
    }

    #[test]
    fn options_from_initialization() {
        let off = serde_json::json!({ "coverage": false });
        assert!(!CoverageOptions::from_init(Some(&off)).enabled);
        let custom = serde_json::json!({ "coverage": { "threshold": 0.5, "path": "cov/lcov.info" } });
        let options = CoverageOptions::from_init(Some(&custom));
        assert!(options.enabled);
        assert_eq!(options.threshold, 0.5);
        assert_eq!(options.path, Some(PathBuf::from("cov/lcov.info")));
        assert_eq!(CoverageOptions::from_init(None), CoverageOptions::default());
    }
}
