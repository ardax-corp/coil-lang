// Doc stubs for the builtin `prelude::process` module (hover docs; never compiled).
//
// These are the natives the `process` module is written with. User code
// should not call them: use `process::Command`, `Child` and `exec` instead.

/// Starts a program and returns its child id. Low-level: use `Command::spawn`.
///
/// `env` holds `key, value` pairs to set, `env_remove` keys to remove, and
/// `flags` packs the stdio modes (two bits each for stdin, stdout and stderr:
/// `0` inherit, `1` pipe, `2` null) and bit 6 for an empty environment.
/// Needs `--allow-exec`.
fn process_spawn(string program, Vec<string> args, Vec<string> env, Vec<string> env_remove, string cwd, int flags) -> Result<int, IoError> {}

/// Takes the child's pipe `which` (`0` stdin, `1` stdout, `2` stderr) as a
/// stream; `None` when it was not piped or was taken. Low-level: use
/// `Child::stdin`, `Child::stdout`, `Child::stderr`.
fn process_take_pipe(int child, int which) -> Option<Stream> {}

/// Waits for the child to end and returns its exit code. Low-level: use `Child::wait`.
fn process_wait(int child) -> Result<int, IoError> {}

/// The child's exit code if it has ended. Low-level: use `Child::try_wait`.
fn process_try_wait(int child) -> Result<Option<int>, IoError> {}

/// Reads the pipes still held to the end and waits: `(code, stdout, stderr)`.
/// Low-level: use `Child::output`.
fn process_output(int child) -> Result<(int, string, string), IoError> {}

/// Ends the child at once. Low-level: use `Child::kill`.
fn process_kill(int child) -> Result<unit, IoError> {}

/// Sends a POSIX signal to the child. Low-level: use `Child::signal`.
fn process_signal(int child, int sig) -> Result<unit, IoError> {}

/// The child's OS process id. Low-level: use `Child::pid`.
fn process_pid(int child) -> Result<int, IoError> {}

/// Forgets the child (the `Child` finalizer); a running process keeps running.
fn process_release(int child) {}
