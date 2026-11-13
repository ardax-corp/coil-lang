//! CLI-level error codes: the ones no `.coil` corpus file can produce (#1875).

use std::process::{Command, Output};

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

/// A reachable host call without its grant is E0414; the corpora run with
/// every grant, so only the CLI can show it.
#[test]
fn ungranted_read_is_e0414() {
    let dir = std::env::temp_dir().join(format!("coil-e0414-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("reads.coil");
    std::fs::write(&src, "use io::fs::exists;\n\nfn main() {\n    let _ = exists(\"cfg.toml\");\n}\n").unwrap();
    let mut args = vec!["compile".to_string(), src.to_string_lossy().into_owned(), "-o".into(), dir.join("reads.ccoil").to_string_lossy().into_owned()];
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
