use std::io::Write;
use std::path::Path;
use std::process::exit;

use coil_cli::{
    LoadErr, LoadedArchive, dispatch_helper, dispatch_helper_as, execute_archived_program,
    load_archive_bytes, try_load_archive,
};
use coil_host::{
    CachedProgram, ExecutePipelineArgs, bind_cli_roots, execute_cached, execute_pipeline,
    ffi_entry_path, pipeline_dload_gate,
};
use common::{ARCHIVE_VERSION, ArchivedProgram, ProgramDebug, format_archive_version};
use compiler::{CompileStep, Pipeline};
use reporting::progress::{ClearingWriter, Progress};
use reporting::{ErrorCode, ReportConfig, ReportFormat};
use rkyv::rancor::Error;

mod build_cache;
mod cli;
mod package_app;

use cli::{CliArgs, Command, DEFAULT_OUT, parse_args, print_version};
use package_app::{FfiNative, cmd_package, native_lock_from_ffi_natives};


/// The bar `coil FILE`, `compile` and `package` show while compiling (on a
/// terminal, once compiling takes long enough to notice).
static COMPILE_PROGRESS: std::sync::OnceLock<std::sync::Arc<Progress>> = std::sync::OnceLock::new();

/// Show compile progress for `pipeline`: one step per module, then linking.
/// Returns the diagnostics writer, which takes the bar off the screen first.
fn compile_progress(pipeline_format: ReportFormat) -> Box<dyn Write + Send> {
    let progress = std::sync::Arc::new(
        Progress::new("compile", "modules", 0).with_delay(std::time::Duration::from_millis(300)),
    );
    if pipeline_format != ReportFormat::Pretty || !progress.is_drawing() {
        return writer_for(pipeline_format);
    }
    let _ = COMPILE_PROGRESS.set(std::sync::Arc::clone(&progress));
    Box::new(ClearingWriter::new(progress, std::io::stderr()))
}

fn wire_compile_progress(pipeline: &mut Pipeline) {
    let Some(progress) = COMPILE_PROGRESS.get().cloned() else { return };
    let started = std::sync::atomic::AtomicBool::new(false);
    pipeline.set_progress(Some(std::sync::Arc::new(move |step: CompileStep<'_>| match step {
        // The modules, then linking.
        CompileStep::Modules(n) => progress.add_total(n + 1),
        CompileStep::Module(path) => {
            if started.swap(true, std::sync::atomic::Ordering::Relaxed) {
                progress.finish_one(true);
            }
            progress.start(&path.display().to_string());
        }
        CompileStep::Link => {
            progress.finish_one(true);
            progress.start("linking");
        }
    })));
}

/// Take the compile bar off the screen for good (before the program runs or
/// the command reports).
fn end_compile_progress() {
    if let Some(progress) = COMPILE_PROGRESS.get() {
        progress.finish();
    }
}

fn writer_for(format: ReportFormat) -> Box<dyn Write + Send> {
    match format {
        ReportFormat::Pretty => Box::new(std::io::stderr()),
        ReportFormat::Sarif | ReportFormat::Lsp => Box::new(std::io::stdout()),
    }
}

pub(crate) fn fail_and_exit(
    pipeline: &mut Pipeline,
    code: ErrorCode,
    message: impl Into<String>,
) -> ! {
    pipeline.emit_spanless_error(code, message);
    let _ = pipeline.finish_reporting();
    exit(1);
}

fn resolve_entry_filename(filename: &str) -> Result<String, String> {
    if filename.is_empty() {
        Err("missing input file (pass a .hy file or `--entry`)".into())
    } else {
        Ok(filename.to_string())
    }
}

/// `env::args()` for the program `coil` runs: its path, then the arguments
/// after `--`. coil's own argv (flags, `--root`, …) stays invisible to it.
fn set_program_args(program: &str, args: &[String]) {
    let argv = std::iter::once(program.to_string())
        .chain(args.iter().cloned())
        .collect();
    machine::env::set_program_args(argv);
}

fn print_opt_stats(text: bool, json: bool) {
    if !text && !json {
        return;
    }
    let stats = compiler::last_opt_stats();
    if text {
        eprint!("{}", stats.format_text());
    }
    if json {
        eprintln!("{}", stats.format_json());
    }
}

fn compile_to_archive(pipeline: &mut Pipeline, filename: &str, output: &str, cache: Option<BuildCache>) {
    if let Some(cache) = &cache
        && let Some(bytes) = cache.load()
    {
        write_archive(pipeline, output, &bytes);
        return;
    }
    // Multi-file entry: discovers `use` / `mod` via bound `--root` / default `src`.
    let (bytecode, constants) = match pipeline.compile_src_from_file(filename) {
        Ok(ok) => ok,
        Err(_) => {
            let _ = pipeline.finish_reporting();
            exit(1);
        }
    };
    let bytes = archive_bytes(pipeline, bytecode, constants);
    write_archive(pipeline, output, &bytes);
    if let Some(cache) = &cache {
        cache.store(pipeline, filename, &bytes);
    }
}

fn write_archive(pipeline: &mut Pipeline, output: &str, bytes: &[u8]) {
    if let Err(e) = std::fs::write(output, bytes) {
        fail_and_exit(
            pipeline,
            ErrorCode::IoError,
            format!("Unable to write compiled output to `{output}`: {e}"),
        );
    }
}

/// The `.hyc` bytes of the program `pipeline` just compiled.
fn archive_bytes(pipeline: &mut Pipeline, bytecode: Vec<common::Byte>, constants: Vec<u64>) -> Vec<u8> {
    let mut program = ArchivedProgram {
        version: ARCHIVE_VERSION,
        static_slot_count: pipeline.static_slot_count(),
        constants,
        strings: pipeline.strings().to_vec(),
        bytecode,
        source_files: Vec::new(),
        debug_locs: Vec::new(),
        fn_symbols: Vec::new(),
        struct_layouts: pipeline.archived_struct_layouts(),
        operand_stack_slots: pipeline.operand_stack_slots(),
        stack_maps: Vec::new(),
        precise_frames: pipeline.precise_frames().to_vec(),
        class_word_kinds: pipeline.class_word_kinds(),
        static_word_kinds: pipeline.static_word_kinds(),
        debug_lines: Vec::new(),
        cleanup_ranges: Vec::new(),
        line_table: Vec::new(),
        span_table: Vec::new(),
    };
    program.set_debug(pipeline.program_debug_full(), pipeline.debug_level());

    match rkyv::to_bytes::<Error>(&program) {
        Ok(b) => b.to_vec(),
        Err(e) => fail_and_exit(
            pipeline,
            ErrorCode::IoError,
            format!("Unable to serialize bytecode archive: {e}"),
        ),
    }
}

/// One compile's entry in the build cache ([`build_cache`], #1881).
struct BuildCache {
    dir: std::path::PathBuf,
    key: String,
}

impl BuildCache {
    /// The entry for compiling `filename` with `options` (see
    /// [`cache_options`]), or `None` when the cache is off.
    fn new(options: Option<&str>, filename: &str) -> Option<Self> {
        let options = options?;
        Some(Self { dir: build_cache::dir()?, key: build_cache::key(Path::new(filename), options)? })
    }

    fn load(&self) -> Option<Vec<u8>> {
        build_cache::load(&self.dir, &self.key)
    }

    /// Keep the archive of a compile that reported nothing: a cached run
    /// would not repeat its warnings.
    fn store(&self, pipeline: &Pipeline, filename: &str, archive: &[u8]) {
        if pipeline.diagnostics_emitted() > 0 {
            return;
        }
        let roots: Vec<std::path::PathBuf> =
            pipeline.module_roots().iter().map(|r| pipeline.project_root().join(r)).collect();
        let inputs = build_cache::inputs(Path::new(filename), pipeline.discovered_files(), &roots);
        // A cache that cannot be written only costs the next compile.
        let _ = build_cache::store(&self.dir, &self.key, &inputs, archive);
    }
}

/// What on the command line can change a compile's archive, or `None` when
/// the build cache is off for it: `--no-build-cache`, `--opt-stats` (they
/// come from compiling), or compile-time IO grants (a run could read files
/// or the environment the cache cannot see).
fn cache_options(cli: &CliArgs) -> Option<String> {
    if !cli.build_cache || cli.opt_stats || cli.opt_stats_json || !cli.host_grants.comptime_io.is_empty() {
        return None;
    }
    Some(format!(
        "{:?}",
        (
            cli.include_tests,
            cli.opt_level,
            cli.debug_level,
            cli.contracts,
            cli.fold_calls,
            &cli.host_grants,
            &cli.module_roots,
        )
    ))
}

mod archive_staleness {
    use std::path::Path;
    use std::time::SystemTime;

    use common::ProgramDebug;

    pub(super) fn archive_mtime(path: &str) -> Option<SystemTime> {
        std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
    }

    /// Like [`archive_mtime`], but also tries project-root-relative paths.
    pub(super) fn archive_source_mtime(path: &str) -> Option<SystemTime> {
        if let Some(m) = archive_mtime(path) {
            return Some(m);
        }
        let p = Path::new(path);
        if p.is_absolute() {
            return None;
        }
        let Ok(dir) = std::env::current_dir() else {
            return None;
        };
        let candidate = dir.join(p);
        candidate.to_str().and_then(archive_mtime)
    }

    /// True when `path` refers to the same file as `other` (best-effort).
    pub(super) fn same_source_path(path: &str, other: &str) -> bool {
        if path == other {
            return true;
        }
        let a = Path::new(path);
        let b = Path::new(other);
        if let (Ok(ca), Ok(cb)) = (a.canonicalize(), b.canonicalize()) {
            return ca == cb;
        }
        let norm = |s: &str| s.replace('\\', "/").trim_start_matches("./").to_string();
        let a_s = norm(path);
        let b_s = norm(other);
        if a_s == b_s {
            return true;
        }
        fn proper_path_suffix(full: &str, suffix: &str) -> bool {
            if suffix.is_empty() || !suffix.contains('/') {
                return false;
            }
            full.len() > suffix.len()
                && full.ends_with(suffix)
                && full.as_bytes()[full.len() - suffix.len() - 1] == b'/'
        }
        proper_path_suffix(&a_s, &b_s) || proper_path_suffix(&b_s, &a_s)
    }

    /// Whether a cached archive must be rebuilt for `entry`.
    pub(super) fn archive_is_stale(entry: &str, archive: &str, debug: &ProgramDebug) -> bool {
        let Some(arch_mtime) = archive_mtime(archive) else {
            return true;
        };

        if debug.source_files.is_empty() {
            return match archive_source_mtime(entry) {
                Some(src) => src > arch_mtime,
                None => true,
            };
        }

        let entry_known = debug
            .source_files
            .iter()
            .any(|s| same_source_path(s, entry));
        if !entry_known {
            return true;
        }

        for src in &debug.source_files {
            match archive_source_mtime(src) {
                Some(m) if m > arch_mtime => return true,
                None => return true,
                _ => {}
            }
        }

        match archive_source_mtime(entry) {
            Some(src) => src > arch_mtime,
            None => true,
        }
    }

    /// True when a recorded source that still exists is newer than `archive`.
    /// Missing sources are ignored: a shipped archive may outlive its tree.
    pub(super) fn recorded_sources_newer(archive: &str, debug: &ProgramDebug) -> bool {
        let Some(arch_mtime) = archive_mtime(archive) else {
            return false;
        };
        debug
            .source_files
            .iter()
            .filter_map(|src| archive_source_mtime(src))
            .any(|m| m > arch_mtime)
    }
}

/// Warn when a cached `.hyc` is older than sources recorded in its debug bundle.
fn maybe_warn_stale_archive(
    pipeline: &mut Pipeline,
    archive: &str,
    debug: &ProgramDebug,
) {
    if archive_staleness::recorded_sources_newer(archive, debug) {
        pipeline.emit_spanless_warning(
            ErrorCode::IoError,
            format!(
                "Bytecode archive `{archive}` may be stale (recorded sources are newer). Recompile with `coil compile … -o {archive}` or run `coil <entry.hy>` directly."
            ),
        );
    }
}

/// Warn when a stale default `out.hyc` built **from this entry** exists
/// beside an in-memory run. An `out.hyc` from another program is unrelated.
fn maybe_warn_stale_default_out(pipeline: &mut Pipeline, entry: &str, debug: &ProgramDebug) {
    if !Path::new(DEFAULT_OUT).exists() {
        return;
    }
    let from_this_entry = try_load_archive(DEFAULT_OUT).is_ok_and(|archived| {
        archived
            .debug
            .source_files
            .iter()
            .any(|src| archive_staleness::same_source_path(src, entry))
    });
    if from_this_entry && archive_staleness::archive_is_stale(entry, DEFAULT_OUT, debug) {
        pipeline.emit_spanless_warning(
            ErrorCode::IoError,
            format!(
                "`{DEFAULT_OUT}` is older than sources for `{entry}` and is not used by the default run. Refresh with `coil compile {entry} -o {DEFAULT_OUT}`."
            ),
        );
    }
}

fn cmd_build_and_run(
    pipeline: &mut Pipeline,
    filename: &str,
    opt_stats: bool,
    opt_stats_json: bool,
    cache: Option<BuildCache>,
) {
    if let Some(cache) = &cache
        && let Some(loaded) = cache.load().and_then(|b| load_archive_bytes(&b).ok())
    {
        run_cached(pipeline, filename, &loaded);
        return;
    }
    let compiled = pipeline.compile_src_from_file(filename);
    end_compile_progress();
    let (bytecode, constants) = match compiled {
        Ok(ok) => ok,
        Err(_) => {
            let _ = pipeline.finish_reporting();
            exit(1);
        }
    };
    print_opt_stats(opt_stats, opt_stats_json);
    if let Some(cache) = &cache {
        let bytes = archive_bytes(pipeline, bytecode.clone(), constants.clone());
        cache.store(pipeline, filename, &bytes);
    }

    let strings = pipeline.strings().to_vec();
    let static_slots = pipeline.static_slot_count();
    // Staleness reads every source path, whatever `-g` drops from the run.
    let full = pipeline.program_debug_full();
    let debug = full.clone().with_level(pipeline.debug_level());

    if let Err(e) = pipeline.finish_reporting() {
        pipeline.emit_spanless_warning(
            ErrorCode::IoError,
            format!("failed to flush diagnostics: {e}"),
        );
        let _ = pipeline.finish_reporting();
    }

    maybe_warn_stale_default_out(pipeline, filename, &full);
    let entry = ffi_entry_path(Path::new(filename));
    let panicked = execute_pipeline(ExecutePipelineArgs {
        pipeline,
        bytecode: &bytecode,
        constants: &constants,
        strings: &strings,
        static_slots,
        debug,
        entry: Some(entry.as_path()),
        operand_stack_slots: pipeline.operand_stack_slots(),
    });
    if panicked {
        exit(1);
    }
}

/// `coil FILE` from a build-cache hit: what [`cmd_build_and_run`] does after
/// a compile, with the program from the archive.
fn run_cached(pipeline: &mut Pipeline, filename: &str, loaded: &LoadedArchive) {
    end_compile_progress();
    if let Err(e) = pipeline.finish_reporting() {
        pipeline.emit_spanless_warning(ErrorCode::IoError, format!("failed to flush diagnostics: {e}"));
        let _ = pipeline.finish_reporting();
    }
    maybe_warn_stale_default_out(pipeline, filename, &loaded.debug);
    let entry = ffi_entry_path(Path::new(filename));
    let panicked = execute_cached(
        pipeline,
        CachedProgram {
            bytecode: &loaded.bytecode,
            constants: &loaded.constants,
            strings: &loaded.strings,
            static_slots: loaded.static_slots,
            debug: loaded.debug.clone(),
            operand_stack_slots: loaded.operand_stack_slots.unwrap_or(0),
            struct_layouts: &loaded.struct_layouts,
            precise_frames: loaded.precise_frames.clone(),
            class_word_kinds: loaded.class_word_kinds.clone(),
            static_word_kinds: loaded.static_word_kinds.clone(),
        },
        Some(entry.as_path()),
    );
    if panicked {
        exit(1);
    }
}

fn cmd_compile(
    pipeline: &mut Pipeline,
    filename: &str,
    output: &str,
    opt_stats: bool,
    opt_stats_json: bool,
    cache: Option<BuildCache>,
) {
    compile_to_archive(pipeline, filename, output, cache);
    end_compile_progress();
    print_opt_stats(opt_stats, opt_stats_json);
    if let Err(e) = pipeline.finish_reporting() {
        pipeline.emit_spanless_warning(
            ErrorCode::IoError,
            format!("failed to flush diagnostics: {e}"),
        );
        let _ = pipeline.finish_reporting();
    }
}

fn cmd_run(pipeline: &mut Pipeline, archive: &str) {
    if archive.ends_with(".hy") {
        fail_and_exit(
            pipeline,
            ErrorCode::IoError,
            format!(
                "`{archive}` is a source file, not a bytecode archive: run it with `coil {archive}`, or build one with `coil compile {archive}`"
            ),
        );
    }
    let loaded = match try_load_archive(archive) {
        Ok(ok) => ok,
        Err(LoadErr::Missing) => fail_and_exit(
            pipeline,
            ErrorCode::IoError,
            format!("Bytecode archive `{archive}` not found"),
        ),
        Err(LoadErr::Corrupt) => fail_and_exit(
            pipeline,
            ErrorCode::IoError,
            format!("Bytecode archive `{archive}` is corrupt"),
        ),
        Err(LoadErr::Version(v)) => fail_and_exit(
            pipeline,
            ErrorCode::ArchiveVersionMismatch,
            format!(
                "Bytecode archive version {} is not compatible with runtime {}. Please recompile from source.",
                format_archive_version(v),
                format_archive_version(ARCHIVE_VERSION)
            ),
        ),
        Err(LoadErr::Invalid(e)) => fail_and_exit(
            pipeline,
            ErrorCode::IoError,
            format!("Bytecode archive `{archive}` is invalid ({e}). Please recompile from source."),
        ),
    };

    maybe_warn_stale_archive(pipeline, archive, &loaded.debug);

    if let Err(e) = pipeline.finish_reporting() {
        pipeline.emit_spanless_warning(
            ErrorCode::IoError,
            format!("failed to flush diagnostics: {e}"),
        );
        let _ = pipeline.finish_reporting();
    }

    // Weak base_dir: archive parent, for relative FFI dload paths.
    // Allow flags are compile-time only; the artifact is the grant.
    let entry = Path::new(archive);
    if execute_archived_program(
        &loaded,
        Some(entry),
        pipeline.ffi_search_path_bufs(),
        Some(pipeline_dload_gate(pipeline)),
    ) {
        exit(1);
    }
}

fn cmd_natives_dump(pipeline: &mut Pipeline, exe: Option<&str>, natives: &[FfiNative], tsv: bool) {
    use common::{read_embedded_native_lock, read_package_trailer};

    let lock = if let Some(path) = exe {
        let data = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => fail_and_exit(
                pipeline,
                ErrorCode::IoError,
                format!("cannot read `{path}`: {e}"),
            ),
        };
        let trailer = match read_package_trailer(&data) {
            Some(t) => t,
            None => fail_and_exit(
                pipeline,
                ErrorCode::IoError,
                format!("`{path}` is not a packaged Coil executable"),
            ),
        };
        match read_embedded_native_lock(&data, trailer) {
            Ok(Some(lock)) => lock,
            Ok(None) => fail_and_exit(
                pipeline,
                ErrorCode::IoError,
                format!(
                    "`{path}` has no embedded native lock (no `--ffi-native` at package time)"
                ),
            ),
            Err(e) => fail_and_exit(pipeline, ErrorCode::IoError, e),
        }
    } else {
        match native_lock_from_ffi_natives(natives) {
            Ok(lock) => {
                for e in lock.entries.iter().filter(|e| e.missing) {
                    eprintln!(
                        "note: --ffi-native `{}`: `{}` is not present locally; listed as missing (no sha256)",
                        e.stem, e.filename
                    );
                }
                lock
            }
            Err(e) => fail_and_exit(pipeline, ErrorCode::IoError, e),
        }
    };

    let out = if tsv {
        lock.to_tsv()
    } else {
        lock.to_json()
    };
    print!("{out}");
    let _ = pipeline.finish_reporting();
}

/// `coil test` on wasm, where there is no `coil-test` to re-exec: run the
/// test runner here with the same arguments.
#[cfg(target_family = "wasm")]
fn run_test_in_process() -> ! {
    use coil_test::args::{Parsed, parse_args, print_help};
    let argv: Vec<String> = std::iter::once("coil-test".to_string())
        .chain(std::env::args().skip(2))
        .collect();
    match parse_args(&argv) {
        Ok(Parsed::Help) => {
            print_help();
            exit(0)
        }
        Ok(Parsed::Run(config, options)) => exit(coil_test::runner::cmd_test(config, *options)),
        Ok(Parsed::MutateHelp | Parsed::Mutate(..)) => {
            eprintln!("coil: `coil mutate` is not available on this platform");
            exit(1)
        }
        Err(msg) => {
            coil_args::print_cli_error(&msg);
            exit(1)
        }
    }
}

fn main() {
    comptime::install();
    let raw_args: Vec<String> = std::env::args().collect();
    let cli = match parse_args(&raw_args) {
        Ok(c) => c,
        Err(msg) => {
            let config = ReportConfig::default();
            let mut pipeline = Pipeline::with_reporter(config, Box::new(std::io::stderr()));
            let code = if msg.contains("mutually")
                || msg.contains("unrecognized")
                || msg.contains("only valid")
                || msg.contains("duplicate")
                || msg.contains("missing path")
            {
                ErrorCode::InvalidCliFlags
            } else {
                ErrorCode::MissingInputFile
            };
            fail_and_exit(&mut pipeline, code, msg);
        }
    };

    if let Command::Version = cli.command {
        print_version();
        exit(0);
    }

    let config = match ReportConfig::from_cli_flags(cli.log_json, cli.log_lsp) {
        Ok(c) => c,
        Err(msg) => {
            let mut pipeline =
                Pipeline::with_reporter(ReportConfig::default(), Box::new(std::io::stderr()));
            fail_and_exit(&mut pipeline, ErrorCode::InvalidCliFlags, msg);
        }
    };

    let cache_options = cache_options(&cli);
    match cli.command {
        #[cfg(not(target_family = "wasm"))]
        Command::Test => dispatch_helper("test"),
        #[cfg(target_family = "wasm")]
        Command::Test => run_test_in_process(),
        Command::Mutate => dispatch_helper_as("mutate", "test", &["mutate"]),
        Command::Verify => dispatch_helper("verify"),
        Command::Dissect { .. } => dispatch_helper("dissect"),
        Command::Debug { .. } => dispatch_helper("debug"),
        Command::Fmt => dispatch_helper("fmt"),
        Command::Lsp => dispatch_helper("lsp"),
        command => {
            let format = config.format;
            let compiles = matches!(command, Command::BuildAndRun { .. } | Command::Compile { .. } | Command::Package { .. });
            let writer = if compiles { compile_progress(format) } else { writer_for(format) };
            let mut pipeline = Pipeline::with_reporter(config, writer);
            wire_compile_progress(&mut pipeline);
            pipeline.set_host_grants(cli.host_grants.clone());
            bind_cli_roots(&mut pipeline, cli.module_roots.clone());
            if cli.include_tests {
                pipeline.set_include_tests(true);
            }
            pipeline.set_opt_level(cli.opt_level);
            if let Some(level) = cli.debug_level {
                pipeline.set_debug_level(level);
            } else if matches!(command, Command::Package { .. }) {
                pipeline.set_debug_level(common::DebugLevel::Lines);
            }
            if let Some(level) = cli.contracts {
                pipeline.set_contracts(level);
            }
            if let Some(on) = cli.fold_calls {
                pipeline.set_call_fold(on);
            }
            if cli.opt_stats || cli.opt_stats_json {
                pipeline.set_collect_opt_stats(true);
            }
            match command {
                Command::BuildAndRun { filename } => {
                    let filename = match resolve_entry_filename(&filename) {
                        Ok(f) => f,
                        Err(msg) => fail_and_exit(&mut pipeline, ErrorCode::MissingInputFile, msg),
                    };
                    set_program_args(&filename, &cli.program_args);
                    let cache = BuildCache::new(cache_options.as_deref(), &filename);
                    cmd_build_and_run(
                        &mut pipeline,
                        &filename,
                        cli.opt_stats,
                        cli.opt_stats_json,
                        cache,
                    );
                }
                Command::Compile { filename, output } => {
                    let filename = match resolve_entry_filename(&filename) {
                        Ok(f) => f,
                        Err(msg) => fail_and_exit(&mut pipeline, ErrorCode::MissingInputFile, msg),
                    };
                    let cache = BuildCache::new(cache_options.as_deref(), &filename);
                    cmd_compile(
                        &mut pipeline,
                        &filename,
                        &output,
                        cli.opt_stats,
                        cli.opt_stats_json,
                        cache,
                    );
                }
                Command::Run { archive } => {
                    set_program_args(&archive, &cli.program_args);
                    cmd_run(&mut pipeline, &archive);
                }
                Command::Package {
                    filename,
                    output,
                    runner,
                    natives,
                    check_native,
                    // Folded into `cli.debug_level` (`-g0`).
                    strip_debug: _,
                } => {
                    cmd_package(
                        &mut pipeline,
                        &filename,
                        &output,
                        runner.as_deref(),
                        &natives,
                        check_native,
                    );
                    print_opt_stats(cli.opt_stats, cli.opt_stats_json);
                }
                Command::Natives { exe, natives, tsv } => {
                    cmd_natives_dump(&mut pipeline, exe.as_deref(), &natives, tsv);
                }
                Command::Test
                | Command::Mutate
                | Command::Verify
                | Command::Dissect { .. }
                | Command::Debug { .. }
                | Command::Fmt
                | Command::Lsp
                | Command::Version => {
                    unreachable!()
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::archive_staleness::{
        archive_is_stale, archive_mtime, archive_source_mtime, same_source_path,
    };
    use super::*;
    use common::Byte;
    use std::path::PathBuf;

    fn unique_tmp(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "coil_cli_{label}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn try_load_archive_missing_corrupt_version_and_ok() {
        let missing = unique_tmp("missing");
        assert!(matches!(
            try_load_archive(missing.to_str().unwrap()),
            Err(LoadErr::Missing)
        ));

        let corrupt = unique_tmp("corrupt");
        std::fs::write(&corrupt, b"not-an-archive").unwrap();
        assert!(matches!(
            try_load_archive(corrupt.to_str().unwrap()),
            Err(LoadErr::Corrupt)
        ));
        let _ = std::fs::remove_file(&corrupt);

        let stale = unique_tmp("stale");
        // Newer minor than this runtime must be rejected.
        let stale_version = common::pack_archive_version(0, 1);
        let bytes = rkyv::to_bytes::<Error>(&ArchivedProgram {
            version: stale_version,
            static_slot_count: 0,
            constants: vec![],
            strings: vec![],
            bytecode: vec![Byte::new(common::Instruction::HALT)],
            source_files: vec![],
            debug_locs: vec![common::DebugLoc::unknown()],
            fn_symbols: Vec::new(),
            struct_layouts: Vec::new(),
            operand_stack_slots: 256,
            stack_maps: Vec::new(),
            precise_frames: Vec::new(),
            class_word_kinds: Vec::new(),
            static_word_kinds: Vec::new(),
            debug_lines: Vec::new(),
            cleanup_ranges: Vec::new(),
            line_table: Vec::new(),
            span_table: Vec::new(),
        })
        .unwrap();
        std::fs::write(&stale, bytes.as_slice()).unwrap();
        let loaded = try_load_archive(stale.to_str().unwrap());
        match &loaded {
            Err(LoadErr::Version(v)) if *v == stale_version => {}
            Err(e) => panic!("expected Version({stale_version}), got Err({e:?})"),
            Ok(_) => panic!("expected Version({stale_version}), got Ok(..)"),
        }
        let _ = std::fs::remove_file(&stale);

        let ok_path = unique_tmp("ok");
        let ok_prog = ArchivedProgram {
            version: ARCHIVE_VERSION,
            static_slot_count: 0,
            constants: vec![42],
            strings: vec![],
            bytecode: vec![Byte::new(common::Instruction::HALT)],
            source_files: vec![],
            debug_locs: vec![common::DebugLoc::unknown()],
            fn_symbols: Vec::new(),
            struct_layouts: Vec::new(),
            operand_stack_slots: 256,
            stack_maps: Vec::new(),
            precise_frames: Vec::new(),
            class_word_kinds: Vec::new(),
            static_word_kinds: Vec::new(),
            debug_lines: Vec::new(),
            cleanup_ranges: Vec::new(),
            line_table: Vec::new(),
            span_table: Vec::new(),
        };
        let ok_bytes = rkyv::to_bytes::<Error>(&ok_prog).unwrap();
        std::fs::write(&ok_path, ok_bytes.as_slice()).unwrap();
        let loaded = try_load_archive(ok_path.to_str().unwrap()).expect("ok archive");
        assert_eq!(loaded.constants, vec![42]);
        assert!(loaded.strings.is_empty());
        assert_eq!(loaded.bytecode.len(), 1);
        assert!(loaded.struct_layouts.is_empty());
        assert_eq!(loaded.operand_stack_slots, Some(256));
        let _ = std::fs::remove_file(&ok_path);

        let bad_path = unique_tmp("bad_jump");
        let bad_prog = ArchivedProgram {
            bytecode: vec![Byte::new(common::Instruction::JMP).with_operand_u32(99)],
            ..ok_prog
        };
        let bad_bytes = rkyv::to_bytes::<Error>(&bad_prog).unwrap();
        std::fs::write(&bad_path, bad_bytes.as_slice()).unwrap();
        assert!(matches!(
            try_load_archive(bad_path.to_str().unwrap()),
            Err(LoadErr::Invalid(e)) if e.pc == 0
        ));
        let _ = std::fs::remove_file(&bad_path);
    }

    #[test]
    fn archive_is_stale_when_entry_not_in_source_files() {
        let dir = unique_tmp("stale_entry");
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.hy");
        let b = dir.join("b.hy");
        let arch = dir.join("out.hyc");
        std::fs::write(&a, b"fn main() {}").unwrap();
        std::fs::write(&b, b"fn main() {}").unwrap();
        std::fs::write(&arch, b"x").unwrap();
        let debug = ProgramDebug {
            source_files: vec![a.to_string_lossy().into_owned()],
            debug_locs: vec![],
            fn_symbols: Vec::new(),
            debug_lines: Vec::new(),
            cleanup: Vec::new(),
            table: Default::default(),
        };
        // Running b.hy against an archive built from a.hy must rebuild.
        assert!(archive_is_stale(
            b.to_str().unwrap(),
            arch.to_str().unwrap(),
            &debug
        ));
        // Same entry, sources not newer => fresh.
        assert!(!archive_is_stale(
            a.to_str().unwrap(),
            arch.to_str().unwrap(),
            &debug
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_is_stale_when_dependency_source_newer() {
        let dir = unique_tmp("stale_dep");
        std::fs::create_dir_all(&dir).unwrap();
        let entry = dir.join("main.hy");
        let dep = dir.join("worker.hy");
        let arch = dir.join("out.hyc");
        std::fs::write(&entry, b"fn main() {}").unwrap();
        std::fs::write(&dep, b"fn w() {}").unwrap();
        std::fs::write(&arch, b"x").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        // Touch only the dependency — entry mtime stays older than the archive.
        std::fs::write(&dep, b"fn w() { /* edited */ }").unwrap();
        let debug = ProgramDebug {
            source_files: vec![
                entry.to_string_lossy().into_owned(),
                dep.to_string_lossy().into_owned(),
            ],
            debug_locs: vec![],
            fn_symbols: Vec::new(),
            debug_lines: Vec::new(),
            cleanup: Vec::new(),
            table: Default::default(),
        };
        assert!(archive_is_stale(
            entry.to_str().unwrap(),
            arch.to_str().unwrap(),
            &debug
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_is_stale_empty_source_files_uses_entry_mtime() {
        let dir = unique_tmp("stale_empty_sources");
        std::fs::create_dir_all(&dir).unwrap();
        let entry = dir.join("main.hy");
        let arch = dir.join("out.hyc");
        std::fs::write(&entry, b"fn main() {}").unwrap();
        std::fs::write(&arch, b"x").unwrap();
        let debug = ProgramDebug {
            source_files: vec![],
            debug_locs: vec![],
            fn_symbols: Vec::new(),
            debug_lines: Vec::new(),
            cleanup: Vec::new(),
            table: Default::default(),
        };
        // Entry not newer than archive → fresh via the empty-list branch.
        assert!(!archive_is_stale(
            entry.to_str().unwrap(),
            arch.to_str().unwrap(),
            &debug
        ));
        std::thread::sleep(std::time::Duration::from_millis(30));
        std::fs::write(&entry, b"fn main() { /* edited */ }").unwrap();
        assert!(archive_is_stale(
            entry.to_str().unwrap(),
            arch.to_str().unwrap(),
            &debug
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_is_stale_when_recorded_source_missing() {
        let dir = unique_tmp("stale_missing_dep");
        std::fs::create_dir_all(&dir).unwrap();
        let entry = dir.join("main.hy");
        let arch = dir.join("out.hyc");
        std::fs::write(&entry, b"fn main() {}").unwrap();
        std::fs::write(&arch, b"x").unwrap();
        let missing = dir.join("gone.hy");
        let debug = ProgramDebug {
            source_files: vec![
                entry.to_string_lossy().into_owned(),
                missing.to_string_lossy().into_owned(),
            ],
            debug_locs: vec![],
            fn_symbols: Vec::new(),
            debug_lines: Vec::new(),
            cleanup: Vec::new(),
            table: Default::default(),
        };
        assert!(
            archive_is_stale(entry.to_str().unwrap(), arch.to_str().unwrap(), &debug),
            "missing recorded dependency must invalidate the archive"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recorded_sources_newer_only_flags_edited_sources() {
        use super::archive_staleness::recorded_sources_newer;
        let dir = unique_tmp("run_archive_sources");
        std::fs::create_dir_all(dir.join("stdlib/io")).unwrap();
        let dep = dir.join("stdlib/io/sync.hy");
        let entry = dir.join("main.hy");
        let arch = dir.join("out.hyc");
        std::fs::write(&dep, b"fn f() {}").unwrap();
        std::fs::write(&entry, b"fn main() {}").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        std::fs::write(&arch, b"x").unwrap();
        let debug = ProgramDebug {
            source_files: vec![
                dep.to_string_lossy().into_owned(),
                entry.to_string_lossy().into_owned(),
                dir.join("gone.hy").to_string_lossy().into_owned(),
            ],
            debug_locs: vec![],
            fn_symbols: Vec::new(),
            debug_lines: Vec::new(),
            cleanup: Vec::new(),
            table: Default::default(),
        };
        let a = arch.to_str().unwrap();
        assert!(!recorded_sources_newer(a, &debug), "fresh archive");
        std::thread::sleep(std::time::Duration::from_millis(30));
        std::fs::write(&entry, b"fn main() { /* edited */ }").unwrap();
        assert!(recorded_sources_newer(a, &debug), "edited entry");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_mtime_returns_none_for_missing() {
        assert!(archive_mtime(unique_tmp("no_mtime").to_str().unwrap()).is_none());
    }

    #[test]
    fn same_source_path_rejects_unrelated_same_basename() {
        assert!(!same_source_path("examples/foo.hy", "vendor/pkg/foo.hy"));
        assert!(!same_source_path("main.hy", "vendor/pkg/main.hy"));
        assert!(same_source_path("examples/foo.hy", "./examples/foo.hy"));
        assert!(same_source_path("src/lib/io.hy", "project/src/lib/io.hy"));
    }

    #[test]
    fn archive_source_mtime_resolves_cwd_relative() {
        let root = unique_tmp("mtime_cwd");
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("worker.hy");
        std::fs::write(&file, b"fn f() {}").unwrap();

        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(&root).unwrap();
        let m = archive_source_mtime("src/worker.hy");
        std::env::set_current_dir(&prev).unwrap();
        assert!(
            m.is_some(),
            "should resolve src/worker.hy relative to cwd"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
