//! Discovery, per-file compile, and per-case execution.

use std::io::Write;
use std::path::{Path, PathBuf};

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use coil_host::{
    ExecutePipelineArgs, bind_cli_roots, execute_pipeline,
};
use common::ProgramDebug;
use compiler::{HostGrants, OptLevel, Pipeline};
use machine::reactor::{Reactor, TestCase, TestHandle, TestReport};
use machine::thread::ThreadSpawnContext;
use parser::doc_examples::{self, ExampleMode};
use parser::test_headers;
use reporting::progress::{Progress, paint};
use reporting::{ErrorCode, ReportConfig, ReportFormat};

use compiler::native_mir::coverage::Probes;

use crate::coverage::{Coverage, CoverageOptions, ProgramLines, project_filter, test_fn_ranges};
use crate::events::{self, Event};
use crate::order::{Order, format_seed};
use crate::watchdog::{WatchId, Watchdog};

/// How a run reports its progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Report {
    /// Text on stderr.
    #[default]
    Human,
    /// NDJSON events on stdout (`--json`).
    Json,
    /// Nothing (the `coil mutate --json` baseline).
    Silent,
}

/// What to run and how to compile it.
#[derive(Debug, Clone)]
pub struct TestOptions {
    /// Test root (a directory walked for `.coil` files).
    pub root: PathBuf,
    /// Stop after the first failed case.
    pub fail_fast: bool,
    /// File and case order (`--seed` / `--no-shuffle`).
    pub order: Order,
    /// Reactor workers running cases in parallel (`--jobs`, at least 1).
    pub jobs: usize,
    /// Also print passing cases' captured output (`--show-output`).
    pub show_output: bool,
    /// List every file with its verdict, not only failures (`--verbose`).
    pub verbose: bool,
    /// Line coverage (`--coverage`); `None` = off.
    pub coverage: Option<CoverageOptions>,
    pub opt_level: OptLevel,
    /// `-g`; `None` is `-g2` (`-g1` at `-Os`), and `-g2` with coverage.
    pub debug_level: Option<compiler::DebugLevel>,
    /// `--contracts`; tests check every clause unless told otherwise.
    pub contracts: Option<compiler::ContractLevel>,
    /// `--fold-calls on|off`; `None` keeps the pipeline default.
    pub fold_calls: Option<bool>,
    /// `--early-free on|off|check`; `None` follows the opt level.
    pub early_free: Option<compiler::EarlyFree>,
    /// `--nursery on|off`; `None` is on.
    pub nursery: Option<bool>,
    /// `--auto-par on|off`; `None` keeps the pipeline default.
    pub auto_par: Option<bool>,
    /// `--contract-runs`: calls per generated contract test (0: none).
    pub contract_runs: u32,
    pub grants: HostGrants,
    /// Extra `--root` module search directories.
    pub extra_roots: Vec<PathBuf>,
    /// Text, `--json` events, or nothing.
    pub report: Report,
    /// Run the examples in `///` docs (`--no-doc-tests`, `--doc-tests-only`).
    pub doc_tests: DocTests,
    /// `--wall-timeout`: a case that runs this long ends the run
    /// (`crate::watchdog`); `None` never does.
    pub wall_timeout: Option<std::time::Duration>,
    /// What runs the compiled code (`--engine`).
    pub engine: Engine,
    /// `--cell`: the name of this run's configuration, which selects the
    /// `// @xfail` header lines that apply ([`known_failure`]).
    pub cell: Option<String>,
}

/// `--engine`: native code (`docs/internals/native.md`) or the MIR
/// interpreter. A suite that passes on both shows a pass or the backend
/// changes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Engine {
    #[default]
    Native,
    /// The MIR interpreter (`docs/internals/mir-interp.md`).
    Mir,
}

impl Engine {
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "native" => Ok(Self::Native),
            "mir" => Ok(Self::Mir),
            _ => Err(format!("invalid --engine `{text}` (expected native|mir)")),
        }
    }

    /// What runs a suite unless asked otherwise: native code where there
    /// is any.
    pub fn host_default() -> Self {
        if cfg!(target_family = "wasm") { Self::Mir } else { Self::Native }
    }

    /// Open the capture this engine runs from at `level`: at `-O 0` the
    /// interpreter takes MIR as lowered, the reference the rest is compared
    /// against; otherwise it takes what native code compiles.
    fn start_capture(self, level: compiler::OptLevel) {
        if self == Self::Mir && level == compiler::OptLevel::None {
            compiler::native_mir::start_raw_capture();
        } else {
            compiler::native_mir::start_native_capture();
        }
    }
}

/// Make what this compile captured runnable. `None` when nothing compiles
/// or the backend fails (the file then fails to run).
fn native_code(pipeline: &Pipeline, file: &str, engine: Engine, coverage: bool) -> (Option<Arc<machine::native::NativeCode>>, Option<Probes>) {
    let mut bodies = compiler::native_mir::take_native_capture();
    let probes = coverage.then(|| compiler::native_mir::coverage::instrument(&mut bodies));
    let code = compiled_code(pipeline, bodies, file, engine);
    let probes = probes.filter(|_| code.is_some());
    (code, probes)
}

/// Make `bodies` runnable on `engine`; a host with no native code interprets.
fn compiled_code(pipeline: &Pipeline, bodies: Vec<compiler::native_mir::NativeBody>, file: &str, engine: Engine) -> Option<Arc<machine::native::NativeCode>> {
    let debug = pipeline.program_debug_full();
    let pcs = debug.fn_symbols.iter().map(|s| (s.name.clone(), s.body_id)).collect();
    if engine == Engine::Mir || cfg!(target_family = "wasm") {
        let program = coil_mir_interp::prepare(bodies, &pcs);
        inventory(&format!("file\t{file}\t{}\t{}", program.compiled.len(), program.refused.len()));
        for (name, why) in &program.refused {
            inventory(&format!("refused\t{file}\t{name}\t{why}"));
        }
        let complete = program.refused.is_empty();
        let code = machine::native::NativeCode::new(program.entries, program.sites).with_complete(complete).with_keep(program.keep);
        return (!code.is_empty()).then(|| Arc::new(code));
    }
    jitted_code(&bodies, &pcs, file)
}

#[cfg(target_family = "wasm")]
fn jitted_code(_: &[compiler::native_mir::NativeBody], _: &std::collections::HashMap<String, u32>, _: &str) -> Option<Arc<machine::native::NativeCode>> {
    None
}

/// JIT `bodies`.
#[cfg(not(target_family = "wasm"))]
fn jitted_code(bodies: &[compiler::native_mir::NativeBody], pcs: &std::collections::HashMap<String, u32>, file: &str) -> Option<Arc<machine::native::NativeCode>> {
    let program = match coil_native::jit(bodies, pcs) {
        Ok(p) => p,
        Err(e) => {
            inventory(&format!("failed\t{file}\t{e}"));
            return None;
        }
    };
    inventory(&format!("file\t{file}\t{}\t{}", program.compiled.len(), program.refused.len()));
    for (name, why) in &program.refused {
        inventory(&format!("refused\t{file}\t{name}\t{why}"));
    }
    if std::env::var_os("COIL_NATIVE_STATS").is_some() {
        eprintln!("{} ({} bodies)", coil_native::describe(&program), bodies.len());
        if std::env::var_os("COIL_NATIVE_STATS").is_some_and(|v| v == "2") {
            for (name, why) in &program.refused {
                eprintln!("  refused: {name}: {why}");
            }
        }
    }
    let complete = program.refused.is_empty();
    let code = machine::native::NativeCode::new(program.entries, program.sites).with_complete(complete).with_keep(program.keep);
    (!code.is_empty()).then(|| Arc::new(code))
}

/// Append one line to the native inventory file, `COIL_NATIVE_INVENTORY`
/// (`scripts/native_inventory.py` sums them): what a `--engine native` run
/// left on the runtime. Tab-separated: `file`, `refused`, `failed` per compile,
/// `counts` once per run. No variable: nothing is written.
fn inventory(line: &str) {
    use std::io::Write;
    let Some(path) = std::env::var_os("COIL_NATIVE_INVENTORY") else {
        return;
    };
    // One `write` per line on an append-mode file, so the compile threads
    // and the corpus processes sharing it never interleave a line.
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(format!("{}\n", line.replace('\n', " ")).as_bytes());
    }
}

/// Whether a run includes doc-tests (the code examples in `///` comments).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DocTests {
    /// Test files' cases, then the examples in the test files and in the
    /// project sources they import (the default).
    #[default]
    On,
    /// No doc-tests (`--no-doc-tests`; `coil mutate`).
    Off,
    /// Only the examples of every file under the test root; the files
    /// themselves are parsed, not compiled (`--doc-tests-only`).
    Only,
}

pub(crate) fn writer_for(format: ReportFormat) -> Box<dyn Write + Send> {
    match format {
        ReportFormat::Pretty => Box::new(std::io::stderr()),
        ReportFormat::Sarif | ReportFormat::Lsp => Box::new(std::io::stdout()),
    }
}

/// Every `.coil` file under `dir`, recursively, sorted by full path.
pub fn collect_test_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    if !dir.is_dir() {
        return Err(format!("tests directory `{}` not found", dir.display()));
    }
    let mut files = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
        let entries = std::fs::read_dir(dir)
            .map_err(|e| format!("unable to read `{}`: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("unable to read directory entry: {e}"))?;
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out)?;
            } else if common::ext::is_source_path(&path) {
                out.push(path);
            }
        }
        Ok(())
    }
    walk(dir, &mut files)?;
    files.sort();
    if files.is_empty() {
        return Err(format!(
            "no `.coil` test files found under `{}`",
            dir.display()
        ));
    }
    Ok(files)
}

/// Negative syntax / type tests live under any path segment named `compile_fail`.
/// Those files must fail to compile; a successful compile is a harness failure.
pub fn is_compile_fail(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == "compile_fail")
}

/// Error codes (`E0209`) a `compile_fail/` file declares on the `// @expected`
/// lines of its leading comment header.
pub fn declared_error_codes(src: &str) -> Vec<String> {
    test_headers::values(src, "expected").flat_map(error_codes_in).collect()
}

/// Grant and compile-time limit flags a file declares on the `// @flags` line
/// of its leading comment header (`// @flags --comptime-steps 100000`), added
/// to the run's own. `Ok(None)` when there is no such line.
pub fn declared_flags(src: &str) -> Result<Option<coil_args::HostGrantFlags>, String> {
    let Some(line) = test_headers::value(src, "flags") else {
        return Ok(None);
    };
    let words: Vec<&str> = line.split_whitespace().collect();
    coil_args::parse_grant_flags(&words)
        .map(Some)
        .map_err(|e| format!("bad `// @flags` header: {e}"))
}

/// Platform features a file declares it needs on `// @needs` lines
/// (`// @needs sockets`). `Err` names one this platform lacks, or one that
/// is not a known feature.
pub fn missing_platform_feature(src: &str) -> Result<(), String> {
    for name in test_headers::values(src, "needs").flat_map(str::split_whitespace) {
        let has = match name {
            // wasm (the website playground) has no sockets, no symlinks,
            // no dynamic libraries and no child processes. `unix` is for
            // behaviour only Unix has (signals, Unix paths).
            "sockets" | "symlinks" | "ffi" | "processes" => !cfg!(target_family = "wasm"),
            "unix" => cfg!(unix),
            // A file about the wasm build's own limits (its fixed stack).
            "wasm" => cfg!(target_family = "wasm"),
            other => return Err(format!("unknown `// @needs` feature `{other}`")),
        };
        if !has {
            return Err(format!("needs {name}, which this platform lacks"));
        }
    }
    Ok(())
}

/// Why a file is a known failure in the configuration `cell` names: the
/// text after the name on its `// @xfail <cell> #<issue> <why>` header line.
/// `Ok(None)` when no line names `cell`; `Err` when the line has no issue
/// number, so every known failure points at one.
pub fn known_failure(src: &str, cell: &str) -> Result<Option<String>, String> {
    for line in test_headers::values(src, "xfail") {
        let (name, why) = line.trim().split_once(char::is_whitespace).unwrap_or((line.trim(), ""));
        if name != cell {
            continue;
        }
        let issue = why.split(|c: char| !(c == '#' || c.is_ascii_digit())).any(|w| {
            w.strip_prefix('#').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        });
        if !issue {
            return Err(format!("`// @xfail {cell}` names no issue: write `// @xfail {cell} #<issue> <why>`"));
        }
        return Ok(Some(why.trim().to_string()));
    }
    Ok(None)
}

/// Why a file is not run in the configuration `cell` names: the text after
/// the name on its `// @skip <cell> <why>` header line (a failure the runner
/// does not survive there, or output that the cell cannot compare).
/// `Ok(None)` when no line names `cell`; `Err` when the line gives no reason.
pub fn skipped_in(src: &str, cell: &str) -> Result<Option<String>, String> {
    for line in test_headers::values(src, "skip") {
        let (name, why) = line.trim().split_once(char::is_whitespace).unwrap_or((line.trim(), ""));
        if name != cell {
            continue;
        }
        if why.trim().is_empty() {
            return Err(format!("`// @skip {cell}` gives no reason: write `// @skip {cell} <why>`"));
        }
        return Ok(Some(why.trim().to_string()));
    }
    Ok(None)
}

/// [`skipped_in`] for `path` in this run's `--cell`.
fn cell_skip(options: &TestOptions, path: &Path) -> Option<Result<String, String>> {
    let cell = options.cell.as_deref()?;
    let src = std::fs::read_to_string(path).ok()?;
    skipped_in(&src, cell).transpose()
}

/// The stdout a `fn main()`-only file declares on the `// @output` line of
/// its leading comment header, unescaped (`\n`, `\t`, `\e`, `\\`), as
/// `examples/` document theirs. `None` when there is no such line.
pub fn declared_output(src: &str) -> Option<String> {
    test_headers::value(src, "output").map(test_headers::unescape)
}

/// A header line still in the old `// @output` spelling, as a failure
/// message: it would no longer be checked.
pub fn legacy_header(src: &str) -> Option<String> {
    test_headers::legacy_line(src)
        .map(|(line, new)| format!("`{line}`: header directives are spelled `{new}` now"))
}

/// Every `E` + four digits in `text`, in order.
fn error_codes_in(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut codes = Vec::new();
    for i in 0..bytes.len() {
        let code = bytes.get(i..i + 5);
        let boundary = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        if let Some(code) = code
            && boundary
            && code[0] == b'E'
            && code[1..].iter().all(u8::is_ascii_digit)
            && bytes.get(i + 5).is_none_or(|b| !b.is_ascii_alphanumeric())
        {
            codes.push(String::from_utf8_lossy(code).into_owned());
        }
    }
    codes
}

/// A rejected `compile_fail/` file passes when its diagnostics include one of
/// the error codes its header declares, so a test cannot keep passing after
/// it starts failing for an unrelated reason.
fn compile_fail_verdict(src: &str, reported: &str) -> (bool, Option<String>) {
    let declared = declared_error_codes(src);
    if declared.is_empty() {
        return (
            false,
            Some("no expected error code: start the file with `// @expected E…`".to_string()),
        );
    }
    let mut got = error_codes_in(reported);
    got.dedup();
    if got.iter().any(|c| declared.contains(c)) {
        (true, None)
    } else {
        let got = if got.is_empty() {
            "none".to_string()
        } else {
            got.join(", ")
        };
        (
            false,
            Some(format!(
                "expected {}, compiler reported {got}",
                declared.join(" or ")
            )),
        )
    }
}

/// Classify a `catch_unwind` compile result for a `compile_fail/` file.
/// Only a clean diagnostic rejection (`Ok(Err(_))`) is harness success.
/// Panic does not count (release builds use `panic = "abort"`).
fn compile_fail_rejected<T, E>(compiled: &std::thread::Result<Result<T, E>>) -> bool {
    matches!(compiled, Ok(Err(_)))
}

/// Counts plus the order files were started in.
#[derive(Default)]
pub struct SuiteResult {
    pub passed: usize,
    pub failed: usize,
    pub files_run: Vec<PathBuf>,
    /// Summed line coverage when `--coverage` was on.
    pub coverage: Option<Coverage>,
    /// Every case's outcome, in report order.
    pub cases: Vec<CaseOutcome>,
    /// Text reports of the failed cases and files, for the end of the run.
    pub failures: Vec<String>,
    /// Passing files whose compiler warnings were not shown.
    pub quiet_diagnostics: usize,
    /// Files that failed as their `// @xfail` line for this `--cell` says.
    pub known_failures: usize,
    /// Files their `// @skip` line keeps out of this `--cell`.
    pub skipped: usize,
}

/// One case's verdict, as `coil mutate` needs it from the baseline run.
#[derive(Debug, Clone)]
pub struct CaseOutcome {
    /// Test file, as the runner was given it.
    pub file: PathBuf,
    pub name: String,
    pub passed: bool,
    /// Step-budget charges (see `Isolate::set_step_budget`).
    pub steps: u64,
    /// Covered project lines per canonical source path (with `--coverage`).
    pub covered: Vec<(PathBuf, Vec<u32>)>,
}

/// What every file of one run shares.
struct Run<'a> {
    config: &'a ReportConfig,
    options: &'a TestOptions,
    reactor: &'a Arc<Reactor>,
    coverage: Option<&'a Mutex<Coverage>>,
    /// Project sources found while compiling tests.
    sources: Discovered,
    /// The text report's progress bar.
    progress: Option<Progress>,
    /// The cases in flight, for `--wall-timeout`.
    watchdog: Watchdog,
}

/// Project sources (under the current directory, outside the test root and
/// `.deps/`) the test files import, found while compiling them.
#[derive(Default)]
pub(crate) struct Discovered {
    /// Sources with functions that get contract tests.
    contracts: Mutex<BTreeSet<PathBuf>>,
    /// Sources with code examples in their `///` docs.
    docs: Mutex<BTreeSet<PathBuf>>,
}

/// One file to report: compile it (its cases, or its contract tests when it
/// is a project source) and/or run the examples in its docs.
#[derive(Debug, Clone)]
struct Target {
    path: PathBuf,
    compile: bool,
    docs: bool,
}

/// A case's verdict, or the handle to wait on for it.
enum CaseState {
    Running(TestHandle),
    Done(TestReport),
}

/// One case with its captured output.
struct PendingCase {
    name: String,
    state: CaseState,
    /// Everything the case printed, panic messages and stderr included.
    output: Arc<Mutex<Vec<u8>>>,
    /// Its stdout alone, which `// @output` checks (#1828).
    stdout: Arc<Mutex<Vec<u8>>>,
    /// Its stderr writes and panic message.
    stderr: Arc<Mutex<Vec<u8>>>,
    kind: CaseKind,
    /// Its entry on the `--wall-timeout` list, until it is waited on.
    watch: Option<WatchId>,
}

/// A file's own case, or one of its doc examples.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CaseKind {
    Test,
    /// A doc example; `should_panic` passes only when it panics.
    Doc { should_panic: bool },
}

/// A file as the runner reports it. Files print in start order, so output
/// is the same for a given seed whatever `--jobs` is.
struct PendingFile {
    path: PathBuf,
    display: String,
    /// Compiler diagnostics, captured so parallel compiles do not interleave.
    diagnostics: Captured,
    /// File-level verdict (compile outcome, `compile_fail/`); `None` when
    /// the verdict is its cases'.
    verdict: Option<(bool, Option<String>)>,
    cases: Vec<PendingCase>,
    /// PC → source line map for recording this program's coverage.
    lines: Option<ProgramLines>,
    /// What the file's single case must print (`// @output` header).
    expected_output: Option<String>,
}

impl PendingFile {
    fn decided(path: &Path, diagnostics: Captured, verdict: (bool, Option<String>)) -> Self {
        PendingFile {
            path: path.to_path_buf(),
            display: path.display().to_string(),
            diagnostics,
            verdict: Some(verdict),
            cases: Vec::new(),
            lines: None,
            expected_output: None,
        }
    }
}

/// Shared byte buffer usable as a `Pipeline` report sink.
#[derive(Clone, Default)]
pub(crate) struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn take(&self) -> Vec<u8> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// Wait for a file's cases, report it, and add it to `result`. Returns the
/// file's failed count.
///
/// As text, a file's outcome moves the progress bar; a failure prints a
/// `FAILED` line at once, and its details (reason, the case's stdout and
/// stderr, the file's diagnostics) are kept in `result.failures` for the
/// report at the end. Passing files print nothing unless `--verbose` or
/// `--show-output`.
fn finish_file(run: &Run<'_>, file: PendingFile, result: &mut SuiteResult) -> usize {
    let report = run.options.report;
    let diagnostics = file.diagnostics.take();
    // SARIF / LSP diagnostics are machine-read: they go out as they come.
    let pretty = run.config.format == ReportFormat::Pretty;
    if !diagnostics.is_empty() && report == Report::Human && !pretty {
        let _ = writer_for(run.config.format).write_all(&diagnostics);
    }
    let (mut passed, mut failed) = (0, 0);
    let mut message = None;
    // Failure details for the end-of-run report.
    let mut blocks = Vec::new();
    let mut shown = Vec::new();
    if let Some((ok, m)) = file.verdict {
        if !ok {
            blocks.push(failure_block(&file.display, None, m.as_deref(), b"", b""));
        }
        message = m;
        if ok {
            passed += 1;
        } else {
            failed += 1;
        }
    }
    let mut case_events = Vec::new();
    let single_case = file.cases.iter().filter(|c| c.kind == CaseKind::Test).count() == 1;
    for case in file.cases {
        let TestReport {
            passed: report_ok,
            reason,
            hits,
            steps,
            timed_out,
        } = match case.state {
            CaseState::Running(handle) => handle.wait(),
            CaseState::Done(report) => report,
        };
        run.watchdog.done(case.watch);
        let mut ok = report_ok && !timed_out;
        let mut reason = reason;
        if case.kind == (CaseKind::Doc { should_panic: true }) && !timed_out {
            ok = !report_ok;
            reason = report_ok.then(|| "example did not panic (`should_panic`)".to_string());
        }
        let output = std::mem::take(&mut *case.output.lock().unwrap_or_else(|e| e.into_inner()));
        let stdout = std::mem::take(&mut *case.stdout.lock().unwrap_or_else(|e| e.into_inner()));
        let stderr = std::mem::take(&mut *case.stderr.lock().unwrap_or_else(|e| e.into_inner()));
        // A `// @output` header checks a passing case's prints; it names
        // one program, so a file with `test` blocks cannot use it.
        let expected_output = file.expected_output.as_ref().filter(|_| case.kind == CaseKind::Test);
        let reason = match expected_output {
            Some(_) if ok && !single_case => {
                ok = false;
                Some("`// @output` needs a file whose only case is `fn main()`".to_string())
            }
            Some(want) if ok && stdout != want.as_bytes() => {
                ok = false;
                Some(format!(
                    "printed {:?}, `// @output` expects {want:?}",
                    String::from_utf8_lossy(&stdout)
                ))
            }
            _ => reason,
        };
        let covered = match (run.coverage, &file.lines, hits) {
            (Some(cov), Some(lines), Some(hits)) => cov
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .record(lines, &hits, &file.display, &case.name),
            _ => Vec::new(),
        };
        result.cases.push(CaseOutcome {
            file: file.path.clone(),
            name: case.name.clone(),
            passed: ok,
            steps,
            covered,
        });
        let show = !ok || run.options.show_output;
        if ok {
            passed += 1;
        } else {
            failed += 1;
        }
        match report {
            Report::Human => {
                let reason = match (timed_out, &reason) {
                    (true, _) => Some("timed out".to_string()),
                    (false, r) => r.clone(),
                };
                if !ok {
                    blocks.push(failure_block(&file.display, Some(&case.name), reason.as_deref(), &stdout, &stderr));
                } else if show && !output.is_empty() {
                    shown.push(output_block(&file.display, &case.name, &stdout, &stderr));
                }
            }
            Report::Json => {
                let text = String::from_utf8_lossy(&output);
                let out = String::from_utf8_lossy(&stdout);
                let err = String::from_utf8_lossy(&stderr);
                case_events.push(
                    Event::object()
                        .str("name", &case.name)
                        .bool("ok", ok)
                        .bool("timed_out", timed_out)
                        .opt_str("reason", reason.as_deref())
                        .opt_str("output", (show && !text.is_empty()).then_some(&*text))
                        .opt_str("stdout", (show && !out.is_empty()).then_some(&*out))
                        .opt_str("stderr", (show && !err.is_empty()).then_some(&*err))
                        .finish(),
                );
            }
            Report::Silent => {}
        }
    }
    // A known failure of this `--cell` (`// @xfail`): the file must fail.
    // Its failing cases are not counted, and a file that passes fails, so
    // the line goes when its issue is fixed.
    let skipped = matches!(cell_skip(run.options, &file.path), Some(Ok(_)));
    if skipped {
        result.skipped += 1;
    }
    let known = run.options.cell.as_deref().filter(|_| !skipped && !is_compile_fail(&file.path)).and_then(|cell| {
        let src = std::fs::read_to_string(&file.path).unwrap_or_default();
        known_failure(&src, cell).map(|why| why.map(|why| (cell, why))).transpose()
    });
    let mut xfail = None;
    match known {
        Some(Ok((_, why))) if failed != 0 => {
            failed = 0;
            blocks.clear();
            result.known_failures += 1;
            xfail = Some(why);
        }
        Some(Ok((cell, why))) => {
            failed = 1;
            let m = format!("passes, but `// @xfail {cell} {why}` lists it as a known failure: remove that line");
            blocks.push(failure_block(&file.display, None, Some(&m), b"", b""));
            message = Some(m);
        }
        Some(Err(why)) => {
            failed = failed.max(1);
            blocks.insert(0, failure_block(&file.display, None, Some(&why), b"", b""));
            message = Some(why);
        }
        None => {}
    }
    match report {
        Report::Human => {
            let verbose = run.options.verbose || run.options.show_output;
            if xfail.is_some() {
                // Expected diagnostics and prints: nothing to show.
            } else if pretty && !diagnostics.is_empty() {
                let text = String::from_utf8_lossy(&diagnostics).trim_end().to_string();
                if failed != 0 {
                    // With the file's first failure: they are often why.
                    match blocks.first_mut() {
                        Some(block) => block.push_str(&format!("\ncompiler diagnostics:\n{text}")),
                        None => blocks.push(format!("---- {} ----\ncompiler diagnostics:\n{text}", file.display)),
                    }
                } else if verbose {
                    shown.push(format!("---- {} (compiler diagnostics) ----\n{text}", file.display));
                } else {
                    result.quiet_diagnostics += 1;
                }
            }
            let line = if let Some(why) = &xfail {
                verbose.then(|| format!("{} {} ({why})", paint("xfail", true), file.display))
            } else if failed == 0 {
                verbose.then(|| format!("{}   {}", paint("ok", true), file.display))
            } else {
                Some(format!("{} {}", paint("FAILED", false), file.display))
            };
            match &run.progress {
                Some(progress) => {
                    for text in line.iter().chain(&shown) {
                        progress.println(text);
                    }
                    progress.start(&file.display);
                    progress.finish_one(failed == 0);
                }
                None => {
                    for text in line.iter().chain(&shown) {
                        eprintln!("{text}");
                    }
                }
            }
            result.failures.extend(blocks);
        }
        Report::Json => {
            let diagnostics = String::from_utf8_lossy(&diagnostics);
            Event::new("file")
                .str("file", &file.display)
                .bool("ok", failed == 0)
                .num("passed", passed)
                .num("failed", failed)
                .opt_str("message", message.as_deref())
                .opt_str("xfail", xfail.as_deref())
                .opt_str(
                    "diagnostics",
                    (!diagnostics.is_empty()).then_some(&*diagnostics),
                )
                .raw("cases", &events::array(&case_events))
                .emit();
        }
        Report::Silent => {}
    }
    result.passed += passed;
    result.failed += failed;
    failed
}

/// A failed case (or file, with no case name) for the report at the end:
/// why it failed, then what it printed on each stream.
fn failure_block(file: &str, case: Option<&str>, reason: Option<&str>, stdout: &[u8], stderr: &[u8]) -> String {
    let mut block = match case {
        Some(case) if case != file => format!("---- {file}: {case} ----"),
        _ => format!("---- {file} ----"),
    };
    // File-level messages read `> Test "…" failed (why)`: keep the why.
    let reason = reason.map(|r| match r.strip_prefix(&format!("> Test \"{file}\" failed")) {
        Some(rest) => rest.trim().trim_start_matches('(').trim_end_matches(')'),
        None => r,
    });
    match reason {
        Some(reason) if !reason.is_empty() => block.push_str(&format!("\n{}: {reason}", paint("failed", false))),
        _ if case.is_none() => block.push_str(&format!("\n{}: does not compile", paint("failed", false))),
        _ => {}
    }
    push_stream(&mut block, "stdout", stdout);
    push_stream(&mut block, "stderr", stderr);
    block
}

/// A passing case's prints (`--show-output`).
fn output_block(file: &str, case: &str, stdout: &[u8], stderr: &[u8]) -> String {
    let mut block = if case == file {
        format!("---- {file} (output) ----")
    } else {
        format!("---- {file}: {case} (output) ----")
    };
    push_stream(&mut block, "stdout", stdout);
    push_stream(&mut block, "stderr", stderr);
    block
}

/// `\n<name>:\n<text>` when `bytes` is not empty.
fn push_stream(block: &mut String, name: &str, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    let text = String::from_utf8_lossy(bytes);
    block.push_str(&format!("\n{name}:\n{}", text.trim_end_matches('\n')));
}

/// Queue cases on the pool, or run them right here (`--jobs 1`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dispatch {
    Pool,
    Inline,
}

/// A test file compiled and wired, ready to run its cases.
pub(crate) struct Prepared {
    pub ctx: ThreadSpawnContext,
    /// `(name, entry)` in run order. A file without cases runs `main` once
    /// as a single case named after the file.
    pub cases: Vec<(String, u32)>,
    /// See [`TestCase::expect_ok_result`].
    pub expect_ok_result: bool,
    pub debug: ProgramDebug,
    /// Native code's coverage probes; `None` counts PCs on the runtime.
    pub probes: Option<Probes>,
}

impl Prepared {
    pub fn case(&self, entry: u32, coverage: bool, step_budget: Option<u64>) -> TestCase {
        TestCase {
            entry,
            expect_ok_result: self.expect_ok_result,
            coverage: coverage && self.probes.is_none(),
            probes: self.probes.as_ref().filter(|_| coverage).map_or(0, |p| p.locs.len() as u32),
            step_budget,
        }
    }

    /// What this file's coverage counts by: its probes' locations and the
    /// ranges of them in test bodies, else its PCs'.
    fn coverage_map(&self) -> (std::borrow::Cow<'_, ProgramDebug>, Vec<std::ops::Range<usize>>) {
        let entries = self.cases.iter().map(|(_, entry)| *entry);
        let Some(probes) = &self.probes else {
            return (std::borrow::Cow::Borrowed(&self.debug), test_fn_ranges(&self.debug, entries));
        };
        let names: Vec<&str> = entries
            .filter_map(|e| self.debug.fn_symbols.iter().find(|s| s.body_id == e).map(|s| s.name.as_str()))
            .collect();
        let tests = probes.bodies.iter().filter(|(name, _)| names.contains(&name.as_str())).map(|(_, r)| r.clone()).collect();
        let mut debug = ProgramDebug::default();
        debug.source_files = self.debug.source_files.clone();
        debug.debug_locs = probes.locs.clone();
        (std::borrow::Cow::Owned(debug), tests)
    }
}

thread_local! {
    /// The file this thread is compiling, for [`note_compiler_panics`].
    static COMPILING: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Make a panic inside a compile name the file being compiled. A release
/// harness aborts on a panic (`panic = "abort"`), so the run's log ends at
/// the panic message, which names a pass or a function at best (a
/// `COIL_MIR_VERIFY` violation: `MIR verify failed after licm in main`).
fn note_compiler_panics() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            previous(info);
            COMPILING.with(|c| {
                if let Ok(c) = c.try_borrow()
                    && let Some(file) = c.as_deref()
                {
                    eprintln!("note: the compiler panicked while compiling {file}");
                }
            });
        }));
    });
}

/// A test file's compile result.
pub(crate) enum Compiled {
    /// The file's verdict is its compile (`compile_fail/`, errors, a file
    /// without cases or `main` run from the top): `(ok, message)`.
    Decided(bool, Option<String>),
    Ready(Box<Prepared>),
    /// A project source compiled for its contract tests has none to run.
    Nothing,
}

/// Compile one test file with `overlays` (path spelling → text) standing in
/// for sources on disk. Diagnostics go to `diagnostics`; `None` drops them.
pub(crate) fn compile_test_file(
    config: &ReportConfig,
    options: &TestOptions,
    reactor: &Arc<Reactor>,
    path: &Path,
    diagnostics: Option<&Captured>,
    overlays: &[(PathBuf, String)],
    sources: Option<&Discovered>,
) -> Compiled {
    // A project source outside the test root runs only its generated
    // contract tests: not its `main`, not its own `test` blocks.
    let contract_source = is_contract_source(options, path);
    let display = path.display().to_string();
    let expect_compile_fail = is_compile_fail(path);
    // Expected compile rejection: suppress ariadne noise so the harness
    // summary stays readable when many compile_fail files exist.
    // A rejection's diagnostics are kept apart: the case checks their error
    // codes against the file's `// @expected` header instead of printing them.
    let rejection = Captured::default();
    let reporter: Box<dyn Write + Send> = match diagnostics {
        Some(d) if !expect_compile_fail => Box::new(d.clone()),
        _ if expect_compile_fail => Box::new(rejection.clone()),
        _ => Box::new(std::io::sink()),
    };
    let mut pipeline = run_pipeline(config, options, reporter);
    let header = overlays
        .iter()
        .find(|(file, _)| file == path)
        .map(|(_, text)| text.clone())
        .or_else(|| std::fs::read_to_string(path).ok());
    if let Some(why) = header.as_deref().and_then(legacy_header) {
        return Compiled::Decided(false, Some(format!("> Test \"{display}\" failed ({why})")));
    }
    if let Some(Err(why)) = header.as_deref().map(missing_platform_feature) {
        if why.starts_with("unknown") {
            return Compiled::Decided(false, Some(format!("> Test \"{display}\" failed ({why})")));
        }
        // Skipped: a platform without the feature cannot run the file.
        return Compiled::Nothing;
    }
    match header.as_deref().map(declared_flags) {
        Some(Ok(Some(flags))) => {
            let mut grants = options.grants.clone();
            flags.apply_to(&mut grants);
            pipeline.set_host_grants(grants);
        }
        Some(Err(why)) => {
            let m = format!("> Test \"{display}\" failed ({why})");
            return Compiled::Decided(false, Some(m));
        }
        _ => {}
    }
    pipeline.set_include_tests(true);
    pipeline.set_contract_runs(options.contract_runs);
    if let Some(cov) = &options.coverage {
        // Keep never-called project functions so they report as uncovered.
        pipeline.set_keep_fns_in(Some(project_filter(canonical(&cov.project_root))));
    }
    for (file, text) in overlays {
        pipeline.set_file_text(file.clone(), text.clone());
    }

    // catch_unwind isolates a compiler ICE from aborting the whole
    // harness under panic=unwind. Release builds use panic=abort, so
    // compile_fail fixtures must reject via Ok(Err(_)), not panic.
    let native = !expect_compile_fail;
    if native {
        options.engine.start_capture(options.opt_level);
    }
    note_compiler_panics();
    COMPILING.with(|c| *c.borrow_mut() = Some(display.clone()));
    let compiled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pipeline.compile_src_from_file(&display)
    }));
    COMPILING.with(|c| *c.borrow_mut() = None);
    let native = match &compiled {
        Ok(Ok(_)) if native => native_code(&pipeline, &display, options.engine, options.coverage.is_some()),
        _ => {
            let _ = compiler::native_mir::take_native_capture();
            (None, None)
        }
    };
    let (native, probes) = native;
    let mut cases: Vec<(String, u32)> = pipeline.test_cases().to_vec();
    if contract_source {
        cases.retain(|(name, _)| name.starts_with(compiler::CONTRACT_CASE_PREFIX));
    } else if let Some(sources) = sources {
        let found = project_sources(options, pipeline.contract_sources());
        lock(&sources.contracts).extend(found);
        if options.doc_tests == DocTests::On {
            let found = project_sources(options, pipeline.discovered_files());
            lock(&sources.docs).extend(found.into_iter().filter(|f| has_examples(f)));
        }
    }
    options.order.order_cases(&options.root, path, &mut cases);
    let _ = pipeline.finish_reporting();

    if expect_compile_fail {
        // Only a clean diagnostic rejection counts. A panic is a
        // harness failure (and aborts under release panic=abort).
        let (ok, why) = match &compiled {
            _ if compile_fail_rejected(&compiled) => {
                let src = std::fs::read_to_string(path).unwrap_or_default();
                let reported = String::from_utf8_lossy(&rejection.take()).into_owned();
                compile_fail_verdict(&src, &reported)
            }
            Ok(Ok(_)) => (false, Some("expected compile failure".to_string())),
            _ => (false, Some("compiler panicked".to_string())),
        };
        let message = why.map(|why| format!("> Test \"{display}\" failed ({why})"));
        return Compiled::Decided(ok, message);
    }
    let constants = match compiled {
        Err(_) => {
            let m = format!("> Test \"{display}\" failed (compiler panicked)");
            return Compiled::Decided(false, Some(m));
        }
        Ok(Err(_)) => {
            return Compiled::Decided(false, Some(format!("> Test \"{display}\" failed")));
        }
        Ok(Ok(ok)) => ok,
    };
    if contract_source && cases.is_empty() {
        return Compiled::Nothing;
    }
    let main = pipeline.main_offset();

    if cases.is_empty() && main.is_none() {
        let strings = pipeline.strings().to_vec();
        // No cases and no `main` entry: run the program from the top, as
        // `coil <file>` would, on this thread.
        if let Some(code) = &native {
            machine::native::install_shared_for_next_run(Arc::clone(code));
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            execute_pipeline(ExecutePipelineArgs {
                pipeline: &pipeline,
                constants: &constants,
                strings: &strings,
                static_slots: pipeline.static_slot_count(),
                debug: pipeline.program_debug(),
                entry: Some(path),
                })
        }));
        let ok = matches!(result, Ok(false));
        return Compiled::Decided(ok, (!ok).then(|| format!("> Test \"{display}\" failed")));
    }

    let Some((ctx, debug)) = wire_program(pipeline, constants, path, reactor, native) else {
        let m = format!("> Test \"{display}\" failed (no thread program)");
        return Compiled::Decided(false, Some(m));
    };

    // A file without cases runs `main` once as a single case. An `Err` that
    // reaches the end of `main` panics (#1353), so a panic is what fails it.
    let (cases, expect_ok_result) = if cases.is_empty() {
        (vec![(display, main.unwrap_or(0))], false)
    } else {
        (cases, true)
    };
    Compiled::Ready(Box::new(Prepared {
        probes,
        ctx,
        cases,
        expect_ok_result,
        debug,
    }))
}

/// A pipeline set up as the run asks: `-O`, contracts, grants and the
/// `--root` search path. Diagnostics go to `reporter`.
fn run_pipeline(config: &ReportConfig, options: &TestOptions, reporter: Box<dyn Write + Send>) -> Pipeline {
    let mut pipeline = Pipeline::with_reporter(config.clone(), reporter);
    pipeline.set_opt_level(options.opt_level);
    pipeline.set_runs_mir();
    if let Some(level) = options.debug_level {
        pipeline.set_debug_level(level);
    } else if options.coverage.is_some() {
        // Coverage maps hits through byte spans.
        pipeline.set_debug_level(compiler::DebugLevel::Full);
    }
    pipeline.set_contracts(options.contracts.unwrap_or(compiler::ContractLevel::All));
    if let Some(mode) = options.early_free {
        pipeline.set_early_free(mode.compiles());
    }
    if let Some(on) = options.fold_calls {
        pipeline.set_call_fold(on);
    } else if options.coverage.is_some() {
        // A computed call never runs its body, so its lines would read
        // uncovered and `coil mutate` would skip them (#1856).
        pipeline.set_call_fold(false);
    }
    if let Some(on) = options.auto_par {
        pipeline.set_auto_par(on);
    }
    pipeline.set_host_grants(options.grants.clone());
    // Same search path CI passes with `--root`: examples and a sibling
    // coil-stdlib checkout, when those directories exist.
    bind_cli_roots(&mut pipeline, search_roots(options));
    pipeline
}

/// `--root` directories plus this repo's examples and a sibling coil-stdlib
/// checkout, when those exist.
fn search_roots(options: &TestOptions) -> Vec<PathBuf> {
    let mut roots = options.extra_roots.clone();
    for extra in compiler::Pipeline::workspace_language_extra_roots() {
        if extra.is_dir() && !roots.contains(&extra) {
            roots.push(extra);
        }
    }
    roots
}

/// Wire a compiled program for test jobs: the thread context every case
/// starts from, where static init starts (when it must stop before `main`),
/// and the debug info. `None` when the program has no thread context.
fn wire_program(
    pipeline: Pipeline,
    constants: Vec<u64>,
    path: &Path,
    reactor: &Arc<Reactor>,
    native: Option<Arc<machine::native::NativeCode>>,
) -> Option<(ThreadSpawnContext, ProgramDebug)> {
    let strings = pipeline.strings().to_vec();
    let debug = pipeline.program_debug();
    let start = coil_host::native_start(&pipeline, &pipeline.archived_struct_layouts(), Some(path));
    // The same word maps as `coil <file>`, so a case's `collect()` exercises
    // production GC roots (#555).
    let program = coil_host::thread_program(
        &pipeline,
        coil_host::CompiledProgram {
            constants: &constants,
            strings: &strings,
            static_slots: pipeline.static_slot_count(),
            debug: debug.clone(),
        },
    );
    let ctx = machine::spawn_context(Arc::new(program), start, Arc::clone(reactor), native);
    drop(pipeline);
    Some((ctx?, debug))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// True when `path` is not under the test root: a project source compiled
/// for its contract tests.
fn is_contract_source(options: &TestOptions, path: &Path) -> bool {
    !canonical(path).starts_with(canonical(&options.root))
}

/// The files of `found` that are project sources: under the current
/// directory, outside the test root and `.deps/`.
fn project_sources(options: &TestOptions, found: &[PathBuf]) -> Vec<PathBuf> {
    let Ok(cwd) = std::env::current_dir().map(|d| canonical(&d)) else { return Vec::new() };
    let root = canonical(&options.root);
    found
        .iter()
        .map(|f| canonical(f))
        .filter(|f| f.is_file() && f.starts_with(&cwd) && !f.starts_with(&root))
        .filter(|f| !f.strip_prefix(&cwd).is_ok_and(|rel| rel.components().any(|c| c.as_os_str() == ".deps")))
        .map(|f| f.strip_prefix(&cwd).map(Path::to_path_buf).unwrap_or(f))
        .collect()
}

/// Compile one file, then decide it or start its cases; then start its
/// doc examples.
fn start_file(run: &Run<'_>, dispatch: Dispatch, target: &Target) -> PendingFile {
    let path = target.path.as_path();
    // `// @skip <cell>`: nothing of the file runs in this cell.
    match cell_skip(run.options, path) {
        Some(Ok(_)) => {
            return PendingFile {
                path: path.to_path_buf(),
                display: path.display().to_string(),
                diagnostics: Captured::default(),
                verdict: None,
                cases: Vec::new(),
                lines: None,
                expected_output: None,
            };
        }
        Some(Err(why)) => return PendingFile::decided(path, Captured::default(), (false, Some(why))),
        None => {}
    }
    let mut file = if target.compile {
        compile_and_start(run, dispatch, path)
    } else {
        PendingFile {
            path: path.to_path_buf(),
            display: path.display().to_string(),
            diagnostics: Captured::default(),
            verdict: None,
            cases: Vec::new(),
            lines: None,
            expected_output: None,
        }
    };
    if target.docs && !is_compile_fail(path) {
        file.cases.extend(doc_cases(run, dispatch, path, !target.compile));
    }
    file
}

fn compile_and_start(run: &Run<'_>, dispatch: Dispatch, path: &Path) -> PendingFile {
    let display = path.display().to_string();
    let diagnostics = Captured::default();
    let compiled = compile_test_file(
        run.config,
        run.options,
        run.reactor,
        path,
        Some(&diagnostics),
        &[],
        Some(&run.sources),
    );
    let prepared = match compiled {
        Compiled::Decided(ok, message) => {
            return PendingFile::decided(path, diagnostics, (ok, message));
        }
        Compiled::Nothing => {
            return PendingFile {
                path: path.to_path_buf(),
                display: path.display().to_string(),
                diagnostics,
                verdict: None,
                cases: Vec::new(),
                lines: None,
                expected_output: None,
            };
        }
        Compiled::Ready(prepared) => prepared,
    };
    let lines = run.coverage.map(|cov| {
        let (debug, tests) = prepared.coverage_map();
        cov.lock()
            .unwrap_or_else(|e| e.into_inner())
            .register_program(&debug, &tests)
    });
    let cases = prepared
        .cases
        .iter()
        .map(|(name, entry)| {
            let case = prepared.case(*entry, run.coverage.is_some(), None);
            let ctx = prepared.ctx.clone();
            start_case(run, dispatch, &display, name.clone(), ctx, case, CaseKind::Test)
        })
        .collect::<Vec<_>>();
    let expected_output = std::fs::read_to_string(path)
        .ok()
        .and_then(|src| declared_output(&src));
    PendingFile {
        path: path.to_path_buf(),
        display: path.display().to_string(),
        diagnostics,
        verdict: None,
        cases,
        lines,
        expected_output,
    }
}

/// Queue (or run) one case of `file`, capturing its output.
fn start_case(
    run: &Run<'_>,
    dispatch: Dispatch,
    file: &str,
    name: String,
    mut ctx: ThreadSpawnContext,
    case: TestCase,
    kind: CaseKind,
) -> PendingCase {
    let output = Arc::new(Mutex::new(Vec::new()));
    let print = Arc::clone(&output);
    // The case and the threads it spawns copy their stdout here too.
    let stdout = Arc::new(Mutex::new(Vec::new()));
    ctx.shared_stdout = Some(Arc::clone(&stdout));
    let stderr = Arc::new(Mutex::new(Vec::new()));
    ctx.shared_stderr = Some(Arc::clone(&stderr));
    let (state, watch) = match dispatch {
        Dispatch::Pool => {
            let handle = run.reactor.submit_test(ctx, case, print);
            let watch = run.watchdog.watch(file, &name, Some(handle.probe()), Arc::clone(&output));
            (CaseState::Running(handle), watch)
        }
        Dispatch::Inline => {
            // On this thread: the watch ends when the case returns.
            let watch = run.watchdog.watch(file, &name, None, Arc::clone(&output));
            let report = run.reactor.run_test_here(ctx, case, print);
            run.watchdog.done(watch);
            (CaseState::Done(report), None)
        }
    };
    PendingCase {
        name,
        state,
        output,
        stdout,
        stderr,
        kind,
        watch,
    }
}

/// A case decided without running anything.
fn decided_case(name: String, passed: bool, reason: Option<String>, output: Vec<u8>, kind: CaseKind) -> PendingCase {
    PendingCase {
        name,
        state: CaseState::Done(TestReport {
            passed,
            reason,
            hits: None,
            steps: 0,
            timed_out: false,
        }),
        output: Arc::new(Mutex::new(output.clone())),
        stdout: Arc::default(),
        stderr: Arc::new(Mutex::new(output)),
        kind,
        watch: None,
    }
}

/// `path` with `/` between its components on every OS, so doc-test names
/// are the same on Windows as elsewhere.
fn slash_path(path: &Path) -> String {
    let display = path.display().to_string();
    if cfg!(windows) { display.replace('\\', "/") } else { display }
}

/// True when `path` has a fenced block in its text (a cheap check before
/// parsing it for doc examples).
fn has_examples(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|src| src.contains("```"))
}

/// One doc-test per coil example in the `///` docs of `path`, each compiled
/// as its own program with the run's roots and grants, then started. A file
/// that does not parse yields no doc-tests (its compile reports the error)
/// unless it is not compiled (`report_parse_error`).
fn doc_cases(run: &Run<'_>, dispatch: Dispatch, path: &Path, report_parse_error: bool) -> Vec<PendingCase> {
    let Ok(source) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    if !source.contains("```") {
        return Vec::new();
    }
    let display = slash_path(path);
    let ast = match parser::Pratt::default().parse(&source) {
        Ok(ast) => ast,
        Err(e) if report_parse_error => {
            let name = format!("doc: {display}");
            let reason = format!("cannot read its doc examples: {e:?}");
            return vec![decided_case(name, false, Some(reason), Vec::new(), CaseKind::Doc { should_panic: false })];
        }
        Err(_) => return Vec::new(),
    };
    let module = module_path(run.options, path);
    let mut found = Vec::new();
    for item in doc_examples::documented_items(&ast) {
        for example in doc_examples::examples(item.docs) {
            if example.mode == ExampleMode::Ignore {
                continue;
            }
            let line = doc_examples::line_of(&source, item.docs[example.fence]).unwrap_or(0);
            let item_path = if module.is_empty() {
                item.path.clone()
            } else {
                format!("{module}::{}", item.path)
            };
            let name = format!("doc: {item_path} ({display}:{line})");
            found.push((name, line, example.mode, example.program()));
        }
    }
    run.options.order.order_cases(&run.options.root, path, &mut found);
    found
        .into_iter()
        .map(|(name, line, mode, program)| start_doc_case(run, dispatch, path, name, line, mode, program))
        .collect()
}

/// Compile one doc example as its own program next to the file it documents
/// (so relative paths and `--root` imports resolve the same), then start it.
fn start_doc_case(
    run: &Run<'_>,
    dispatch: Dispatch,
    path: &Path,
    name: String,
    line: usize,
    mode: ExampleMode,
    program: String,
) -> PendingCase {
    let kind = CaseKind::Doc {
        should_panic: mode == ExampleMode::ShouldPanic,
    };
    // A program that did not get to run fails even under `should_panic`.
    let kind_failed = CaseKind::Doc { should_panic: false };
    let display = path.display().to_string();
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("doc");
    let entry = path.with_file_name(format!("{stem}__doc_{line}.coil"));
    let diagnostics = Captured::default();
    let mut pipeline = run_pipeline(run.config, run.options, Box::new(diagnostics.clone()));
    pipeline.set_file_text(entry.clone(), program);
    run.options.engine.start_capture(run.options.opt_level);
    let compiled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pipeline.compile_src_from_file(&entry.display().to_string())
    }));
    let native = match &compiled {
        Ok(Ok(_)) => native_code(&pipeline, &entry.display().to_string(), run.options.engine, false).0,
        _ => {
            let _ = compiler::native_mir::take_native_capture();
            None
        }
    };
    let _ = pipeline.finish_reporting();
    let constants = match compiled {
        Ok(Ok(ok)) => ok,
        Ok(Err(_)) => {
            let why = "example does not compile".to_string();
            return decided_case(name, false, Some(why), diagnostics.take(), kind_failed);
        }
        Err(_) => {
            let why = "compiler panicked on the example".to_string();
            return decided_case(name, false, Some(why), diagnostics.take(), kind_failed);
        }
    };
    if mode == ExampleMode::NoRun {
        return decided_case(name, true, None, Vec::new(), kind);
    }
    let Some(main) = pipeline.main_offset() else {
        let why = "example has no `main` to run".to_string();
        return decided_case(name, false, Some(why), Vec::new(), kind_failed);
    };
    let Some((ctx, _debug)) = wire_program(pipeline, constants, &entry, run.reactor, native) else {
        let why = "no thread program".to_string();
        return decided_case(name, false, Some(why), Vec::new(), kind_failed);
    };
    // An `Err` that reaches the end of `main` panics (#1353), so a failed
    // `assert!` in the example fails it.
    let case = TestCase {
        entry: main,
        expect_ok_result: false,
        coverage: false,
        probes: 0,
        step_budget: None,
    };
    start_case(run, dispatch, &display, name, ctx, case, kind)
}

/// The module path of `path` for doc-test names: its path without `.coil`
/// under the deepest of the test root, the `--root` directories and `src`
/// that holds it (`src/io/sync.coil` → `io::sync`).
fn module_path(options: &TestOptions, path: &Path) -> String {
    let file = canonical(path);
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut roots = vec![options.root.clone(), cwd.join("src")];
    roots.extend(options.extra_roots.iter().map(|r| cwd.join(r)));
    let rel = roots
        .iter()
        .map(|r| canonical(r))
        .filter(|r| file.starts_with(r))
        .max_by_key(|r| r.components().count())
        .and_then(|r| file.strip_prefix(r).ok().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from(path.file_name().unwrap_or_default()));
    let rel = rel.with_extension("");
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("::")
}

/// Run the harness over `options.root` without exiting.
pub fn run_test_suite(config: ReportConfig, options: &TestOptions) -> Result<SuiteResult, String> {
    // Every case runs in this process: `env::exit` ends its case, not the run (#1401).
    machine::env::trap_exit(true);
    let mut files = collect_test_files(&options.root)?;
    options.order.order_files(&mut files);
    let jobs = options.jobs.max(1);
    match options.report {
        Report::Human => eprintln!(
            "running {} file{} ({}, {} job{})",
            files.len(),
            if files.len() == 1 { "" } else { "s" },
            options.order.describe(),
            jobs,
            if jobs == 1 { "" } else { "s" },
        ),
        Report::Json => Event::new("start")
            .num("files", files.len())
            .opt_str("seed", seed_of(options.order).as_deref())
            .num("jobs", jobs)
            .emit(),
        Report::Silent => {}
    }

    let reactor = Reactor::new(jobs);
    let coverage = options.coverage.as_ref().map(|c| {
        Mutex::new(Coverage::new(
            canonical(&c.project_root),
            c.per_test_out.is_some(),
        ))
    });
    let run = Run {
        config: &config,
        options,
        reactor: &reactor,
        coverage: coverage.as_ref(),
        sources: Discovered::default(),
        progress: (options.report == Report::Human)
            .then(|| Progress::new("test", "files", files.len())),
        watchdog: match options.wall_timeout {
            Some(limit) => Watchdog::start(limit, seed_of(options.order), options.report == Report::Json),
            None => Watchdog::off(),
        },
    };
    let targets: Vec<Target> = files
        .into_iter()
        .map(|path| Target {
            path,
            compile: options.doc_tests != DocTests::Only,
            docs: options.doc_tests != DocTests::Off,
        })
        .collect();
    let mut result = run_targets(&run, &targets, jobs);
    // Then the contract tests and doc examples of the project sources the
    // tests compiled.
    let contracts = std::mem::take(&mut *lock(&run.sources.contracts));
    let docs = std::mem::take(&mut *lock(&run.sources.docs));
    let stopped = run.options.fail_fast && result.failed != 0;
    if !(contracts.is_empty() && docs.is_empty()) && !stopped {
        if let Some(progress) = &run.progress {
            for (what, set) in [("contracts", &contracts), ("doc-tests", &docs)] {
                if !set.is_empty() && run.options.verbose {
                    progress.println(&format!(
                        "{what} of {} project file{}",
                        set.len(),
                        if set.len() == 1 { "" } else { "s" }
                    ));
                }
            }
            progress.add_total(contracts.union(&docs).count());
        }
        let mut sources: Vec<Target> = contracts
            .union(&docs)
            .map(|path| Target {
                path: path.clone(),
                compile: contracts.contains(path),
                docs: docs.contains(path),
            })
            .collect();
        options.order.order_files(&mut sources);
        let more = run_targets(&run, &sources, jobs);
        result.passed += more.passed;
        result.failed += more.failed;
        result.files_run.extend(more.files_run);
        result.cases.extend(more.cases);
        result.failures.extend(more.failures);
        result.quiet_diagnostics += more.quiet_diagnostics;
        result.known_failures += more.known_failures;
        result.skipped += more.skipped;
    }
    reactor.shutdown();
    result.coverage = coverage.map(|c| c.into_inner().unwrap_or_else(|e| e.into_inner()));
    Ok(result)
}

fn run_targets(run: &Run<'_>, targets: &[Target], jobs: usize) -> SuiteResult {
    if jobs == 1 {
        run_serial(run, targets)
    } else {
        run_parallel(run, targets, jobs)
    }
}

/// `--jobs 1`: compile and run each file on this thread, one at a time. No
/// pool worker starts unless a case spawns threads.
fn run_serial(run: &Run<'_>, files: &[Target]) -> SuiteResult {
    let mut result = SuiteResult::default();
    for target in files {
        result.files_run.push(target.path.clone());
        let file = start_file(run, Dispatch::Inline, target);
        let failed = finish_file(run, file, &mut result);
        if failed != 0 && run.options.fail_fast {
            break;
        }
    }
    result
}

/// Compile threads claim files in order and queue their cases on the pool;
/// this thread reports files in order, helping run cases while it waits.
fn run_parallel(run: &Run<'_>, files: &[Target], jobs: usize) -> SuiteResult {
    use std::sync::Condvar;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    // A compile thread may start file `i` only once `i < reported + window`,
    // so a bounded number of compiled programs is alive at a time.
    let window = jobs * 2;
    let progress = Mutex::new((0usize, 0usize)); // (next to claim, reported)
    let advanced = Condvar::new();
    let stop = AtomicBool::new(false);
    let (tx, rx) = mpsc::channel::<(usize, PendingFile)>();
    let mut result = SuiteResult::default();

    std::thread::scope(|scope| {
        for _ in 0..jobs {
            let tx = tx.clone();
            let (progress, advanced, stop) = (&progress, &advanced, &stop);
            std::thread::Builder::new()
                .name("coil-test-compile".into())
                // Deep ASTs recurse in the front end; match the main thread.
                .stack_size(8 * 1024 * 1024)
                .spawn_scoped(scope, move || {
                    loop {
                        let index = {
                            let mut p = progress.lock().unwrap_or_else(|e| e.into_inner());
                            loop {
                                if stop.load(Ordering::SeqCst) || p.0 >= files.len() {
                                    return;
                                }
                                if p.0 < p.1 + window {
                                    break;
                                }
                                p = advanced.wait(p).unwrap_or_else(|e| e.into_inner());
                            }
                            p.0 += 1;
                            p.0 - 1
                        };
                        let file = start_file(run, Dispatch::Pool, &files[index]);
                        if tx.send((index, file)).is_err() {
                            return;
                        }
                    }
                })
                .expect("spawn coil-test compile thread");
        }
        drop(tx);

        let mut ready: std::collections::BTreeMap<usize, PendingFile> = Default::default();
        let mut next = 0usize;
        for (index, file) in rx.iter() {
            ready.insert(index, file);
            while let Some(file) = ready.remove(&next) {
                result.files_run.push(files[next].path.clone());
                let failed = finish_file(run, file, &mut result);
                next += 1;
                if failed != 0 && run.options.fail_fast {
                    // Files already claimed still finish and are reported.
                    stop.store(true, Ordering::SeqCst);
                }
                progress.lock().unwrap_or_else(|e| e.into_inner()).1 = next;
                advanced.notify_all();
            }
        }
    });
    result
}

/// The `--seed` value that reproduces this order (`None` when sorted).
fn seed_of(order: Order) -> Option<String> {
    match order {
        Order::Shuffled(seed) => Some(format_seed(seed)),
        Order::Sorted => None,
    }
}

pub(crate) fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Write `text` to `path`, creating parent directories.
pub(crate) fn write_file(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text)
}

/// Process status for a finished suite: non-zero iff any case failed.
pub fn suite_exit_code(failed: usize) -> i32 {
    if failed == 0 { 0 } else { 1 }
}

/// `coil test` entry: run the suite, print the summary, return the process status.
///
/// What `--early-free check` and `--nursery` ask of the runs in this process.
pub fn set_memory_options(options: &TestOptions) {
    if options.early_free == Some(compiler::EarlyFree::Check) {
        machine::set_early_free_mode(machine::EarlyFreeMode::Check);
    }
    if let Some(on) = options.nursery {
        machine::set_nursery(on);
    }
}

/// A `test result: FAILED` summary always returns 1. Callers must `exit` that
/// value — falling off `main` after a red summary would green CI.
pub fn cmd_test(config: ReportConfig, options: TestOptions) -> i32 {
    set_memory_options(&options);
    let json = options.report == Report::Json;
    let started = std::time::Instant::now();
    let SuiteResult {
        passed,
        failed,
        coverage,
        failures,
        quiet_diagnostics,
        known_failures,
        skipped,
        ..
    } = match run_test_suite(config.clone(), &options) {
        Ok(result) => result,
        Err(msg) => {
            if json {
                events::emit_error(&msg);
                return 1;
            }
            let format = config.format;
            let mut pipeline = Pipeline::with_reporter(config, writer_for(format));
            pipeline.emit_spanless_error(ErrorCode::IoError, msg);
            let _ = pipeline.finish_reporting();
            return 1;
        }
    };

    if !json {
        print_failures(&failures);
        if quiet_diagnostics != 0 {
            eprintln!(
                "\n{quiet_diagnostics} passing file{} had compiler warnings (`--verbose` shows them)",
                if quiet_diagnostics == 1 { "" } else { "s" }
            );
        }
        eprintln!();
        let mut known = match known_failures {
            0 => String::new(),
            n => format!(" {n} known failure{} (`// @xfail`);", if n == 1 { "" } else { "s" }),
        };
        if skipped != 0 {
            known.push_str(&format!(" {skipped} skipped (`// @skip`);"));
        }
        eprintln!(
            "test result: {}. {passed} passed; {failed} failed; {} total;{known} finished in {:.2}s",
            if failed == 0 { "ok" } else { "FAILED" },
            passed + failed,
            started.elapsed().as_secs_f64()
        );
    }
    let mut coverage_event = "null".to_string();
    if let (Some(cov), Some(out)) = (&coverage, &options.coverage) {
        if !json {
            eprintln!();
            eprint!("{}", cov.summary());
        }
        let lcov = out.lcov_out.display().to_string();
        let mut lcov_written = true;
        if let Err(e) = write_file(&out.lcov_out, &cov.lcov()) {
            lcov_written = false;
            report_problem(json, &format!("coverage: cannot write `{lcov}`: {e}"));
        } else if !json {
            eprintln!("coverage: lcov written to {lcov}");
        }
        if let (Some(path), Some(per_test)) = (&out.per_test_out, cov.per_test_json())
            && let Err(e) = write_file(path, &per_test)
        {
            report_problem(
                json,
                &format!("coverage: cannot write `{}`: {e}", path.display()),
            );
        }
        let (mut hit, mut total) = (0usize, 0usize);
        let files: Vec<String> = cov
            .file_totals()
            .into_iter()
            .map(|(path, h, t)| {
                hit += h;
                total += t;
                Event::object()
                    .str("file", &path)
                    .num("hit", h)
                    .num("total", t)
                    .finish()
            })
            .collect();
        coverage_event = Event::object()
            .num("hit", hit)
            .num("total", total)
            .opt_str("lcov", lcov_written.then_some(lcov.as_str()))
            .raw("files", &events::array(&files))
            .finish();
    }

    if json {
        Event::new("summary")
            .bool("ok", failed == 0)
            .num("passed", passed)
            .num("failed", failed)
            .num("total", passed + failed)
            .num("known_failures", known_failures)
            .num("skipped", skipped)
            .opt_str("seed", seed_of(options.order).as_deref())
            .raw("coverage", &coverage_event)
            .emit();
    } else if failed != 0
        && let Order::Shuffled(seed) = options.order
    {
        eprintln!("rerun in this order with `--seed {}`", format_seed(seed));
    }
    suite_exit_code(failed)
}

/// The failed cases' details, then their names, before the summary line.
pub(crate) fn print_failures(failures: &[String]) {
    if failures.is_empty() {
        return;
    }
    eprintln!("\nfailures:");
    for block in failures {
        eprintln!("\n{block}");
    }
    eprintln!("\nfailed:");
    for block in failures {
        let title = block.lines().next().unwrap_or_default();
        let title = title.trim_start_matches("---- ").trim_end_matches(" ----");
        eprintln!("    {title}");
    }
}

/// A non-fatal problem: an `error` event under `--json`, else stderr.
fn report_problem(json: bool, message: &str) {
    if json {
        events::emit_error(message);
    } else {
        eprintln!("{message}");
    }
}

#[cfg(test)]
#[path = "runner.tests.rs"]
mod tests;
