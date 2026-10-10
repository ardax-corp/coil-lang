//! Host capability grants and `dload` integrity inputs, from CLI flags only.
//!
//! Capabilities are checked at typecheck and the VM does not
//! re-apply them (the compiled artifact is the grant). Native pins and
//! trusted stems feed the run-time `dload` gate.

use std::path::PathBuf;

/// Deny-by-default host capabilities (`dload`, attach, env exec/exit, and
/// read / write / net / env).
///
/// The default denies everything. `dload("c")` /
/// libc aliases stay denied even when listed in [`Self::allow_dload`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostGrants {
    /// `Stream.attach` at typecheck (`--allow-attach`).
    pub allow_attach: bool,
    /// `env::exec`.
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
}

impl HostGrants {
    /// All capabilities denied; empty dload allow and search paths.
    pub fn deny_all() -> Self {
        Self::default()
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
