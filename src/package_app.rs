//! `coil package` — compile entry and append to a runner template.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::exit;

use coil_cli::resolve_default_runner;
use common::{
    append_package_payload_with_natives, dload_request_stem,
    is_packaged_executable, is_system_ffi_stem, linked_package, ArchivedProgram,
    NativeLock, NativeLockEntry, ARCHIVE_VERSION, PACKAGE_FLAG_USES_FFI,
};
use compiler::Pipeline;
use machine::platform_shared_lib_filename;
use reporting::ErrorCode;
use rkyv::rancor::Error;
use sha2::{Digest, Sha256};

use crate::fail_and_exit;

/// Compile `filename` to an archive at the pipeline's `-g` level
/// (`coil package` defaults to `-g1`; `--strip-debug` is `-g0`).
fn compile_program(pipeline: &mut Pipeline, filename: &str) -> Result<ArchivedProgram, ()> {
    let constants = pipeline.compile_src_from_file(filename).map_err(|_| ())?;
    // Cleanup ranges are behaviour (panics run `defer`s), not debug info:
    // `set_debug` keeps them at every level.
    // Native code locates its panics by byte span: an AOT payload keeps the
    // spans with the lines.
    Ok(pipeline.archive(constants, |_| Vec::new()))
}

/// The payload bytes of `program`.
fn payload_bytes(pipeline: &mut Pipeline, program: &ArchivedProgram) -> Vec<u8> {
    match rkyv::to_bytes::<Error>(program) {
        Ok(b) => b.as_slice().to_vec(),
        Err(e) => fail_and_exit(pipeline, ErrorCode::IoError, format!("Unable to serialize archive: {e}")),
    }
}

/// `program` with its native MIR, for the runner to JIT. The runner has no interpreter
/// to run what does not compile, so that shows here and not on the target.
#[cfg(not(target_family = "wasm"))]
fn add_native_mir(pipeline: &mut Pipeline, program: &mut ArchivedProgram, bodies: &[compiler::native_mir::NativeBody]) {
    let pcs = pipeline.program_debug_full().fn_symbols.iter().map(|s| (s.name.clone(), s.body_id)).collect();
    // The inline scopes the `-g` level keeps.
    let scopes = pipeline.program_debug().inline_scopes;
    match coil_native::jit(bodies, &pcs) {
        Ok(jitted) if jitted.refused.is_empty() => program.native_mir = coil_native::section::encode(bodies, &pcs, &scopes),
        Ok(jitted) => refused_exit(&jitted.refused),
        Err(e) => fail_and_exit(pipeline, ErrorCode::IoError, format!("native code for the package failed ({e})")),
    }
}

#[cfg(target_family = "wasm")]
fn add_native_mir(_: &mut Pipeline, _: &mut ArchivedProgram, _: &[compiler::native_mir::NativeBody]) {}

/// A package has no interpreter: a body native code refuses is an error.
#[cfg(not(target_family = "wasm"))]
fn refused_exit(refused: &[(String, String)]) -> ! {
    for (name, why) in refused {
        eprintln!("error: `{name}` cannot compile to native code: {why}");
    }
    eprintln!("  help: a package has no interpreter; `coil FILE` still runs the program");
    exit(1);
}

/// Link an AOT executable for the program just compiled: its MIR through
/// The object for the program: native code, its tables and `package`.
/// `Err` when the backend fails; a body it refuses ends the run.
#[cfg(not(target_family = "wasm"))]
fn aot_object(pipeline: &Pipeline, bodies: &[compiler::native_mir::NativeBody], link: &LinkInputs, package: &[u8]) -> Result<coil_native::AotObject, String> {
    let debug = pipeline.program_debug_full();
    let mut pcs: std::collections::HashMap<String, u32> = debug.fn_symbols.iter().map(|s| (s.name.clone(), s.body_id)).collect();
    if bodies.iter().any(|b| b.name == coil_native::START_NAME) {
        pcs.insert(coil_native::START_NAME.to_string(), start_id(bodies));
    }
    let linked: Vec<(String, String)> = pipeline
        .extern_symbols()
        .into_iter()
        .filter_map(|(lib, sym)| {
            let stem = machine::dload_request_stem(&lib);
            link.statics.iter().any(|s| s.stem == stem).then_some((stem, sym))
        })
        .collect();
    for s in &link.statics {
        if !linked.iter().any(|(stem, _)| *stem == s.stem) {
            eprintln!("warning: `--link-static {}`: no `extern \"{}\"` block declares a function", s.path.display(), s.stem);
        }
    }
    let object = coil_native::aot(bodies, &pcs, &linked, package).map_err(|e| format!("native code for the package failed ({e})"))?;
    if std::env::var_os("COIL_NATIVE_STATS").is_some_and(|v| v == "2") {
        for (name, why) in &object.refused {
            eprintln!("  refused: {name}: {why}");
        }
    }
    if !object.refused.is_empty() {
        refused_exit(&object.refused);
    }
    Ok(object)
}

/// The body id of an AOT executable's `__start`: past every body's.
#[cfg(not(target_family = "wasm"))]
fn start_id(bodies: &[compiler::native_mir::NativeBody]) -> u32 {
    bodies.iter().filter_map(|b| b.entry).map(|l| l.0 + 1).max().unwrap_or(0) + 1
}

/// What an AOT executable is made from: `bodies` with `__start`
/// (`coil_native::start_body`), and `program` as data only, whose `main`
/// is `__start` and whose startup is empty.
#[cfg(not(target_family = "wasm"))]
fn with_native_start(pipeline: &mut Pipeline, bodies: &[compiler::native_mir::NativeBody], program: &ArchivedProgram) -> (Vec<compiler::native_mir::NativeBody>, ArchivedProgram) {
    let ids = pipeline.program_debug_full().fn_symbols.iter().map(|s| (s.name.clone(), s.body_id)).collect();
    let start = match coil_native::start_body(bodies, &ids, &program.startup, program.main) {
        Ok(b) => b,
        Err(e) => fail_and_exit(pipeline, ErrorCode::IoError, format!("native start for the package failed ({e})")),
    };
    let id = start_id(bodies);
    let mut all = bodies.to_vec();
    all.push(start);
    let mut data = program.clone();
    data.startup = Vec::new();
    data.main = Some(id);
    // Past every other body: the symbols stay sorted by id.
    data.fn_symbols.push(common::FnDebugSym { name: coil_native::START_NAME.to_string(), body_id: id });
    (all, data)
}

#[cfg(target_family = "wasm")]
fn with_native_start(_: &mut Pipeline, bodies: &[compiler::native_mir::NativeBody], program: &ArchivedProgram) -> (Vec<compiler::native_mir::NativeBody>, ArchivedProgram) {
    (bodies.to_vec(), program.clone())
}

/// [`aot_object`], then `cc` with the runtime library (`libcoil_rt.a`
/// beside `coil`, or `COIL_RT_LIB`). Returns the image, how many functions
/// compiled and the runtime used; `None` (with a warning) when this host
/// cannot link one, and the JIT runner is packaged instead.
#[cfg(not(target_family = "wasm"))]
fn link_aot_executable(pipeline: &Pipeline, bodies: &[compiler::native_mir::NativeBody], link: &LinkInputs, package: &[u8]) -> Option<(Vec<u8>, usize, PathBuf)> {
    let fallback = |why: String| {
        // Static libraries link only into an AOT executable.
        if !link.statics.is_empty() {
            eprintln!("error: {why}; `--link-static` needs the AOT executable");
            exit(1);
        }
        eprintln!("warning: {why}; packaging the JIT runner instead (pass `--no-aot` to skip linking)");
        None
    };
    let object = match aot_object(pipeline, bodies, link, package) {
        Ok(o) => o,
        Err(e) => return fallback(e),
    };
    let rt = match runtime_library() {
        Ok(p) => p,
        Err(e) => return fallback(e),
    };
    match link_with_cc(&object.bytes, &rt, link) {
        Ok(exe) => Some((exe, object.compiled.len(), rt)),
        Err(e) => fallback(e),
    }
}

#[cfg(target_family = "wasm")]
fn link_aot_executable(_: &Pipeline, _: &[compiler::native_mir::NativeBody], _: &LinkInputs, _: &[u8]) -> Option<(Vec<u8>, usize, PathBuf)> {
    None
}

/// `--emit obj` and `--emit lib`: write what the caller's own link takes,
/// and say how to link it.
#[cfg(not(target_family = "wasm"))]
fn write_linkable(pipeline: &mut Pipeline, bodies: &[compiler::native_mir::NativeBody], link: &LinkInputs, package: &[u8], output: &str) {
    let object = match aot_object(pipeline, bodies, link, package) {
        Ok(o) => o,
        Err(e) => fail_and_exit(pipeline, ErrorCode::IoError, e),
    };
    let rt = match runtime_library() {
        Ok(p) => p,
        Err(e) => fail_and_exit(pipeline, ErrorCode::IoError, e),
    };
    let bytes = match link.emit {
        Emit::Lib => match static_library(&object.bytes, &rt) {
            Ok(b) => b,
            Err(e) => fail_and_exit(pipeline, ErrorCode::IoError, e),
        },
        _ => object.bytes,
    };
    if let Err(e) = fs::write(output, &bytes) {
        fail_and_exit(pipeline, ErrorCode::IoError, format!("cannot write `{output}`: {e}"));
    }
    if let Err(e) = pipeline.finish_reporting() {
        pipeline.emit_spanless_warning(ErrorCode::IoError, format!("failed to flush diagnostics: {e}"));
        let _ = pipeline.finish_reporting();
    }
    eprintln!(
        "wrote `{output}` for {}-{} ({} bytes; native code for {} functions)",
        std::env::consts::OS,
        std::env::consts::ARCH,
        bytes.len(),
        object.compiled.len()
    );
    let statics: Vec<String> = link.statics.iter().map(|s| s.path.display().to_string()).collect();
    let runtime = if link.emit == Emit::Lib { Vec::new() } else { vec![rt.display().to_string()] };
    let line = [vec!["cc".to_string(), output.to_string()], statics, runtime, vec!["-o".to_string(), "app".to_string()], system_libs().iter().map(|l| (*l).to_string()).collect()].concat();
    eprintln!("link it with: {}", line.join(" "));
}

#[cfg(target_family = "wasm")]
fn write_linkable(pipeline: &mut Pipeline, _: &[compiler::native_mir::NativeBody], _: &LinkInputs, _: &[u8], _: &str) {
    fail_and_exit(pipeline, ErrorCode::IoError, "`--emit` needs a native host".to_string())
}

/// How a package's executable is made.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LinkInputs {
    /// Link native code for the program (an AOT executable); `false` with
    /// `--no-aot` or `--runner`: the JIT runner runs it.
    pub aot: bool,
    /// What to write (`--emit`).
    pub emit: Emit,
    /// Static libraries for `extern` blocks (`--link-static`).
    pub statics: Vec<StaticLink>,
    /// Extra linker arguments, after the libraries (`--link-arg`).
    pub args: Vec<String>,
    /// What the linker strips from the executable (`--strip-symbols`).
    pub strip: StripSymbols,
}

/// What `coil package` writes (`--emit`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Emit {
    /// An executable.
    #[default]
    Exe,
    /// A relocatable object holding the program, to link with the runtime.
    Obj,
    /// A static library holding the program and the runtime.
    Lib,
}

/// What `coil package` strips from an AOT executable (`--strip-symbols`):
/// the runtime's DWARF and symbol table. Coil's own backtraces come from
/// the program's line table (`-g`), not from these.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum StripSymbols {
    /// Keep everything (for a native debugger or profiler).
    None,
    /// Drop debug info, keep the symbol table (named frames in `perf`).
    #[default]
    Debug,
    /// Drop debug info and the symbol table: the smallest executable.
    All,
}

#[cfg(not(target_family = "wasm"))]
impl StripSymbols {
    /// The `cc` arguments that strip this much.
    fn cc_args(self) -> &'static [&'static str] {
        match (self, cfg!(target_os = "macos")) {
            (StripSymbols::None, _) => &[],
            (StripSymbols::Debug, false) => &["-Wl,--strip-debug"],
            (StripSymbols::Debug, true) => &["-Wl,-S"],
            (StripSymbols::All, false) => &["-s"],
            (StripSymbols::All, true) => &["-Wl,-S", "-Wl,-x"],
        }
    }
}

/// What `rustc` names `coil-rt`'s static library for this target: Windows
/// has two, and the GNU one, which releases build, is named as on Unix.
#[cfg(not(target_family = "wasm"))]
const RUNTIME_LIB: &str = if cfg!(target_env = "msvc") { "coil_rt.lib" } else { "libcoil_rt.a" };

/// [`RUNTIME_LIB`]: `COIL_RT_LIB`, else the copy embedded in this binary
/// (written to the cache on first use), else the file beside it.
#[cfg(not(target_family = "wasm"))]
fn runtime_library() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("COIL_RT_LIB") {
        return Ok(PathBuf::from(p));
    }
    let exe = std::env::current_exe().map_err(|e| format!("cannot resolve current executable: {e}"))?;
    if let Some(cache) = compiler::comptime_cache::user_cache_root()
        && let Some(lib) = embedded_runtime(&exe, &cache.join("runtime"))?
    {
        return Ok(lib);
    }
    let lib = exe.with_file_name(RUNTIME_LIB);
    if lib.is_file() {
        Ok(lib)
    } else {
        Err(format!("the native runtime `{}` is not installed", lib.display()))
    }
}

/// The trailer `scripts/embed_runtime.py` appends to `coil` after the
/// runtime library: its length (u64 LE), then this magic.
#[cfg(not(target_family = "wasm"))]
pub const RUNTIME_MAGIC: &[u8; 8] = b"COILRT\x00\x01";

/// The runtime library embedded at the end of `exe` (see
/// [`RUNTIME_MAGIC`]), written once to `cache/<sha256 prefix>/` and reused
/// from there; `None` when `exe` carries none.
#[cfg(not(target_family = "wasm"))]
pub fn embedded_runtime(exe: &Path, cache: &Path) -> Result<Option<PathBuf>, String> {
    use std::io::{Seek, SeekFrom};
    let read_err = |e: std::io::Error| format!("cannot read `{}`: {e}", exe.display());
    let mut f = fs::File::open(exe).map_err(read_err)?;
    let size = f.metadata().map_err(read_err)?.len();
    if size < 16 {
        return Ok(None);
    }
    let mut tail = [0u8; 16];
    f.seek(SeekFrom::End(-16)).map_err(read_err)?;
    f.read_exact(&mut tail).map_err(read_err)?;
    if &tail[8..] != RUNTIME_MAGIC {
        return Ok(None);
    }
    let len = u64::from_le_bytes(tail[..8].try_into().expect("8 bytes"));
    if len > size - 16 {
        return Err(format!("`{}`: the embedded runtime is truncated", exe.display()));
    }
    let mut lib = vec![0u8; len as usize];
    f.seek(SeekFrom::Start(size - 16 - len)).map_err(read_err)?;
    f.read_exact(&mut lib).map_err(read_err)?;
    let digest = Sha256::digest(&lib);
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    let dir = cache.join(hex);
    let path = dir.join(RUNTIME_LIB);
    if fs::metadata(&path).is_ok_and(|m| m.len() == len) {
        return Ok(Some(path));
    }
    let write_err = |e: std::io::Error| format!("cannot write the runtime to `{}`: {e}", dir.display());
    fs::create_dir_all(&dir).map_err(write_err)?;
    // Another `coil package` may be writing the same file: write aside,
    // then rename over.
    let tmp = dir.join(format!(".tmp-{}", std::process::id()));
    fs::write(&tmp, &lib).map_err(write_err)?;
    fs::rename(&tmp, &path).map_err(write_err)?;
    Ok(Some(path))
}

/// The system libraries the runtime needs, and dropping what nothing the
/// program runs references.
#[cfg(not(target_family = "wasm"))]
fn system_libs() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        &["-Wl,-dead_strip", "-liconv", "-lSystem", "-lc", "-lm"]
    } else if cfg!(windows) {
        &["-Wl,--gc-sections", "-lkernel32", "-lntdll", "-luserenv", "-lws2_32", "-ldbghelp"]
    } else {
        &["-Wl,--gc-sections", "-ldl", "-lgcc_s", "-lutil", "-lrt", "-lpthread", "-lm", "-lc"]
    }
}

/// A new directory for one link's files.
#[cfg(not(target_family = "wasm"))]
fn link_dir() -> Result<PathBuf, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let dir = std::env::temp_dir().join(format!("coil-aot-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create `{}`: {e}", dir.display()))?;
    Ok(dir)
}

/// `cc OBJECT STATIC-LIBS RUNTIME -o EXE LINK-ARGS` with the system
/// libraries the runtime needs.
#[cfg(not(target_family = "wasm"))]
fn link_with_cc(object: &[u8], rt: &Path, link: &LinkInputs) -> Result<Vec<u8>, String> {
    let dir = link_dir()?;
    let obj = dir.join("program.o");
    // Named as the linker names it: gcc on Windows adds `.exe` to any other.
    let exe = dir.join(format!("program{}", std::env::consts::EXE_SUFFIX));
    let out = (|| {
        fs::write(&obj, object).map_err(|e| format!("cannot write `{}`: {e}", obj.display()))?;
        let cc = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
        let run = std::process::Command::new(&cc)
            .arg(&obj)
            .args(link.statics.iter().map(|s| &s.path))
            .arg(rt)
            .arg("-o")
            .arg(&exe)
            .args(&link.args)
            .args(system_libs())
            .args(link.strip.cc_args())
            .output()
            .map_err(|e| format!("cannot run `{}` to link ({e})", cc.to_string_lossy()))?;
        if !run.status.success() {
            let err = String::from_utf8_lossy(&run.stderr);
            let first = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("no output");
            return Err(format!("linking with `{}` failed: {first}", cc.to_string_lossy()));
        }
        fs::read(&exe).map_err(|e| format!("cannot read `{}`: {e}", exe.display()))
    })();
    let _ = fs::remove_dir_all(&dir);
    out
}

/// The runtime library `rt` with `object` added (`ar`, or `AR`).
#[cfg(not(target_family = "wasm"))]
fn static_library(object: &[u8], rt: &Path) -> Result<Vec<u8>, String> {
    let dir = link_dir()?;
    let obj = dir.join("coil_program.o");
    let lib = dir.join("libprogram.a");
    let out = (|| {
        fs::write(&obj, object).map_err(|e| format!("cannot write `{}`: {e}", obj.display()))?;
        fs::copy(rt, &lib).map_err(|e| format!("cannot copy `{}`: {e}", rt.display()))?;
        let ar = std::env::var_os("AR").unwrap_or_else(|| "ar".into());
        let run = std::process::Command::new(&ar)
            .arg("rs")
            .arg(&lib)
            .arg(&obj)
            .output()
            .map_err(|e| format!("cannot run `{}` ({e})", ar.to_string_lossy()))?;
        if !run.status.success() {
            let err = String::from_utf8_lossy(&run.stderr);
            let first = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("no output");
            return Err(format!("`{}` failed: {first}", ar.to_string_lossy()));
        }
        fs::read(&lib).map_err(|e| format!("cannot read `{}`: {e}", lib.display()))
    })();
    let _ = fs::remove_dir_all(&dir);
    out
}

fn resolve_runner_path(runner: Option<&Path>) -> Result<PathBuf, String> {
    match runner {
        Some(p) => Ok(p.to_path_buf()),
        None => resolve_default_runner(),
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(perms.mode() | 0o111);
        let _ = fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

fn sha256_hex_file(path: &Path) -> Result<(String, u64), String> {
    let mut f =
        fs::File::open(path).map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("read `{}`: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok((
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
        size,
    ))
}

/// A static library an AOT package links for an `extern` block (one
/// `--link-static [STEM=]PATH` flag).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticLink {
    /// The library stem `extern "STEM"` names.
    pub stem: String,
    /// The archive (`libSTEM.a`, `STEM.lib`).
    pub path: PathBuf,
}

/// Parse `[STEM=]PATH`; without `STEM=`, the stem is the file name's
/// (`libsum.a` and `sum.lib` are `sum`).
pub fn parse_static_link(spec: &str) -> Result<StaticLink, String> {
    if let Some((stem, path)) = spec.split_once('=')
        && !stem.is_empty()
        && !stem.contains(['/', '\\'])
    {
        return Ok(StaticLink { stem: stem.to_string(), path: PathBuf::from(path) });
    }
    let path = PathBuf::from(spec);
    let name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| format!("`{spec}` names no file"))?;
    let base = name.strip_suffix(".a").or_else(|| name.strip_suffix(".lib")).ok_or_else(|| format!("`{spec}` is not a static library (`libNAME.a` or `NAME.lib`); give its stem as `STEM={spec}`"))?;
    let stem = base.strip_prefix("lib").filter(|s| !s.is_empty()).unwrap_or(base);
    Ok(StaticLink { stem: stem.to_string(), path })
}

/// A native library the packaged app loads (one `--ffi-native` flag).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfiNative {
    /// `dload` stem (e.g. `regex`).
    pub name: String,
    /// Natives cache package name (defaults to `name`).
    pub package: String,
    pub version: String,
    /// Directory holding the platform library file (relative to the cwd).
    pub path: PathBuf,
    /// Transitive sonames expected from the OS (diagnostics only).
    pub requires: Vec<String>,
    /// Install hint when a `requires` soname is missing.
    pub requires_hint: String,
}

/// Parse `name=…,version=…,path=…[,package=…][,requires=a;b][,requires-hint=…]`.
/// `\,` is a literal comma inside a value.
pub fn parse_ffi_native(spec: &str) -> Result<FfiNative, String> {
    let mut fields: Vec<String> = vec![String::new()];
    let mut chars = spec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&',') => {
                chars.next();
                fields.last_mut().expect("one field").push(',');
            }
            ',' => fields.push(String::new()),
            c => fields.last_mut().expect("one field").push(c),
        }
    }
    let (mut name, mut package, mut version, mut path) = (None, None, None, None);
    let mut requires = Vec::new();
    let mut requires_hint = String::new();
    for field in &fields {
        let (key, value) = field
            .split_once('=')
            .ok_or_else(|| format!("`{field}` is not key=value"))?;
        let value = value.to_string();
        match key {
            "name" => name = Some(value),
            "package" => package = Some(value),
            "version" => version = Some(value),
            "path" => path = Some(PathBuf::from(value)),
            "requires" => {
                requires = value
                    .split(';')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            }
            "requires-hint" => requires_hint = value,
            _ => return Err(format!("unknown key `{key}`")),
        }
    }
    let missing = |key: &str| format!("missing `{key}` in `{spec}`");
    let name = name.ok_or_else(|| missing("name"))?;
    Ok(FfiNative {
        package: package.unwrap_or_else(|| name.clone()),
        version: version.ok_or_else(|| missing("version"))?,
        path: path.ok_or_else(|| missing("path"))?,
        name,
        requires,
        requires_hint,
    })
}

/// Build a [`NativeLock`] from `dload` stems and `--ffi-native` rows.
pub fn build_native_lock(
    natives: &[FfiNative],
    libraries: &[String],
    linked: &[StaticLink],
) -> Result<NativeLock, String> {
    // Reduce each literal to the stem the runtime dload gate uses
    // (`./native/libtls.so` → `tls`), so one `--ffi-native name=tls` row covers
    // every spelling of that library.
    let mut stems: Vec<(String, String)> = Vec::new();
    for literal in libraries {
        let stem = dload_request_stem(literal);
        if !stems.iter().any(|(s, _)| *s == stem) {
            stems.push((stem, literal.clone()));
        }
    }
    let mut entries = Vec::new();

    for (stem, literal) in &stems {
        // System libraries, and ones linked into the executable, ship
        // with no artifact.
        if is_system_ffi_stem(stem) || linked.iter().any(|l| l.stem == *stem) {
            continue;
        }
        let decl = natives.iter().find(|n| n.name == *stem).ok_or_else(|| {
            let loaded = if literal == stem {
                format!("`{stem}`")
            } else {
                format!("`{literal}` (stem `{stem}`)")
            };
            format!(
                "FFI library {loaded} is loaded but has no `--ffi-native name={stem},…`; \
                     pass one with name/version/path (system libs like `c` need no entry)"
            )
        })?;
        let filename = platform_shared_lib_filename(&decl.name);
        let lib_path = decl.path.join(&filename);
        if !lib_path.is_file() {
            return Err(format!(
                "--ffi-native `{stem}`: expected library at `{}`",
                lib_path.display()
            ));
        }
        let (sha256, size) = sha256_hex_file(&lib_path)?;
        entries.push(NativeLockEntry {
            package: decl.package.clone(),
            version: decl.version.clone(),
            stem: decl.name.clone(),
            filename,
            sha256,
            size,
            requires: decl.requires.clone(),
            requires_hint: decl.requires_hint.clone(),
            missing: false,
        });
    }

    // Constant stems only: `--ffi-native` rows the program never loads are ignored.

    Ok(NativeLock {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        entries,
    })
}

/// Build a native lock from every `--ffi-native` row (`coil natives dump`).
pub fn native_lock_from_ffi_natives(natives: &[FfiNative]) -> Result<NativeLock, String> {
    let mut entries = Vec::new();
    for decl in natives {
        if is_system_ffi_stem(&decl.name) {
            continue;
        }
        let filename = platform_shared_lib_filename(&decl.name);
        let lib_path = decl.path.join(&filename);
        // The pin is the local file's hash. A file that is not there yet is
        // listed as missing (no hash) so a downloader can fetch it; anything
        // else at that path that is not a readable file is still an error.
        match fs::metadata(&lib_path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                entries.push(NativeLockEntry {
                    package: decl.package.clone(),
                    version: decl.version.clone(),
                    stem: decl.name.clone(),
                    filename,
                    sha256: String::new(),
                    size: 0,
                    requires: decl.requires.clone(),
                    requires_hint: decl.requires_hint.clone(),
                    missing: true,
                });
                continue;
            }
            Ok(meta) if meta.is_file() => {}
            _ => {
                return Err(format!(
                    "--ffi-native `{}`: expected library at `{}`",
                    decl.name,
                    lib_path.display()
                ));
            }
        }
        let (sha256, size) = sha256_hex_file(&lib_path)?;
        entries.push(NativeLockEntry {
            package: decl.package.clone(),
            version: decl.version.clone(),
            stem: decl.name.clone(),
            filename,
            sha256,
            size,
            requires: decl.requires.clone(),
            requires_hint: decl.requires_hint.clone(),
            missing: false,
        });
    }
    Ok(NativeLock {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        entries,
    })
}

pub fn cmd_package(
    pipeline: &mut Pipeline,
    filename: &str,
    output: &str,
    runner: Option<&Path>,
    natives: &[FfiNative],
    check_native: bool,
    link: &LinkInputs,
) {
    let aot = link.aot;
    if link.emit != Emit::Exe && !aot {
        fail_and_exit(pipeline, ErrorCode::IoError, "`--emit` needs native code; it cannot be used with `--no-aot` or `--runner`".to_string());
    }
    if link.emit != Emit::Exe && !link.args.is_empty() {
        fail_and_exit(pipeline, ErrorCode::IoError, "`--link-arg` goes to the link of an executable; `--emit` leaves that link to you".to_string());
    }
    if !aot && !link.statics.is_empty() {
        fail_and_exit(
            pipeline,
            ErrorCode::IoError,
            "`--link-static` links into the AOT executable; it cannot be used with `--no-aot` or `--runner`".to_string(),
        );
    }
    // Both kinds of executable are made from this compile's MIR.
    #[cfg(not(target_family = "wasm"))]
    {
        pipeline.set_runs_mir();
        compiler::native_mir::start_native_capture();
    }
    let program = compile_program(pipeline, filename);
    // Notes below and errors go to a clean line.
    crate::end_compile_progress();
    let mut program = match program {
        Ok(p) => p,
        Err(()) => {
            let _ = pipeline.finish_reporting();
            exit(1);
        }
    };
    let bodies = compiler::native_mir::take_native_capture();
    let (uses_ffi, libraries) = compiler::native_mir::ffi_use(&bodies, &program.strings);
    let mut flags = 0u32;
    if uses_ffi {
        flags |= PACKAGE_FLAG_USES_FFI;
    }

    let native_lock = match build_native_lock(natives, &libraries, &link.statics) {
        Ok(lock) => lock,
        Err(msg) => fail_and_exit(pipeline, ErrorCode::IoError, msg),
    };

    if !link.statics.is_empty() {
        let stems: Vec<&str> = link.statics.iter().map(|l| l.stem.as_str()).collect();
        eprintln!("note: linking `{}` into the executable statically", stems.join("`, `"));
    } else if uses_ffi && native_lock.entries.is_empty() {
        // FFI opcodes present but only system libs (or dynamic dload) — OK for libc-only.
        eprintln!(
            "note: this program uses FFI; only system libraries were detected. \
             Userland natives need `--ffi-native` and the library on the target."
        );
    } else if !native_lock.entries.is_empty() {
        eprintln!(
            "note: {} native artifact(s) declared; the target needs them in the natives \
             cache, beside {} or in its lib/",
            native_lock.entries.len(),
            output
        );
    }

    let base_dir = Path::new(filename)
        .parent()
        .filter(|p| !p.as_os_str().is_empty());
    if check_native && uses_ffi {
        let libs = libraries;
    let mut gate = machine::DloadGate::from_consumer_trusted(
        &pipeline.dload_native_pins(),
        pipeline.dload_trusted_stems(),
    );
        for stem in pipeline.extra_dload_stems() {
            gate.grant_stem(stem);
        }
        for (stem, path) in pipeline.extra_dload_grants() {
            let _ = gate.grant_file(stem, path);
        }
        let search = pipeline.ffi_search_path_bufs();
        // A library linked statically has no file to find on the target.
        let linked = |name: &str| link.statics.iter().any(|l| l.stem == machine::dload_request_stem(name));
        for name in libs.iter().filter(|n| !linked(n)) {
            if let Err(e) = machine::resolve_library(name, base_dir, &search, &gate) {
                fail_and_exit(
                    pipeline,
                    ErrorCode::IoError,
                    format!("packaging check failed for `{name}`: {e}"),
                );
            }
        }
    }

    // The executable the payload goes on: one linked from native code for
    // the program, or the JIT runner (`--no-aot`, `--runner`, or when
    // linking is not possible here).
    // An AOT executable starts in its own code: `__start` runs the startup
    // and `main`, and the payload is data only.
    let start = aot.then(|| with_native_start(pipeline, &bodies, &program));
    let aot_bodies = start.as_ref().map_or(&[][..], |(b, _)| b.as_slice());
    let package = start.as_ref().map(|(_, data)| {
        let archive = payload_bytes(pipeline, data);
        let lock = (!native_lock.entries.is_empty()).then(|| native_lock.to_json());
        linked_package(&archive, lock.as_deref().map(str::as_bytes), flags, ARCHIVE_VERSION)
    });
    if link.emit != Emit::Exe {
        write_linkable(pipeline, aot_bodies, link, package.as_deref().unwrap_or_default(), output);
        return;
    }
    let linked = package.as_deref().and_then(|p| link_aot_executable(pipeline, aot_bodies, link, p));
    if let Some((packaged, compiled, rt)) = linked {
        if let Err(e) = fs::write(output, &packaged) {
            fail_and_exit(pipeline, ErrorCode::IoError, format!("cannot write packaged output `{}`: {e}", output));
        }
        make_executable(Path::new(output));
        if let Err(e) = pipeline.finish_reporting() {
            pipeline.emit_spanless_warning(ErrorCode::IoError, format!("failed to flush diagnostics: {e}"));
            let _ = pipeline.finish_reporting();
        }
        eprintln!(
            "packaged `{}` for {}-{} ({} bytes; native code for {} functions, runtime {})",
            output,
            std::env::consts::OS,
            std::env::consts::ARCH,
            packaged.len(),
            compiled,
            rt.display()
        );
        return;
    }
    let runner_path = match resolve_runner_path(runner) {
        Ok(p) => p,
        Err(msg) => fail_and_exit(pipeline, ErrorCode::IoError, msg),
    };
    let runner_bytes = match fs::read(&runner_path) {
        Ok(b) => b,
        Err(e) => fail_and_exit(
            pipeline,
            ErrorCode::IoError,
            format!("cannot read runner `{}`: {e}", runner_path.display()),
        ),
    };

    if is_packaged_executable(&runner_bytes) {
        fail_and_exit(
            pipeline,
            ErrorCode::IoError,
            format!(
                "runner `{}` is already a packaged executable; use an unpackaged `coil-embed` \
                 binary as the template",
                runner_path.display()
            ),
        );
    }

    if !coil_cli::is_coil_embed_runner(&runner_bytes) {
        fail_and_exit(
            pipeline,
            ErrorCode::IoError,
            format!(
                "runner `{}` is not a `coil-embed` binary; only `coil-embed` runs packaged programs",
                runner_path.display()
            ),
        );
    }

    add_native_mir(pipeline, &mut program, &bodies);
    let archive_bytes = payload_bytes(pipeline, &program);
    let lock_json = if native_lock.entries.is_empty() {
        None
    } else {
        Some(native_lock.to_json())
    };
    let packaged = append_package_payload_with_natives(
        &runner_bytes,
        &archive_bytes,
        lock_json.as_deref().map(|s| s.as_bytes()),
        flags,
        ARCHIVE_VERSION,
    );

    if let Err(e) = fs::write(output, &packaged) {
        fail_and_exit(
            pipeline,
            ErrorCode::IoError,
            format!("cannot write packaged output `{}`: {e}", output),
        );
    }
    make_executable(Path::new(output));

    if let Err(e) = pipeline.finish_reporting() {
        pipeline.emit_spanless_warning(
            ErrorCode::IoError,
            format!("failed to flush diagnostics: {e}"),
        );
        let _ = pipeline.finish_reporting();
    }

    eprintln!(
        "packaged `{}` for {}-{} ({} bytes; runner {})",
        output,
        std::env::consts::OS,
        std::env::consts::ARCH,
        packaged.len(),
        runner_path.display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_link_stem_comes_from_the_file_or_the_flag() {
        let l = parse_static_link("deps/libsum.a").unwrap();
        assert_eq!((l.stem.as_str(), l.path.as_path()), ("sum", Path::new("deps/libsum.a")));
        assert_eq!(parse_static_link("sum.lib").unwrap().stem, "sum");
        let l = parse_static_link("plugin=out/libplugin-x86.a").unwrap();
        assert_eq!((l.stem.as_str(), l.path.as_path()), ("plugin", Path::new("out/libplugin-x86.a")));
        assert!(parse_static_link("libsum.so").is_err());
    }

    /// A runtime appended to `coil` (`scripts/embed_runtime.py`'s layout) is
    /// written to the cache once and found there after; a binary without
    /// one has none, and a cut-off one is an error.
    #[test]
    fn embedded_runtime_is_cached_by_content() {
        let dir = std::env::temp_dir().join(format!("coil_embedded_rt_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let (exe, cache) = (dir.join("coil"), dir.join("cache"));
        let lib = b"!<arch>\nruntime objects".to_vec();
        let mut image = b"\x7fELF the compiler".to_vec();
        fs::write(&exe, &image).unwrap();
        assert_eq!(embedded_runtime(&exe, &cache).unwrap(), None);
        image.extend_from_slice(&lib);
        image.extend_from_slice(&(lib.len() as u64).to_le_bytes());
        image.extend_from_slice(RUNTIME_MAGIC);
        fs::write(&exe, &image).unwrap();
        let path = embedded_runtime(&exe, &cache).unwrap().expect("embedded runtime");
        assert!(path.starts_with(&cache));
        assert_eq!(fs::read(&path).unwrap(), lib);
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(embedded_runtime(&exe, &cache).unwrap(), Some(path.clone()));
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified, "reused, not rewritten");
        let mut cut = image[..4].to_vec();
        cut.extend_from_slice(&image[image.len() - 16..]);
        fs::write(&exe, &cut).unwrap();
        assert!(embedded_runtime(&exe, &cache).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Project-mode `coil natives dump` lists a row whose local file is not
    /// there yet as missing instead of failing, so a downloader can fetch it;
    /// a file that is there is still hashed (coil-lang#1657).
    #[test]
    fn native_lock_from_ffi_natives_lists_missing_rows() {
        let dir = std::env::temp_dir().join(format!("coil_dump_missing_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let here = platform_shared_lib_filename("here");
        fs::write(dir.join(&here), b"abc").unwrap();
        let gone = platform_shared_lib_filename("gone");
        // A directory where the library should be is still an error.
        fs::create_dir_all(dir.join("odd").join(platform_shared_lib_filename("odd"))).unwrap();
        let row = |name: &str, path: &Path| {
            parse_ffi_native(&format!("name={name},version=1.0.0,path={}", path.display())).unwrap()
        };
        let lock = native_lock_from_ffi_natives(&[row("here", &dir), row("gone", &dir)]);
        let odd = native_lock_from_ffi_natives(&[row("odd", &dir.join("odd"))]);
        let _ = fs::remove_dir_all(&dir);

        let lock = lock.expect("a missing local file is listed, not an error");
        assert_eq!(lock.entries.len(), 2);
        let (h, g) = (&lock.entries[0], &lock.entries[1]);
        assert!(!h.missing);
        assert_eq!(h.size, 3);
        assert_eq!(
            h.sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(g.missing);
        assert_eq!((g.filename.as_str(), g.sha256.as_str(), g.size), (gone.as_str(), "", 0));

        let tsv = lock.to_tsv();
        assert!(tsv.contains(&format!("here\t1.0.0\t{here}\t{}\t3\n", h.sha256)), "{tsv}");
        assert!(tsv.contains(&format!("gone\t1.0.0\t{gone}\t\t\tmissing\n")), "{tsv}");
        let back = NativeLock::from_json(&lock.to_json()).expect("json round trip");
        assert_eq!(back, lock);

        assert!(odd.is_err(), "a directory at the library path must not pass");
    }

}
