//! `coil fmt` must not change what a program means, and formatting its own
//! output must change nothing (#1873).
//!
//! For every `.hy` file the CI `coil fmt --check` job covers (`examples`,
//! `tests`, `compiler/src/prelude`, `comptime/tests`, and the `compile_fail/`
//! files that parse), and for a copy of each with the indentation of every
//! line removed:
//!
//! 1. idempotence: `fmt(fmt(x)) == fmt(x)`;
//! 2. meaning: `parse(fmt(x))` is `parse(x)` up to byte positions.
//!
//! The corpus is kept `coil fmt`-clean, so on the files as written both hold
//! as soon as `fmt(x) == x`; the dedented copy makes the formatter lay out
//! every line again, which is where it can go wrong.
//!
//! The AST has no span-free equality: spans sit in every node tuple and in
//! patterns, contracts and effect clauses. Each is a chumsky `SimpleSpan`,
//! whose `Debug` is `start..end`, so trees are compared by their `Debug`
//! text with those positions blanked (see [`without_spans`]).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use parser::{Pratt, format_source};

/// The directories `coil fmt --check` runs over in `.github/workflows/ci.yml`.
const CORPUS: [&str; 4] = [
    "examples",
    "tests",
    "compiler/src/prelude",
    "comptime/tests",
];

/// Parsing recurses deeply in a debug build.
const STACK: usize = 16 * 1024 * 1024;

#[test]
fn fmt_is_idempotent_and_keeps_the_ast_on_the_corpus() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut files = Vec::new();
    for dir in CORPUS {
        hy_files(&repo.join(dir), &mut files);
    }
    files.sort();
    assert!(
        files.len() > 1000,
        "corpus not found under {}",
        repo.display()
    );

    let next = AtomicUsize::new(0);
    let checked = AtomicUsize::new(0);
    let failures: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            std::thread::Builder::new()
                .stack_size(STACK)
                .spawn_scoped(scope, || {
                    while let Some(path) = files.get(next.fetch_add(1, Ordering::Relaxed)) {
                        let name = path
                            .strip_prefix(repo)
                            .unwrap_or(path.as_path())
                            .display()
                            .to_string();
                        let outcome =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                check_file(&name, path)
                            }))
                            .unwrap_or_else(|_| Err(format!("{name}: panicked (message above)")));
                        match outcome {
                            Ok(true) => {
                                checked.fetch_add(1, Ordering::Relaxed);
                            }
                            Ok(false) => {}
                            Err(report) => failures.lock().unwrap().push((name, report)),
                        }
                    }
                })
                .expect("spawn fmt round-trip worker");
        }
    });

    let checked = checked.into_inner();
    let mut failures = failures.into_inner().unwrap();
    failures.sort();
    if let Some((_, report)) = failures.first() {
        let others: Vec<&str> = failures[1..]
            .iter()
            .take(20)
            .map(|(name, _)| name.as_str())
            .collect();
        panic!(
            "coil fmt round trip: {} of {} files fail; the first:\n\n{report}\n\nalso failing (first 20): [{}]",
            failures.len(),
            files.len(),
            others.join(", ")
        );
    }
    assert!(
        checked * 10 >= files.len() * 9,
        "only {checked} of {} files were checked (the rest did not parse)",
        files.len()
    );
}

/// `.hy` files under `dir`, recursively, `compile_fail/` included.
fn hy_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            hy_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "hy") {
            out.push(path);
        }
    }
}

/// Check one file as written and dedented. `Ok(false)`: a `compile_fail/`
/// file that does not parse, so there is nothing to format.
fn check_file(name: &str, path: &Path) -> Result<bool, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("{name}: read: {e}"))?;
    let compile_fail = path.components().any(|c| c.as_os_str() == "compile_fail");
    let once = match format_source(&src) {
        Ok(once) => once,
        Err(_) if compile_fail && Pratt::default().parse(&src).is_err() => return Ok(false),
        Err(e) => return Err(format!("{name}: coil fmt failed: {}", e.message())),
    };
    check(name, &src, &once, None)?;
    // `once` now formats to itself: `check` verified it, or it is `src`.
    let flat = dedent(&src);
    match format_source(&flat) {
        Ok(flat_once) => check(
            &format!("{name} (dedented)"),
            &flat,
            &flat_once,
            Some(once.as_str()),
        )?,
        // Removing indentation cannot break a parse today; if it ever does,
        // the copy is not a program and there is nothing to check.
        Err(_) if Pratt::default().parse(&flat).is_err() => {}
        Err(e) => {
            return Err(format!(
                "{name} (dedented): coil fmt failed: {}",
                e.message()
            ));
        }
    }
    Ok(true)
}

/// Both properties for `input`, given `once = fmt(input)`. `fixpoint` is a
/// text already known to format to itself, so it is not formatted again.
fn check(what: &str, input: &str, once: &str, fixpoint: Option<&str>) -> Result<(), String> {
    if once == input {
        // fmt(fmt(x)) = fmt(x), and both trees are parsed from the same text.
        return Ok(());
    }
    if fixpoint != Some(once) {
        let twice = format_source(once)
            .map_err(|e| format!("{what}: formatting fmt(x) failed: {}", e.message()))?;
        if twice != once {
            return Err(format!(
                "{what}: not idempotent, fmt(fmt(x)) != fmt(x):\n{}",
                unified_diff(once, &twice, "fmt(x)", "fmt(fmt(x))")
            ));
        }
    }
    let before = Pratt::default()
        .parse(input)
        .map_err(|e| format!("{what}: parse(x) failed: {}", e.message()))?;
    let after = Pratt::default()
        .parse(once)
        .map_err(|e| format!("{what}: parse(fmt(x)) failed: {}", e.message()))?;
    if without_spans(&format!("{before:?}")) != without_spans(&format!("{after:?}")) {
        let (before, after) = (
            without_spans(&format!("{before:#?}")),
            without_spans(&format!("{after:#?}")),
        );
        return Err(format!(
            "{what}: formatting changed the AST (positions ignored):\n{}",
            unified_diff(&before, &after, "parse(x)", "parse(fmt(x))")
        ));
    }
    Ok(())
}

/// `src` with the leading spaces and tabs of every line removed.
fn dedent(src: &str) -> String {
    src.split_inclusive('\n')
        .map(|line| line.trim_start_matches([' ', '\t']))
        .collect()
}

/// An AST's `Debug` text without byte positions, for comparing trees:
///
/// - every `start..end` (a `SimpleSpan`) outside a string literal becomes
///   `_`. Nothing else in the AST's derived `Debug` prints `..` outside a
///   string;
/// - a contract's `text` (the clause as laid out in the source) becomes `_`;
///   its parsed `expr` is still compared.
///
/// String literals are skipped whole: `Debug` escapes `"` and `\` in them,
/// so the first `"` not after a `\` ends one.
fn without_spans(debug: &str) -> String {
    let bytes = debug.as_bytes();
    let mut out = String::with_capacity(debug.len());
    // `debug[copied..i]` is still to be copied to `out`.
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let mut end = i + 1;
                while end < bytes.len() && bytes[end] != b'"' {
                    end += if bytes[end] == b'\\' { 2 } else { 1 };
                }
                let end = (end + 1).min(bytes.len());
                if debug[..i].ends_with(" text: ") {
                    out.push_str(&debug[copied..i]);
                    out.push('_');
                    copied = end;
                }
                i = end;
            }
            b'0'..=b'9' => {
                let start_end = digits_end(bytes, i);
                let end_start = start_end + 2;
                let is_span = bytes.get(start_end..end_start) == Some(b"..".as_slice())
                    && bytes.get(end_start).is_some_and(u8::is_ascii_digit);
                if is_span {
                    out.push_str(&debug[copied..i]);
                    out.push('_');
                    i = digits_end(bytes, end_start);
                    copied = i;
                } else {
                    i = start_end;
                }
            }
            _ => i += 1,
        }
    }
    out.push_str(&debug[copied..]);
    out
}

/// End of the run of ASCII digits starting at `from`.
fn digits_end(bytes: &[u8], from: usize) -> usize {
    from + bytes[from..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count()
}

/// A unified diff of `old` against `new`: one hunk around the changed lines
/// with three lines of context.
fn unified_diff(old: &str, new: &str, old_name: &str, new_name: &str) -> String {
    const CONTEXT: usize = 3;
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    if a == b {
        return format!(
            "--- {old_name}\n+++ {new_name}\n(only line endings or the final newline differ)\n"
        );
    }
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (a_end, b_end) = (a.len() - suffix, b.len() - suffix);
    let lead = prefix.saturating_sub(CONTEXT);
    let trail = suffix.min(CONTEXT);
    let mut out = format!(
        "--- {old_name}\n+++ {new_name}\n@@ -{},{} +{},{} @@\n",
        lead + 1,
        a_end + trail - lead,
        lead + 1,
        b_end + trail - lead
    );
    for line in &a[lead..prefix] {
        out.push_str(&format!(" {line}\n"));
    }
    for line in changed_lines(&a[prefix..a_end], &b[prefix..b_end]) {
        out.push_str(&line);
        out.push('\n');
    }
    for line in &a[a_end..a_end + trail] {
        out.push_str(&format!(" {line}\n"));
    }
    out
}

/// `-` / `+` / ` ` lines turning `a` into `b` (longest common subsequence).
fn changed_lines(a: &[&str], b: &[&str]) -> Vec<String> {
    if a.len().saturating_mul(b.len()) > 4_000_000 {
        let removed = a.iter().map(|line| format!("-{line}"));
        return removed
            .chain(b.iter().map(|line| format!("+{line}")))
            .collect();
    }
    // `lcs[i * w + j]`: longest common subsequence of `a[i..]` and `b[j..]`.
    let w = b.len() + 1;
    let mut lcs = vec![0u32; (a.len() + 1) * w];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i * w + j] = if a[i] == b[j] {
                lcs[(i + 1) * w + j + 1] + 1
            } else {
                lcs[(i + 1) * w + j].max(lcs[i * w + j + 1])
            };
        }
    }
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            out.push(format!(" {}", a[i]));
            i += 1;
            j += 1;
        } else if j < b.len() && (i == a.len() || lcs[i * w + j + 1] > lcs[(i + 1) * w + j]) {
            out.push(format!("+{}", b[j]));
            j += 1;
        } else {
            out.push(format!("-{}", a[i]));
            i += 1;
        }
    }
    out
}
