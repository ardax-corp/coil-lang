//! `coil package` — compile entry and append to a runner template.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::exit;

use coil_cli::resolve_default_runner;
use common::{
    append_package_payload_with_natives, bytecode_uses_ffi, dload_request_stem,
    ffi_library_names_from_bytecode,
    is_packaged_executable, is_system_ffi_stem, ArchivedArchivedProgram, ArchivedProgram, Byte,
    NativeLock, NativeLockEntry, ARCHIVE_VERSION, PACKAGE_FLAG_USES_FFI,
};
use compiler::Pipeline;
use machine::platform_shared_lib_filename;
use reporting::ErrorCode;
use rkyv::rancor::Error;
use sha2::{Digest, Sha256};

use crate::fail_and_exit;

/// Compile `filename` to archive bytes at the pipeline's `-g` level
/// (`coil package` defaults to `-g1`; `--strip-debug` is `-g0`).
fn compile_program_archive_bytes(pipeline: &mut Pipeline, filename: &str) -> Result<Vec<u8>, ()> {
    let (bytecode, constants) = pipeline.compile_src_from_file(filename).map_err(|_| ())?;
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
        native_mir: Vec::new(),
    };
    // Cleanup ranges are behaviour (panics run `defer`s), not debug info:
    // `set_debug` keeps them at every level.
    // Native code locates its panics by byte span: an AOT payload keeps the
    // spans with the lines.
    let level = crate::native_debug_level(pipeline.debug_level(), compiler::native_mir::native_capture_active());
    program.set_debug(pipeline.program_debug_full(), level);
    rkyv::to_bytes::<Error>(&program)
        .map(|b| b.as_slice().to_vec())
        .map_err(|_| ())
}

/// Link an AOT executable for the program just compiled: its MIR through
/// `coil_native::aot`, then `cc` with the runtime library (`libcoil_rt.a`
/// beside `coil`, or `COIL_RT_LIB`). Returns the image, how many functions
/// compiled and the runtime used; `None` (with a warning) when this host
/// cannot link one, and the VM runner is packaged instead.
#[cfg(not(target_family = "wasm"))]
fn link_aot_executable(pipeline: &Pipeline, link: &LinkInputs) -> Option<(Vec<u8>, usize, PathBuf)> {
    let bodies = compiler::native_mir::take_native_capture();
    let debug = pipeline.program_debug_full();
    let pcs = debug.fn_symbols.iter().map(|s| (s.name.clone(), s.entry_pc)).collect();
    let fallback = |why: String| {
        // Static libraries link only into an AOT executable.
        if !link.statics.is_empty() {
            eprintln!("error: {why}; `--link-static` needs the AOT executable");
            exit(1);
        }
        eprintln!("warning: {why}; packaging the VM runner instead (pass `--no-aot` to skip linking)");
        None
    };
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
    let object = match coil_native::aot(&bodies, &pcs, &linked) {
        Ok(o) => o,
        Err(e) => return fallback(format!("native code for the package failed ({e})")),
    };
    if std::env::var_os("COIL_NATIVE_STATS").is_some_and(|v| v == "2") {
        for (name, why) in &object.refused {
            eprintln!("  vm: {name}: {why}");
        }
    }
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
fn link_aot_executable(_: &Pipeline, _: &LinkInputs) -> Option<(Vec<u8>, usize, PathBuf)> {
    None
}

/// How a package's executable is made.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LinkInputs {
    /// Link native code for the program (an AOT executable); `false` with
    /// `--no-aot` or `--runner`: the VM runner runs it.
    pub aot: bool,
    /// Static libraries for `extern` blocks (`--link-static`).
    pub statics: Vec<StaticLink>,
    /// Extra linker arguments, after the libraries (`--link-arg`).
    pub args: Vec<String>,
    /// What the linker strips from the executable (`--strip-symbols`).
    pub strip: StripSymbols,
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

/// `libcoil_rt.a`: `COIL_RT_LIB`, else the copy embedded in this binary
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
    let lib = exe.with_file_name(if cfg!(windows) { "coil_rt.lib" } else { "libcoil_rt.a" });
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
    let path = dir.join(if cfg!(windows) { "coil_rt.lib" } else { "libcoil_rt.a" });
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

/// `cc OBJECT STATIC-LIBS RUNTIME -o EXE LINK-ARGS` with the system
/// libraries the runtime needs.
#[cfg(not(target_family = "wasm"))]
fn link_with_cc(object: &[u8], rt: &Path, link: &LinkInputs) -> Result<Vec<u8>, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let dir = std::env::temp_dir().join(format!("coil-aot-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create `{}`: {e}", dir.display()))?;
    let obj = dir.join("program.o");
    let exe = dir.join("program");
    let out = (|| {
        fs::write(&obj, object).map_err(|e| format!("cannot write `{}`: {e}", obj.display()))?;
        let cc = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
        // Dead code goes: what nothing the program runs references (the
        // interpreter, when the driver runs it all) is not linked in.
        let libs: &[&str] = if cfg!(target_os = "macos") {
            &["-Wl,-dead_strip", "-lSystem", "-lc", "-lm"]
        } else {
            &["-Wl,--gc-sections", "-ldl", "-lgcc_s", "-lutil", "-lrt", "-lpthread", "-lm", "-lc"]
        };
        let run = std::process::Command::new(&cc)
            .arg(&obj)
            .args(link.statics.iter().map(|s| &s.path))
            .arg(rt)
            .arg("-o")
            .arg(&exe)
            .args(&link.args)
            .args(libs)
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

/// Build a [`NativeLock`] from bytecode `dload` stems and `--ffi-native` rows.
pub fn build_native_lock(
    natives: &[FfiNative],
    bytecode: &[Byte],
    strings: &[String],
    linked: &[StaticLink],
) -> Result<NativeLock, String> {
    // Reduce each literal to the stem the runtime dload gate uses
    // (`./native/libtls.so` → `tls`), so one `--ffi-native name=tls` row covers
    // every spelling of that library.
    let mut stems: Vec<(String, String)> = Vec::new();
    for literal in ffi_library_names_from_bytecode(bytecode, strings) {
        let stem = dload_request_stem(&literal);
        if !stems.iter().any(|(s, _)| *s == stem) {
            stems.push((stem, literal));
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

    // Constant stems only: `--ffi-native` rows the bytecode never loads are ignored.

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
    if !aot && !link.statics.is_empty() {
        fail_and_exit(
            pipeline,
            ErrorCode::IoError,
            "`--link-static` links into the AOT executable; it cannot be used with `--no-aot` or `--runner`".to_string(),
        );
    }
    // An AOT executable is linked from this compile's MIR.
    #[cfg(not(target_family = "wasm"))]
    if aot {
        compiler::native_mir::start_native_capture();
    }
    let archive_bytes = compile_program_archive_bytes(pipeline, filename);
    // Notes below and errors go to a clean line.
    crate::end_compile_progress();
    let archive_bytes = match archive_bytes {
        Ok(b) => b,
        Err(()) => {
            let _ = pipeline.finish_reporting();
            exit(1);
        }
    };

    let program = rkyv::access::<ArchivedArchivedProgram, Error>(&archive_bytes)
        .expect("freshly serialized archive");
    let bytecode: Vec<Byte> =
        rkyv::deserialize::<Vec<Byte>, Error>(&program.bytecode).expect("bytecode");
    let strings: Vec<String> =
        rkyv::deserialize::<Vec<String>, Error>(&program.strings).expect("strings");
    let uses_ffi = bytecode_uses_ffi(&bytecode);
    let mut flags = 0u32;
    if uses_ffi {
        flags |= PACKAGE_FLAG_USES_FFI;
    }

    let native_lock = match build_native_lock(natives, &bytecode, &strings, &link.statics) {
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
        let libs = ffi_library_names_from_bytecode(&bytecode, &strings);
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
    // the program, or the VM runner (`--no-aot`, `--runner`, or when
    // linking is not possible here).
    let linked = if aot { link_aot_executable(pipeline, link) } else { None };
    if let Some((exe, compiled, rt)) = linked {
        let lock_json = (!native_lock.entries.is_empty()).then(|| native_lock.to_json());
        let packaged = append_package_payload_with_natives(
            &exe,
            &archive_bytes,
            lock_json.as_deref().map(|s| s.as_bytes()),
            flags,
            ARCHIVE_VERSION,
        );
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
    use coil_cli::{LoadErr, load_archive_bytes};
    use common::{ArchivedProgram, Instruction, pack_archive_version};

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

    #[test]
    fn load_archive_bytes_rejects_version() {
        let too_new = ArchivedProgram {
            version: pack_archive_version(0, 1),
            static_slot_count: 0,
            constants: vec![],
            strings: vec![],
            bytecode: vec![Byte::new(Instruction::HALT)],
            source_files: vec![],
            debug_locs: vec![],
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
            native_mir: Vec::new(),
        };
        let bytes = rkyv::to_bytes::<Error>(&too_new).unwrap();
        assert!(matches!(
            load_archive_bytes(bytes.as_slice()),
            Err(LoadErr::Version(_))
        ));

        let other_minor = ArchivedProgram {
            version: pack_archive_version(1, 99),
            static_slot_count: 0,
            constants: vec![],
            strings: vec![],
            bytecode: vec![Byte::new(Instruction::HALT)],
            source_files: vec![],
            debug_locs: vec![],
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
            native_mir: Vec::new(),
        };
        let bytes = rkyv::to_bytes::<Error>(&other_minor).unwrap();
        assert!(matches!(
            load_archive_bytes(bytes.as_slice()),
            Err(LoadErr::Version(_))
        ));
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

    /// A `dload` path literal reduces to the stem the runtime gate uses, so
    /// one `--ffi-native name=<stem>` row covers it (coil-lang#1656).
    #[test]
    fn build_native_lock_reduces_dload_path_to_stem() {
        let dir = std::env::temp_dir().join(format!("coil_lock_stem_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let filename = platform_shared_lib_filename("tls");
        fs::write(dir.join(&filename), b"not really a library").unwrap();
        let natives = vec![parse_ffi_native(&format!(
            "name=tls,version=1.0.0,path={}",
            dir.display()
        ))
        .unwrap()];
        let strings = vec![format!("./native/{filename}"), "tls".to_string()];
        let bytecode = vec![
            Byte::new(Instruction::STRING).with_operand_u32(0),
            Byte::new(Instruction::FfiLoad),
            Byte::new(Instruction::STRING).with_operand_u32(1),
            Byte::new(Instruction::FfiLoad),
        ];
        let lock = build_native_lock(&natives, &bytecode, &strings, &[]);
        let _ = fs::remove_dir_all(&dir);
        let lock = lock.expect("the `tls` row covers `./native/libtls.so`");
        assert_eq!(lock.entries.len(), 1, "one entry per stem: {:?}", lock.entries);
        assert_eq!(lock.entries[0].stem, "tls");
        assert_eq!(lock.entries[0].filename, filename);
    }

    #[test]
    fn load_archive_bytes_accepts_unaligned_prefix() {
        let program = ArchivedProgram {
            version: ARCHIVE_VERSION,
            static_slot_count: 0,
            constants: vec![],
            strings: vec![],
            bytecode: vec![Byte::new(Instruction::HALT)],
            source_files: vec![],
            debug_locs: vec![],
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
            native_mir: Vec::new(),
        };
        let bytes = rkyv::to_bytes::<Error>(&program).unwrap();
        let mut prefixed = vec![0u8; 1];
        prefixed.extend_from_slice(bytes.as_slice());
        load_archive_bytes(&prefixed[1..]).expect("unaligned overlay slice");
    }
}
