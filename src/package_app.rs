//! `coil package` — compile entry and append to a runner template.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::exit;

use coil_cli::resolve_default_runner;
use common::{
    append_package_payload_with_natives, bytecode_uses_ffi, ffi_library_names_from_bytecode,
    is_packaged_executable, is_system_ffi_stem, ArchivedArchivedProgram, ArchivedProgram, Byte,
    NativeLock, NativeLockEntry, ARCHIVE_VERSION, PACKAGE_FLAG_USES_FFI,
};
use compiler::Pipeline;
use machine::platform_shared_lib_filename;
use reporting::ErrorCode;
use rkyv::rancor::Error;
use sha2::{Digest, Sha256};

use crate::fail_and_exit;

fn compile_program_archive_bytes(
    pipeline: &mut Pipeline,
    filename: &str,
    strip_debug: bool,
) -> Result<Vec<u8>, ()> {
    let (bytecode, constants) = pipeline.compile_src_from_file(filename).map_err(|_| ())?;
    let debug = pipeline.program_debug();
    // Cleanup ranges are behaviour (panics run `defer`s), not debug info.
    let cleanup_ranges = debug.cleanup.clone();
    let (source_files, debug_locs, debug_lines) = if strip_debug {
        (Vec::new(), Vec::new(), Vec::new())
    } else {
        (debug.source_files, debug.debug_locs, debug.debug_lines)
    };
    let program = ArchivedProgram {
        version: ARCHIVE_VERSION,
        static_slot_count: pipeline.static_slot_count(),
        constants,
        strings: pipeline.strings().to_vec(),
        bytecode,
        source_files,
        debug_locs,
        fn_symbols: Vec::new(),
        struct_layouts: pipeline.archived_struct_layouts(),
        operand_stack_slots: pipeline.operand_stack_slots(),
        stack_maps: pipeline.stack_maps().to_vec(),
        precise_frames: pipeline.precise_frames().to_vec(),
        class_word_kinds: pipeline.class_word_kinds(),
        static_word_kinds: pipeline.static_word_kinds(),
        debug_lines,
        cleanup_ranges,
    };
    rkyv::to_bytes::<Error>(&program)
        .map(|b| b.as_slice().to_vec())
        .map_err(|_| ())
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
) -> Result<NativeLock, String> {
    let stems = ffi_library_names_from_bytecode(bytecode, strings);
    let mut entries = Vec::new();

    for stem in &stems {
        if is_system_ffi_stem(stem) {
            continue;
        }
        let decl = natives.iter().find(|n| n.name == *stem).ok_or_else(|| {
            format!(
                "FFI library `{stem}` is loaded but has no `--ffi-native name={stem},…`; \
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
        // The pin is the local file's hash, so the file must exist.
        if !lib_path.is_file() {
            return Err(format!(
                "--ffi-native `{}`: expected library at `{}`",
                decl.name,
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
    strip_debug: bool,
) {
    let archive_bytes = match compile_program_archive_bytes(pipeline, filename, strip_debug) {
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

    let native_lock = match build_native_lock(natives, &bytecode, &strings) {
        Ok(lock) => lock,
        Err(msg) => fail_and_exit(pipeline, ErrorCode::IoError, msg),
    };

    if uses_ffi && native_lock.entries.is_empty() {
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
        for name in &libs {
            if let Err(e) = machine::resolve_library(name, base_dir, &search, &gate) {
                fail_and_exit(
                    pipeline,
                    ErrorCode::IoError,
                    format!("packaging check failed for `{name}`: {e}"),
                );
            }
        }
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
                 (or `coil`) binary as the template",
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
        };
        let bytes = rkyv::to_bytes::<Error>(&other_minor).unwrap();
        assert!(matches!(
            load_archive_bytes(bytes.as_slice()),
            Err(LoadErr::Version(_))
        ));
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
        };
        let bytes = rkyv::to_bytes::<Error>(&program).unwrap();
        let mut prefixed = vec![0u8; 1];
        prefixed.extend_from_slice(bytes.as_slice());
        load_archive_bytes(&prefixed[1..]).expect("unaligned overlay slice");
    }
}
