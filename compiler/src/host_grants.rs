//! Host capability grants for compile/typecheck. Independent of `coil.toml`.
//!
//! Spool may still parse Manifest `[env]` / `[ffi] allow` keys. The language
//! path (Pipeline, CLI) uses this struct and CLI flags at typecheck. The VM
//! does not re-apply these flags; the compiled artifact is the grant.

use std::path::PathBuf;

/// Deny-by-default host capabilities (`dload`, attach, env exec/exit, and
/// read / write / net / env).
///
/// Defaults match a missing `coil.toml` (everything denied). `dload("c")` /
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
    /// Consumer `dload` stems (`--allow-dload`). Still need lock hash or
    /// `trusted = true`. Lookup paths are not a grant.
    pub allow_dload: Vec<String>,
    /// Extra FFI library search dirs (`--ffi-search-path`). Lookup only.
    pub ffi_search_paths: Vec<PathBuf>,
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
}
