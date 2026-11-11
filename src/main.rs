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
use common::{ARCHIVE_VERSION, ProgramDebug, format_archive_version};
use compiler::{CompileStep, Pipeline};
use reporting::progress::{ClearingWriter, Progress};
use reporting::{ErrorCode, ReportConfig, ReportFormat};
use rkyv::rancor::Error;

mod build_cache;
mod cli;
#[cfg(not(target_family = "wasm"))]
mod code_cache;
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
        Err("missing input file (pass a .coil file or `--entry`)".into())
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

/// Print the diagnostics of the compile a cache hit stands for.
fn repeat_report(report: &[u8]) {
    use std::io::Write;
    let _ = std::io::stderr().write_all(report);
}

fn compile_to_archive(pipeline: &mut Pipeline, filename: &str, output: &str, cache: Option<BuildCache>) {
    if let Some(cache) = &cache
        && let Some(hit) = cache.load()
    {
        repeat_report(&hit.report);
        write_archive(pipeline, output, &hit.archive);
        return;
    }
    // Multi-file entry: discovers `use` / `mod` via bound `--root` / default `src`.
    let constants = match pipeline.compile_src_from_file(filename) {
        Ok(ok) => ok,
        Err(_) => {
            let _ = pipeline.finish_reporting();
            exit(1);
        }
    };
    // `coil compile`: keep the MIR the program runs from.
    let bodies = compiler::native_mir::take_native_capture();
    let bytes = archive_bytes(pipeline, constants, &bodies);
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

/// The `.ccoil` bytes of the program `pipeline` just compiled, with a native
/// MIR section when it has `bodies`.
fn archive_bytes(pipeline: &mut Pipeline, constants: Vec<u64>, bodies: &[compiler::native_mir::NativeBody]) -> Vec<u8> {
    let program = pipeline.archive(constants, |full| {
        if bodies.is_empty() {
            Vec::new()
        } else {
            compiler::native_mir::archive::encode(bodies, &body_ids(full), &full.inline_scopes)
        }
    });

    match rkyv::to_bytes::<Error>(&program) {
        Ok(b) => b.to_vec(),
        Err(e) => fail_and_exit(
            pipeline,
            ErrorCode::IoError,
            format!("Unable to serialize archive: {e}"),
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

    fn load(&self) -> Option<build_cache::Hit> {
        build_cache::load(&self.dir, &self.key)
    }

    /// Keep the archive of a compile, with the warnings a hit repeats. A
    /// report in a machine format is not kept, so that compile is not either.
    fn store(&self, pipeline: &Pipeline, filename: &str, archive: &[u8]) {
        let report = build_cache::report();
        if pipeline.diagnostics_emitted() > 0 && report.is_empty() {
            return;
        }
        let roots: Vec<std::path::PathBuf> =
            pipeline.module_roots().iter().map(|r| pipeline.project_root().join(r)).collect();
        let inputs = build_cache::inputs(Path::new(filename), pipeline.discovered_files(), &roots);
        // A cache that cannot be written only costs the next compile.
        let _ = build_cache::store(&self.dir, &self.key, &inputs, &report, archive);
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
            cli.early_free.map(compiler::EarlyFree::compiles),
            cli.auto_par,
            &cli.host_grants,
            &cli.module_roots,
            (cli.log_json, cli.log_lsp),
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

/// Warn when a cached `.ccoil` is older than sources recorded in its debug bundle.
fn maybe_warn_stale_archive(
    pipeline: &mut Pipeline,
    archive: &str,
    debug: &ProgramDebug,
) {
    if archive_staleness::recorded_sources_newer(archive, debug) {
        pipeline.emit_spanless_warning(
            ErrorCode::IoError,
            format!(
                "Archive `{archive}` may be stale (recorded sources are newer). Recompile with `coil compile … -o {archive}` or run `coil <entry.coil>` directly."
            ),
        );
    }
}

/// Warn when a stale default `out.ccoil` built **from this entry** exists
/// beside an in-memory run. An `out.ccoil` from another program is unrelated.
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
    // The interpreter runs every body, and an archive's section holds only
    // what native code compiles.
    let cache = if interprets() { None } else { cache };
    pipeline.set_runs_mir();
    if RAW_MIR.load(std::sync::atomic::Ordering::Relaxed) {
        compiler::native_mir::start_raw_capture();
    } else {
        compiler::native_mir::start_native_capture();
    }
    if let Some(cache) = &cache
        && let Some((report, loaded)) = cache.load().and_then(|hit| Some((hit.report, load_archive_bytes(&hit.archive).ok()?)))
    {
        end_compile_progress();
        repeat_report(&report);
        install_section(filename, &loaded.native_mir);
        run_cached(pipeline, filename, &loaded);
        return;
    }
    let compiled = pipeline.compile_src_from_file(filename);
    end_compile_progress();
    let constants = match compiled {
        Ok(ok) => ok,
        Err(_) => {
            let _ = pipeline.finish_reporting();
            exit(1);
        }
    };
    print_opt_stats(opt_stats, opt_stats_json);
    let bodies = compiler::native_mir::take_native_capture();
    if let Some(cache) = &cache {
        let bytes = archive_bytes(pipeline, constants.clone(), &bodies);
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
    install_native(bodies, &full);
    let entry = ffi_entry_path(Path::new(filename));
    let panicked = execute_pipeline(ExecutePipelineArgs {
        pipeline,
        constants: &constants,
        strings: &strings,
        static_slots,
        debug,
        entry: Some(entry.as_path()),
    });
    if panicked {
        exit(1);
    }
}

/// `coil FILE --mir -O 0`: capture MIR with no MIR pass run on it.
static RAW_MIR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// `--mir` or `--cow-report`: the MIR goes to the interpreter, not to
/// Cranelift.
static RUN_ON_MIR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Off with `--no-build-cache`: see [`code_cache_on`].
#[cfg(not(target_family = "wasm"))]
static CODE_CACHE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Whether the interpreter runs this process's MIR: `--mir`, or a host with
/// no native code.
fn interprets() -> bool {
    cfg!(target_family = "wasm") || RUN_ON_MIR.load(std::sync::atomic::Ordering::Relaxed)
}

/// Hand the captured MIR to the engine that runs it for the coming run.
fn install_native(bodies: Vec<compiler::native_mir::NativeBody>, debug: &common::ProgramDebug) {
    let pcs = body_ids(debug);
    #[cfg(not(target_family = "wasm"))]
    let key = code_cache_on().then(|| code_cache::key(&compiler::native_mir::archive::encode(&bodies, &pcs, &[]))).flatten();
    #[cfg(target_family = "wasm")]
    let key: Option<String> = None;
    if !install_cached_code(key.as_deref()) {
        install_bodies(bodies, &pcs, key.as_deref());
    }
}

/// Whether this run uses the code cache: `coil FILE` and `coil run` on native
/// code, without `--no-build-cache`.
#[cfg(not(target_family = "wasm"))]
fn code_cache_on() -> bool {
    !interprets() && CODE_CACHE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Install the code stored for `key`. `false` when there is none that loads:
/// a damaged entry goes, and the run compiles.
fn install_cached_code(key: Option<&str>) -> bool {
    #[cfg(not(target_family = "wasm"))]
    if let (Some(key), Some(dir)) = (key, code_cache::dir())
        && let Some(image) = code_cache::load(&dir, key)
    {
        let stats = std::env::var_os("COIL_NATIVE_STATS");
        return match coil_native::load_image(&image) {
            Ok(program) => {
                if stats.is_some() {
                    eprintln!("{}", coil_native::describe(&program));
                    eprintln!("native: code from the cache");
                }
                if stats.as_ref().is_some_and(|v| v == "2") {
                    for name in &program.compiled {
                        eprintln!("  native: {name}");
                    }
                }
                program.install();
                true
            }
            Err(e) => {
                eprintln!("warning: cached code for this program does not load ({e}); compiling it again");
                code_cache::remove(&dir, key);
                false
            }
        };
    }
    #[cfg(target_family = "wasm")]
    let _ = key;
    false
}

/// Hand `bodies` to the engine that runs them, keeping the code under `key`.
/// The interpreter runs what native code refuses.
fn install_bodies(bodies: Vec<compiler::native_mir::NativeBody>, pcs: &std::collections::HashMap<String, u32>, key: Option<&str>) {
    #[cfg(not(target_family = "wasm"))]
    if !interprets() && install_native_bodies(&bodies, pcs, key) {
        return;
    }
    #[cfg(target_family = "wasm")]
    let _ = key;
    let program = coil_mir_interp::prepare(bodies, pcs);
    for (name, why) in &program.refused {
        eprintln!("error: `{name}` cannot run: {why}");
    }
    let complete = program.refused.is_empty();
    // Its entries point into it for the rest of the process.
    std::mem::forget(program.keep);
    machine::native::install_for_next_run(program.entries, program.sites, complete);
}

/// Install an archive's MIR for the coming run. `name` is what the error
/// says.
fn install_section(name: &str, section: &[u8]) {
    if section.is_empty() {
        return;
    }
    #[cfg(not(target_family = "wasm"))]
    let key = code_cache_on().then(|| code_cache::key(section)).flatten();
    #[cfg(target_family = "wasm")]
    let key: Option<String> = None;
    if install_cached_code(key.as_deref()) {
        return;
    }
    match compiler::native_mir::archive::decode(section) {
        Ok((bodies, pcs)) => install_bodies(bodies, &pcs, key.as_deref()),
        Err(e) => {
            eprintln!("error: `{name}`: {e}");
            exit(1);
        }
    }
}

/// Function name to body entry.
fn body_ids(debug: &common::ProgramDebug) -> std::collections::HashMap<String, u32> {
    debug.fn_symbols.iter().map(|s| (s.name.clone(), s.body_id)).collect()
}

/// Native code that cannot run the whole program, for each `why`: a
/// release build warns and the run goes to the interpreter; a debug build
/// stops, so a backend gap fails a test instead of passing slowly.
#[cfg(not(target_family = "wasm"))]
fn native_fallback(why: &[String]) {
    if cfg!(debug_assertions) {
        for why in why {
            eprintln!("error: {why}");
        }
        eprintln!("a debug build does not fall back to the interpreter; pass `--mir` to run on it");
        exit(1);
    }
    for why in why {
        eprintln!("warning: {why}; running on the interpreter");
    }
}

/// JIT `bodies` and install them for the coming run: `false`, with nothing
/// installed, when native code cannot run the whole program
/// ([`native_fallback`]).
#[cfg(not(target_family = "wasm"))]
fn install_native_bodies(
    bodies: &[compiler::native_mir::NativeBody],
    pcs: &std::collections::HashMap<String, u32>,
    key: Option<&str>,
) -> bool {
    let compiled = if key.is_some() { coil_native::jit_keeping_image(bodies, pcs) } else { coil_native::jit(bodies, pcs) };
    let mut program = match compiled {
        Ok(program) => program,
        Err(e) => {
            native_fallback(&[format!("native compile failed ({e})")]);
            return false;
        }
    };
    if std::env::var_os("COIL_NATIVE_STATS").is_some() {
        eprintln!("{}", coil_native::describe(&program));
        if std::env::var_os("COIL_NATIVE_STATS").is_some_and(|v| v == "2") {
            for name in &program.compiled {
                eprintln!("  native: {name}");
            }
        }
    }
    if !program.refused.is_empty() {
        native_fallback(&program.refused.iter().map(|(name, why)| format!("`{name}` has no native code: {why}")).collect::<Vec<_>>());
        return false;
    }
    if let (Some(key), Some(dir), Some(image)) = (key, code_cache::dir(), program.image.take()) {
        // A run that cannot keep its code still runs.
        let _ = code_cache::store(&dir, key, &image);
    }
    program.install();
    true
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
            startup: &loaded.startup,
            main: loaded.main,
            constants: &loaded.constants,
            strings: &loaded.strings,
            static_slots: loaded.static_slots,
            debug: loaded.debug.clone(),
            struct_layouts: &loaded.struct_layouts,
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
    pipeline.set_runs_mir();
    compiler::native_mir::start_native_capture();
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
    if common::ext::is_source_path(Path::new(archive)) {
        fail_and_exit(
            pipeline,
            ErrorCode::IoError,
            format!(
                "`{archive}` is a source file, not an archive: run it with `coil {archive}`, or build one with `coil compile {archive}`"
            ),
        );
    }
    let loaded = match try_load_archive(archive) {
        Ok(ok) => ok,
        Err(LoadErr::Missing) => fail_and_exit(
            pipeline,
            ErrorCode::IoError,
            format!("archive `{archive}` not found"),
        ),
        Err(LoadErr::Corrupt) => fail_and_exit(
            pipeline,
            ErrorCode::IoError,
            format!("archive `{archive}` is corrupt or from an older coil: rebuild it"),
        ),
        Err(LoadErr::Version(v)) => fail_and_exit(
            pipeline,
            ErrorCode::ArchiveVersionMismatch,
            format!(
                "archive version {} is not compatible with runtime {}. Please recompile from source.",
                format_archive_version(v),
                format_archive_version(ARCHIVE_VERSION)
            ),
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

    install_section(archive, &loaded.native_mir);

    // Weak base_dir: archive parent, for relative FFI dload paths.
    // Allow flags are compile-time only; the artifact is the grant.
    let entry = Path::new(archive);
    if execute_archived_program(
        &loaded,
        Some(entry),
        pipeline.ffi_search_path_bufs(),
        Some(pipeline_dload_gate(pipeline)),
        machine::standard_host_natives(),
    ) {
        exit(1);
    }
}

fn cmd_natives_dump(pipeline: &mut Pipeline, exe: Option<&str>, natives: &[FfiNative], tsv: bool) {
    use common::{find_linked_package, read_embedded_native_lock, read_package_trailer};

    let lock = if let Some(path) = exe {
        let data = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => fail_and_exit(
                pipeline,
                ErrorCode::IoError,
                format!("cannot read `{path}`: {e}"),
            ),
        };
        // A JIT runner ends with its package; an AOT executable has it inside.
        let package = if read_package_trailer(&data).is_some() { &data[..] } else { find_linked_package(&data).unwrap_or_default() };
        let trailer = match read_package_trailer(package) {
            Some(t) => t,
            None => fail_and_exit(
                pipeline,
                ErrorCode::IoError,
                format!("`{path}` is not a packaged Coil executable"),
            ),
        };
        match read_embedded_native_lock(package, trailer) {
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

/// `coil dissect` on wasm, where there is no `coil-dissect` to re-exec.
#[cfg(target_family = "wasm")]
fn run_dissect_in_process() -> ! {
    use std::io::Write;
    let argv: Vec<String> = std::iter::once("coil-dissect".to_string())
        .chain(std::env::args().skip(2))
        .collect();
    coil_dissect::run(&argv);
    let _ = std::io::stdout().flush();
    exit(0)
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
        #[cfg(not(target_family = "wasm"))]
        Command::Dissect { .. } => dispatch_helper("dissect"),
        #[cfg(target_family = "wasm")]
        Command::Dissect { .. } => run_dissect_in_process(),
        Command::Debug { .. } => dispatch_helper("debug"),
        Command::Fmt => dispatch_helper("fmt"),
        Command::Lsp => dispatch_helper("lsp"),
        command => {
            let format = config.format;
            let compiles = matches!(command, Command::BuildAndRun { .. } | Command::Compile { .. } | Command::Package { .. });
            let writer = if compiles { compile_progress(format) } else { writer_for(format) };
            let writer: Box<dyn std::io::Write + Send> =
                if compiles && format == ReportFormat::Pretty { Box::new(build_cache::Reported(writer)) } else { writer };
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
            if let Some(mode) = cli.early_free {
                pipeline.set_early_free(mode.compiles());
                if mode == compiler::EarlyFree::Check {
                    machine::set_early_free_mode(machine::EarlyFreeMode::Check);
                }
            }
            if let Some(on) = cli.nursery {
                machine::set_nursery(on);
            }
            if cli.cow_report {
                // SAFETY: set before any thread starts.
                unsafe { std::env::set_var("COIL_COW_REPORT", "1") };
            }
            if let Some(on) = cli.auto_par {
                pipeline.set_auto_par(on);
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
                    RUN_ON_MIR.store(cli.mir || cli.cow_report, std::sync::atomic::Ordering::Relaxed);
                    #[cfg(not(target_family = "wasm"))]
                    CODE_CACHE.store(cli.build_cache, std::sync::atomic::Ordering::Relaxed);
                    // Only `-O 0` is raw: above it the interpreter runs what
                    // native code compiles.
                    RAW_MIR.store(cli.mir && cli.opt_level == compiler::OptLevel::None, std::sync::atomic::Ordering::Relaxed);
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
                    RUN_ON_MIR.store(cli.mir || cli.cow_report, std::sync::atomic::Ordering::Relaxed);
                    #[cfg(not(target_family = "wasm"))]
                    CODE_CACHE.store(cli.build_cache, std::sync::atomic::Ordering::Relaxed);
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
                    link,
                } => {
                    cmd_package(
                        &mut pipeline,
                        &filename,
                        &output,
                        runner.as_deref(),
                        &natives,
                        check_native,
                        &link,
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
    fn archive_mtime_returns_none_for_missing() {
        assert!(archive_mtime(unique_tmp("no_mtime").to_str().unwrap()).is_none());
    }

    #[test]
    fn same_source_path_rejects_unrelated_same_basename() {
        assert!(!same_source_path("examples/foo.coil", "vendor/pkg/foo.coil"));
        assert!(!same_source_path("main.coil", "vendor/pkg/main.coil"));
        assert!(same_source_path("examples/foo.coil", "./examples/foo.coil"));
        assert!(same_source_path("src/lib/io.coil", "project/src/lib/io.coil"));
    }

    #[test]
    fn archive_source_mtime_resolves_cwd_relative() {
        let root = unique_tmp("mtime_cwd");
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("worker.coil");
        std::fs::write(&file, b"fn f() {}").unwrap();

        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(&root).unwrap();
        let m = archive_source_mtime("src/worker.coil");
        std::env::set_current_dir(&prev).unwrap();
        assert!(
            m.is_some(),
            "should resolve src/worker.coil relative to cwd"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn recorded_sources_newer_only_flags_edited_sources() {
        use super::archive_staleness::recorded_sources_newer;
        let dir = unique_tmp("run_archive_sources");
        std::fs::create_dir_all(dir.join("stdlib/io")).unwrap();
        let dep = dir.join("stdlib/io/sync.coil");
        let entry = dir.join("main.coil");
        let arch = dir.join("out.ccoil");
        std::fs::write(&dep, b"fn f() {}").unwrap();
        std::fs::write(&entry, b"fn main() {}").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        std::fs::write(&arch, b"x").unwrap();
        let debug = ProgramDebug {
            source_files: vec![
                dep.to_string_lossy().into_owned(),
                entry.to_string_lossy().into_owned(),
                dir.join("gone.coil").to_string_lossy().into_owned(),
            ],
            debug_locs: vec![],
            fn_symbols: Vec::new(),
            debug_lines: Vec::new(),
            inline_scopes: Vec::new(),
        };
        let a = arch.to_str().unwrap();
        assert!(!recorded_sources_newer(a, &debug), "fresh archive");
        std::thread::sleep(std::time::Duration::from_millis(30));
        std::fs::write(&entry, b"fn main() { /* edited */ }").unwrap();
        assert!(recorded_sources_newer(a, &debug), "edited entry");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_is_stale_when_recorded_source_missing() {
        let dir = unique_tmp("stale_missing_dep");
        std::fs::create_dir_all(&dir).unwrap();
        let entry = dir.join("main.coil");
        let arch = dir.join("out.ccoil");
        std::fs::write(&entry, b"fn main() {}").unwrap();
        std::fs::write(&arch, b"x").unwrap();
        let missing = dir.join("gone.coil");
        let debug = ProgramDebug {
            source_files: vec![
                entry.to_string_lossy().into_owned(),
                missing.to_string_lossy().into_owned(),
            ],
            debug_locs: vec![],
            fn_symbols: Vec::new(),
            debug_lines: Vec::new(),
            inline_scopes: Vec::new(),
        };
        assert!(
            archive_is_stale(entry.to_str().unwrap(), arch.to_str().unwrap(), &debug),
            "missing recorded dependency must invalidate the archive"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_is_stale_empty_source_files_uses_entry_mtime() {
        let dir = unique_tmp("stale_empty_sources");
        std::fs::create_dir_all(&dir).unwrap();
        let entry = dir.join("main.coil");
        let arch = dir.join("out.ccoil");
        std::fs::write(&entry, b"fn main() {}").unwrap();
        std::fs::write(&arch, b"x").unwrap();
        let debug = ProgramDebug {
            source_files: vec![],
            debug_locs: vec![],
            fn_symbols: Vec::new(),
            debug_lines: Vec::new(),
            inline_scopes: Vec::new(),
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
    fn archive_is_stale_when_dependency_source_newer() {
        let dir = unique_tmp("stale_dep");
        std::fs::create_dir_all(&dir).unwrap();
        let entry = dir.join("main.coil");
        let dep = dir.join("worker.coil");
        let arch = dir.join("out.ccoil");
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
            inline_scopes: Vec::new(),
        };
        assert!(archive_is_stale(
            entry.to_str().unwrap(),
            arch.to_str().unwrap(),
            &debug
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_is_stale_when_entry_not_in_source_files() {
        let dir = unique_tmp("stale_entry");
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.coil");
        let b = dir.join("b.coil");
        let arch = dir.join("out.ccoil");
        std::fs::write(&a, b"fn main() {}").unwrap();
        std::fs::write(&b, b"fn main() {}").unwrap();
        std::fs::write(&arch, b"x").unwrap();
        let debug = ProgramDebug {
            source_files: vec![a.to_string_lossy().into_owned()],
            debug_locs: vec![],
            fn_symbols: Vec::new(),
            debug_lines: Vec::new(),
            inline_scopes: Vec::new(),
        };
        // Running b.coil against an archive built from a.coil must rebuild.
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
}
