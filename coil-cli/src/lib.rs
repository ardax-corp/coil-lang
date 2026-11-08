//! Shared CLI helpers for coil binaries (archive load, runtime execute, git-style dispatch).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, exit};

use common::{
    ARCHIVE_VERSION, ArchiveDecodeError, ArchivedArchivedProgram, NativeLock, ProgramDebug,
    archive_version_compatible, decode_archived_program, default_natives_root,
    embedded_archive_slice, format_archive_version, read_embedded_native_lock,
    read_package_trailer,
};
use machine::DloadGate;

/// Errors loading a `.ccoil` / embedded archive blob.
#[derive(Debug)]
pub enum LoadErr {
    Missing,
    Corrupt,
    Version(u32),
}

/// Owned archive payload restored by CLI and packaged execute.
pub struct LoadedArchive {
    pub startup: Vec<common::StartupOp>,
    pub main: Option<u32>,
    pub constants: Vec<u64>,
    pub strings: Vec<String>,
    pub static_slots: u32,
    pub debug: ProgramDebug,
    pub struct_layouts: Vec<common::CStructLayout>,
    /// Per-class field word kinds. Empty = every field unknown.
    pub class_word_kinds: Vec<common::ClassWordKinds>,
    pub static_word_kinds: Vec<u8>,
    /// The bodies' MIR.
    pub native_mir: Vec<u8>,
}

/// Deserialize an `ArchivedProgram` blob (from `.ccoil` or an embedded slice).
pub fn load_archive_bytes(buffer: &[u8]) -> Result<LoadedArchive, LoadErr> {
    let align = std::mem::align_of::<ArchivedArchivedProgram>();
    if (buffer.as_ptr() as usize).is_multiple_of(align) {
        decode_archive(buffer)
    } else {
        let mut aligned = rkyv::util::AlignedVec::<16>::with_capacity(buffer.len());
        aligned.extend_from_slice(buffer);
        decode_archive(&aligned)
    }
}

fn decode_archive(buffer: &[u8]) -> Result<LoadedArchive, LoadErr> {
    let program = decode_archived_program(buffer).map_err(|e| match e {
        ArchiveDecodeError::Corrupt => LoadErr::Corrupt,
        ArchiveDecodeError::Version(v) => LoadErr::Version(v),
    })?;
    let debug = program.debug_bundle();
    Ok(LoadedArchive {
        constants: program.constants,
        strings: program.strings,
        static_slots: program.static_slot_count,
        debug,
        struct_layouts: program.struct_layouts,
        class_word_kinds: program.class_word_kinds,
        static_word_kinds: program.static_word_kinds,
        native_mir: program.native_mir,
        startup: program.startup,
        main: program.main,
    })
}

/// Load a `.ccoil` file from disk.
pub fn try_load_archive(path: &str) -> Result<LoadedArchive, LoadErr> {
    let mut f = std::fs::File::open(path).map_err(|_| LoadErr::Missing)?;
    let mut buffer = Vec::with_capacity(1024);
    f.read_to_end(&mut buffer).map_err(|_| LoadErr::Corrupt)?;
    load_archive_bytes(&buffer)
}

pub fn execute_archived_program(
    loaded: &LoadedArchive,
    entry: Option<&Path>,
    ffi_search_paths: Vec<PathBuf>,
    dload_gate: Option<DloadGate>,
    host_natives: Vec<std::sync::Arc<dyn machine::NativeFn>>,
) -> bool {
    let base_dir = entry.and_then(|p| p.parent()).map(PathBuf::from);
    // Native code that covers every body (an AOT executable's) runs the
    // program on the driver, with no interpreter.
    let program = std::sync::Arc::new(machine::ThreadProgram {
        startup: std::sync::Arc::new(loaded.startup.clone()),
        main: loaded.main,
        constants: std::sync::Arc::new(loaded.constants.clone()),
        strings: std::sync::Arc::new(loaded.strings.clone()),
        static_slot_count: loaded.static_slots,
        debug: loaded.debug.clone(),
        class_word_kinds: machine::class_kind_table(&loaded.class_word_kinds),
        static_word_kinds: std::sync::Arc::new(loaded.static_word_kinds.clone()),
    });
    let start = machine::NativeStart {
        base_dir,
        ffi_search_paths,
        dload_gate,
        struct_layouts: loaded.struct_layouts.iter().map(machine::CStructLayout::from_archive).collect(),
        shared_print: None,
        host_natives,
    };
    if let Some(panicked) = machine::run_natively(program, start) {
        return panicked;
    }
    eprintln!("coil: this archive's code does not cover the program");
    true
}

/// Natives-cache directories that hold a lock entry's library (size checked).
///
/// A library may also sit beside the executable or in `lib/` there; one found
/// nowhere fails at its `dload`. Getting the files in place is up to the user.
fn native_cache_dirs(lock: &NativeLock) -> Vec<PathBuf> {
    let root = default_natives_root();
    let mut dirs = Vec::new();
    for entry in &lock.entries {
        let path = NativeLock::entry_cache_path(&root, entry);
        let dir = NativeLock::entry_cache_dir(&root, entry);
        let cached = std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() == entry.size);
        if cached && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// Run the package linked into this executable on the host natives its
/// object links (`machine::native::aot_host_natives`): `Some(panicked)`,
/// `None` when it cannot be read.
pub fn run_linked_package(package: &[u8]) -> Option<bool> {
    let exe = std::env::current_exe().ok()?;
    run_package(package, exe, machine::native::aot_host_natives(), |_| Ok(()))
}

/// If this process is a packaged JIT runner, run the program appended to it,
/// `Some(panicked)`. It makes its native code at start:
/// `prepare` takes the archive's native MIR section and installs it for the
/// run. Its error stops the program.
pub fn try_run_embedded_with(prepare: impl FnOnce(&[u8]) -> Result<(), String>) -> Option<bool> {
    let exe = std::env::current_exe().ok()?;
    let data = std::fs::read(&exe).ok()?;
    run_package(&data, exe, machine::standard_host_natives(), prepare)
}

/// Run the package that ends `data`; `exe` is the running executable.
fn run_package(
    data: &[u8],
    exe: PathBuf,
    host_natives: Vec<std::sync::Arc<dyn machine::NativeFn>>,
    prepare: impl FnOnce(&[u8]) -> Result<(), String>,
) -> Option<bool> {
    use machine::packaged_app_ffi_startup_check;

    let trailer = read_package_trailer(data)?;
    let archive = embedded_archive_slice(data, trailer)?;

    if !archive_version_compatible(trailer.archive_version, ARCHIVE_VERSION) {
        eprintln!(
            "embedded archive version {} does not match this runner ({}); rebuild with `coil package`",
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
            eprintln!("embedded archive is corrupt");
            exit(1);
        }
    };

    if let Err(msg) = packaged_app_ffi_startup_check(trailer.uses_ffi()) {
        eprintln!("error: {msg}");
        exit(1);
    }

    let mut ffi_search_paths = Vec::new();
    let mut dload_gate = None;
    match read_embedded_native_lock(data, trailer) {
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
            ffi_search_paths = native_cache_dirs(&lock);
            let pins: Vec<(String, String)> = lock
                .entries
                .iter()
                .map(|e| (e.stem.clone(), e.sha256.clone()))
                .collect();
            dload_gate = Some(DloadGate::from_consumer(&pins));
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

    if let Err(msg) = prepare(&loaded.native_mir) {
        eprintln!("error: {msg}");
        exit(1);
    }
    let panicked =
        execute_archived_program(&loaded, Some(exe.as_path()), ffi_search_paths, dload_gate, host_natives);
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
    dispatch_helper_as(sub, sub, &[])
}

/// `coil {sub}` served by `coil-{helper}`: re-exec it with `leading` before
/// the forwarded args (`coil mutate …` → `coil-test mutate …`).
pub fn dispatch_helper_as(sub: &str, helper: &str, leading: &[&str]) -> ! {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("coil: cannot resolve current executable: {e}");
            exit(1);
        }
    };
    let helper_name = format!("coil-{helper}");
    let helper = sibling_bin(&exe, &helper_name);
    if !helper.is_file() {
        eprintln!(
            "`coil {sub}` requires `{helper_name}` next to this binary\n\
             (looked for {})",
            helper.display()
        );
        exit(1);
    }
    let args: Vec<String> = leading
        .iter()
        .map(|a| (*a).to_string())
        .chain(std::env::args().skip(2))
        .collect();
    let mut command = Command::new(&helper);
    command.args(&args);
    // The helper refuses to run when built from other sources (#1598).
    coil_build_id::mark_helper(&mut command, &exe);
    let status = command.status();
    match status {
        Ok(s) if s.code().is_none() => {
            // Killed by a signal (a crash in native code, or the OOM killer):
            // the helper printed nothing about it, so say so here.
            let what = signal_name(&s);
            eprintln!("coil: `{helper_name}` was terminated by {what}");
            if helper_name == "coil-test" && !single_job(&args) {
                eprintln!(
                    "  note: tests run on several workers at once; a native library that is not \
                     thread-safe can crash this way (try `-j 1`)"
                );
            }
            exit(1)
        }
        Ok(s) => exit(s.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("coil: failed to exec `{}`: {e}", helper.display());
            exit(1);
        }
    }
}

/// Whether `coil test` args already ask for one worker (`-j 1`, `-j1`,
/// `--jobs 1`, `--jobs=1`), so suggesting `-j 1` would not help.
fn single_job(args: &[String]) -> bool {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let value = match a.as_str() {
            "-j" | "--jobs" => it.next().map(String::as_str),
            _ => a.strip_prefix("--jobs=").or_else(|| a.strip_prefix("-j")),
        };
        if value.map(str::trim) == Some("1") {
            return true;
        }
    }
    false
}

/// `signal 11 (SIGSEGV)` for an exit status a signal ended.
fn signal_name(status: &std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            let name = match sig {
                4 => "SIGILL",
                6 => "SIGABRT",
                7 => "SIGBUS",
                8 => "SIGFPE",
                9 => "SIGKILL",
                11 => "SIGSEGV",
                15 => "SIGTERM",
                _ => return format!("signal {sig}"),
            };
            return format!("signal {sig} ({name})");
        }
    }
    let _ = status;
    "a signal".to_string()
}

/// Resolve the default package runner template (`coil-embed` beside this binary).
pub fn resolve_default_runner() -> Result<PathBuf, String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("cannot resolve current executable: {e}"))?;
    let embed = sibling_bin(&exe, "coil-embed");
    if embed.is_file() {
        return Ok(embed);
    }
    Err(format!(
        "`coil-embed` not found next to {}; install it beside `coil` or pass `--runner PATH`",
        exe.display()
    ))
}

/// Marker `coil-embed` keeps in its image so `coil package` can tell a real
/// runner from any other binary. Stored reversed here so this crate's own
/// rodata never contains it: only `coil-embed` holds it forwards.
const RUNNER_MARKER_REVERSED: &[u8] = b"1v:rennur-debme-lioc";

/// Whether `bytes` is a `coil-embed` image (it carries [`RUNNER_MARKER_REVERSED`] forwards).
pub fn is_coil_embed_runner(bytes: &[u8]) -> bool {
    let marker: Vec<u8> = RUNNER_MARKER_REVERSED.iter().rev().copied().collect();
    bytes.windows(marker.len()).any(|w| w == marker.as_slice())
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

    #[test]
    fn single_job_spots_every_spelling() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for one in [&["-j", "1"][..], &["-j1"], &["--jobs", "1"], &["--jobs=1"], &["tests", "-j", "1"]] {
            assert!(single_job(&args(one)), "{one:?}");
        }
        for many in [&[][..], &["-j", "4"], &["--jobs=12"], &["tests"]] {
            assert!(!single_job(&args(many)), "{many:?}");
        }
    }

}
