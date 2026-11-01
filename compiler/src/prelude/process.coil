// Child processes: run a program (no shell), pass it arguments, environment
// and a working directory, connect its standard streams, wait for it.
// Embedded in the compiler as module `process`; the natives are in
// `machine/src/process.rs`. Starting a program needs `--allow-exec`.
use prelude::process::{process_spawn, process_take_pipe, process_wait, process_try_wait, process_output, process_kill, process_signal, process_pid, process_release};
use io::{IoError, Stream};

/// Where a child's standard stream goes.
#[repr(int)]
enum Stdio {
    /// The child shares this process's stream (the default for `spawn`).
    Inherit = 0,
    /// A pipe this process reads or writes: `Child::stdin`, `stdout`, `stderr`.
    Pipe = 1,
    /// Nothing: reads see end of input, writes are dropped.
    Null = 2,
}

/// Why a process call failed.
#[derive(Show, Eq)]
enum ProcessError {
    /// The program could not be started: `IoError::NotFound` when there is
    /// no such program, `PermissionDenied` when it is not executable,
    /// `InvalidInput` for a NUL byte in the program, an argument or the
    /// environment.
    Spawn(IoError),
    /// Waiting for, reading from or signalling the child failed.
    Io(IoError),
}

/// A program to run: its arguments, environment, working directory and
/// standard streams. Build it with `Command::new` and the setters, then
/// `spawn`, `status` or `output`. One `Command` can start any number of
/// children.
///
/// # Examples
///
/// ```
/// use process::{Command};
/// # use tests::assert;
/// let cmd = Command::new("echo");
/// cmd.arg("hello");
/// let out = cmd.output();
/// let text = match out {
///     Result::Ok(o) => o.stdout,
///     Result::Err(_) => "",
/// };
/// assert!(text == "hello\n");
/// ```
class Command {
    program: string,
    argv: Vec<string>,
    envs: Vec<string>,
    removed: Vec<string>,
    clear: bool,
    dir: string,
    // -1: the default (`Inherit`, or `Null` stdin under `output`).
    in_mode: int,
    out_mode: int,
    err_mode: int,
}

/// A started child process.
///
/// Dropping a `Child` does not stop the process; call `kill` or `wait`.
class Child {
    __id: int,
}

/// What `output` collected: the exit code and everything the child wrote.
/// Bytes that are not UTF-8 become U+FFFD; read `Child::stdout` as a
/// `Stream` for raw bytes.
class Output {
    pub status: int,
    pub stdout: string,
    pub stderr: string,
}

fn __mode(int mode, int fallback) -> int {
    if mode < 0 {
        return fallback;
    }
    return mode;
}

// The flags word of `process_spawn`: two bits per stream, then bit 6 for
// an empty environment.
fn __flags(bool clear, int stdin_mode, int stdout_mode, int stderr_mode) -> int {
    let bits = stdin_mode | (stdout_mode << 2) | (stderr_mode << 4);
    if clear {
        bits = bits | 64;
    }
    return bits;
}

// `items` without the records (of `width` strings) whose first string is `key`.
fn __without(Vec<string> items, string key, int width) -> Vec<string> {
    let out: Vec<string> = Vec::new();
    let i = 0;
    while i < items.len() {
        if items[i] != key {
            let j = 0;
            while j < width {
                out.push(items[i + j]);
                j = j + 1;
            }
        }
        i = i + width;
    }
    return out;
}

fn __pid_or(int id) -> int {
    return match process_pid(id) {
        Result::Ok(pid) => pid,
        Result::Err(_) => -1,
    };
}

/// Runs `program` with `argv` (the arguments after the program name) and
/// waits for it. It shares this process's working directory, environment
/// and standard streams. Returns the exit code (`128 + signal` on POSIX
/// when a signal ended it).
///
/// # Errors
///
/// `Err(ProcessError::Spawn(e))` when the program could not be started.
///
/// # Examples
///
/// ```
/// use process::{exec};
/// # use tests::assert;
/// let code = exec("sh", ["-c", "exit 3"]).unwrap_or(-1);
/// assert!(code == 3);
/// ```
fn exec(string program, Vec<string> argv) -> Result<int, ProcessError> {
    let cmd = Command::new(program);
    cmd.args(argv);
    return cmd.status();
}

impl Command {
    /// A command that runs `program`, found on `PATH` unless it contains a
    /// path separator. No shell is involved: pass `sh` and `-c` for one.
    pub static fn new(string program) -> Command {
        let argv: Vec<string> = Vec::new();
        let envs: Vec<string> = Vec::new();
        let removed: Vec<string> = Vec::new();
        return new Command(program, argv, envs, removed, false, "", -1, -1, -1);
    }

    /// Adds one argument.
    pub fn arg(string value) {
        self.argv.push(value);
    }

    /// Adds each of `values` as an argument.
    pub fn args(Vec<string> values) {
        for v in values {
            self.argv.push(v);
        }
    }

    /// Sets environment variable `key` to `value` in the child.
    /// The last `env` or `env_remove` of a key wins.
    pub fn env(string key, string value) {
        self.removed = __without(self.removed, key, 1);
        self.envs = __without(self.envs, key, 2);
        self.envs.push(key);
        self.envs.push(value);
    }

    /// Removes environment variable `key` from the child's environment.
    pub fn env_remove(string key) {
        self.envs = __without(self.envs, key, 2);
        self.removed.push(key);
    }

    /// Starts the child with an empty environment (then only what `env`
    /// sets).
    pub fn env_clear() {
        self.clear = true;
    }

    /// Runs the child in directory `dir`.
    pub fn cwd(string dir) {
        self.dir = dir;
    }

    /// Where the child's standard input comes from.
    pub fn stdin(Stdio mode) {
        self.in_mode = mode;
    }

    /// Where the child's standard output goes.
    pub fn stdout(Stdio mode) {
        self.out_mode = mode;
    }

    /// Where the child's standard error goes.
    pub fn stderr(Stdio mode) {
        self.err_mode = mode;
    }

    /// Starts the program and returns at once. Streams not set otherwise
    /// are inherited.
    ///
    /// # Errors
    ///
    /// `Err(ProcessError::Spawn(e))` when the program could not be started.
    pub fn spawn() -> Result<Child, ProcessError> {
        return self.__start(__mode(self.in_mode, 0));
    }

    /// Starts the program, waits for it and returns its exit code.
    ///
    /// # Errors
    ///
    /// `Err(ProcessError::Spawn(e))` when the program could not be
    /// started, `Err(ProcessError::Io(e))` when waiting failed.
    pub fn status() -> Result<int, ProcessError> {
        let child = self.spawn()?;
        return child.wait();
    }

    /// Starts the program with its output and error piped, reads both to
    /// the end and waits for it. Standard input is `Null` unless set.
    ///
    /// # Errors
    ///
    /// `Err(ProcessError::Spawn(e))` when the program could not be
    /// started, `Err(ProcessError::Io(e))` when reading or waiting failed.
    pub fn output() -> Result<Output, ProcessError> {
        let bits = __flags(self.clear, __mode(self.in_mode, 2), 1, 1);
        let id = match process_spawn(
            self.program,
            self.argv,
            self.envs,
            self.removed,
            self.dir,
            bits,
        ) {
            Result::Ok(id) => id,
            Result::Err(e) => raise ProcessError::Spawn(e),
        };
        let child = new Child(id);
        return child.output();
    }

    fn __start(int stdin_mode) -> Result<Child, ProcessError> {
        let bits = __flags(
            self.clear,
            stdin_mode,
            __mode(self.out_mode, 0),
            __mode(self.err_mode, 0),
        );
        return match process_spawn(
            self.program,
            self.argv,
            self.envs,
            self.removed,
            self.dir,
            bits,
        ) {
            Result::Ok(id) => new Child(id),
            Result::Err(e) => raise ProcessError::Spawn(e),
        };
    }
}

impl Child {
    fn drop() {
        process_release(self.__id);
    }

    /// The child's OS process id.
    pub fn pid() -> int {
        return __pid_or(self.__id);
    }

    /// The write end of the child's standard input, when the command set it
    /// to `Stdio::Pipe`; `None` otherwise and after the first call. Close
    /// it (`io::close`) to send end of input.
    pub fn stdin() -> Option<Stream> {
        return process_take_pipe(self.__id, 0);
    }

    /// The read end of the child's standard output, when the command set it
    /// to `Stdio::Pipe`; `None` otherwise and after the first call.
    pub fn stdout() -> Option<Stream> {
        return process_take_pipe(self.__id, 1);
    }

    /// The read end of the child's standard error, when the command set it
    /// to `Stdio::Pipe`; `None` otherwise and after the first call.
    pub fn stderr() -> Option<Stream> {
        return process_take_pipe(self.__id, 2);
    }

    /// Waits for the child to end and returns its exit code (`128 + signal`
    /// on POSIX when a signal ended it). A standard input pipe not yet taken
    /// is closed first. Waiting again returns the same code.
    ///
    /// This blocks the thread; in a `task::scope`, run it in
    /// `task::blocking` to let other tasks go on.
    ///
    /// # Errors
    ///
    /// `Err(ProcessError::Io(e))` when waiting failed.
    pub fn wait() -> Result<int, ProcessError> {
        return match process_wait(self.__id) {
            Result::Ok(code) => code,
            Result::Err(e) => raise ProcessError::Io(e),
        };
    }

    /// The exit code if the child has ended, `None` if it is still running.
    ///
    /// # Errors
    ///
    /// `Err(ProcessError::Io(e))` when the check failed.
    pub fn try_wait() -> Result<Option<int>, ProcessError> {
        return match process_try_wait(self.__id) {
            Result::Ok(code) => code,
            Result::Err(e) => raise ProcessError::Io(e),
        };
    }

    /// Reads the output and error pipes not yet taken to the end, then
    /// waits. A stream that was not piped, or was taken, reads as `""`.
    ///
    /// # Errors
    ///
    /// `Err(ProcessError::Io(e))` when reading or waiting failed.
    pub fn output() -> Result<Output, ProcessError> {
        return match process_output(self.__id) {
            Result::Ok(r) => {
                let (code, out, err) = r;
                new Output(code, out, err)
            },
            Result::Err(e) => raise ProcessError::Io(e),
        };
    }

    /// Ends the child at once (`SIGKILL`; `TerminateProcess` on Windows).
    /// Killing a child that already ended does nothing.
    ///
    /// # Errors
    ///
    /// `Err(ProcessError::Io(e))` when the OS refused.
    pub fn kill() -> Result<unit, ProcessError> {
        return match process_kill(self.__id) {
            Result::Ok(_) => (),
            Result::Err(e) => raise ProcessError::Io(e),
        };
    }

    /// Sends POSIX signal `sig` (15 is `SIGTERM`, 2 `SIGINT`) to the child.
    ///
    /// # Errors
    ///
    /// `Err(ProcessError::Io(IoError::InvalidInput))` on Windows, for a
    /// child that was already waited for, and for an unknown signal.
    pub fn signal(int sig) -> Result<unit, ProcessError> {
        return match process_signal(self.__id, sig) {
            Result::Ok(_) => (),
            Result::Err(e) => raise ProcessError::Io(e),
        };
    }
}
