//! `io::open` that runs out of file descriptors collects and tries once
//! more: streams nothing reaches give theirs back, so a loop that leaks
//! them keeps going under a low descriptor limit.
#![cfg(unix)]

use std::process::Command;

#[test]
fn open_retries_after_collecting_leaked_streams() {
    let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
    let dir = std::env::temp_dir().join(format!("coil_fd_retry_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("out.txt");
    let src = format!(
        "use io::{{open, IoError}};
use io::sync::{{println, write_all}};
use string::to_bytes;

fn leak() -> Result<unit, IoError> {{
    let f = open(\"{}\", \"w\")?;
    write_all(f, to_bytes(\"x\"))?;
    return Result::Ok(());
}}

fn main() {{
    let i = 0;
    while i < 2000 {{
        match leak() {{
            Result::Ok(_) => {{}},
            Result::Err(e) => {{
                let _ = println(\"failed at \" + i.show() + \": \" + e.show());
                return;
            }},
        }}
        i = i + 1;
    }}
    let _ = println(\"all opened\");
}}
",
        target.display()
    );
    let path = dir.join("leak.hy");
    std::fs::write(&path, src).unwrap();
    let mut roots = String::new();
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        roots.push_str(&format!(" --root '{}'", root.display()));
    }
    let script = format!("ulimit -n 64 && exec '{bin}' --allow-write{roots} '{}'", path.display());
    let out = Command::new("sh").arg("-c").arg(&script).output().expect("run coil");
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "{text}");
    assert!(text.contains("all opened"), "leaked streams must not exhaust the descriptors:\n{text}");
}
