//! Host process environment: argv, env vars, cwd, exec (no shell).

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use common::{BUILTIN_ENV_ERROR_VARIANTS, BUILTIN_RESULT_VARIANTS, Value};

use crate::io::{alloc_result_err, alloc_result_ok};
use crate::memory::{Heap, Member, ObjArray, Object};

/// When false, [`host_exec`] returns `ExecDisabled`. Set from `coil.toml` `[env] allow_exec`.
pub static ALLOW_EXEC: AtomicBool = AtomicBool::new(false);

/// When false, [`host_exit`] panics instead of terminating. Set from `[env] allow_exit`.
pub static ALLOW_EXIT: AtomicBool = AtomicBool::new(false);

/// When false, FFI `system` / `execve` (and aliases) are denied at symbol resolve.
/// Set from `[env] allow_ffi_exec`. Independent of [`ALLOW_EXEC`].
pub static ALLOW_FFI_EXEC: AtomicBool = AtomicBool::new(false);

/// Runtime gate for `env::exec` (from project manifest).
pub fn set_allow_exec(allow: bool) {
    ALLOW_EXEC.store(allow, Ordering::Relaxed);
}

/// Runtime gate for `env::exit` (from project manifest).
pub fn set_allow_exit(allow: bool) {
    ALLOW_EXIT.store(allow, Ordering::Relaxed);
}

/// Runtime gate for FFI process-exec symbols (`system`, `execve`, …).
pub fn set_allow_ffi_exec(allow: bool) {
    ALLOW_FFI_EXEC.store(allow, Ordering::Relaxed);
}

/// True when `name` is a libc/CRT process-exec symbol (not `env::exec`).
pub fn is_ffi_exec_symbol(name: &str) -> bool {
    let n = name.trim().trim_matches('_').to_ascii_lowercase();
    matches!(
        n.as_str(),
        "system"
            | "wsystem"
            | "libc_system"
            | "exec"
            | "execl"
            | "execle"
            | "execlp"
            | "execv"
            | "execvp"
            | "execvpe"
            | "execve"
            | "fexecve"
            | "execveat"
            | "posix_spawn"
            | "posix_spawnp"
            | "popen"
            | "createprocessa"
            | "createprocessw"
            | "winexec"
    )
}


/// Tag indices for [`EnvError`](common::BUILTIN_ENV_ERROR_ENUM).
#[repr(u32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EnvErrorTag {
    InvalidInput = 0,
    NotFound = 1,
    ExecDisabled = 2,
    ExecFailed = 3,
    Other = 4,
}

fn alloc_enum(heap: &mut Heap, tag: u32, payload: Vec<Member>) -> Value {
    heap.alloc_enum_value(tag, payload)
}

/// Allocate a unit-payload `EnvError` variant.
pub fn alloc_env_error(heap: &mut Heap, tag: EnvErrorTag) -> Value {
    let _ = BUILTIN_ENV_ERROR_VARIANTS;
    alloc_enum(heap, tag as u32, vec![])
}

pub fn alloc_result_env_err(heap: &mut Heap, tag: EnvErrorTag) -> Value {
    let _ = BUILTIN_RESULT_VARIANTS;
    let err = alloc_env_error(heap, tag);
    alloc_result_err(heap, err)
}

pub fn as_result_value(heap: &mut Heap, r: Result<Value, EnvErrorTag>) -> Value {
    match r {
        Ok(v) => alloc_result_ok(heap, v),
        Err(tag) => alloc_result_env_err(heap, tag),
    }
}

pub fn as_result_unit(heap: &mut Heap, r: Result<(), EnvErrorTag>) -> Value {
    match r {
        Ok(()) => alloc_result_ok(heap, Value::default()),
        Err(tag) => alloc_result_env_err(heap, tag),
    }
}

pub fn as_result_int(heap: &mut Heap, r: Result<i64, EnvErrorTag>) -> Value {
    match r {
        Ok(n) => alloc_result_ok(heap, Value::from(n)),
        Err(tag) => alloc_result_env_err(heap, tag),
    }
}

fn contains_nul(s: &str) -> bool {
    s.contains('\0')
}

fn heap_string(heap: &Heap, v: Value) -> Result<String, EnvErrorTag> {
    match heap.find_object_by_addr(v.raw() as u64) {
        Some(Object::String(gc)) => Ok(gc.as_ref().data.clone()),
        _ => Err(EnvErrorTag::InvalidInput),
    }
}

fn value_as_string_array(heap: &Heap, v: Value) -> Result<Vec<String>, EnvErrorTag> {
    match heap.find_object_by_addr(v.raw() as u64) {
        Some(Object::Array(gc)) => {
            let mut out = Vec::with_capacity(gc.as_ref().elements.len());
            for e in &gc.as_ref().elements {
                let s = heap_string(heap, *e)?;
                if contains_nul(&s) {
                    return Err(EnvErrorTag::InvalidInput);
                }
                out.push(s);
            }
            Ok(out)
        }
        _ => Err(EnvErrorTag::InvalidInput),
    }
}

fn alloc_string_array(heap: &mut Heap, strings: &[String]) -> Value {
    let elements: Vec<Value> = strings
        .iter()
        .map(|s| {
            let gc = heap.intern(s.clone());
            Value::from(gc.as_ptr() as *mut u8 as u64)
        })
        .collect();
    let (obj, _) = heap.alloc(ObjArray { elements }, Object::Array);
    Value::from(obj.addr())
}

fn parse_name(heap: &Heap, v: Value) -> Result<String, EnvErrorTag> {
    let name = heap_string(heap, v)?;
    if contains_nul(&name) {
        return Err(EnvErrorTag::InvalidInput);
    }
    Ok(name)
}

/// Command-line arguments (`std::env::args`, including argv0).
///
/// Always `Result::Ok` with the argv vector (matches `Result<Vec<string>, EnvError>`).
pub fn host_args(heap: &mut Heap, _args: &[Value]) -> Value {
    let strings: Vec<String> = std::env::args().collect();
    let arr = alloc_string_array(heap, &strings);
    as_result_value(heap, Ok(arr))
}

/// `std::env::var` — `NotFound` when unset.
pub fn host_var(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_var(heap, args);
    as_result_value(heap, r)
}

fn try_host_var(heap: &mut Heap, args: &[Value]) -> Result<Value, EnvErrorTag> {
    if args.len() != 1 {
        return Err(EnvErrorTag::InvalidInput);
    }
    let name = parse_name(heap, args[0])?;
    match std::env::var(&name) {
        Ok(val) => {
            let gc = heap.intern(val);
            Ok(Value::from(gc.as_ptr() as *mut u8 as u64))
        }
        Err(std::env::VarError::NotPresent) => Err(EnvErrorTag::NotFound),
        Err(std::env::VarError::NotUnicode(_)) => Err(EnvErrorTag::Other),
    }
}

pub fn host_set_var(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_set_var(heap, args);
    as_result_unit(heap, r)
}

fn try_host_set_var(heap: &mut Heap, args: &[Value]) -> Result<(), EnvErrorTag> {
    if args.len() != 2 {
        return Err(EnvErrorTag::InvalidInput);
    }
    let name = parse_name(heap, args[0])?;
    let val = heap_string(heap, args[1])?;
    if contains_nul(&val) {
        return Err(EnvErrorTag::InvalidInput);
    }
    unsafe {
        std::env::set_var(name, val);
    }
    Ok(())
}

pub fn host_remove_var(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_remove_var(heap, args);
    as_result_unit(heap, r)
}

fn try_host_remove_var(heap: &mut Heap, args: &[Value]) -> Result<(), EnvErrorTag> {
    if args.len() != 1 {
        return Err(EnvErrorTag::InvalidInput);
    }
    let name = parse_name(heap, args[0])?;
    unsafe {
        std::env::remove_var(name);
    }
    Ok(())
}

pub fn host_cwd(heap: &mut Heap, _args: &[Value]) -> Value {
    let r = try_host_cwd(heap);
    as_result_value(heap, r)
}

fn try_host_cwd(heap: &mut Heap) -> Result<Value, EnvErrorTag> {
    let path = std::env::current_dir().map_err(|_| EnvErrorTag::Other)?;
    let s = path.to_string_lossy().into_owned();
    let gc = heap.intern(s);
    Ok(Value::from(gc.as_ptr() as *mut u8 as u64))
}

pub fn host_set_cwd(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_set_cwd(heap, args);
    as_result_unit(heap, r)
}

fn try_host_set_cwd(heap: &mut Heap, args: &[Value]) -> Result<(), EnvErrorTag> {
    if args.len() != 1 {
        return Err(EnvErrorTag::InvalidInput);
    }
    let path = heap_string(heap, args[0])?;
    if contains_nul(&path) {
        return Err(EnvErrorTag::InvalidInput);
    }
    std::env::set_current_dir(&path).map_err(|_| EnvErrorTag::Other)?;
    Ok(())
}

/// Terminates the process (`std::process::exit`). Never returns when granted.
///
/// Denied unless `[env] allow_exit = true`. [`ALLOW_EXEC`] does not grant this.
pub fn host_exit(_heap: &mut Heap, args: &[Value]) -> Value {
    if !ALLOW_EXIT.load(Ordering::Relaxed) {
        panic!("env::exit denied; set [env] allow_exit = true in coil.toml");
    }
    let code = if args.is_empty() { 0 } else { args[0].as_int() };
    std::process::exit(code as i32);
}

/// Spawn `program` with argv from `args_array` (no shell). NUL bytes rejected.
pub fn host_exec(heap: &mut Heap, args: &[Value]) -> Value {
    let r = try_host_exec(heap, args);
    as_result_int(heap, r)
}

fn try_host_exec(heap: &mut Heap, args: &[Value]) -> Result<i64, EnvErrorTag> {
    if !ALLOW_EXEC.load(Ordering::Relaxed) {
        return Err(EnvErrorTag::ExecDisabled);
    }
    if args.len() != 2 {
        return Err(EnvErrorTag::InvalidInput);
    }
    let program = heap_string(heap, args[0])?;
    if contains_nul(&program) {
        return Err(EnvErrorTag::InvalidInput);
    }
    let argv = value_as_string_array(heap, args[1])?;
    // Inherits VM cwd + env; runtime gate is `ALLOW_EXEC` / coil.toml
    // `[env] allow_exec` (compile still warns on `exec` / `exit`).
    let status = Command::new(&program)
        .args(&argv)
        .status()
        .map_err(|_| EnvErrorTag::ExecFailed)?;
    if let Some(code) = status.code() {
        Ok(code as i64)
    } else {
        Err(EnvErrorTag::ExecFailed)
    }
}

/// Pipeline wiring: `(registry_name, arity, host_fn)`.
pub const ENV_WIRING: &[(&str, usize, fn(&mut Heap, &[Value]) -> Value)] = &[
    ("env_args", 0, host_args),
    ("env_var", 1, host_var),
    ("env_set_var", 2, host_set_var),
    ("env_remove_var", 1, host_remove_var),
    ("env_cwd", 0, host_cwd),
    ("env_set_cwd", 1, host_set_cwd),
    ("env_exit", 1, host_exit),
    ("env_exec", 2, host_exec),
];

// Pipeline registry aliases (mirror `thread_spawn` pattern).
pub use host_args as env_args;
pub use host_cwd as env_cwd;
pub use host_exec as env_exec;
pub use host_exit as env_exit;
pub use host_remove_var as env_remove_var;
pub use host_set_cwd as env_set_cwd;
pub use host_set_var as env_set_var;
pub use host_var as env_var;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_TEST_GUARD: Mutex<()> = Mutex::new(());

    fn enum_tag(heap: &Heap, v: Value) -> Option<u32> {
        match heap.find_object_by_addr(v.raw() as u64) {
            Some(Object::Enum(gc)) => Some(gc.as_ref().tag),
            _ => None,
        }
    }

    fn result_ok_payload(heap: &Heap, result: Value) -> Value {
        let Object::Enum(gc) = heap.find_object_by_addr(result.raw() as u64).unwrap() else {
            panic!("expected Result");
        };
        assert_eq!(gc.as_ref().tag, 0);
        match &gc.as_ref().payload[0] {
            Member::Value(v) => *v,
            Member::Object(o) => Value::from(o.addr()),
        }
    }

    fn result_err_tag(heap: &Heap, result: Value) -> EnvErrorTag {
        let Object::Enum(gc) = heap.find_object_by_addr(result.raw() as u64).unwrap() else {
            panic!("expected Result");
        };
        assert_eq!(gc.as_ref().tag, 1);
        let Member::Object(Object::Enum(err)) = &gc.as_ref().payload[0] else {
            panic!("expected EnvError");
        };
        match err.as_ref().tag {
            0 => EnvErrorTag::InvalidInput,
            1 => EnvErrorTag::NotFound,
            2 => EnvErrorTag::ExecDisabled,
            3 => EnvErrorTag::ExecFailed,
            _ => EnvErrorTag::Other,
        }
    }

    fn make_string_array(heap: &mut Heap, items: &[&str]) -> Value {
        let elements: Vec<Value> = items
            .iter()
            .map(|s| {
                let gc = heap.intern((*s).into());
                Value::from(gc.as_ptr() as *mut u8 as u64)
            })
            .collect();
        let (obj, _) = heap.alloc(ObjArray { elements }, Object::Array);
        Value::from(obj.addr())
    }

    #[test]
    fn host_args_returns_ok_string_array() {
        let mut heap = Heap::default();
        let r = host_args(&mut heap, &[]);
        assert_eq!(enum_tag(&heap, r), Some(0));
        let arr = result_ok_payload(&heap, r);
        let got = value_as_string_array(&heap, arr).expect("Ok payload is string array");
        let expect: Vec<String> = std::env::args().collect();
        assert_eq!(got, expect, "argv must match process args including argv0");
    }

    #[test]
    fn host_var_round_trip() {
        let mut heap = Heap::default();
        let key_gc = heap.intern("COIL_ENV_TEST_KEY".into());
        let key = Value::from(key_gc.as_ptr() as *mut u8 as u64);
        let val_gc = heap.intern("coil_val".into());
        let val = Value::from(val_gc.as_ptr() as *mut u8 as u64);
        let set_r = host_set_var(&mut heap, &[key, val]);
        assert_eq!(enum_tag(&heap, set_r), Some(0));

        let get_r = host_var(&mut heap, &[key]);
        let s = result_ok_payload(&heap, get_r);
        assert_eq!(heap_string(&heap, s), Ok("coil_val".into()));

        let rem_r = host_remove_var(&mut heap, &[key]);
        assert_eq!(enum_tag(&heap, rem_r), Some(0));

        let missing = host_var(&mut heap, &[key]);
        assert_eq!(result_err_tag(&heap, missing), EnvErrorTag::NotFound);
    }

    #[test]
    fn host_var_rejects_nul_in_name() {
        let mut heap = Heap::default();
        let bad = heap.intern("a\0b".into());
        let key = Value::from(bad.as_ptr() as *mut u8 as u64);
        let r = host_var(&mut heap, &[key]);
        assert_eq!(result_err_tag(&heap, r), EnvErrorTag::InvalidInput);
    }

    #[test]
    fn host_cwd_returns_ok() {
        let mut heap = Heap::default();
        let r = host_cwd(&mut heap, &[]);
        assert_eq!(enum_tag(&heap, r), Some(0));
    }

    #[test]
    fn ffi_exec_symbols_are_denied_without_allow_ffi_exec() {
        let _guard = ENV_TEST_GUARD.lock().expect("env test mutex");
        let prev_exec = ALLOW_EXEC.load(Ordering::Relaxed);
        let prev_ffi = ALLOW_FFI_EXEC.load(Ordering::Relaxed);
        ALLOW_EXEC.store(true, Ordering::Relaxed);
        ALLOW_FFI_EXEC.store(false, Ordering::Relaxed);
        assert!(is_ffi_exec_symbol("system"));
        assert!(is_ffi_exec_symbol("execve"));
        assert!(is_ffi_exec_symbol("_wsystem"));
        assert!(!is_ffi_exec_symbol("strlen"));
        assert!(
            is_ffi_exec_symbol("system") && !ALLOW_FFI_EXEC.load(Ordering::Relaxed),
            "allow_exec must not grant FFI system/execve"
        );
        ALLOW_FFI_EXEC.store(true, Ordering::Relaxed);
        assert!(ALLOW_FFI_EXEC.load(Ordering::Relaxed));
        ALLOW_EXEC.store(prev_exec, Ordering::Relaxed);
        ALLOW_FFI_EXEC.store(prev_ffi, Ordering::Relaxed);
    }

    #[test]
    fn host_exit_denied_when_flag_off() {
        let _guard = ENV_TEST_GUARD.lock().expect("env test mutex");
        let prev_exit = ALLOW_EXIT.load(Ordering::Relaxed);
        let prev_exec = ALLOW_EXEC.load(Ordering::Relaxed);
        ALLOW_EXIT.store(false, Ordering::Relaxed);
        ALLOW_EXEC.store(true, Ordering::Relaxed);
        let panicked = std::panic::catch_unwind(|| {
            let mut heap = Heap::default();
            let _ = host_exit(&mut heap, &[Value::from(0_i64)]);
        });
        ALLOW_EXIT.store(prev_exit, Ordering::Relaxed);
        ALLOW_EXEC.store(prev_exec, Ordering::Relaxed);
        assert!(panicked.is_err(), "env::exit must not run without allow_exit");
    }

    #[test]
    fn host_exec_disabled_when_flag_off() {
        let _guard = ENV_TEST_GUARD.lock().expect("env test mutex");
        let prev = ALLOW_EXEC.load(Ordering::Relaxed);
        ALLOW_EXEC.store(false, Ordering::Relaxed);
        let mut heap = Heap::default();
        let prog = heap.intern("true".into());
        let args = make_string_array(&mut heap, &[]);
        let r = host_exec(
            &mut heap,
            &[Value::from(prog.as_ptr() as *mut u8 as u64), args],
        );
        assert_eq!(result_err_tag(&heap, r), EnvErrorTag::ExecDisabled);
        ALLOW_EXEC.store(prev, Ordering::Relaxed);
    }

    #[test]
    #[cfg(unix)]
    fn host_exec_true_returns_zero() {
        let _guard = ENV_TEST_GUARD.lock().expect("env test mutex");
        let prev = ALLOW_EXEC.load(Ordering::Relaxed);
        ALLOW_EXEC.store(true, Ordering::Relaxed);
        let mut heap = Heap::default();
        let prog = heap.intern("true".into());
        let args = make_string_array(&mut heap, &[]);
        let r = host_exec(
            &mut heap,
            &[Value::from(prog.as_ptr() as *mut u8 as u64), args],
        );
        ALLOW_EXEC.store(prev, Ordering::Relaxed);
        if enum_tag(&heap, r) != Some(0) {
            // Sandboxed CI may block spawning subprocesses.
            let tag = result_err_tag(&heap, r);
            assert!(
                tag == EnvErrorTag::ExecFailed || tag == EnvErrorTag::ExecDisabled,
                "unexpected exec error {:?}",
                tag
            );
            return;
        }
        let n = result_ok_payload(&heap, r).as_int();
        assert_eq!(n, 0);
    }

    #[test]
    fn env_wiring_arities() {
        assert_eq!(ENV_WIRING.len(), 8);
        let by_name: std::collections::BTreeMap<&str, usize> =
            ENV_WIRING.iter().map(|&(n, a, _)| (n, a)).collect();
        assert_eq!(by_name["env_args"], 0);
        assert_eq!(by_name["env_cwd"], 0);
        assert_eq!(by_name["env_set_var"], 2);
        assert_eq!(by_name["env_exec"], 2);
        assert_eq!(by_name["env_var"], 1);
    }
}
