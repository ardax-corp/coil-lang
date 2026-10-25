// Doc stubs for the builtin `env` module (hover docs; never compiled).

/// The error of every fallible `env` function.
///
/// It implements `Show`, so `format("%v", e)` gives the variant name.
///
/// # Examples
///
/// ```
/// use env::{var, EnvError};
/// # use tests::assert;
/// let missing = match var("COIL_DOC_SURELY_UNSET") {
///     Result::Err(EnvError::NotFound) => true,
///     default => false,
/// };
/// assert!(missing);
/// ```
enum EnvError {
    /// An argument is malformed, for example a name, value or path that
    /// contains a NUL byte.
    InvalidInput,
    /// `var` asked for a variable that is not set.
    NotFound,
    /// `exec` could not start the program, or the program was ended by a
    /// signal and has no exit code.
    ExecFailed,
    /// Any other failure: the working directory cannot be read or changed,
    /// or a variable's value is not valid UTF-8.
    Other,
}

/// Returns the program's command-line arguments, starting with the program name.
///
/// Always `Ok`. Needs no permission.
///
/// # Examples
///
/// ```
/// use env::{args};
/// # use tests::assert;
/// let argv = match args() {
///     Result::Ok(a) => a,
///     Result::Err(_) => Vec::new(),
/// };
/// assert!(argv.len() >= 1);
/// ```
fn args() -> Result<Vec<string>, EnvError> {}

/// Returns the value of the environment variable `name`.
///
/// Returns `Err(EnvError::NotFound)` when it is not set and
/// `Err(EnvError::Other)` when its value is not valid UTF-8. Needs
/// `--allow-env`.
///
/// # Examples
///
/// ```
/// use env::{var, set_var};
/// # use tests::assert;
/// let _ = set_var("COIL_DOC_GREETING", "hello");
/// let v = match var("COIL_DOC_GREETING") {
///     Result::Ok(s) => s,
///     Result::Err(_) => "",
/// };
/// assert!(v == "hello");
/// ```
fn var(string name) -> Result<string, EnvError> {}

/// Sets the environment variable `name` to `value` for this process.
///
/// Programs started later with `exec` inherit it. A name or value with a
/// NUL byte is `Err(EnvError::InvalidInput)`. Needs `--allow-env`.
///
/// # Examples
///
/// ```
/// use env::{set_var, var, remove_var};
/// # use tests::assert;
/// assert!(match set_var("COIL_DOC_MODE", "test") { Result::Ok(_) => true, Result::Err(_) => false });
/// assert!(match var("COIL_DOC_MODE") { Result::Ok(s) => s == "test", Result::Err(_) => false });
/// let _ = remove_var("COIL_DOC_MODE");
/// ```
fn set_var(string name, string value) -> Result<unit, EnvError> {}

/// Removes the environment variable `name` from this process.
///
/// Removing a variable that is not set is not an error. Needs `--allow-env`.
///
/// # Examples
///
/// ```
/// use env::{set_var, remove_var, var, EnvError};
/// # use tests::assert;
/// let _ = set_var("COIL_DOC_TMP", "1");
/// let _ = remove_var("COIL_DOC_TMP");
/// let gone = match var("COIL_DOC_TMP") {
///     Result::Err(EnvError::NotFound) => true,
///     default => false,
/// };
/// assert!(gone);
/// ```
fn remove_var(string name) -> Result<unit, EnvError> {}

/// Returns the current working directory as an absolute path.
///
/// Returns `Err(EnvError::Other)` if it cannot be read (for example when it
/// was deleted). Needs no permission.
///
/// # Examples
///
/// ```
/// use env::{cwd};
/// # use tests::assert;
/// let dir = match cwd() {
///     Result::Ok(d) => d,
///     Result::Err(_) => "",
/// };
/// assert!(dir.len() > 0);
/// ```
fn cwd() -> Result<string, EnvError> {}

/// Changes the current working directory of the whole process.
///
/// Relative paths opened later, and programs started with `exec`, use the
/// new directory. Returns `Err(EnvError::Other)` if `path` does not exist
/// or is not an accessible directory. Needs `--allow-env`.
///
/// # Examples
///
/// ```
/// use env::{cwd, set_cwd};
/// # use tests::assert;
/// let before = match cwd() { Result::Ok(d) => d, Result::Err(_) => "/" };
/// assert!(match set_cwd("/tmp") { Result::Ok(_) => true, Result::Err(_) => false });
/// let _ = set_cwd(before);
/// ```
fn set_cwd(string path) -> Result<unit, EnvError> {}

/// Ends the process at once with exit status `code`.
///
/// Does not return. Needs `--allow-exit`; without the grant the program does
/// not compile (error `E0407`).
///
/// # Examples
///
/// ```no_run
/// use env::{exit};
/// let ok = true;
/// if ok {
///     exit(0);
/// }
/// ```
fn exit(int code) {}

/// Runs `program` with the arguments `argv` and waits for it to finish.
///
/// There is no shell: `program` is looked up on `PATH` (unless it is a
/// path) and `argv` excludes the program name. The child inherits this
/// process's working directory, environment and standard streams. Returns
/// `Ok(exit_code)`, or `Err(EnvError::ExecFailed)` if the program cannot be
/// started or was killed by a signal. Needs `--allow-exec`; without the
/// grant the program does not compile (error `E0406`).
///
/// # Examples
///
/// ```
/// use env::{exec};
/// # use tests::assert;
/// let argv: Vec<string> = Vec::from(["-c", "exit 3"]);
/// let code = match exec("sh", argv) {
///     Result::Ok(c) => c,
///     Result::Err(_) => -1,
/// };
/// assert!(code == 3);
/// ```
fn exec(string program, Vec<string> argv) -> Result<int, EnvError> {}
