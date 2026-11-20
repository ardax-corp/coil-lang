//! The code cache: `coil run` and `coil FILE` keep compiled machine code, and
//! a later run of the same MIR on this host loads it instead of compiling.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const HEADER: &str = "use io::sync::println;\nuse string::format;\n";

fn coil(cache: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_coil"));
    cmd.env("XDG_CACHE_HOME", cache).env("COIL_NATIVE_STATS", "1");
    cmd
}

fn with_roots(cmd: &mut Command) {
    for root in compiler::Pipeline::workspace_language_extra_roots() {
        cmd.arg("--root").arg(root);
    }
}

struct Project {
    dir: PathBuf,
    cache: PathBuf,
    archive: PathBuf,
}

impl Project {
    /// `src` compiled to an archive, with an empty cache.
    fn new(name: &str, src: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("coil-code-cache-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("app.coil");
        std::fs::write(&file, src).expect("write source");
        let (cache, archive) = (dir.join("cache"), dir.join("app.ccoil"));
        let mut cmd = coil(&cache);
        cmd.args(["compile", "--no-build-cache", "--fold-calls", "off", "-o"]).arg(&archive).arg(&file);
        with_roots(&mut cmd);
        let out = cmd.output().expect("spawn coil compile");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        Self { dir, cache, archive }
    }

    fn run(&self) -> Output {
        coil(&self.cache).arg("run").arg(&self.archive).output().expect("spawn coil run")
    }

    fn entries(&self) -> Vec<PathBuf> {
        let Ok(read) = std::fs::read_dir(self.cache.join("coil/code")) else { return Vec::new() };
        read.filter_map(|e| e.ok()).map(|e| e.path()).collect()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn hit(out: &Output) -> bool {
    String::from_utf8_lossy(&out.stderr).lines().any(|l| l == "native: code from the cache")
}

/// What a program shows, without the statistics.
fn shown(out: &Output) -> (String, String, Option<i32>) {
    let stderr = String::from_utf8_lossy(&out.stderr).lines().filter(|l| !l.starts_with("native:")).collect::<Vec<_>>().join("\n");
    (String::from_utf8_lossy(&out.stdout).into_owned(), stderr, out.status.code())
}

const WORK: &str = "#[inline(never)]\nfn work(int n) -> int { return n * 2; }\nfn main() { let _ = println(format(\"%i\", work(21))); }\n";

#[test]
fn a_second_run_loads_the_code_and_says_the_same() {
    let p = Project::new("hit", &format!("{HEADER}{WORK}"));
    let first = p.run();
    assert!(!hit(&first), "the first run compiles");
    assert_eq!(p.entries().len(), 1, "it keeps its code");
    let second = p.run();
    assert!(hit(&second), "{}", String::from_utf8_lossy(&second.stderr));
    assert_eq!(shown(&first), shown(&second));
    assert_eq!(String::from_utf8_lossy(&second.stdout), "42\n");
}

/// Sites, stack maps, pads and fibers all come from the image.
#[test]
fn cached_code_panics_collects_and_suspends_as_compiled_code_does() {
    let src = format!(
        "{HEADER}use gc::collect;\nclass Box {{\n    pub n: int,\n}}\n\
         gen fn squares(int n) {{ let i = 0; while i < n {{ yield i * i; i = i + 1; }} }}\n\
         #[inline(never)]\nfn hold(int n) -> int {{ let b = new Box(n); collect(); let c = new Box(n + 1); collect(); return b.n + c.n; }}\n\
         #[inline(never)]\nfn down(int n) -> int {{ if n == 0 {{ return 0; }} return down(n - 1); }}\n\
         #[inline(never)]\nfn last(Vec<int> xs, int i) -> int {{ defer {{ let _ = println(\"defer ran\"); }} return xs[i]; }}\n\
         fn main() {{\n  let t = 0;\n  for x in squares(5) {{ t = t + x; }}\n  let _ = println(format(\"%i %i %i\", t, hold(20), down(1000000)));\n  let _ = println(format(\"%i\", last([1, 2, 3], 3)));\n}}\n"
    );
    let p = Project::new("behaviour", &src);
    let first = p.run();
    let second = p.run();
    assert!(hit(&second), "{}", String::from_utf8_lossy(&second.stderr));
    assert_eq!(shown(&first), shown(&second));
    let (stdout, stderr, status) = shown(&second);
    assert!(stdout.contains("30 41 0") && stdout.contains("defer ran"), "{stdout}");
    assert!(stderr.contains("index out of bounds") && stderr.contains("app.coil:"), "{stderr}");
    assert_eq!(status, Some(1));
}

#[test]
fn a_damaged_entry_is_compiled_again() {
    let p = Project::new("damaged", &format!("{HEADER}{WORK}"));
    let _ = p.run();
    let entry = p.entries().pop().expect("an entry");
    let good = std::fs::read(&entry).unwrap();
    let mut flipped = good.clone();
    let at = flipped.len() / 2;
    flipped[at] ^= 0xff;
    for bad in [flipped, good[..good.len() / 2].to_vec(), Vec::new(), b"not a code image".to_vec()] {
        std::fs::write(&entry, &bad).unwrap();
        let out = p.run();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(String::from_utf8_lossy(&out.stdout), "42\n", "{stderr}");
        assert!(!hit(&out) && stderr.contains("does not load"), "{stderr}");
        assert!(hit(&p.run()), "the run replaced the entry");
    }
}

#[test]
fn no_build_cache_neither_reads_nor_writes() {
    let p = Project::new("off", &format!("{HEADER}{WORK}"));
    let run = |p: &Project| coil(&p.cache).args(["run", "--no-build-cache"]).arg(&p.archive).output().expect("spawn coil run");
    let out = run(&p);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "42\n");
    assert!(p.entries().is_empty(), "{:?}", p.entries());
    let _ = p.run();
    assert!(!hit(&run(&p)), "it does not read one either");
}

#[test]
fn another_program_gets_its_own_code() {
    let a = Project::new("a", &format!("{HEADER}{WORK}"));
    let b = Project::new("b", &format!("{HEADER}{}", WORK.replace("n * 2", "n * 3")));
    let _ = a.run();
    let shared = coil(&a.cache).arg("run").arg(&b.archive).output().expect("spawn coil run");
    assert!(!hit(&shared), "{}", String::from_utf8_lossy(&shared.stderr));
    assert_eq!(String::from_utf8_lossy(&shared.stdout), "63\n");
}

/// A `COIL_*` variable can change the code: another value is another entry.
#[test]
fn another_environment_gets_its_own_code() {
    let p = Project::new("env", &format!("{HEADER}{WORK}"));
    let _ = p.run();
    let out = coil(&p.cache).arg("run").arg(&p.archive).env("COIL_NATIVE_INLINE_ALLOC", "0").output().expect("spawn coil run");
    assert!(!hit(&out));
    assert_eq!(p.entries().len(), 2);
}

/// The interpreter makes no machine code, so it neither reads nor keeps any.
#[test]
fn the_interpreter_leaves_the_cache_alone() {
    let p = Project::new("mir", &format!("{HEADER}{WORK}"));
    let out = coil(&p.cache).args(["run", "--mir"]).arg(&p.archive).output().expect("spawn coil run");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "42\n");
    assert!(p.entries().is_empty());
}

/// `coil FILE` shares the cache: with the build cache warm it compiles nothing
/// at all, and a changed file gets new code.
#[test]
fn coil_file_loads_the_code_too() {
    let p = Project::new("file", &format!("{HEADER}{WORK}"));
    let file = p.dir.join("app.coil");
    let run = |p: &Project| {
        let mut cmd = coil(&p.cache);
        cmd.arg(&file).args(["--fold-calls", "off"]);
        with_roots(&mut cmd);
        cmd.output().expect("spawn coil")
    };
    assert!(!hit(&run(&p)));
    let again = run(&p);
    assert!(hit(&again), "{}", String::from_utf8_lossy(&again.stderr));
    assert_eq!(String::from_utf8_lossy(&again.stdout), "42\n");
    std::fs::write(&file, format!("{HEADER}{}", WORK.replace("n * 2", "n * 4"))).unwrap();
    let changed = run(&p);
    assert!(!hit(&changed));
    assert_eq!(String::from_utf8_lossy(&changed.stdout), "84\n");
}

/// Functions compile on several threads, and the code does not depend on how
/// many: the program is big enough to use them, and an entry's bytes are its
/// code.
#[test]
fn the_code_is_the_same_on_any_number_of_threads() {
    let mut src = HEADER.to_string();
    for i in 0..200 {
        src.push_str(&format!("#[inline(never)]\nfn f{i}(int n) -> int {{ if n > {i} {{ return n - {i}; }} return n + {i}; }}\n"));
    }
    let calls: Vec<String> = (0..200).map(|i| format!("    t = t + f{i}(t);\n")).collect();
    src.push_str(&format!("fn main() {{\n    let t = 1;\n{}    let _ = println(format(\"%i\", t));\n}}\n", calls.concat()));
    let p = Project::new("threads", &src);
    let image = |threads: &str| {
        let cache = p.dir.join(format!("cache-{threads}"));
        let out = coil(&cache).arg("run").arg(&p.archive).env("COIL_NATIVE_THREADS", threads).output().expect("spawn coil run");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let read = std::fs::read_dir(cache.join("coil/code")).expect("an entry").next().expect("an entry").unwrap();
        (std::fs::read(read.path()).unwrap(), out.stdout)
    };
    let one = image("1");
    assert_eq!(one, image("2"));
    assert_eq!(one, image("7"));
}
