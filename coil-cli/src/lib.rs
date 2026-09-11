//! Shared CLI helpers for coil binaries (archive load, VM execute, git-style dispatch).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, exit};

use common::{
    ARCHIVE_VERSION, ArchiveDecodeError, ArchivedArchivedProgram, Byte, NativeLock, ProgramDebug,
    archive_version_compatible, decode_archived_program, default_natives_root,
    embedded_archive_slice, format_archive_version, read_embedded_native_lock,
    read_package_trailer, resolve_archive_operand_slots,
};
use machine::{DloadGate, Machine, wire_standard_host_natives, wire_thread_program_with_maps};

/// Errors loading a `.hyc` / embedded archive blob.
#[derive(Debug)]
pub enum LoadErr {
    Missing,
    Corrupt,
    Version(u32),
}

/// Owned archive payload restored by CLI and packaged execute.
pub struct LoadedArchive {
    pub bytecode: Vec<Byte>,
    pub constants: Vec<u64>,
    pub strings: Vec<String>,
    pub static_slots: u32,
    pub debug: ProgramDebug,
    pub struct_layouts: Vec<common::CStructLayout>,
    /// Analyzed capacity when the envelope stored it (minor 13+).
    pub operand_stack_slots: Option<u32>,
    /// S2b maps when the envelope stored them (minor 14+). Empty = conservative GC.
    pub stack_maps: Vec<common::FrameStackMap>,
}

/// Deserialize an `ArchivedProgram` blob (from `.hyc` or an embedded slice).
pub fn load_archive_bytes(buffer: &[u8]) -> Result<LoadedArchive, LoadErr> {
    let align = std::mem::align_of::<ArchivedArchivedProgram>();
    if (buffer.as_ptr() as usize) % align == 0 {
        decode_archive(buffer)
    } else {
        let mut aligned = rkyv::util::AlignedVec::<16>::with_capacity(buffer.len());
        aligned.extend_from_slice(buffer);
        decode_archive(&aligned)
    }
}

fn decode_archive(buffer: &[u8]) -> Result<LoadedArchive, LoadErr> {
    let decoded = decode_archived_program(buffer).map_err(|e| match e {
        ArchiveDecodeError::Corrupt => LoadErr::Corrupt,
        ArchiveDecodeError::Version(v) => LoadErr::Version(v),
    })?;
    let program = decoded.program;
    Ok(LoadedArchive {
        bytecode: program.bytecode,
        constants: program.constants,
        strings: program.strings,
        static_slots: program.static_slot_count,
        debug: ProgramDebug {
            source_files: program.source_files,
            debug_locs: program.debug_locs,
            fn_symbols: Vec::new(),
        },
        struct_layouts: program.struct_layouts,
        operand_stack_slots: decoded
            .operand_stack_slots_persisted
            .then_some(program.operand_stack_slots),
        stack_maps: if decoded.stack_maps_persisted {
            program.stack_maps
        } else {
            Vec::new()
        },
    })
}

/// Load a `.hyc` file from disk.
pub fn try_load_archive(path: &str) -> Result<LoadedArchive, LoadErr> {
    let mut f = std::fs::File::open(path).map_err(|_| LoadErr::Missing)?;
    let mut buffer = Vec::with_capacity(1024);
    f.read_to_end(&mut buffer).map_err(|_| LoadErr::Corrupt)?;
    load_archive_bytes(&buffer)
}

/// Run archived bytecode with standard host natives (no compiler).
///
/// Returns `true` when a language-level `panic` aborted.
/// `raise` is catchable (`Result.Err`); it does not set this flag (Q5).
///
/// Restores [`common::CStructLayout`] from the archive (CLI `.hyc` and packaged
/// runner share this path). `ffi_search_paths` are searched before `entry`'s parent.
///
/// Host capability flags are **not** stored in `.hyc` and are **not** re-applied
/// here. If the bytecode has the op, it runs. `dload` still uses lock hash /
/// trusted integrity when `dload_gate` is supplied. `coil.toml` is not consulted.
/// Minor 13+ stores the compiler stack bound. Minor 14+ stores S2b maps;
/// older archives keep empty maps (conservative stack GC). Seek+CALL
/// archives still grow to [`machine::MAX_OPERAND_STACK_SLOTS`].
pub fn archive_operand_slots(bytecode: &[Byte]) -> usize {
    common::legacy_archive_operand_slots(bytecode) as usize
}

pub fn execute_archived_program(
    loaded: &LoadedArchive,
    entry: Option<&Path>,
    ffi_search_paths: Vec<PathBuf>,
    dload_gate: Option<DloadGate>,
) -> bool {
    let slots =
        resolve_archive_operand_slots(loaded.operand_stack_slots, &loaded.bytecode) as usize;
    let mut machine = Machine::<256>::with_operand_capacity(slots);
    wire_standard_host_natives(&mut machine);
    if let Some(gate) = dload_gate {
        machine.set_dload_gate(gate);
    }

    let base_dir = entry.and_then(|p| p.parent()).map(PathBuf::from);
    machine.set_ffi_paths(base_dir, ffi_search_paths);
    for layout in &loaded.struct_layouts {
        machine.register_struct_layout(machine::CStructLayout::from_archive(layout));
    }

    wire_thread_program_with_maps(
        &mut machine,
        &loaded.bytecode,
        &loaded.constants,
        &loaded.strings,
        loaded.static_slots,
        loaded.debug.clone(),
        slots as u32,
        loaded.stack_maps.clone(),
    );
    machine.set_program_debug(loaded.debug.clone());
    machine.run_raw(
        &loaded.bytecode,
        &loaded.constants,
        &loaded.strings,
        loaded.static_slots,
    );
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
        missing.push(format!(
            "{} {} ({})",
            entry.package, entry.version, entry.filename
        ));
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

    let loaded = match load_archive_bytes(archive) {
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
    let mut dload_gate = None;
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
                Ok(dirs) => {
                    ffi_search_paths = dirs;
                    let pins: Vec<(String, String)> = lock
                        .entries
                        .iter()
                        .map(|e| (e.stem.clone(), e.sha256.clone()))
                        .collect();
                    dload_gate = Some(DloadGate::from_consumer(&pins));
                }
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

    let panicked =
        execute_archived_program(&loaded, Some(exe.as_path()), ffi_search_paths, dload_gate);
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
    use common::Instruction;

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

    #[test]
    fn archive_stack_grows_for_dense_recursive_call() {
        let seek = Byte::new(Instruction::Seek).with_operand_u32(25);
        let call = Byte::new(Instruction::CALL);
        assert_eq!(
            archive_operand_slots(&[seek, call]),
            machine::MAX_OPERAND_STACK_SLOTS
        );
        assert_eq!(
            archive_operand_slots(&[seek]),
            machine::DEFAULT_OPERAND_STACK_SLOTS
        );
    }

    #[test]
    fn execute_uses_persisted_operand_stack_slots() {
        let seek = Byte::new(Instruction::Seek).with_operand_u32(25);
        let call = Byte::new(Instruction::CALL);
        let loaded = LoadedArchive {
            bytecode: vec![seek, call, Byte::new(Instruction::HALT)],
            constants: vec![],
            strings: vec![],
            static_slots: 0,
            debug: ProgramDebug::default(),
            struct_layouts: vec![],
            operand_stack_slots: Some(512),
            stack_maps: Vec::new(),
        };
        assert_eq!(
            resolve_archive_operand_slots(loaded.operand_stack_slots, &loaded.bytecode),
            512
        );
        assert_eq!(
            resolve_archive_operand_slots(None, &loaded.bytecode),
            machine::MAX_OPERAND_STACK_SLOTS as u32
        );
    }

    #[test]
    fn load_persists_stack_maps() {
        use common::{ArchivedProgram, FrameStackMap, SlotMap, ARCHIVE_VERSION};
        use rkyv::rancor::Error;

        let maps = vec![FrameStackMap {
            entry_pc: 0,
            end_pc: 4,
            frame_slots: vec![0],
            safepoints: vec![SlotMap {
                pc: 0,
                slots: vec![0],
            }],
        }];
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
            stack_maps: maps.clone(),
        };
        let bytes = rkyv::to_bytes::<Error>(&program).unwrap();
        let loaded = load_archive_bytes(bytes.as_slice()).expect("load");
        assert_eq!(loaded.stack_maps, maps);
        assert_eq!(loaded.operand_stack_slots, Some(256));
    }
}
