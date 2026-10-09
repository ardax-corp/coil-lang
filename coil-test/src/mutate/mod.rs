//! `coil mutate`: mutation testing on top of the test runner.
//!
//! 1. **Baseline.** Run the suite once with per-case line coverage; any
//!    failure stops here (mutants need a green suite).
//! 2. **Enumerate.** Parse each covered project source outside the test root
//!    and list [`sites`] (operator swaps, negated conditions, literals).
//! 3. **Filter.** A site no case covers is `no coverage`; nothing runs.
//! 4. **Build + run.** Patch the source in memory (`Pipeline::set_file_text`),
//!    recompile only the test files whose cases cover the site, and run only
//!    those cases, each with a step budget of `--timeout-factor` × its
//!    baseline steps. The first failing case kills the mutant; running out of
//!    budget is a timeout (also a kill); a compile error makes it unviable.

pub mod job;
pub mod sites;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};

use machine::reactor::Reactor;
use reporting::{ErrorCode, ReportConfig};

use crate::coverage::CoverageOptions;
use crate::events::{self, Event};
use crate::runner::{
    CaseOutcome, Compiled, Report, SuiteResult, TestOptions, canonical, compile_test_file,
    run_test_suite, writer_for,
};
use job::{Isolation, MutantJob, Verdict, run_in_child, run_job};
use sites::{Operator, Site};

/// Steps every mutant run may take on top of its budget (static init, and
/// cases so short that a multiple of their steps is no margin).
const BUDGET_SLACK: u64 = 10_000;

/// `coil mutate` flags beyond the shared test ones.
#[derive(Debug, Clone)]
pub struct MutateOptions {
    /// Test root, order, jobs, opt level, grants, module roots.
    pub test: TestOptions,
    /// Only mutate sources whose path (relative to the current directory)
    /// matches one of these globs (`*`, `**`, `?`). Empty: every project
    /// source outside the test root.
    pub files: Vec<String>,
    pub operators: Vec<Operator>,
    /// Step budget per case = baseline steps × this, plus slack.
    pub timeout_factor: u64,
    /// Print a JSON report on stdout.
    pub json: bool,
    /// Exit non-zero when the score is below this percentage.
    pub min_score: Option<f64>,
    /// Sources under this directory (outside `.deps/`) are mutation targets;
    /// the current directory.
    pub project_root: PathBuf,
    /// Where mutants run (`coil mutate`: a child process each).
    pub isolation: Isolation,
}

/// What happened to one mutant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Status {
    Killed,
    TimedOut,
    Survived,
    Unviable,
    NoCoverage,
}

impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Status::Killed => "killed",
            Status::TimedOut => "timeout",
            Status::Survived => "survived",
            Status::Unviable => "unviable",
            Status::NoCoverage => "no coverage",
        }
    }
}

/// One mutant and its outcome.
#[derive(Debug, Clone)]
pub struct MutantResult {
    /// Source path relative to the current directory.
    pub file: String,
    pub line: u32,
    pub operator: Operator,
    pub original: String,
    pub replacement: String,
    pub status: Status,
    /// The case that killed it (`file: name`).
    pub killed_by: Option<String>,
}

/// A mutation run's results, in source order.
#[derive(Debug, Default)]
pub struct MutateReport {
    pub mutants: Vec<MutantResult>,
}

impl MutateReport {
    pub fn count(&self, status: Status) -> usize {
        self.mutants.iter().filter(|m| m.status == status).count()
    }

    /// Killed + timed out over every mutant that ran and compiled; `None`
    /// when none did.
    pub fn score(&self) -> Option<f64> {
        let caught = self.count(Status::Killed) + self.count(Status::TimedOut);
        let scored = caught + self.count(Status::Survived);
        (scored > 0).then(|| caught as f64 * 100.0 / scored as f64)
    }

    pub fn summary(&self) -> String {
        format!(
            "mutation score: {} ({} killed, {} timed out, {} survived; {} unviable, {} without coverage)",
            self.score().map_or("-".to_string(), |s| format!("{s:.1}%")),
            self.count(Status::Killed),
            self.count(Status::TimedOut),
            self.count(Status::Survived),
            self.count(Status::Unviable),
            self.count(Status::NoCoverage),
        )
    }

    /// The `--json` summary event.
    pub fn summary_event(&self, min_score: Option<f64>) -> Event {
        let score = self.score();
        Event::new("summary")
            .bool("ok", !below(score, min_score))
            .raw(
                "score",
                &score.map_or("null".to_string(), |s| format!("{s:.2}")),
            )
            .raw(
                "min_score",
                &min_score.map_or("null".to_string(), |s| s.to_string()),
            )
            .num("killed", self.count(Status::Killed))
            .num("timed_out", self.count(Status::TimedOut))
            .num("survived", self.count(Status::Survived))
            .num("unviable", self.count(Status::Unviable))
            .num("no_coverage", self.count(Status::NoCoverage))
    }
}

/// The `--json` event for one mutant.
pub fn mutant_event(m: &MutantResult) -> Event {
    Event::new("mutant")
        .str("file", &m.file)
        .num("line", m.line)
        .str("operator", m.operator.name())
        .str("from", &m.original)
        .str("to", &m.replacement)
        .str("status", &m.status.name().replace(' ', "_"))
        .opt_str("killed_by", m.killed_by.as_deref())
}

/// True when a score exists and is under `--min-score`.
fn below(score: Option<f64>, min: Option<f64>) -> bool {
    matches!((score, min), (Some(s), Some(m)) if s < m)
}

/// A source file to mutate.
struct Target {
    display: String,
    /// The compiler's spellings of this path (overlay keys).
    spellings: Vec<PathBuf>,
    source: String,
}

/// One mutant to run: its site and the baseline cases covering it.
struct Planned {
    target: usize,
    site: Site,
    /// Indices into the baseline's cases; empty = no coverage.
    covering: Vec<usize>,
}

/// Run the baseline, then every mutant. `Err` for a harness error or a red
/// baseline.
pub fn run_mutate(config: ReportConfig, options: &MutateOptions) -> Result<MutateReport, String> {
    let cwd = canonical(&options.project_root);
    let mut baseline_options = options.test.clone();
    baseline_options.fail_fast = false;
    baseline_options.show_output = false;
    if options.json {
        baseline_options.report = Report::Silent;
    }
    baseline_options.coverage = Some(CoverageOptions {
        lcov_out: PathBuf::new(),
        per_test_out: None,
        project_root: cwd.clone(),
    });
    if options.json {
        Event::new("baseline").emit();
    } else {
        eprintln!("mutate: baseline run");
    }
    let SuiteResult {
        failed,
        coverage,
        cases,
        ..
    } = run_test_suite(config.clone(), &baseline_options)?;
    if failed != 0 {
        return Err(format!(
            "baseline has {failed} failing test{}; mutants need a green suite",
            if failed == 1 { "" } else { "s" }
        ));
    }
    let coverage = coverage.ok_or("baseline produced no coverage")?;

    // (canonical file, line) → covering baseline cases.
    let mut by_line: HashMap<(&Path, u32), Vec<usize>> = HashMap::new();
    for (i, case) in cases.iter().enumerate() {
        for (file, lines) in &case.covered {
            for &line in lines {
                by_line.entry((file.as_path(), line)).or_default().push(i);
            }
        }
    }

    let test_root = canonical(&options.test.root);
    let mut targets = Vec::new();
    let mut planned = Vec::new();
    for (abs, display, lines) in coverage.files() {
        let selected = if options.files.is_empty() {
            !abs.starts_with(&test_root)
        } else {
            options.files.iter().any(|g| glob_match(g, display))
        };
        if !selected {
            continue;
        }
        let source =
            std::fs::read_to_string(abs).map_err(|e| format!("cannot read `{display}`: {e}"))?;
        let found = match sites::enumerate(&source, &options.operators) {
            Ok(found) => found,
            Err(e) => {
                eprintln!("mutate: skipping `{display}` (does not parse: {e})");
                continue;
            }
        };
        let target = targets.len();
        for site in found {
            // The statement's first line carries its coverage: the nearest
            // coverable line at or above the site, inside its declaration.
            let covering = lines
                .range(site.scope_line..=site.line)
                .next_back()
                .and_then(|(line, _)| by_line.get(&(abs, *line)))
                .cloned()
                .unwrap_or_default();
            planned.push(Planned {
                target,
                site,
                covering,
            });
        }
        targets.push(Target {
            display: display.to_string(),
            spellings: overlay_keys(abs, &coverage.spellings(abs), &options.project_root),
            source,
        });
    }
    let runnable = planned.iter().filter(|p| !p.covering.is_empty()).count();
    if options.json {
        Event::new("plan")
            .num("mutants", planned.len())
            .num("files", targets.len())
            .num("covered", runnable)
            .num("jobs", options.test.jobs)
            .emit();
    } else {
        eprintln!(
            "\nmutate: {} mutant{} in {} file{} ({} covered, {} job{})",
            planned.len(),
            if planned.len() == 1 { "" } else { "s" },
            targets.len(),
            if targets.len() == 1 { "" } else { "s" },
            runnable,
            options.test.jobs,
            if options.test.jobs == 1 { "" } else { "s" },
        );
    }

    let mut run_options = options.test.clone();
    run_options.coverage = None;
    let reactor = Reactor::new(options.test.jobs.max(1));
    // An overlay the compiler never reads would make every mutant survive.
    for (i, target) in targets.iter().enumerate() {
        let Some(file) = planned
            .iter()
            .find(|p| p.target == i && !p.covering.is_empty())
            .map(|p| cases[p.covering[0]].file.as_path())
        else {
            continue;
        };
        let broken = format!("{}\n)(\n", target.source);
        let overlays: Vec<(PathBuf, String)> = target
            .spellings
            .iter()
            .map(|s| (s.clone(), broken.clone()))
            .collect();
        if let Compiled::Ready(_) =
            compile_test_file(&config, &run_options, &reactor, file, None, &overlays, None)
        {
            reactor.shutdown();
            return Err(format!(
                "cannot patch `{}` in memory (the compiler reads it by another path)",
                target.display
            ));
        }
    }
    let ctx = MutantCtx {
        config: &config,
        options: &run_options,
        reactor: &reactor,
        targets: &targets,
        cases: &cases,
        timeout_factor: options.timeout_factor.max(1),
        isolation: &options.isolation,
        json: options.json,
    };
    let mutants = run_all(&ctx, &planned, options.test.jobs.max(1));
    reactor.shutdown();
    Ok(MutateReport { mutants })
}

/// What every mutant run shares.
struct MutantCtx<'a> {
    config: &'a ReportConfig,
    options: &'a TestOptions,
    reactor: &'a Arc<Reactor>,
    targets: &'a [Target],
    cases: &'a [CaseOutcome],
    timeout_factor: u64,
    isolation: &'a Isolation,
    /// Mutant events instead of text lines.
    json: bool,
}

/// Run `planned` on `jobs` threads; print and return results in order.
fn run_all(ctx: &MutantCtx<'_>, planned: &[Planned], jobs: usize) -> Vec<MutantResult> {
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel::<(usize, MutantResult)>();
    let mut results = Vec::with_capacity(planned.len());
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            let tx = tx.clone();
            let next = &next;
            std::thread::Builder::new()
                .name("coil-mutate".into())
                // Deep ASTs recurse in the front end; match the main thread.
                .stack_size(8 * 1024 * 1024)
                .spawn_scoped(scope, move || {
                    loop {
                        let i = next.fetch_add(1, Ordering::SeqCst);
                        let Some(p) = planned.get(i) else { return };
                        if tx.send((i, run_one(ctx, p))).is_err() {
                            return;
                        }
                    }
                })
                .expect("spawn coil-mutate thread");
        }
        drop(tx);
        let mut ready = BTreeMap::new();
        for (i, result) in rx.iter() {
            ready.insert(i, result);
            while let Some(result) = ready.remove(&results.len()) {
                if ctx.json {
                    mutant_event(&result).emit();
                } else {
                    print_result(&result);
                }
                results.push(result);
            }
        }
    });
    results
}

fn print_result(m: &MutantResult) {
    let by = m
        .killed_by
        .as_deref()
        .map_or(String::new(), |t| format!("  ({t})"));
    eprintln!(
        "{:<11} {}:{}  `{}` → `{}`  [{}]{by}",
        m.status.name(),
        m.file,
        m.line,
        m.original,
        m.replacement,
        m.operator.name(),
    );
}

/// Build and run one mutant.
fn run_one(ctx: &MutantCtx<'_>, p: &Planned) -> MutantResult {
    let target = &ctx.targets[p.target];
    let (status, killed_by) = if p.covering.is_empty() {
        (Status::NoCoverage, None)
    } else {
        test_mutant(ctx, target, p)
    };
    MutantResult {
        file: target.display.clone(),
        line: p.site.line,
        operator: p.site.operator,
        original: target.source[p.site.start..p.site.end].to_string(),
        replacement: p.site.replacement.clone(),
        status,
        killed_by,
    }
}

fn test_mutant(ctx: &MutantCtx<'_>, target: &Target, p: &Planned) -> Verdict {
    // Covering cases grouped by test file, in baseline order.
    let mut files: Vec<(PathBuf, Vec<(String, u64)>)> = Vec::new();
    for &i in &p.covering {
        let case = &ctx.cases[i];
        let budget = case
            .steps
            .saturating_mul(ctx.timeout_factor)
            .saturating_add(BUDGET_SLACK);
        let entry = (case.name.clone(), budget);
        match files.iter_mut().find(|(f, _)| *f == case.file) {
            Some((_, list)) => list.push(entry),
            None => files.push((case.file.clone(), vec![entry])),
        }
    }
    let job = MutantJob {
        overlay_keys: target.spellings.clone(),
        patched: p.site.apply(&target.source),
        files,
    };
    match ctx.isolation {
        Isolation::InProcess => run_job(ctx.config, ctx.options, ctx.reactor, &job),
        Isolation::Child { exe, args, wall } => run_in_child(exe, args, *wall, &job),
    }
}

/// Every path the compiler may read `abs` by: debug info keeps a module's
/// path as resolved (often relative), while the read joins it onto the
/// current directory.
fn overlay_keys(abs: &Path, spellings: &[String], project_root: &Path) -> Vec<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut keys = vec![abs.to_path_buf()];
    for s in spellings {
        let p = PathBuf::from(s);
        keys.extend([cwd.join(&p), project_root.join(&p), p]);
    }
    keys.sort();
    keys.dedup();
    keys
}

/// `*` (not `/`), `**` (anything), `?` (one non-`/` char) glob match.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match p {
            [] => s.is_empty(),
            [b'*', b'*', rest @ ..] => {
                let rest = rest.strip_prefix(b"/").unwrap_or(rest);
                (0..=s.len()).any(|i| go(rest, &s[i..]))
            }
            [b'*', rest @ ..] => {
                let max = s.iter().position(|&c| c == b'/').unwrap_or(s.len());
                (0..=max).any(|i| go(rest, &s[i..]))
            }
            [b'?', rest @ ..] => matches!(s, [c, tail @ ..] if *c != b'/' && go(rest, tail)),
            [c, rest @ ..] => matches!(s, [d, tail @ ..] if d == c && go(rest, tail)),
        }
    }
    go(pattern.as_bytes(), path.as_bytes())
}

/// `coil mutate` entry: run, print the summary (and JSON), exit non-zero on a
/// harness error, a red baseline, or a score under `--min-score`.
pub fn cmd_mutate(config: ReportConfig, options: MutateOptions) {
    let report = match run_mutate(config.clone(), &options) {
        Ok(report) => report,
        Err(msg) => {
            let msg = format!("coil mutate: {msg}");
            if options.json {
                events::emit_error(&msg);
                exit(1);
            }
            let format = config.format;
            let mut pipeline = compiler::Pipeline::with_reporter(config, writer_for(format));
            pipeline.emit_spanless_error(ErrorCode::IoError, msg);
            let _ = pipeline.finish_reporting();
            exit(1);
        }
    };
    let score = report.score();
    if options.json {
        report.summary_event(options.min_score).emit();
    } else {
        eprintln!();
        eprintln!("{}", report.summary());
        if let (Some(min), Some(score)) = (options.min_score, score)
            && score < min
        {
            eprintln!("mutation score {score:.1}% is below --min-score {min}");
        }
    }
    if below(score, options.min_score) {
        exit(1);
    }
}

#[cfg(test)]
#[path = "mod.tests.rs"]
mod tests;
