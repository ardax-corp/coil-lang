//! `coil package` — compile entry and append to a runner template.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::exit;

use coil_cli::resolve_default_runner;
use common::{
    append_package_payload, bytecode_uses_ffi, ffi_library_names_from_bytecode,
    is_packaged_executable, ArchivedArchivedProgram, ArchivedProgram, Byte, ARCHIVE_VERSION,
    PACKAGE_FLAG_USES_FFI,
};
use compiler::Pipeline;
use machine::check_native_libraries;
use reporting::ErrorCode;
use rkyv::rancor::Error;

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

    if uses_ffi {
        eprintln!(
            "note: this program uses FFI. Target machines need the shared libraries it loads \
             (libffi is linked into this runner; user `.so` / `.dll` files are not bundled)."
        );
    }

    let base_dir = Path::new(filename)
        .parent()
        .filter(|p| !p.as_os_str().is_empty());
    if check_native && uses_ffi {
        let libs = ffi_library_names_from_bytecode(&bytecode, &strings);
        let gate = pipeline.build_dload_gate();
        if let Err(msg) = check_native_libraries(&libs, base_dir, &gate) {
            fail_and_exit(pipeline, ErrorCode::IoError, msg);
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

    let packaged = append_package_payload(&runner_bytes, &archive_bytes, flags, ARCHIVE_VERSION);

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

        // Same major with a newer minor than the runtime must be rejected.
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
