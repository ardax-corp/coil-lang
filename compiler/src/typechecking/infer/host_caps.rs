//! Compile-time host capability gates (`HostGrants`, default deny).

use std::ops::Range;

use parser::ast::{Expression, Output};
use reporting::ErrorCode;

use crate::typechecking::Checker;
use crate::typechecking::infer::unwrap_expr_wrappers;

impl Checker {
    /// Apply CLI / Pipeline grants for this typecheck (deny-all by default).
    pub(crate) fn set_host_grants(&mut self, grants: crate::HostGrants, extra_dload_stems: Vec<String>) {
        self.host_grants = grants;
        self.dload_host_stems = extra_dload_stems;
    }

    pub(super) fn gate_env_exec(&mut self, range: Range<usize>) {
        if self.host_grants.allow_exec {
            return;
        }
        let _ = self.error_with_help(
            ErrorCode::HostExecDenied,
            "`env::exec` requires `--allow-exec`".to_string(),
            range,
            Some("pass `--allow-exec` (or `Pipeline::grant_exec`)".to_string()),
        );
    }

    pub(super) fn gate_env_exit(&mut self, range: Range<usize>) {
        if self.host_grants.allow_exit {
            return;
        }
        let _ = self.error_with_help(
            ErrorCode::HostExitDenied,
            "`env::exit` requires `--allow-exit`".to_string(),
            range,
            Some("pass `--allow-exit` (or `Pipeline::grant_exit`)".to_string()),
        );
    }

    pub(super) fn gate_stream_attach(&mut self, range: Range<usize>) {
        if self.host_grants.allow_attach {
            return;
        }
        let _ = self.error_with_help(
            ErrorCode::HostAttachDenied,
            "`Stream.attach` requires `--allow-attach`".to_string(),
            range,
            Some("pass `--allow-attach` (or `Pipeline::grant_attach`)".to_string()),
        );
    }

    pub(super) fn gate_ffi_exec_symbol(&mut self, symbol: &str, bind_name: &str, range: Range<usize>) {
        if !common::is_ffi_exec_symbol(symbol) {
            return;
        }
        self.ffi_exec_names.insert(bind_name.to_string());
        if self.host_grants.allow_ffi_exec {
            return;
        }
        let _ = self.error_with_help(
            ErrorCode::HostFfiExecDenied,
            format!("FFI process-exec `{symbol}` requires `--allow-ffi-exec`"),
            range,
            Some("pass `--allow-ffi-exec` (or `Pipeline::grant_ffi_exec`)".to_string()),
        );
    }

    pub(super) fn gate_ffi_exec_call(&mut self, ident: &str, range: Range<usize>) {
        if self.host_grants.allow_ffi_exec || !self.ffi_exec_names.contains(ident) {
            return;
        }
        let _ = self.error_with_help(
            ErrorCode::HostFfiExecDenied,
            format!("call to FFI process-exec `{ident}` requires `--allow-ffi-exec`"),
            range,
            Some("pass `--allow-ffi-exec` (or `Pipeline::grant_ffi_exec`)".to_string()),
        );
    }

    pub(super) fn gate_dload_arg(&mut self, path: &Output, range: Range<usize>) {
        match const_string_expr(path) {
            None => {
                let _ = self.error_with_help(
                    ErrorCode::HostDloadNonConst,
                    "`dload` path must be a string literal".to_string(),
                    path.0.into_range(),
                    Some("non-const paths cannot be checked against `--allow-dload`".to_string()),
                );
            }
            Some(raw) => self.gate_dload_stem(&raw, range),
        }
    }

    pub(super) fn gate_dload_stem(&mut self, raw: &str, range: Range<usize>) {
        let stem = common::dload_request_stem(raw);
        if common::is_libc_alias(raw) || common::is_libc_alias(&stem) {
            let _ = self.error_with_help(
                ErrorCode::HostDloadDenied,
                format!("`dload(\"{raw}\")` is always denied (libc alias)"),
                range,
                Some("`dload(\"c\")` and libc aliases cannot be granted".to_string()),
            );
            return;
        }
        if self.dload_stem_granted(&stem) {
            return;
        }
        let _ = self.error_with_help(
            ErrorCode::HostDloadDenied,
            format!("`dload` of `{stem}` requires `--allow-dload {stem}`"),
            range,
            Some("pass `--allow-dload STEM` (still needs a lock hash or `trusted`)".to_string()),
        );
    }

    fn dload_stem_granted(&self, stem: &str) -> bool {
        if self.host_grants.allows_dload_stem(stem) {
            return true;
        }
        self.dload_host_stems.iter().any(|s| {
            !common::is_libc_alias(s) && common::dload_request_stem(s) == stem
        })
    }
}

pub(super) fn const_string_expr(expr: &Output<'_>) -> Option<String> {
    match unwrap_expr_wrappers(expr).1.as_ref() {
        Expression::String(s) => Some((*s).to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::typechecking::Checker;
    use parser::Pratt;
    use reporting::ErrorCode;

    fn check_codes(src: &str, grants: crate::HostGrants, extra: &[&str]) -> Vec<ErrorCode> {
        let ast = Pratt::default().parse(src).expect("parse");
        let mut c = Checker::new();
        c.set_host_grants(
            grants,
            extra.iter().map(|s| (*s).to_string()).collect(),
        );
        let _ = c.check_program(&ast);
        c.take_messages()
            .into_iter()
            .filter_map(|m| m.code())
            .collect()
    }

    #[test]
    fn exec_without_grant_is_error() {
        let src = r#"
use env::{exec};
fn main() {
    let args: Vec<string> = [];
    let _ = exec("true", args);
}
"#;
        let codes = check_codes(src, crate::HostGrants::deny_all(), &[]);
        assert!(codes.contains(&ErrorCode::HostExecDenied), "{codes:?}");
    }

    #[test]
    fn exec_with_grant_typechecks() {
        let src = r#"
use env::{exec};
fn main() {
    let args: Vec<string> = [];
    let _ = exec("true", args);
}
"#;
        let mut g = crate::HostGrants::deny_all();
        g.allow_exec = true;
        let codes = check_codes(src, g, &[]);
        assert!(codes.is_empty(), "{codes:?}");
    }

    #[test]
    fn exit_without_grant_is_error() {
        let src = r#"
use env::{exit};
fn main() { exit(0); }
"#;
        let codes = check_codes(src, crate::HostGrants::deny_all(), &[]);
        assert!(codes.contains(&ErrorCode::HostExitDenied), "{codes:?}");
    }

    #[test]
    fn attach_without_grant_is_error() {
        let src = r#"
use io::{stdout};
fn main() { let _ = stdout().attach(0, 0, 0, 0, 0); }
"#;
        let codes = check_codes(src, crate::HostGrants::deny_all(), &[]);
        assert!(codes.contains(&ErrorCode::HostAttachDenied), "{codes:?}");
    }

    #[test]
    fn exit_with_grant_typechecks() {
        let src = r#"
use env::{exit};
fn main() { exit(0); }
"#;
        let mut g = crate::HostGrants::deny_all();
        g.allow_exit = true;
        let codes = check_codes(src, g, &[]);
        assert!(!codes.contains(&ErrorCode::HostExitDenied), "{codes:?}");
    }

    #[test]
    fn attach_with_grant_typechecks() {
        let src = r#"
use io::{stdout};
fn main() { let _ = stdout().attach(0, 0, 0, 0, 0); }
"#;
        let mut g = crate::HostGrants::deny_all();
        g.allow_attach = true;
        let codes = check_codes(src, g, &[]);
        assert!(!codes.contains(&ErrorCode::HostAttachDenied), "{codes:?}");
    }

    #[test]
    fn dload_ungranted_const_is_error() {
        let src = r#"
use ffi::{dload};
fn main() { let _ = dload("notalist"); }
"#;
        let codes = check_codes(src, crate::HostGrants::deny_all(), &[]);
        assert!(codes.contains(&ErrorCode::HostDloadDenied), "{codes:?}");
    }

    #[test]
    fn dload_granted_const_typechecks() {
        let src = r#"
use ffi::{dload};
fn main() { let _ = dload("plugin"); }
"#;
        let mut g = crate::HostGrants::deny_all();
        g.grant_dload_allow("plugin");
        let codes = check_codes(src, g, &[]);
        assert!(!codes.contains(&ErrorCode::HostDloadDenied), "{codes:?}");
        assert!(!codes.contains(&ErrorCode::HostDloadNonConst), "{codes:?}");
    }

    #[test]
    fn dload_libc_denied_even_when_flagged() {
        let src = r#"
use ffi::{dload};
fn main() { let _ = dload("c"); }
"#;
        let mut g = crate::HostGrants::deny_all();
        g.grant_dload_allow("c");
        let codes = check_codes(src, g, &["c"]);
        assert!(codes.contains(&ErrorCode::HostDloadDenied), "{codes:?}");
    }

    #[test]
    fn dload_nonconst_is_error() {
        let src = r#"
use ffi::{dload};
fn main() { let name = "plugin"; let _ = dload(name); }
"#;
        let mut g = crate::HostGrants::deny_all();
        g.grant_dload_allow("plugin");
        let codes = check_codes(src, g, &[]);
        assert!(codes.contains(&ErrorCode::HostDloadNonConst), "{codes:?}");
    }

    #[test]
    fn declare_system_without_ffi_exec_is_error() {
        let src = r#"
use ffi::{declare};
use ffi::types::{Int, Ptr};
fn main() { let _ = declare(0, "system", (Ptr,), Int); }
"#;
        let codes = check_codes(src, crate::HostGrants::deny_all(), &[]);
        assert!(codes.contains(&ErrorCode::HostFfiExecDenied), "{codes:?}");
    }

    #[test]
    fn declare_execve_without_ffi_exec_is_error() {
        let src = r#"
use ffi::{declare};
use ffi::types::{Int, Ptr};
fn main() { let _ = declare(0, "execve", (Ptr, Ptr, Ptr), Int); }
"#;
        let codes = check_codes(src, crate::HostGrants::deny_all(), &[]);
        assert!(codes.contains(&ErrorCode::HostFfiExecDenied), "{codes:?}");
    }

    #[test]
    fn declare_system_with_ffi_exec_typechecks() {
        let src = r#"
use ffi::{declare};
use ffi::types::{Int, Ptr};
fn main() { let _ = declare(0, "system", (Ptr,), Int); }
"#;
        let mut g = crate::HostGrants::deny_all();
        g.allow_ffi_exec = true;
        let codes = check_codes(src, g, &[]);
        assert!(!codes.contains(&ErrorCode::HostFfiExecDenied), "{codes:?}");
    }

    #[test]
    fn extern_system_call_without_ffi_exec_is_error() {
        let src = r#"
extern "plugin" {
    fn system() -> int;
}
fn main() { let _ = system(); }
"#;
        let mut g = crate::HostGrants::deny_all();
        g.grant_dload_allow("plugin");
        let codes = check_codes(src, g, &[]);
        assert!(codes.contains(&ErrorCode::HostFfiExecDenied), "{codes:?}");
        assert!(!codes.contains(&ErrorCode::HostDloadDenied), "{codes:?}");
    }

    #[test]
    fn extra_host_stem_allows_const_dload() {
        let src = r#"
use ffi::{dload};
fn main() { let _ = dload("plugin"); }
"#;
        let codes = check_codes(src, crate::HostGrants::deny_all(), &["plugin"]);
        assert!(!codes.contains(&ErrorCode::HostDloadDenied), "{codes:?}");
    }
}

