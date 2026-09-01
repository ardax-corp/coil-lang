//! Coverage for `compile_dissect` symbols, locals, and IL capture.

use compiler::{Pipeline, format_bytecode, format_il, matches_fn_pat};

#[test]
fn compile_dissect_fib_symbols_locals_and_il() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root");
    let path = workspace_root.join("examples/fib.hy");
    let mut pipeline = Pipeline::new();
    pipeline.bind_workspace_language_roots();
    let arts = pipeline
        .compile_dissect(path.to_str().unwrap(), true)
        .expect("fib.hy should compile via compile_dissect");

    let fib = arts
        .functions
        .iter()
        .find(|s| matches_fn_pat(&s.name, "fib"))
        .expect("fib symbol");
    assert!(
        fib.locals.iter().any(|(n, _)| n == "n"),
        "fib should expose param local n, locals={:?}",
        fib.locals
    );
    assert!(fib.entry_pc > 0, "entry PC should be past prologue");

    let bc = format_bytecode(&arts, Some("fib")).expect("format fib bytecode");
    assert!(
        bc.contains("CALL") || bc.contains("TailCall"),
        "expected recursive call in fib disasm, got:\n{bc}"
    );

    let snap = arts.il.as_ref().expect("capture_il=true should retain IL");
    let il = format_il(snap, Some("fib")).expect("format fib IL");
    assert!(
        il.contains(";; fn") && il.to_ascii_lowercase().contains("fib"),
        "expected fib IL section, got:\n{il}"
    );
}

#[test]
fn compile_dissect_without_il_leaves_snapshot_none() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root");
    let path = workspace_root.join("examples/fib.hy");
    let mut pipeline = Pipeline::new();
    pipeline.bind_workspace_language_roots();
    let arts = pipeline
        .compile_dissect(path.to_str().unwrap(), false)
        .expect("fib.hy should compile");
    assert!(arts.il.is_none());
    assert!(
        format_bytecode(&arts, None).is_ok(),
        "full bytecode dump should succeed"
    );
}
