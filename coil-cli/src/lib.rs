//! Shared CLI helpers for coil binaries (archive load, VM execute, git-style dispatch).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, exit};
use std::sync::Arc;

use common::{
    ARCHIVE_VERSION, ArchivedArchivedProgram, Byte, NativeLock, ProgramDebug,
    archive_version_compatible, default_natives_root, embedded_archive_slice,
    format_archive_version, read_embedded_native_lock, read_package_trailer,
};
use machine::{Machine, wire_standard_host_natives};
use machine::thread::ThreadProgram;
use rkyv::rancor::Error;

/// Errors loading a `.hyc` / embedded archive blob.
#[derive(Debug)]
pub enum LoadErr {
    Missing,
    Corrupt,
    Version(u32),
}

/// Deserialize an `ArchivedProgram` blob (from `.hyc` or an embedded slice).
pub fn load_archive_bytes(
    buffer: &[u8],
) -> Result<(Vec<Byte>, Vec<u64>, Vec<String>, u32, ProgramDebug), LoadErr> {
    let align = std::mem::align_of::<ArchivedArchivedProgram>();
    if (buffer.as_ptr() as usize) % align == 0 {
        decode_archive(buffer)
    } else {
        let mut aligned = rkyv::util::AlignedVec::<16>::with_capacity(buffer.len());
        aligned.extend_from_slice(buffer);
        decode_archive(&aligned)
    }
}

fn decode_archive(
    buffer: &[u8],
) -> Result<(Vec<Byte>, Vec<u64>, Vec<String>, u32, ProgramDebug), LoadErr> {
    let archived =
        rkyv::access::<ArchivedArchivedProgram, Error>(buffer).map_err(|_| LoadErr::Corrupt)?;
    let version = u32::from(archived.version);
    if !archive_version_compatible(version, ARCHIVE_VERSION) {
        return Err(LoadErr::Version(version));
    }
    let bytecode =
        rkyv::deserialize::<Vec<Byte>, Error>(&archived.bytecode).map_err(|_| LoadErr::Corrupt)?;
    let constants =
        rkyv::deserialize::<Vec<u64>, Error>(&archived.constants).map_err(|_| LoadErr::Corrupt)?;
    let strings =
        rkyv::deserialize::<Vec<String>, Error>(&archived.strings).map_err(|_| LoadErr::Corrupt)?;
    let static_slot_count = u32::from(archived.static_slot_count);
    let source_files = rkyv::deserialize::<Vec<String>, Error>(&archived.source_files)
        .map_err(|_| LoadErr::Corrupt)?;
    let debug_locs = rkyv::deserialize::<Vec<common::DebugLoc>, Error>(&archived.debug_locs)
        .map_err(|_| LoadErr::Corrupt)?;
    Ok((
        bytecode,
        constants,
        strings,
        static_slot_count,
        ProgramDebug {
            source_files,
            debug_locs,
            fn_symbols: Vec::new(),
        },
    ))
}

/// Load a `.hyc` file from disk.
pub fn try_load_archive(
    path: &str,
) -> Result<(Vec<Byte>, Vec<u64>, Vec<String>, u32, ProgramDebug), LoadErr> {
    let mut f = std::fs::File::open(path).map_err(|_| LoadErr::Missing)?;
    let mut buffer = Vec::with_capacity(1024);
    f.read_to_end(&mut buffer).map_err(|_| LoadErr::Corrupt)?;
    load_archive_bytes(&buffer)
}

/// Run archived bytecode with standard host natives (no compiler).
///
/// Returns `true` when a language-level `panic` aborted.
///
/// FFI struct layouts from `extern struct` are not restored here (not stored in
/// the archive yet). `ffi_search_paths` are searched before `entry`'s parent.
pub fn execute_archived_program(
    bytecode: &[Byte],
    constants: &[u64],
    strings: &[String],
    static_slots: u32,
    debug: ProgramDebug,
    entry: Option<&Path>,
    ffi_search_paths: Vec<PathBuf>,
) -> bool {
    let mut machine = Machine::<256>::with_operand_capacity(machine::DEFAULT_OPERAND_STACK_SLOTS);
    wire_standard_host_natives(&mut machine);

    let base_dir = entry.and_then(|p| p.parent()).map(PathBuf::from);
    machine.set_ffi_paths(base_dir, ffi_search_paths);

    machine.set_thread_program(Arc::new(ThreadProgram {
        code: Arc::from(bytecode.to_vec()),
        constants: Arc::from(constants.to_vec()),
        strings: Arc::from(strings.to_vec()),
        static_slot_count: static_slots,
        debug: debug.clone(),
        operand_stack_slots: machine::DEFAULT_OPERAND_STACK_SLOTS as u32,
    }));
    machine.set_program_debug(debug);
    machine.run_raw(bytecode, constants, strings, static_slots);
    machine.panicked()
}

/// Verify every direct native lock entry exists in the natives cache with matching size.
fn ensure_native_cache(lock: &NativeLock, exe: &Path) -> Result<Vec<PathBuf>, String> {
    let root = default_natives_root();
    let mut dirs = Vec::new();
    let mut missing = Vec::new();
    for entry in &lock.entries {
        let path = NativeLock::entry_cache_path(&root, entry);
        let dir = NativeLock::entry_cache_dir(&root, entry);
        if path.is_file() {
            if let Ok(meta) = std::fs::metadata(&path) {
                if meta.len() == entry.size {
                    if !dirs.iter().any(|d: &PathBuf| d == &dir) {
                        dirs.push(dir);
                    }
                    continue;
                }
            }
        }
        missing.push(format!("{} {} ({})", entry.package, entry.version, entry.filename));
    }
    if !missing.is_empty() {
        return Err(format!(
            "Unable to continue: native libraries missing:\n  {}\nRun: spool download {}",
            missing.join("\n  "),
            exe.display()
        ));
    }
    Ok(dirs)
}

/// If this process is a packaged binary, run the embedded program and return `Some(panicked)`.
pub fn try_run_embedded() -> Option<bool> {
    use machine::packaged_app_ffi_startup_check;

    let exe = std::env::current_exe().ok()?;
    let data = std::fs::read(&exe).ok()?;
    let trailer = read_package_trailer(&data)?;
    let archive = embedded_archive_slice(&data, trailer)?;

    if !archive_version_compatible(trailer.archive_version, ARCHIVE_VERSION) {
        eprintln!(
            "embedded bytecode version {} does not match this runner ({}); rebuild with `coil package`",
            format_archive_version(trailer.archive_version),
            format_archive_version(ARCHIVE_VERSION)
        );
        exit(1);
    }

    let (bytecode, constants, strings, static_slots, debug) = match load_archive_bytes(archive) {
        Ok(ok) => ok,
        Err(LoadErr::Version(v)) => {
            eprintln!(
                "embedded archive version {} is not compatible with runner {}",
                format_archive_version(v),
                format_archive_version(ARCHIVE_VERSION)
            );
            exit(1);
        }
        Err(_) => {
            eprintln!("embedded bytecode archive is corrupt");
            exit(1);
        }
    };

    if let Err(msg) = packaged_app_ffi_startup_check(trailer.uses_ffi()) {
        eprintln!("error: {msg}");
        exit(1);
    }

    let mut ffi_search_paths = Vec::new();
    match read_embedded_native_lock(&data, trailer) {
        Ok(Some(lock)) if !lock.entries.is_empty() => {
            if lock.os != std::env::consts::OS || lock.arch != std::env::consts::ARCH {
                eprintln!(
                    "error: native lock is for {}-{}, this host is {}-{}",
                    lock.os,
                    lock.arch,
                    std::env::consts::OS,
                    std::env::consts::ARCH
                );
                exit(1);
            }
            match ensure_native_cache(&lock, &exe) {
                Ok(dirs) => ffi_search_paths = dirs,
                Err(msg) => {
                    eprintln!("error: {msg}");
                    exit(1);
                }
            }
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!("error: corrupt native lock: {e}");
            exit(1);
        }
    }

    // Prefer cache dirs, then $ORIGIN and $ORIGIN/lib.
    if let Some(parent) = exe.parent() {
        ffi_search_paths.push(parent.to_path_buf());
        ffi_search_paths.push(parent.join("lib"));
    }

    let panicked = execute_archived_program(
        &bytecode,
        &constants,
        &strings,
        static_slots,
        debug,
        Some(exe.as_path()),
        ffi_search_paths,
    );
    Some(panicked)
}

/// Path of `name` beside `exe`, including the host suffix (`.exe` on Windows).
pub fn sibling_bin(exe: &Path, name: &str) -> PathBuf {
    exe.with_file_name(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

/// Resolve `coil-{name}` beside the current executable and re-exec with remaining args.
///
/// Argv for the helper is `env::args().skip(2)` (drops program name + subcommand).
pub fn dispatch_helper(sub: &str) -> ! {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("coil: cannot resolve current executable: {e}");
            exit(1);
        }
    };
    let helper_name = format!("coil-{sub}");
    let helper = sibling_bin(&exe, &helper_name);
    if !helper.is_file() {
        eprintln!(
            "`coil {sub}` requires `{helper_name}` next to this binary\n\
             (looked for {})",
            helper.display()
        );
        exit(1);
    }
    let args: Vec<String> = std::env::args().skip(2).collect();
    let status = Command::new(&helper).args(&args).status();
    match status {
        Ok(s) => exit(s.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("coil: failed to exec `{}`: {e}", helper.display());
            exit(1);
        }
    }
}

/// Resolve the default package runner template (`coil-embed` beside this binary).
pub fn resolve_default_runner() -> Result<PathBuf, String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("cannot resolve current executable: {e}"))?;
    let embed = sibling_bin(&exe, "coil-embed");
    if embed.is_file() {
        return Ok(embed);
    }
    eprintln!(
        "warning: `coil-embed` not found next to {}; packaging with full `coil` as runner \
         (install `coil-embed` for a smaller packaged binary)",
        exe.display()
    );
    Ok(exe)
}

/// Stdout/stderr writer selection for SARIF / LSP / pretty reports.
pub fn writer_for_format(pretty_on_stderr: bool) -> Box<dyn Write + Send> {
    if pretty_on_stderr {
        Box::new(std::io::stderr())
    } else {
        Box::new(std::io::stdout())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_bin_uses_host_exe_suffix() {
        let exe = Path::new("/opt/coil").join(format!("coil{}", std::env::consts::EXE_SUFFIX));
        let helper = sibling_bin(&exe, "coil-debug");
        let expected = format!("coil-debug{}", std::env::consts::EXE_SUFFIX);
        assert_eq!(
            helper.file_name().and_then(|n| n.to_str()),
            Some(expected.as_str())
        );
    }
}
