//! CLI-level error codes: the ones no `.hy` corpus file can produce (#1875).

use std::process::{Command, Output};

use common::{ArchivedProgram, Byte, Instruction, pack_archive_version};

fn coil(args: &[&str]) -> Output {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    Command::new(bin).args(args).output().expect("spawn coil")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `coil compile` with no source file is E0903.
#[test]
fn compile_without_input_is_e0903() {
    let out = coil(&["compile"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("E0903"), "{}", stderr(&out));
}

/// A `.hyc` from an incompatible archive version is E0901 (#1961).
#[test]
fn run_of_other_archive_version_is_e0901() {
    let stale = ArchivedProgram {
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
    };
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&stale).unwrap();
    let dir = std::env::temp_dir().join(format!("coil-e0901-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let hyc = dir.join("stale.hyc");
    std::fs::write(&hyc, bytes.as_slice()).unwrap();
    let out = coil(&["run", hyc.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("E0901"), "{}", stderr(&out));
}

/// A reachable host call without its grant is E0414; the corpora run with
/// every grant, so only the CLI can show it.
#[test]
fn ungranted_read_is_e0414() {
    let dir = std::env::temp_dir().join(format!("coil-e0414-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("reads.hy");
    std::fs::write(&src, "use io::fs::exists;\n\nfn main() {\n    let _ = exists(\"cfg.toml\");\n}\n").unwrap();
    let mut args = vec!["compile".to_string(), src.to_string_lossy().into_owned(), "-o".into(), dir.join("reads.hyc").to_string_lossy().into_owned()];
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        args.push("--root".into());
        args.push(root.to_string_lossy().into_owned());
    }
    let out = coil(&args.iter().map(String::as_str).collect::<Vec<_>>());
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("E0414") && err.contains("--allow-read"), "{err}");
}
