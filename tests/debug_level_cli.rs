//! Debug info levels on `coil compile` (coil-lang#1491): `-g2` keeps byte
//! spans, `-g1` keeps function names and the compact line table, `-g0` keeps
//! neither. A panic from the `.hyc` prints what its level recorded.

use std::path::Path;
use std::process::Command;

const SRC: &str = "fn inner(int x) -> int {
    if x > 2 {
        panic(\"boom: x too large\");
    }
    return x;
}

fn outer(int x) -> int {
    return inner(x + 1) * 2;
}

fn main() {
    let v: Vec<int> = [1, 2, 3];
    let i = outer(v[2]);
}
";

fn coil(bin: &str) -> Command {
    let mut cmd = Command::new(bin);
    cmd.env("COIL_BACKTRACE", "1");
    cmd
}

fn with_roots(cmd: &mut Command) {
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
}

fn setup(name: &str) -> (String, std::path::PathBuf) {
    let bin = std::env::var("CARGO_BIN_EXE_coil")
        .expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let dir = std::env::temp_dir().join(format!("coil_debug_level_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("panic.hy"), SRC).unwrap();
    (bin, dir)
}

/// Compile `panic.hy` with `flags`; the archive's path.
fn compile(bin: &str, dir: &Path, tag: &str, flags: &[&str]) -> std::path::PathBuf {
    let hyc = dir.join(format!("panic_{tag}.hyc"));
    let mut cmd = coil(bin);
    cmd.current_dir(dir).arg("compile").args(flags);
    with_roots(&mut cmd);
    let out = cmd.arg("panic.hy").arg("-o").arg(&hyc).output().expect("coil compile");
    assert!(out.status.success(), "compile {flags:?}: {}", String::from_utf8_lossy(&out.stderr));
    hyc
}

fn run_archive(bin: &str, dir: &Path, hyc: &Path) -> String {
    let out = coil(bin).current_dir(dir).arg("run").arg(hyc).output().expect("coil run");
    assert!(!out.status.success(), "{}: expected a panic", hyc.display());
    String::from_utf8_lossy(&out.stderr).trim_end().to_string()
}

/// At `-O0` (no inlining) every frame is on the stack.
const FULL: &str = "panic: boom: x too large at panic.hy:3:8
  in inner at panic.hy:3:8
  in outer at panic.hy:9:11
  in main at panic.hy:14:12";

const BARE: &str = "panic: boom: x too large
  in <unknown>
  ... repeated 2 more times";

#[test]
fn hyc_panic_output_at_each_debug_level() {
    let (bin, dir) = setup("hyc");
    for opt in ["0", "2"] {
        let g2 = compile(&bin, &dir, &format!("O{opt}_g2"), &["-O", opt, "-g2"]);
        let g1 = compile(&bin, &dir, &format!("O{opt}_g1"), &["-O", opt, "-g1"]);
        let g0 = compile(&bin, &dir, &format!("O{opt}_g0"), &["-O", opt, "-g0"]);
        let default = compile(&bin, &dir, &format!("O{opt}"), &["-O", opt]);
        let (full, lines, bare) = (
            run_archive(&bin, &dir, &g2),
            run_archive(&bin, &dir, &g1),
            run_archive(&bin, &dir, &g0),
        );
        if opt == "0" {
            assert_eq!(full, FULL, "-O0 -g2");
            assert_eq!(bare, BARE, "-O0 -g0");
        } else {
            // `outer` is inlined: fewer frames, same shape.
            assert!(full.starts_with("panic: boom: x too large at panic.hy:3:8\n  in inner at panic.hy:3:8\n"), "{full}");
            assert!(full.contains("in main at panic.hy:"), "{full}");
            assert!(bare.starts_with("panic: boom: x too large\n  in <unknown>"), "{bare}");
            assert!(!bare.contains(" at "), "{bare}");
        }
        // `-g1` drops only the byte spans: the panic reads the same.
        assert_eq!(lines, full, "-O{opt} -g1");
        // `coil compile` defaults to `-g2`.
        let size = |p: &Path| std::fs::metadata(p).unwrap().len();
        assert_eq!(size(&default), size(&g2), "-O{opt}: default is -g2");
        assert!(size(&g0) < size(&g1) && size(&g1) < size(&g2), "-O{opt}: sizes {} {} {}", size(&g0), size(&g1), size(&g2));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn strip_debug_is_g0_and_size_defaults_to_g1() {
    let (bin, dir) = setup("alias");
    let stripped = compile(&bin, &dir, "strip", &["--strip-debug"]);
    let g0 = compile(&bin, &dir, "g0", &["-g0"]);
    assert_eq!(std::fs::read(&stripped).unwrap(), std::fs::read(&g0).unwrap());
    let os = compile(&bin, &dir, "Os", &["-Os"]);
    let os_g1 = compile(&bin, &dir, "Os_g1", &["-Os", "-g1"]);
    assert_eq!(std::fs::read(&os).unwrap(), std::fs::read(&os_g1).unwrap(), "-Os defaults to -g1");
    let out = run_archive(&bin, &dir, &os);
    assert!(out.contains("in inner at panic.hy:3:8"), "{out}");

    let mut cmd = coil(&bin);
    cmd.current_dir(&dir).args(["compile", "--strip-debug", "-g2", "panic.hy"]);
    let out = cmd.output().expect("coil compile");
    assert!(!out.status.success(), "--strip-debug with -g2 must be rejected");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn default_run_takes_a_debug_level() {
    let (bin, dir) = setup("run");
    for (flag, want) in [("-g2", FULL), ("-g1", FULL), ("-g0", BARE)] {
        let mut cmd = coil(&bin);
        cmd.current_dir(&dir).args(["-O", "0", flag]);
        with_roots(&mut cmd);
        let out = cmd.arg("panic.hy").output().expect("coil run");
        assert!(!out.status.success(), "{flag}: expected a panic");
        assert_eq!(String::from_utf8_lossy(&out.stderr).trim_end(), want, "{flag}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
