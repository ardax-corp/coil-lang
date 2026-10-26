//! The comptime result cache (`--comptime-cache`, `--comptime-offline`,
//! `--comptime-refresh`) across separate builds: a const or macro that read
//! the environment keeps its cached result until refreshed, one that read a
//! file is computed again when the file changes, and offline builds use only
//! cached results; so does a `comptime { … }` block. Compiled macro programs
//! are kept in the user cache.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Project {
    bin: String,
    dir: PathBuf,
}

impl Project {
    fn new(name: &str) -> Self {
        let bin = std::env::var("CARGO_BIN_EXE_coil").expect("CARGO_BIN_EXE_coil (run via `cargo test -p coil`)");
        let dir = std::env::temp_dir().join(format!("coil_comptime_cache_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Project { bin, dir }
    }

    fn write(&self, name: &str, text: &str) {
        let path = self.dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// Build and run `main.hy` with `flags` and `CT_VALUE=value`: success,
    /// stdout, stderr.
    fn run(&self, value: &str, flags: &[&str]) -> (bool, String, String) {
        let mut cmd = Command::new(&self.bin);
        cmd.current_dir(&self.dir).env("CT_VALUE", value).env("COIL_CACHE_DIR", self.dir.join("user-cache"));
        cmd.args(["--root", ".", "--comptime-cache", "cache", "--comptime-env", "CT_VALUE"]);
        cmd.args(flags);
        for root in compiler::Pipeline::workspace_language_extra_roots() {
            cmd.arg("--root").arg(root);
        }
        let out = cmd.arg(Path::new("main.hy")).output().expect("coil run");
        let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        (out.status.success(), text(&out.stdout), text(&out.stderr))
    }
}

const PRINT: &str = "use io::{stdout, write};\nuse string::to_bytes;\n";

#[test]
fn an_environment_read_is_kept_until_refreshed() {
    let p = Project::new("env");
    p.write(
        "main.hy",
        &format!("use env::var;\n{PRINT}\nconst V = var(\"CT_VALUE\").ok() ?? \"none\";\n\nfn main() {{\n    write(stdout(), to_bytes(V));\n}}\n"),
    );
    assert_eq!(p.run("one", &[]).1, "one");
    let (ok, out, _) = p.run("two", &[]);
    assert!(ok);
    assert_eq!(out, "one", "the cached value");
    let (ok, out, err) = p.run("two", &["--comptime-refresh"]);
    assert!(ok, "{err}");
    assert_eq!(out, "two");
    assert!(err.contains("E0135") && err.contains("has a new value"), "{err}");
    let (_, out, err) = p.run("three", &["--comptime-offline"]);
    assert_eq!(out, "two", "offline uses the cached value: {err}");

    std::fs::remove_dir_all(p.dir.join("cache")).unwrap();
    let (ok, _, err) = p.run("three", &["--comptime-offline"]);
    assert!(!ok);
    assert!(err.contains("--comptime-offline") && err.contains("`CT_VALUE`"), "{err}");
}

#[test]
fn a_changed_file_is_read_again() {
    let p = Project::new("file");
    p.write(
        "main.hy",
        &format!(
            "use io::file::read_text;\nuse env::var;\n{PRINT}\nconst V = (read_text(\"data/v.txt\").ok() ?? \"\") + (var(\"CT_VALUE\").ok() ?? \"\");\n\nfn main() {{\n    write(stdout(), to_bytes(V));\n}}\n"
        ),
    );
    p.write("data/v.txt", "a");
    let read = ["--comptime-read", "data"];
    assert_eq!(p.run("1", &read).1, "a1");
    assert_eq!(p.run("2", &read).1, "a1", "unchanged file: cached");
    p.write("data/v.txt", "b");
    assert_eq!(p.run("2", &read).1, "b2", "changed file: computed again");
    // Offline, a miss may not read the environment.
    p.write("data/v.txt", "c");
    let (ok, _, err) = p.run("3", &["--comptime-read", "data", "--comptime-offline"]);
    assert!(!ok, "{err}");
}

#[test]
fn a_macro_result_is_cached() {
    let p = Project::new("macro");
    p.write(
        "gen.hy",
        "use macro::{Expr, Code, lit};\nuse env::var;\n\nmacro env_text(Expr name) -> Code {\n    return lit(var(name.src()).ok() ?? \"\");\n}\n",
    );
    p.write(
        "main.hy",
        &format!("use gen::env_text;\n{PRINT}\nfn main() {{\n    write(stdout(), to_bytes(env_text!(CT_VALUE)));\n}}\n"),
    );
    assert_eq!(p.run("one", &[]).1, "one");
    assert_eq!(p.run("two", &[]).1, "one", "the cached output");
    let (_, out, err) = p.run("two", &["--comptime-refresh"]);
    assert_eq!(out, "two");
    assert!(err.contains("E0135") && err.contains("has new output"), "{err}");
    assert_eq!(p.run("three", &["--comptime-offline"]).1, "two");
}

#[test]
fn a_cached_class_value_survives_new_types() {
    let p = Project::new("types");
    p.write("m/a.hy", "class A1 {\n    pub x: int,\n}\n");
    p.write(
        "m/b.hy",
        "use env::var;\n\nclass P {\n    pub name: string,\n    pub n: int,\n}\n\nconst X = new P(var(\"CT_VALUE\").ok() ?? \"none\", 7);\n",
    );
    p.write(
        "main.hy",
        &format!(
            "use m::a::A1;\nuse m::b::X;\nuse string::format;\n{PRINT}\nfn main() {{\n    let a = new A1(1);\n    write(stdout(), to_bytes(format(\"%s %i\", X.name, X.n + a.x)));\n}}\n"
        ),
    );
    assert_eq!(p.run("one", &[]).1, "one 8");
    // A class declared earlier moves `P`'s type id; the cached value follows.
    p.write("m/a.hy", "class A0 {\n    pub y: int,\n}\n\nclass A1 {\n    pub x: int,\n}\n");
    let (ok, out, err) = p.run("two", &[]);
    assert!(ok, "{err}");
    assert_eq!(out, "one 8");
}

#[test]
fn a_macro_program_from_the_user_cache_runs_the_same() {
    let p = Project::new("program");
    p.write(
        "gen.hy",
        "use macro::{Expr, Code, lit};\n\nmacro shout(Expr e) -> Code {\n    if e.src() == \"boom\" {\n        panic \"no booms\";\n    }\n    return lit(e.src() + \"!\");\n}\n",
    );
    p.write("main.hy", &format!("use gen::shout;\n{PRINT}\nfn main() {{\n    write(stdout(), to_bytes(shout!(hey)));\n}}\n"));
    let first = p.run("", &[]);
    let programs = p.dir.join("user-cache").join("macro-programs");
    assert!(std::fs::read_dir(&programs).is_ok_and(|mut d| d.next().is_some()), "no program kept");
    assert_eq!(first.1, "hey!");
    assert_eq!(p.run("", &[]), first, "from the cache");

    // A panic names the same place either way.
    p.write("main.hy", &format!("use gen::shout;\n{PRINT}\nfn main() {{\n    write(stdout(), to_bytes(shout!(boom)));\n}}\n"));
    let cold = p.run("", &[]);
    assert!(!cold.0 && cold.2.contains("no booms"), "{}", cold.2);
    assert_eq!(p.run("", &[]), cold);
}

#[test]
fn a_block_result_is_cached() {
    let p = Project::new("block");
    p.write(
        "gen.hy",
        "use macro::{Code, raw};\n\nfn value_fn(string v) -> Code {\n    return raw(\"fn value() -> string {\\n    return \\\"\" + v + \"\\\";\\n}\\n\");\n}\n",
    );
    let main = |extra: &str| {
        format!(
            "use env::var;\nuse gen::value_fn;\n{PRINT}\ncomptime {{\n    return value_fn(var(\"CT_VALUE\").ok() ?? \"none\");\n}}\n\nfn main() {{\n    write(stdout(), to_bytes(value(){extra}));\n}}\n"
        )
    };
    p.write("main.hy", &main(""));
    let (ok, out, err) = p.run("one", &[]);
    assert!(ok, "{err}");
    assert_eq!(out, "one");
    // The block does not depend on the rest of its file.
    p.write("main.hy", &main(" + \"!\""));
    assert_eq!(p.run("two", &[]).1, "one!", "the cached output");
    let (_, out, err) = p.run("two", &["--comptime-refresh"]);
    assert_eq!(out, "two!");
    assert!(err.contains("E0135") && err.contains("`comptime` block has new output"), "{err}");
    assert_eq!(p.run("three", &["--comptime-offline"]).1, "two!");
}

/// A server on 127.0.0.1 that answers each connection with `reply` (or,
/// with `None`, accepts and never answers); its port.
fn serve(reply: Option<&'static str>) -> u16 {
    use std::io::Write;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for conn in listener.incoming().flatten() {
            match reply {
                Some(text) => {
                    let mut conn = conn;
                    let _ = conn.write_all(text.as_bytes());
                }
                None => held.push(conn),
            }
        }
    });
    port
}

fn fetch_source(port: u16) -> String {
    format!(
        "use io::close;\nuse io::sync::read_to_end;\nuse io::net::tcp::connect;\nuse string::from_bytes;\n{PRINT}
fn fetch() -> Result<string, IoError> {{
    let s = connect(\"127.0.0.1\", {port})?;
    let text = match from_bytes(read_to_end(s)?) {{
        Result::Ok(t) => t,
        Result::Err(_) => raise IoError::InvalidInput,
    }};
    close(s)?;
    return Result::Ok(text);
}}

const GOT = fetch().ok() ?? \"failed\";

fn main() {{
    write(stdout(), to_bytes(GOT));
}}
"
    )
}

#[test]
fn a_network_read_is_granted_and_cached() {
    let p = Project::new("net");
    let port = serve(Some("from the network"));
    p.write("main.hy", &fetch_source(port));
    let (ok, _, err) = p.run("", &[]);
    assert!(!ok && err.contains("--comptime-net"), "{err}");
    let net = ["--comptime-net", "127.0.0.1"];
    let (ok, out, err) = p.run("", &net);
    assert!(ok, "{err}");
    assert_eq!(out, "from the network");
    assert_eq!(p.run("", &["--comptime-net", "127.0.0.1", "--comptime-offline"]).1, "from the network");

    let other = Project::new("net_other");
    other.write("main.hy", &fetch_source(port));
    let (ok, _, err) = other.run("", &["--comptime-net", "localhost"]);
    assert!(!ok && err.contains(&format!("--comptime-net 127.0.0.1:{port}")), "{err}");
    let (ok, _, err) = other.run("", &["--comptime-net", "127.0.0.1", "--comptime-offline"]);
    assert!(!ok && err.contains("--comptime-offline"), "{err}");
}

#[test]
fn a_silent_server_runs_into_the_timeout() {
    let p = Project::new("net_silent");
    p.write("main.hy", &fetch_source(serve(None)));
    let started = std::time::Instant::now();
    let (ok, _, err) = p.run("", &["--comptime-net", "127.0.0.1", "--comptime-timeout", "1"]);
    assert!(!ok && err.contains("ran out of time") && err.contains("--comptime-timeout"), "{err}");
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
}

