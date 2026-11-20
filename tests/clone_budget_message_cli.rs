//! The E0804 warning names the clone budget in effect (`--clone-budget`),
//! not the default one (#2854).

use std::path::PathBuf;
use std::process::Command;

#[test]
fn the_budget_warning_names_the_budget_in_effect() {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let dir: PathBuf = std::env::temp_dir().join(format!("coil-clone-budget-msg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = "fn id<T>(T x) -> T {\n    return x;\n}\n\nfn main() {\n    let _ = id(1).show();\n}\n";
    std::fs::write(dir.join("main.coil"), src).unwrap();
    let out = Command::new(&bin)
        .arg("compile")
        .arg(dir.join("main.coil"))
        .args(["--no-build-cache", "--clone-budget", "0", "-o"])
        .arg(dir.join("main.ccoil"))
        .output()
        .expect("spawn coil");
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(err.contains("[E0804] monomorphization budget (0 clones) reached at `id`"), "{err}");
}
