// `env::var` of an unset name is `NotFound`; `set_var` overwrites and
// `remove_var` unsets (removing twice is fine); a name with a NUL byte is
// `InvalidInput`; `cwd` names an existing directory; `args` starts with the
// program.
use tests::assert;
use env::{args, cwd, remove_var, set_var, var, EnvError};
use io::fs::{exists, is_dir};

fn var_or(string name, string fallback) -> string {
    return match var(name) {
        Result::Ok(v) => v,
        Result::Err(EnvError::NotFound) => fallback,
        Result::Err(_) => "error",
    };
}

test("an unset variable is NotFound") {
    assert!(var_or("COIL_HOST_SURELY_UNSET_9F2", "unset") == "unset");
}

test("set_var overwrites and remove_var unsets") {
    let name = "COIL_HOST_ENV_OVERWRITE";
    set_var(name, "one")?;
    assert!(var_or(name, "-") == "one");
    set_var(name, "two")?;
    assert!(var_or(name, "-") == "two");
    remove_var(name)?;
    assert!(var_or(name, "-") == "-");
    remove_var(name)?;
    assert!(var_or(name, "-") == "-");
}

test("an empty value is set, not missing") {
    let name = "COIL_HOST_ENV_EMPTY";
    set_var(name, "")?;
    assert!(var_or(name, "missing") == "");
    remove_var(name)?;
}

test("a NUL byte in a name is InvalidInput") {
    let bad = match set_var("COIL\u{0}BAD", "x") {
        Result::Err(EnvError::InvalidInput) => true,
        default => false,
    };
    assert!(bad);
}

test("cwd is an existing directory") {
    let dir = cwd()?;
    assert!(dir.len() > 0);
    assert!(exists(dir)? && is_dir(dir)?);
}

test("args holds at least the program") {
    let a = args()?;
    assert!(a.len() >= 1);
}
