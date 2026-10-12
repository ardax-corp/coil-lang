//! `extern` calls and `dload` / `declare` / `invoke`: the HIR lowering
//! emits them and they run at every opt level.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// Compiles `src` with no HIR refusal, then runs it in each mode.
fn check(bin: &str, tmp: &Path, src: &Path) {
    let why = Command::new(bin)
        .env("COIL_HIR_WHY", "1")
        .args([
            "compile",
            "--allow-dload",
            "sum",
            "--ffi-search-path",
        ])
        .arg(tmp)
        .arg(src)
        .arg("-o")
        .arg(tmp.join("main.hyc"))
        .output()
        .expect("coil compile");
    let why_err = String::from_utf8_lossy(&why.stderr);
    assert!(why.status.success(), "compile failed: {why_err}");
    assert!(
        !why_err.contains("hir fallback"),
        "FFI calls fell back: {why_err}"
    );

    for mode in [&[][..], &["-O", "0"][..]] {
        let run = Command::new(bin)
            .args(mode)
            .args(["--allow-dload", "sum", "--dload-trusted", "sum", "--ffi-search-path"])
            .arg(tmp)
            .arg(src)
            .output()
            .expect("coil run");
        assert!(
            run.status.success(),
            "{mode:?} failed: {}",
            String::from_utf8_lossy(&run.stderr)
        );
    }
}

#[test]
fn hir_lowers_and_runs_ffi_calls() {
    let bin = std::env::var("CARGO_BIN_EXE_coil")
        .expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let tmp = std::env::temp_dir().join(format!("coil_hir_ffi_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let so = tmp.join(machine::platform_shared_lib_filename("sum"));
    let mut cc = Command::new("cc");
    #[cfg(target_os = "macos")]
    {
        cc.arg("-dynamiclib");
    }
    #[cfg(not(target_os = "macos"))]
    {
        cc.arg("-shared").arg("-fPIC");
    }
    let status = cc
        .args([
            "-O2",
            "-o",
            so.to_str().unwrap(),
            manifest_dir.join("examples/sum.c").to_str().unwrap(),
        ])
        .status()
        .expect("cc");
    assert!(status.success(), "failed to build {}", so.display());

    // Nested calls put the library and function statics under other
    // operands; a `ptr` result moves through a local and a parameter.
    let src = tmp.join("main.hy");
    std::fs::write(
        &src,
        r#"
extern "sum" {
    fn sum(int a, int b) -> int;
    fn get_doubler() -> ptr;
}

fn keep(ptr p) -> ptr {
    return p;
}

fn main() {
    let p = keep(get_doubler());
    let q = p;
    if sum(sum(1, 2), sum(19, 20)) != 42 {
        panic "sum failed";
    }
    let n = 1 + sum(40, 1);
    if n != 42 {
        panic "nested sum failed";
    }
}
"#,
    )
    .unwrap();
    check(&bin, &tmp, &src);

    // Dynamic FFI: a signature's tags are constants, a function named in
    // the argument tuple is its code pointer.
    let dynamic = tmp.join("dynamic.hy");
    std::fs::write(
        &dynamic,
        r#"
use ffi::{declare, dload, invoke};
use ffi::types::{Callback, Int};

fn doubler(int x) -> int {
    return x * 2;
}

fn main() {
    let lib = match dload("sum") {
        Result::Ok(h) => h,
        Result::Err(e) => panic e.message,
    };
    let sum_id = match declare(lib, "sum", (Int, Int), Int) {
        Result::Ok(id) => id,
        Result::Err(e) => panic e.message,
    };
    let n = match invoke(lib, sum_id, (40, 2)) {
        Result::Ok(v) => v,
        Result::Err(e) => panic e.message,
    };
    if n != 42 {
        panic "invoke failed";
    }
    let cb_id = match declare(lib, "apply_cb", (Callback, Int), Int) {
        Result::Ok(id) => id,
        Result::Err(e) => panic e.message,
    };
    let m = match invoke(lib, cb_id, (doubler, 21)) {
        Result::Ok(v) => v,
        Result::Err(e) => panic e.message,
    };
    if 1 + m != 43 {
        panic "callback failed";
    }
}
"#,
    )
    .unwrap();
    check(&bin, &tmp, &dynamic);

    // A `Vec<T>` extern param is a pointer to the elements; a fixed
    // `[T; N]` passes the same way, as `[T]` params took it (#890).
    let arrays = tmp.join("arrays.hy");
    std::fs::write(
        &arrays,
        r#"
extern "sum" {
    fn sum_array(Vec<int> arr, int n) -> int;
}

fn main() {
    let v: Vec<int> = [1, 2, 3];
    if sum_array(v, 3) != 6 {
        panic "Vec argument failed";
    }
    let fixed: [int; 3] = [10, 20, 12];
    if sum_array(fixed, 3) != 42 {
        panic "fixed array argument failed";
    }
    if sum_array([4, 5], 2) != 9 {
        panic "literal argument failed";
    }
}
"#,
    )
    .unwrap();
    check(&bin, &tmp, &arrays);

    // `ConstBytes` lends a string's bytes in place and copies a `Vec<byte>`
    // without writing it back. The argument string is a temporary, so only
    // the VM's in-flight argument roots keep it alive while the callback
    // allocates enough to collect.
    let const_bytes = tmp.join("const_bytes.hy");
    std::fs::write(
        &const_bytes,
        r#"
use ffi::{declare, dload, invoke};
use ffi::types::{Callback, ConstBytes, Int};
use string::{format, to_bytes};

fn churn(int n) -> int {
    let i = 0;
    let keep = "";
    while i < 20000 {
        keep = format("%i-%i", i, n);
        i = i + 1;
    }
    return len(keep);
}

fn main() {
    let lib = match dload("sum") {
        Result::Ok(h) => h,
        Result::Err(e) => panic e.message,
    };
    let id = match declare(lib, "sum_bytes", (ConstBytes, Int), Int) {
        Result::Ok(id) => id,
        Result::Err(e) => panic e.message,
    };
    let s = format("%s%s", "AB", "C");
    let n = match invoke(lib, id, (s, 3)) {
        Result::Ok(v) => v,
        Result::Err(e) => panic e.message,
    };
    if n != 65 + 66 + 67 {
        panic "string view failed";
    }
    let v: Vec<byte> = to_bytes("ab");
    let m = match invoke(lib, id, (v, 2)) {
        Result::Ok(x) => x,
        Result::Err(e) => panic e.message,
    };
    if m != 97 + 98 || v[0] != (97 as byte) {
        panic "Vec<byte> view failed";
    }
    let cb_id = match declare(lib, "sum_bytes_after_cb", (ConstBytes, Int, Callback), Int) {
        Result::Ok(id) => id,
        Result::Err(e) => panic e.message,
    };
    let k = match invoke(lib, cb_id, (format("%s%s", "xy", "z"), 3, churn)) {
        Result::Ok(x) => x,
        Result::Err(e) => panic e.message,
    };
    if k != 120 + 121 + 122 {
        panic "view did not survive the callback";
    }
}
"#,
    )
    .unwrap();
    check(&bin, &tmp, &const_bytes);

    // A `static const` or `static let` in an `invoke` tuple is a value, not a
    // callback (#868).
    let globals = tmp.join("globals.hy");
    std::fs::write(
        &globals,
        r#"
use ffi::{declare, dload, invoke};
use ffi::types::Int;

static const FORTY = 40;
static let two: int = 2;

fn main() {
    let lib = match dload("sum") {
        Result::Ok(h) => h,
        Result::Err(e) => panic e.message,
    };
    let sum_id = match declare(lib, "sum", (Int, Int), Int) {
        Result::Ok(id) => id,
        Result::Err(e) => panic e.message,
    };
    let n = match invoke(lib, sum_id, (FORTY, two)) {
        Result::Ok(v) => v,
        Result::Err(e) => panic e.message,
    };
    if n != 42 {
        panic "global arguments failed";
    }
}
"#,
    )
    .unwrap();
    check(&bin, &tmp, &globals);
    let _ = std::fs::remove_dir_all(&tmp);
}
