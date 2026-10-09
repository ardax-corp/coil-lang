//! One mutant's work: compile the covering test files with the patched
//! source and run the covering cases. Runs in this process, or in a child
//! `coil-test` so a mutant that crashes or hangs the VM costs only itself.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use machine::reactor::Reactor;
use reporting::ReportConfig;

use super::Status;
use crate::runner::{Compiled, TestOptions, compile_test_file};

/// Hidden `coil-test` subcommand that runs one [`MutantJob`] from stdin.
pub const WORKER_ARG: &str = "__mutant-worker";

/// Everything a mutant run needs besides the test options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutantJob {
    /// Paths the patched text stands in for.
    pub overlay_keys: Vec<PathBuf>,
    pub patched: String,
    /// Test files with the cases to run in each: `(name, step budget)`.
    pub files: Vec<(PathBuf, Vec<(String, u64)>)>,
}

/// How mutants run.
#[derive(Debug, Clone)]
pub enum Isolation {
    /// In this process (tests; a crashing mutant takes the run down).
    InProcess,
    /// One `exe WORKER_ARG args…` child per mutant, killed after `wall`.
    Child {
        exe: PathBuf,
        /// `coil mutate` flags, forwarded so the child compiles the same way.
        args: Vec<String>,
        wall: Duration,
    },
}

/// A mutant's verdict and the case that decided it.
pub type Verdict = (Status, Option<String>);

/// Compile and run `job`: the first failing case kills it.
pub fn run_job(
    config: &ReportConfig,
    options: &TestOptions,
    reactor: &Arc<Reactor>,
    job: &MutantJob,
) -> Verdict {
    let overlays: Vec<(PathBuf, String)> = job
        .overlay_keys
        .iter()
        .map(|k| (k.clone(), job.patched.clone()))
        .collect();
    for (file, cases) in &job.files {
        let prepared = match compile_test_file(config, options, reactor, file, None, &overlays, None) {
            Compiled::Ready(prepared) => prepared,
            // The mutated source no longer type-checks (or compiles), or
            // its contract tests are gone (it gained an effect).
            Compiled::Decided(..) | Compiled::Nothing => return (Status::Unviable, None),
        };
        for (name, budget) in cases {
            for (_, entry) in prepared.cases.iter().filter(|(n, _)| n == name) {
                let case = prepared.case(*entry, false, Some(*budget));
                let report = reactor.run_test_here(prepared.ctx.clone(), case, Arc::default());
                let by = || Some(format!("{}: {name}", file.display()));
                if report.timed_out {
                    return (Status::TimedOut, by());
                }
                if !report.passed {
                    return (Status::Killed, by());
                }
            }
        }
    }
    (Status::Survived, None)
}

/// Run `job` in a child process.
pub fn run_in_child(exe: &Path, args: &[String], wall: Duration, job: &MutantJob) -> Verdict {
    let child = Command::new(exe)
        .arg(WORKER_ARG)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(e) => return (Status::Unviable, Some(format!("cannot start worker: {e}"))),
    };
    // The worker reads the whole job before it prints anything.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&encode(job));
    }
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        if let Some(s) = stdout.as_mut() {
            let _ = s.read_to_string(&mut out);
        }
        out
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if start.elapsed() >= wall => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => break None,
        }
    };
    let out = reader.join().unwrap_or_default();
    match status {
        Some(status) => classify_exit(status, &out, job),
        None => (
            Status::TimedOut,
            Some(format!("wall clock ({}s)", wall.as_secs())),
        ),
    }
}

/// A worker that reported a verdict is taken at its word; one that died
/// without (a VM crash, an abort) failed its cases, which kills the mutant.
pub fn classify_exit(status: ExitStatus, out: &str, job: &MutantJob) -> Verdict {
    if let Some(verdict) = parse_verdict(out) {
        return verdict;
    }
    let first = job
        .files
        .first()
        .and_then(|(f, cases)| cases.first().map(|(n, _)| format!("{}: {n}", f.display())));
    let how = match status.code() {
        Some(code) => format!("worker exited with {code}"),
        None => "worker crashed".to_string(),
    };
    (
        Status::Killed,
        Some(first.map_or(how.clone(), |t| format!("{t}, {how}"))),
    )
}

fn format_verdict((status, by): &Verdict) -> String {
    format!("{}\n{}\n", status_tag(*status), by.as_deref().unwrap_or(""))
}

fn parse_verdict(out: &str) -> Option<Verdict> {
    let mut lines = out.lines();
    let status = match lines.next()? {
        "killed" => Status::Killed,
        "timeout" => Status::TimedOut,
        "survived" => Status::Survived,
        "unviable" => Status::Unviable,
        _ => return None,
    };
    let by = lines.next().filter(|s| !s.is_empty()).map(str::to_string);
    Some((status, by))
}

fn status_tag(status: Status) -> &'static str {
    match status {
        Status::Killed => "killed",
        Status::TimedOut => "timeout",
        Status::Survived => "survived",
        Status::Unviable | Status::NoCoverage => "unviable",
    }
}

/// Length-prefixed fields: `<len>:<bytes>` each.
pub fn encode(job: &MutantJob) -> Vec<u8> {
    let mut out = Vec::new();
    let mut put = |s: &str| {
        out.extend_from_slice(format!("{}:", s.len()).as_bytes());
        out.extend_from_slice(s.as_bytes());
    };
    put(&job.overlay_keys.len().to_string());
    for k in &job.overlay_keys {
        put(&k.to_string_lossy());
    }
    put(&job.patched);
    put(&job.files.len().to_string());
    for (file, cases) in &job.files {
        put(&file.to_string_lossy());
        put(&cases.len().to_string());
        for (name, budget) in cases {
            put(name);
            put(&budget.to_string());
        }
    }
    out
}

pub fn decode(bytes: &[u8]) -> Option<MutantJob> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut rest = text;
    let mut take = || -> Option<&str> {
        let (len, tail) = rest.split_once(':')?;
        let len: usize = len.parse().ok()?;
        let field = tail.get(..len)?;
        rest = &tail[len..];
        Some(field)
    };
    let n_keys: usize = take()?.parse().ok()?;
    let mut overlay_keys = Vec::with_capacity(n_keys);
    for _ in 0..n_keys {
        overlay_keys.push(PathBuf::from(take()?));
    }
    let patched = take()?.to_string();
    let n_files: usize = take()?.parse().ok()?;
    let mut files = Vec::with_capacity(n_files);
    for _ in 0..n_files {
        let file = PathBuf::from(take()?);
        let n_cases: usize = take()?.parse().ok()?;
        let mut cases = Vec::with_capacity(n_cases);
        for _ in 0..n_cases {
            let name = take()?.to_string();
            let budget: u64 = take()?.parse().ok()?;
            cases.push((name, budget));
        }
        files.push((file, cases));
    }
    Some(MutantJob {
        overlay_keys,
        patched,
        files,
    })
}

/// `coil-test __mutant-worker <coil mutate flags>`: run the job on stdin and
/// print its verdict.
pub fn worker_main(config: ReportConfig, options: &TestOptions) -> ! {
    let mut input = Vec::new();
    if std::io::stdin().read_to_end(&mut input).is_err() {
        std::process::exit(2);
    }
    let Some(job) = decode(&input) else {
        eprintln!("coil-test: malformed mutant job");
        std::process::exit(2);
    };
    let reactor = Reactor::new(1);
    let verdict = run_job(&config, options, &reactor, &job);
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(format_verdict(&verdict).as_bytes());
    let _ = out.flush();
    // Skip joining pool threads a mutant may have left blocked.
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job() -> MutantJob {
        MutantJob {
            overlay_keys: vec!["src/a.hy".into(), "/abs/src/a.hy".into()],
            patched: "fn f() -> int {\n    return 1:2;\n}\n".into(),
            files: vec![
                (
                    "tests/a.hy".into(),
                    vec![("one: two".into(), 10), ("".into(), 0)],
                ),
                ("tests/b.hy".into(), vec![]),
            ],
        }
    }

    #[test]
    fn jobs_round_trip() {
        assert_eq!(decode(&encode(&job())), Some(job()));
        assert_eq!(decode(b"3:abc"), None);
        assert_eq!(decode(b""), None);
    }

    #[test]
    fn verdicts_round_trip_and_crashes_kill() {
        for v in [
            (Status::Killed, Some("tests/a.hy: one".to_string())),
            (Status::TimedOut, Some("t".to_string())),
            (Status::Survived, None),
            (Status::Unviable, None),
        ] {
            assert_eq!(parse_verdict(&format_verdict(&v)), Some(v));
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            let segv = ExitStatus::from_raw(11);
            assert_eq!(
                classify_exit(segv, "", &job()),
                (
                    Status::Killed,
                    Some("tests/a.hy: one: two, worker crashed".to_string())
                )
            );
            let reported = ExitStatus::from_raw(0);
            assert_eq!(
                classify_exit(reported, "survived\n\n", &job()).0,
                Status::Survived
            );
        }
    }
}
