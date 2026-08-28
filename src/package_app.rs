//! `coil package` — compile entry and append to a runner template.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::exit;

use coil_cli::resolve_default_runner;
use common::{
    ARCHIVE_VERSION, ArchivedArchivedProgram, ArchivedProgram, Byte, NativeLock, NativeLockEntry,
    PACKAGE_FLAG_USES_FFI, append_package_payload_with_natives, bytecode_uses_ffi,
    ffi_library_names_from_bytecode, is_packaged_executable, is_system_ffi_stem,
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
    let (bytecode, constants) = pipeline.compile_src_from_file(filename)?;
    let debug = pipeline.program_debug();
    let (source_files, debug_locs) = if strip_debug {
        (Vec::new(), Vec::new())
    } else {
        (debug.source_files, debug.debug_locs)
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
    let mut f = fs::File::open(path).map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
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

/// Build a [`NativeLock`] from bytecode `dload` stems and `[[ffi.native]]` rows.
pub fn build_native_lock(
    pipeline: &Pipeline,
    bytecode: &[Byte],
    strings: &[String],
) -> Result<NativeLock, String> {
    let stems = ffi_library_names_from_bytecode(bytecode, strings);
    let mut entries = Vec::new();
    let natives = &pipeline.manifest().ffi_natives;
    let root = pipeline.project_root();

    for stem in &stems {
        if is_system_ffi_stem(stem) {
            continue;
        }
        let decl = natives
            .iter()
            .find(|n| n.name == *stem)
            .ok_or_else(|| {
                format!(
                    "FFI library `{stem}` is loaded but not declared in `[[ffi.native]]`; \
                     add a row with name/version/path/url (system libs like `c` need no entry)"
                )
            })?;
        let filename = platform_shared_lib_filename(&decl.name);
        let lib_path = root.join(&decl.path).join(&filename);
        if !lib_path.is_file() {
            return Err(format!(
                "[[ffi.native]] `{stem}`: expected library at `{}`",
                lib_path.display()
            ));
        }
        let (sha256, size) = sha256_hex_file(&lib_path)?;
        if !decl.url.starts_with("https://") {
            return Err(format!(
                "[[ffi.native]] `{stem}`: url must be https://, got `{}`",
                decl.url
            ));
        }
        entries.push(NativeLockEntry {
            package: decl.package.clone(),
            version: decl.version.clone(),
            stem: decl.name.clone(),
            filename,
            url: decl.url.clone(),
            sha256,
            size,
            requires: decl.requires.clone(),
            requires_hint: decl.requires_hint.clone(),
        });
    }

    // Also include declared natives that may be loaded via non-constant paths? Plan says
    // constant stems only. Extra [[ffi.native]] rows unused by bytecode are ignored.

    Ok(NativeLock {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        entries,
    })
}

/// Build a native lock from the current project's `[[ffi.native]]` (project-mode download).
pub fn native_lock_from_project_manifest(pipeline: &Pipeline) -> Result<NativeLock, String> {
    let natives = &pipeline.manifest().ffi_natives;
    let root = pipeline.project_root();
    let mut entries = Vec::new();
    for decl in natives {
        if is_system_ffi_stem(&decl.name) {
            continue;
        }
        let filename = platform_shared_lib_filename(&decl.name);
        let lib_path = root.join(&decl.path).join(&filename);
        let (sha256, size) = if lib_path.is_file() {
            sha256_hex_file(&lib_path)?
        } else {
            // Allow project download when the local artifact is not built yet; hash
            // will be verified against the downloaded bytes using the URL payload only
            // if we had a pinned hash — require local file for a trusted pin.
            return Err(format!(
                "[[ffi.native]] `{}`: local library missing at `{}` (build it or package with a known file to pin sha256)",
                decl.name,
                lib_path.display()
            ));
        };
        entries.push(NativeLockEntry {
            package: decl.package.clone(),
            version: decl.version.clone(),
            stem: decl.name.clone(),
            filename,
            url: decl.url.clone(),
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

    let native_lock = match build_native_lock(pipeline, &bytecode, &strings) {
        Ok(lock) => lock,
        Err(msg) => fail_and_exit(pipeline, ErrorCode::IoError, msg),
    };

    if uses_ffi && native_lock.entries.is_empty() {
        // FFI opcodes present but only system libs (or dynamic dload) — OK for libc-only.
        eprintln!(
            "note: this program uses FFI; only system libraries were detected. \
             Userland natives need `[[ffi.native]]` rows and `spool download` on the target."
        );
    } else if !native_lock.entries.is_empty() {
        eprintln!(
            "note: {} native artifact(s) declared; on the target run: spool download {}",
            native_lock.entries.len(),
            output
        );
    }

    let base_dir = Path::new(filename)
        .parent()
        .filter(|p| !p.as_os_str().is_empty());
    if check_native && uses_ffi {
        let libs = ffi_library_names_from_bytecode(&bytecode, &strings);
        let gate = pipeline.build_dload_gate();
        let search: Vec<PathBuf> = pipeline
            .manifest()
            .ffi_search_paths
            .iter()
            .map(|p| pipeline.project_root().join(p))
            .collect();
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

/// Run a freshly packaged binary (integration tests).
#[cfg(test)]
#[allow(dead_code)]
pub fn run_packaged_output(path: &Path) -> Result<String, String> {
    use std::process::Command as StdCommand;
    let out = StdCommand::new(path)
        .output()
        .map_err(|e| format!("spawn {}: {e}", path.display()))?;
    if !out.status.success() {
        return Err(format!(
            "exit {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use coil_cli::{load_archive_bytes, LoadErr};
    use common::{pack_archive_version, ArchivedProgram, Instruction};

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
        };
        let bytes = rkyv::to_bytes::<Error>(&program).unwrap();
        let mut prefixed = vec![0u8; 1];
        prefixed.extend_from_slice(bytes.as_slice());
        load_archive_bytes(&prefixed[1..]).expect("unaligned overlay slice");
    }
}
