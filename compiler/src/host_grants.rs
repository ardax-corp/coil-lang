//! Host capability grants and `dload` integrity inputs, from CLI flags only.
//!
//! Capabilities are checked at typecheck and the VM does not
//! re-apply them (the compiled artifact is the grant). Native pins and
//! trusted stems feed the run-time `dload` gate.

use std::path::PathBuf;

/// Deny-by-default host capabilities (`dload`, attach, running programs,
/// env exit, and
/// read / write / net / env).
///
/// The default denies everything. `dload("c")` /
/// libc aliases stay denied even when listed in [`Self::allow_dload`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostGrants {
    /// `Stream.attach` at typecheck (`--allow-attach`).
    pub allow_attach: bool,
    /// Running a program (`process`).
    pub allow_exec: bool,
    /// `env::exit`.
    pub allow_exit: bool,
    /// FFI process-exec symbols (`system`, `execve`, …).
    pub allow_ffi_exec: bool,
    /// Open files for reading, inspect the file system (`--allow-read`).
    pub allow_read: bool,
    /// Open files for writing, create / remove / rename (`--allow-write`).
    pub allow_write: bool,
    /// Connect, listen or bind sockets (`--allow-net`).
    pub allow_net: bool,
    /// Environment variables and the working directory (`--allow-env`).
    pub allow_env: bool,
    /// Consumer `dload` stems (`--allow-dload`). Still need a pin or a
    /// trusted stem at run time. Lookup paths are not a grant.
    pub allow_dload: Vec<String>,
    /// Extra FFI library search dirs (`--ffi-search-path`). Lookup only.
    pub ffi_search_paths: Vec<PathBuf>,
    /// `(stem, sha256 hex)` a loaded library must match (`--dload-pin`).
    pub dload_pins: Vec<(String, String)>,
    /// Stems loaded without a hash check (`--dload-trusted`).
    pub dload_trusted: Vec<String>,
    /// Most per-type clones of generic code one compile makes
    /// (`--clone-budget`; `None` is [`crate::monomorphize::MAX_TOTAL_SPECIALIZATIONS`]).
    /// Past it a ground call takes the shared generic body (E0804). A
    /// compile limit, not a capability: [`Self::grant_all`] leaves it.
    pub clone_budget: Option<usize>,
    /// Limits for code the build runs at compile time (`--comptime-steps`,
    /// `--comptime-mem`). Not a capability: [`Self::grant_all`] leaves it.
    pub comptime: ComptimeLimits,
    /// What code the build runs at compile time may touch
    /// (`--comptime-read`, …). Separate from the `allow_*` grants, which are
    /// the program's: [`Self::grant_all`] leaves these too.
    pub comptime_io: ComptimeGrants,
    /// Where results of compile-time IO are kept and how they are used
    /// (`--comptime-cache`, `--comptime-offline`, `--comptime-refresh`).
    pub comptime_cache: crate::comptime_cache::CacheConfig,
}

/// One compile-time grant: for any module, or only for code in `module`
/// (`--comptime-read gen=api/`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scoped<T> {
    pub module: Option<String>,
    pub value: T,
}

impl<T> Scoped<T> {
    /// `MODULE=VALUE` scopes to `MODULE` and the modules under it
    /// (`MODULE::…`); anything else is for any module.
    /// `value` turns the text after `=` (or the whole text) into a `T`.
    pub fn parse(text: &str, value: impl FnOnce(&str) -> T) -> Self {
        match text.split_once('=') {
            Some((module, rest)) if is_module_path(module) => Self {
                module: Some(module.to_string()),
                value: value(rest),
            },
            _ => Self { module: None, value: value(text) },
        }
    }

    fn applies_to(&self, module: &str) -> bool {
        self.module.as_deref().is_none_or(|m| {
            module == m || module.strip_prefix(m).is_some_and(|rest| rest.starts_with("::"))
        })
    }
}

/// `a`, `a::b`: identifiers joined by `::`.
fn is_module_path(s: &str) -> bool {
    !s.is_empty()
        && s.split("::").all(|part| {
            let mut chars = part.chars();
            chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// The IO compile-time code may do (`--comptime-read PATH`,
/// `--comptime-write PATH`, `--comptime-env NAME`, `--comptime-net HOST`,
/// each optionally `MODULE=`-scoped). Paths are as given; [`Self::for_module`]
/// resolves them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComptimeGrants {
    pub read: Vec<Scoped<PathBuf>>,
    pub write: Vec<Scoped<PathBuf>>,
    pub env: Vec<Scoped<String>>,
    pub net: Vec<Scoped<String>>,
}

impl ComptimeGrants {
    pub fn is_empty(&self) -> bool {
        self.read.is_empty() && self.write.is_empty() && self.env.is_empty() && self.net.is_empty()
    }

    /// What code in `module` may do: the unscoped grants and those scoped
    /// to it, with paths made absolute (against the working directory) and
    /// symlinks resolved where they exist.
    pub fn for_module(&self, module: &str) -> crate::macros::ComptimeIo {
        fn paths(list: &[Scoped<PathBuf>], module: &str) -> Vec<PathBuf> {
            list.iter()
                .filter(|g| g.applies_to(module))
                .map(|g| crate::macros::absolute_path(&g.value))
                .collect()
        }
        fn names(list: &[Scoped<String>], module: &str) -> Vec<String> {
            list.iter().filter(|g| g.applies_to(module)).map(|g| g.value.clone()).collect()
        }
        crate::macros::ComptimeIo {
            read: paths(&self.read, module),
            write: paths(&self.write, module),
            env: names(&self.env, module),
            net: names(&self.net, module),
            offline: false,
            replay: false,
        }
    }

    /// Add `other`'s grants (duplicates ignored).
    pub fn extend(&mut self, other: ComptimeGrants) {
        fn add<T: PartialEq>(into: &mut Vec<T>, from: Vec<T>) {
            for g in from {
                if !into.contains(&g) {
                    into.push(g);
                }
            }
        }
        add(&mut self.read, other.read);
        add(&mut self.write, other.write);
        add(&mut self.env, other.env);
        add(&mut self.net, other.net);
    }
}

/// How far one compile-time evaluation (a macro call, and later a `const`
/// initializer or `comptime` expression) may run before it fails the build.
///
/// These are the explicit tier's limits: code the user asked to run at
/// compile time gets room to work, and running out is an error naming the
/// flag to raise. Automatic folding uses the fixed, much smaller silent tier
/// ([`Self::SILENT_CALL_STEPS`]) and gives up quietly instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComptimeLimits {
    /// Loop back-edges + calls (see `Machine::set_step_budget`).
    pub steps: u64,
    /// Heap bytes of live objects after a full collection.
    pub heap_bytes: u64,
    /// Wall-clock seconds one evaluation may take, checked at each IO call;
    /// one connect or read waits at most [`Self::NET_OP_SECS`] of it.
    pub timeout_secs: u64,
}

impl ComptimeLimits {
    /// Default `--comptime-steps`: seconds of work.
    pub const DEFAULT_STEPS: u64 = 1_000_000_000;
    /// Default `--comptime-mem`, in MiB.
    pub const DEFAULT_MEM_MIB: u64 = 1024;
    /// Default `--comptime-timeout`, in seconds.
    pub const DEFAULT_TIMEOUT_SECS: u64 = 300;
    /// Longest wait of one network connect or read.
    pub const NET_OP_SECS: u64 = 30;
    /// Silent tier: steps for one automatically folded call.
    pub const SILENT_CALL_STEPS: u64 = 1_000_000;
    /// Silent tier: steps for all automatic folding in one module.
    pub const SILENT_MODULE_STEPS: u64 = 20_000_000;
}

impl Default for ComptimeLimits {
    fn default() -> Self {
        Self {
            steps: Self::DEFAULT_STEPS,
            heap_bytes: Self::DEFAULT_MEM_MIB << 20,
            timeout_secs: Self::DEFAULT_TIMEOUT_SECS,
        }
    }
}

impl HostGrants {
    /// All capabilities denied; empty dload allow and search paths.
    pub fn deny_all() -> Self {
        Self::default()
    }

    /// The `--clone-budget` in effect.
    pub fn clone_budget(&self) -> usize {
        self.clone_budget.unwrap_or(crate::monomorphize::MAX_TOTAL_SPECIALIZATIONS)
    }

    /// The compile-time IO of code in `module`, with the cache's offline
    /// mode.
    pub fn comptime_io_for(&self, module: &str) -> crate::macros::ComptimeIo {
        crate::macros::ComptimeIo {
            offline: self.comptime_cache.offline || self.comptime_cache.replay,
            replay: self.comptime_cache.replay,
            ..self.comptime_io.for_module(module)
        }
    }

    /// Every capability except `dload` (`--allow-all`).
    pub fn grant_all(&mut self) {
        self.allow_attach = true;
        self.allow_exec = true;
        self.allow_exit = true;
        self.allow_ffi_exec = true;
        self.allow_read = true;
        self.allow_write = true;
        self.allow_net = true;
        self.allow_env = true;
    }

    /// Grant the capability named `name` (`read`, `net`, `exec`, …). False
    /// when there is no such capability.
    pub fn grant_named(&mut self, name: &str) -> bool {
        let flag = match name {
            "read" => &mut self.allow_read,
            "write" => &mut self.allow_write,
            "net" => &mut self.allow_net,
            "env" => &mut self.allow_env,
            "exec" => &mut self.allow_exec,
            "exit" => &mut self.allow_exit,
            "attach" => &mut self.allow_attach,
            "ffi-exec" => &mut self.allow_ffi_exec,
            _ => return false,
        };
        *flag = true;
        true
    }

    /// The capabilities checked against reachable host calls.
    pub fn caps(&self) -> common::Caps {
        use common::Caps;
        [
            (self.allow_read, Caps::READ),
            (self.allow_write, Caps::WRITE),
            (self.allow_net, Caps::NET),
            (self.allow_env, Caps::ENV),
            (self.allow_exec, Caps::EXEC),
            (self.allow_exit, Caps::EXIT),
            (self.allow_attach, Caps::ATTACH),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .fold(Caps::NONE, |acc, (_, c)| acc.union(c))
    }

    /// Append a consumer dload stem (duplicates ignored).
    pub fn grant_dload_allow(&mut self, stem: impl Into<String>) {
        let stem = stem.into();
        if !self.allow_dload.iter().any(|s| s == &stem) {
            self.allow_dload.push(stem);
        }
    }

    /// Compile-time `dload` check: libc aliases are never granted.
    pub fn allows_dload_stem(&self, stem: &str) -> bool {
        if common::is_libc_alias(stem) {
            return false;
        }
        let key = common::dload_request_stem(stem);
        self.allow_dload
            .iter()
            .any(|s| common::dload_request_stem(s) == key)
    }

    /// Append an FFI lookup directory (not a dload grant).
    pub fn add_ffi_search_path(&mut self, path: impl Into<PathBuf>) {
        let path = path.into();
        if !self.ffi_search_paths.iter().any(|p| p == &path) {
            self.ffi_search_paths.push(path);
        }
    }

    /// Pin `stem` to a library whose SHA-256 is `sha256` (hex).
    pub fn add_dload_pin(&mut self, stem: impl Into<String>, sha256: impl Into<String>) {
        let pin = (stem.into(), sha256.into());
        if !self.dload_pins.contains(&pin) {
            self.dload_pins.push(pin);
        }
    }

    /// Load `stem` without a hash check (duplicates ignored).
    pub fn add_dload_trusted(&mut self, stem: impl Into<String>) {
        let stem = stem.into();
        if !self.dload_trusted.iter().any(|s| s == &stem) {
            self.dload_trusted.push(stem);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("coil_grants_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("granted/sub")).unwrap();
        std::fs::write(dir.join("granted/sub/a.txt"), "a").unwrap();
        std::fs::write(dir.join("secret.txt"), "s").unwrap();
        dir
    }

    fn io_for(read: &Path) -> crate::macros::ComptimeIo {
        let grants = ComptimeGrants {
            read: vec![Scoped { module: None, value: read.to_path_buf() }],
            write: vec![Scoped { module: None, value: read.to_path_buf() }],
            env: vec![Scoped::parse("gen=TOKEN", |n| n.to_string())],
            net: vec![Scoped::parse("db.internal:3306", |n| n.to_string()), Scoped::parse("example.org", |n| n.to_string())],
        };
        grants.for_module("gen")
    }

    #[test]
    fn scoped_grants_parse_a_module_prefix_only() {
        let g = Scoped::parse("db_gen=db.internal:3306", |v| v.to_string());
        assert_eq!(g.module.as_deref(), Some("db_gen"));
        assert_eq!(g.value, "db.internal:3306");
        let g = Scoped::parse("a::b=x", |v| v.to_string());
        assert_eq!(g.module.as_deref(), Some("a::b"));
        // Not a module path before `=`: the whole text is the value.
        let g = Scoped::parse("./dir=odd", |v| v.to_string());
        assert_eq!(g.module, None);
        assert_eq!(g.value, "./dir=odd");
    }

    #[test]
    fn grants_apply_to_their_module_or_any() {
        let grants = ComptimeGrants {
            env: vec![Scoped::parse("gen=TOKEN", |n| n.to_string()), Scoped::parse("HOME", |n| n.to_string())],
            ..ComptimeGrants::default()
        };
        assert_eq!(grants.for_module("gen").env, ["TOKEN", "HOME"]);
        assert_eq!(grants.for_module("other").env, ["HOME"]);
    }

    #[test]
    fn a_scope_covers_the_modules_under_it() {
        let grants = ComptimeGrants {
            env: vec![Scoped::parse("gen=TOKEN", |n| n.to_string())],
            ..ComptimeGrants::default()
        };
        assert_eq!(grants.for_module("gen::schema").env, ["TOKEN"]);
        assert_eq!(grants.for_module("gen::schema::v2").env, ["TOKEN"]);
        assert!(grants.for_module("generate").env.is_empty(), "a name prefix is not a module prefix");
        assert!(grants.for_module("other::gen").env.is_empty());
    }

    #[test]
    fn reads_stay_under_the_granted_directory() {
        let dir = scratch("read");
        let io = io_for(&dir.join("granted"));
        let p = |rel: &str| dir.join(rel).to_string_lossy().into_owned();
        assert!(io.check_read(&p("granted/sub/a.txt")).is_ok());
        assert!(io.check_read(&p("granted/missing.txt")).is_ok());
        let err = io.check_read(&p("secret.txt")).unwrap_err();
        assert!(err.contains("--comptime-read"), "{err}");
        assert!(io.check_read(&p("granted/../secret.txt")).is_err());
        assert!(io.check_read(&p("granted/nope/../../secret.txt")).is_err());
        assert!(io.check_write(&p("granted/new/file.txt")).is_ok());
        assert!(io.check_write(&p("elsewhere.txt")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_does_not_lead_out() {
        let dir = scratch("link");
        std::os::unix::fs::symlink(dir.join("secret.txt"), dir.join("granted/link.txt")).unwrap();
        std::os::unix::fs::symlink(&dir, dir.join("granted/up")).unwrap();
        let io = io_for(&dir.join("granted"));
        assert!(io.check_read(&dir.join("granted/link.txt").to_string_lossy()).is_err());
        assert!(io.check_write(&dir.join("granted/up/new.txt").to_string_lossy()).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn env_names_and_hosts_must_be_listed() {
        let io = io_for(Path::new("/nonexistent-coil-grant"));
        assert!(io.check_env("TOKEN").is_ok());
        assert!(io.check_env("OTHER").unwrap_err().contains("--comptime-env OTHER"));
        assert!(io.check_net("db.internal", 3306).is_ok());
        assert!(io.check_net("db.internal", 5432).unwrap_err().contains("--comptime-net db.internal:5432"));
        assert!(io.check_net("EXAMPLE.org", 443).is_ok(), "a host grant covers any port");
        assert!(io.check_net("example.com", 443).is_err());
        let offline = crate::macros::ComptimeIo { offline: true, ..io.clone() };
        assert!(offline.check_net("example.org", 443).unwrap_err().contains("--comptime-offline"));
        assert!(offline.check_write("/nonexistent-coil-grant/out.txt").is_ok());
    }

    #[test]
    fn replay_refuses_what_the_cache_must_answer() {
        let mut grants = HostGrants::deny_all();
        grants.comptime_io = ComptimeGrants {
            write: vec![Scoped { module: None, value: PathBuf::from("/nonexistent-coil-grant") }],
            env: vec![Scoped::parse("TOKEN", |n| n.to_string())],
            ..ComptimeGrants::default()
        };
        grants.comptime_cache.replay = true;
        let io = grants.comptime_io_for("gen");
        assert!(io.offline && io.replay);
        assert!(io.check_env("TOKEN").unwrap_err().contains("build the project"));
        assert!(io.check_write("/nonexistent-coil-grant/out.txt").unwrap_err().contains("does not run compile-time code that writes"));
        assert!(io.check_read("/nonexistent-coil-grant/a.txt").is_err(), "reads still need a grant");
    }
}
