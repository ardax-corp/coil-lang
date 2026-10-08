//! E4: capabilities (`--allow-read`, …) checked over the host calls the
//! program can reach from `main` and its tests, so importing a module that
//! merely contains a gated call costs nothing.

use super::*;
use common::{Caps, DEBUG_FILE_UNKNOWN};

/// A reachable host call that needs a capability the build was not granted.
#[derive(Debug, Clone)]
pub struct CapViolation {
    /// The source file of the call, as codegen recorded it.
    pub file: String,
    pub range: std::ops::Range<usize>,
    pub needed: Caps,
    pub native: &'static str,
    /// Functions from an entry point down to the one making the call.
    pub chain: Vec<String>,
}

impl CapViolation {
    /// The call as the user wrote it (`env::exec`), or the native's name.
    fn shown(&self) -> String {
        match self.native {
            "env_exec" => "`env::exec`".into(),
            "env_exit" => "`env::exit`".into(),
            "stream_attach" => "`Stream.attach`".into(),
            "open" => "`io::open`".into(),
            other => {
                let shown = [("fs_", "io::fs::"), ("tcp_", "io::net::tcp::"), ("udp_", "io::net::udp::"), ("env_", "env::")]
                    .iter()
                    .find_map(|(prefix, module)| other.strip_prefix(prefix).map(|rest| format!("{module}{rest}")));
                format!("`{}`", shown.unwrap_or_else(|| other.to_string()))
            }
        }
    }

    pub fn message(&self) -> Message {
        let code = if self.needed.contains(Caps::EXEC) {
            ErrorCode::HostExecDenied
        } else if self.needed.contains(Caps::EXIT) {
            ErrorCode::HostExitDenied
        } else if self.needed.contains(Caps::ATTACH) {
            ErrorCode::HostAttachDenied
        } else {
            ErrorCode::HostCapDenied
        };
        let flags = self.needed.flags();
        let mut text = format!("{} requires `{flags}`", self.shown());
        if !self.chain.is_empty() {
            text.push_str(&format!(": reached from {}", self.chain.join(" → ")));
        }
        let mut message = Message::error(code, text, self.range.clone());
        let names = self.needed.names().join(", ");
        message.with_help(format!(
            "pass `{flags}` (or `-A` for everything); in a spool project add {names} to `[permissions]` in coil.toml"
        ));
        message
    }
}

/// `module::f`, `Owner::m`, `f$mono$…` as the user would name them.
fn shown_fn(name: &str) -> String {
    if let Some(rest) = name.strip_prefix("__static_init$") {
        let static_name = rest.split('$').next().unwrap_or(rest);
        return format!("the initializer of static `{static_name}`");
    }
    let base = name.split('$').next().unwrap_or(name);
    format!("`{base}`")
}

impl Compiler {
    /// When `native` needs a capability, tag the `HostInvoke` just pushed
    /// onto `self.bytecode` with the call's span so the check can find it.
    /// `open_mode` is `open`'s mode argument when it is a literal.
    pub(super) fn tag_gated_host_call(&mut self, native: &str, span: (usize, usize), open_mode: Option<&str>) {
        let Some(row) = common::HOST_NATIVES.iter().find(|n| n.name == native) else {
            return;
        };
        let caps = if row.name == "open" { common::open_mode_caps(open_mode) } else { row.caps };
        if caps.is_empty() {
            return;
        }
        let mut loc = self.loc_from_span(SimpleSpan::from(span.0..span.1));
        if loc.file == DEBUG_FILE_UNKNOWN {
            // No source file (a bare `Compiler`): still keyed by the span,
            // which debug info ignores without a file.
            loc.start_byte = span.0 as u32;
            loc.end_byte = span.1.max(span.0 + 1) as u32;
        }
        let Some(op @ IlOp::HostInvoke { .. }) = self.bytecode.il_mut().ops_slice_mut().last_mut() else {
            return;
        };
        op.set_loc(loc);
        let key = (loc.file, loc.start_byte, loc.end_byte);
        let entry = self.gated_host_calls.entry(key).or_insert(GatedHostCall {
            caps: Caps::NONE,
            native: row.name,
        });
        entry.caps = entry.caps.union(caps);
    }

    /// Report [`Self::capability_violations`] against this compile's grants
    /// as messages (a single-module [`Self::compile`]).
    pub(super) fn report_capability_violations(&mut self) {
        let granted = self.checker.host_grants().caps();
        for v in self.capability_violations(granted) {
            self.messages.push(v.message());
        }
    }

    /// The gated host calls reachable from `main`, the tests (when
    /// compiling them) and static initializers whose capabilities `granted`
    /// lacks. Without `main` or tests every function counts as reachable.
    pub fn capability_violations(&self, granted: Caps) -> Vec<CapViolation> {
        let missing = self
            .gated_host_calls
            .values()
            .fold(Caps::NONE, |acc, g| acc.union(g.caps))
            .without(granted);
        if missing.is_empty() {
            return Vec::new();
        }
        let mut roots = vec!["main".to_string()];
        let test_pcs: Vec<usize> = if self.include_tests {
            self.test_cases.iter().map(|&(_, pc)| pc as usize).collect()
        } else {
            Vec::new()
        };
        let has_entry = self.functions.contains_key("main") || !test_pcs.is_empty();
        if !has_entry {
            roots = self.functions.keys().cloned().collect();
        }
        let (parent, spans) = crate::il::reachable_functions(
            &self.bytecode,
            &self.functions,
            &self.fn_entry_labels,
            &roots,
            &test_pcs,
            &[self.static_init.ops(), self.ffi_init.ops()],
        );
        // Test bodies by their description: `test "parses"`.
        let tests: HashMap<&str, &str> = self
            .test_cases
            .iter()
            .filter_map(|(desc, pc)| {
                let name = spans.iter().find(|(_, span)| span.0 == *pc as usize)?.0;
                Some((name.as_str(), desc.as_str()))
            })
            .collect();
        let shown = |name: &str| match tests.get(name) {
            Some(desc) => format!("test \"{desc}\""),
            None => shown_fn(name),
        };
        let chain_to = |name: &str| {
            let mut chain = vec![shown(name)];
            let mut at = name.to_string();
            while let Some(Some(up)) = parent.get(&at) {
                if chain.len() > 32 {
                    break;
                }
                chain.push(shown(up));
                at = up.clone();
            }
            chain.reverse();
            chain
        };
        // Raw op ranges of every body, to find the function an op is in.
        let ops = self.bytecode.ops();
        let mut bodies: Vec<(usize, usize, &str)> = spans
            .iter()
            .map(|(n, &(s, e))| {
                let (rs, re) = crate::il::opt::emitting_range_to_raw(ops, s, e);
                (rs, re, n.as_str())
            })
            .collect();
        bodies.sort();
        let owner = |i: usize| -> Option<&str> {
            let at = bodies.partition_point(|&(s, _, _)| s <= i);
            bodies[..at].iter().rev().find(|&&(s, e, _)| s <= i && i < e).map(|&(_, _, n)| n)
        };
        let mut found: Vec<CapViolation> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut visit = |op: &IlOp, chain: Option<Vec<String>>| {
            let IlOp::HostInvoke { loc, .. } = op else { return };
            let key = (loc.file, loc.start_byte, loc.end_byte);
            let Some(gated) = self.gated_host_calls.get(&key) else { return };
            let needed = gated.caps.without(granted);
            if needed.is_empty() || !seen.insert(key) {
                return;
            }
            let Some(chain) = chain else { return };
            found.push(CapViolation {
                file: self.source_file_list.get(loc.file as usize).cloned().unwrap_or_default(),
                range: loc.start_byte as usize..loc.end_byte as usize,
                needed,
                native: gated.native,
                chain,
            });
        };
        for (i, op) in ops.iter().enumerate() {
            let chain = match owner(i) {
                Some(name) if parent.contains_key(name) => Some(chain_to(name)),
                Some(_) => None,
                None => Some(Vec::new()),
            };
            visit(op, chain);
        }
        for op in self.static_init.ops().iter().chain(self.ffi_init.ops()) {
            visit(op, Some(Vec::new()));
        }
        found.sort_by(|a, b| (&a.file, a.range.start).cmp(&(&b.file, b.range.start)));
        found
    }
}
