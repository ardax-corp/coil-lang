//! Host capability grants for compile/run. Independent of `coil.toml`.
//!
//! Spool may still parse Manifest `[env]` / `[ffi] allow` keys. The language
//! path (Pipeline, VM wire, CLI) uses this struct and CLI flags only.

use std::path::PathBuf;

/// Deny-by-default host capabilities (`dload`, attach, env exec/exit).
///
/// Defaults match a missing `coil.toml` (everything denied). `dload("c")` /
/// libc aliases stay denied even when listed in [`Self::allow_dload`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostGrants {
    /// `Stream.attach` (per-Machine, on the dload gate).
    pub allow_attach: bool,
    /// `env::exec`.
    pub allow_exec: bool,
    /// `env::exit`.
    pub allow_exit: bool,
    /// FFI process-exec symbols (`system`, `execve`, …).
    pub allow_ffi_exec: bool,
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
