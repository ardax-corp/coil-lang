//! Shared debug session engine for REPL and DAP front-ends.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use common::{ProgramDebug, byte_to_position};
use compiler::{DissectArtifacts, FnSym, HostGrants, Pipeline, matches_fn_pat};
use machine::{DebugController, Machine, StopReason};
use reporting::ReportConfig;

/// Resolved breakpoint for display / DAP.
#[derive(Clone, Debug)]
pub struct BreakpointInfo {
    pub id: usize,
    pub pc: usize,
    pub label: String,
}

/// Result of setting a line breakpoint (DAP `verified` semantics).
#[derive(Clone, Debug)]
pub struct LineBreakpointResult {
    pub line: u32,
    pub verified: bool,
    pub pc: Option<usize>,
}

/// One stack frame for backtrace / DAP `stackTrace`.
#[derive(Clone, Debug)]
pub struct StackFrameInfo {
    pub index: usize,
    pub name: String,
    pub pc: usize,
    pub path: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

/// Named local in the current frame.
#[derive(Clone, Debug)]
pub struct LocalInfo {
    pub name: String,
    pub slot: usize,
    pub value: String,
}

#[derive(Clone, Debug)]
struct Breakpoint {
    id: usize,
    pc: usize,
    label: String,
    /// Source path hint for DAP per-file breakpoint replacement.
    source: Option<String>,
}

/// Line → PCs index for one source file (path as stored in ProgramDebug).
#[derive(Default)]
pub struct LineIndex {
    by_file: HashMap<String, HashMap<u32, Vec<usize>>>,
    by_basename: HashMap<String, String>,
}

impl LineIndex {
    pub fn build(debug: &ProgramDebug, base_dir: Option<&Path>) -> Self {
        let mut idx = LineIndex::default();
        let mut texts: HashMap<u32, String> = HashMap::new();
        for (pc, loc) in debug.debug_locs.iter().enumerate() {
            if !loc.is_known() {
                continue;
            }
            let path = match debug.source_files.get(loc.file as usize) {
                Some(p) => p.clone(),
                None => continue,
            };
            let text = texts.entry(loc.file).or_insert_with(|| {
                let resolved = resolve_path(&path, base_dir);
                fs::read_to_string(resolved).unwrap_or_default()
            });
            if text.is_empty() {
                continue;
            }
            let line = byte_to_position(text, loc.start_byte as usize).line;
            idx.by_file
                .entry(path.clone())
                .or_default()
                .entry(line)
                .or_default()
                .push(pc);
            if let Some(base) = Path::new(&path).file_name().and_then(|s| s.to_str()) {
                idx.by_basename
                    .entry(base.to_string())
                    .or_insert_with(|| path.clone());
            }
        }
        idx
    }

    pub fn pcs_for_line(&self, file_hint: Option<&str>, line: u32, entry_file: &str) -> Vec<usize> {
        let key = if let Some(hint) = file_hint {
            if self.by_file.contains_key(hint) {
                hint.to_string()
            } else if let Some(full) = self.by_basename.get(hint) {
                full.clone()
            } else {
                self.by_file
                    .keys()
                    .find(|k| k.ends_with(hint) || Path::new(k).ends_with(hint))
                    .cloned()
                    .unwrap_or_else(|| entry_file.to_string())
            }
        } else {
            entry_file.to_string()
        };
        self.by_file
            .get(&key)
            .and_then(|m| m.get(&line))
            .cloned()
            .unwrap_or_default()
    }
}

pub fn resolve_path(path: &str, base_dir: Option<&Path>) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() || p.exists() {
        return p;
    }
    if let Some(base) = base_dir {
        let root = base.parent().unwrap_or(base);
        let from_root = root.join(path);
        if from_root.exists() {
            return from_root;
        }
        let from_base = base.join(path);
        if from_base.exists() {
            return from_base;
        }
    }
    p
}

/// In-memory debug session over a compiled program.
pub struct DebugSession {
    pub entry: String,
    pub artifacts: DissectArtifacts,
    static_slots: u32,
    line_index: LineIndex,
    machine: Machine<256>,
    breakpoints: Vec<Breakpoint>,
    next_bp_id: usize,
    started: bool,
    pub base_dir: PathBuf,
}

impl DebugSession {
    pub fn compile(
        config: ReportConfig,
        filename: &str,
        reporter: Box<dyn std::io::Write + Send>,
        grants: HostGrants,
    ) -> Result<Self, ()> {
        let mut pipeline = Pipeline::with_reporter(config, reporter);
        pipeline.set_host_grants(grants);
        let artifacts = match pipeline.compile_dissect(filename, false) {
            Ok(a) => a,
            Err(()) => {
                let _ = pipeline.finish_reporting();
                return Err(());
            }
        };
        let static_slots = pipeline.static_slot_count();
        let entry_path = PathBuf::from(filename);
        let base_dir = entry_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let line_index = LineIndex::build(&artifacts.debug, Some(&base_dir));

        let mut machine = Machine::<256>::default();
        let pins = pipeline.dload_native_pins();
        let trusted = pipeline.dload_trusted_stems();
        let structs = pipeline.archived_struct_layouts();
        let grants = pipeline.host_grants();
        let search = pipeline.ffi_search_path_bufs();
        machine::wire_vm_host(
            &mut machine,
            &machine::VmHostSpec {
                entry_path: Some(&entry_path),
                project_root: pipeline.project_root(),
                ffi_search_paths: &search,
                ffi_allow: &grants.allow_dload,
                native_pins: &pins,
                trusted_stems: &trusted,
                extra_dload_stems: pipeline.extra_dload_stems(),
                extra_dload_grants: pipeline.extra_dload_grants(),
                allow_exec: grants.allow_exec,
                allow_exit: grants.allow_exit,
                allow_ffi_exec: grants.allow_ffi_exec,
                allow_attach: grants.allow_attach,
                c_structs: &structs,
            },
        );
        machine::wire_thread_program(
            &mut machine,
            &artifacts.bytecode,
            &artifacts.constants,
            &artifacts.strings,
            pipeline.static_slot_count(),
            pipeline.program_debug(),
            pipeline.operand_stack_slots(),
        );
        machine.set_program_debug(artifacts.debug.clone());
        machine.attach_debug(DebugController::new());
        let _ = pipeline.finish_reporting();

        Ok(Self {
            entry: filename.to_string(),
            artifacts,
            static_slots,
            line_index,
            machine,
            breakpoints: Vec::new(),
            next_bp_id: 1,
            started: false,
            base_dir,
        })
    }
    pub fn started(&self) -> bool {
        self.started
    }

    pub fn panicked(&self) -> bool {
        self.machine.panicked()
    }

    /// Capture program stdout/stderr writes into `buf` (keeps DAP stdio clean).
    pub fn set_print_capture(&mut self, buf: std::sync::Arc<std::sync::Mutex<Vec<u8>>>) {
        self.machine.set_shared_print(buf);
    }

    pub fn breakpoints(&self) -> Vec<BreakpointInfo> {
        self.breakpoints
            .iter()
            .map(|b| BreakpointInfo {
                id: b.id,
                pc: b.pc,
                label: b.label.clone(),
            })
            .collect()
    }

    pub fn clear_breakpoints(&mut self) {
        self.breakpoints.clear();
        self.sync_vm_breakpoints();
    }

    pub fn delete_breakpoint(&mut self, id: usize) -> Result<(), String> {
        let before = self.breakpoints.len();
        self.breakpoints.retain(|b| b.id != id);
        if self.breakpoints.len() == before {
            return Err(format!("no breakpoint {id}"));
        }
        self.sync_vm_breakpoints();
        Ok(())
    }

    /// Replace all line breakpoints for `source` (DAP `setBreakpoints` semantics).
    pub fn replace_line_breakpoints(
        &mut self,
        source: &str,
        lines: &[u32],
    ) -> Vec<LineBreakpointResult> {
        self.breakpoints
            .retain(|b| !source_matches(&b.source, source));
        let mut out = Vec::with_capacity(lines.len());
        for &line in lines {
            let pcs = self
                .line_index
                .pcs_for_line(Some(source), line, &self.entry);
            if pcs.is_empty() {
                out.push(LineBreakpointResult {
                    line,
                    verified: false,
                    pc: None,
                });
                continue;
            }
            let pc = pcs[0];
            let label = format!("{source}:{line}");
            let id = self.next_bp_id;
            self.next_bp_id += 1;
            self.breakpoints.push(Breakpoint {
                id,
                pc,
                label: format!("{label} (pc {pc})"),
                source: Some(source.to_string()),
            });
            out.push(LineBreakpointResult {
                line,
                verified: true,
                pc: Some(pc),
            });
        }
        self.sync_vm_breakpoints();
        out
    }

    pub fn set_line_breakpoints(
        &mut self,
        file_hint: Option<&str>,
        lines: &[u32],
    ) -> Vec<LineBreakpointResult> {
        let source = file_hint.unwrap_or(&self.entry).to_string();
        self.replace_line_breakpoints(&source, lines)
    }

    pub fn set_function_breakpoint(&mut self, name: &str) -> Result<BreakpointInfo, String> {
        let (pcs, label) = self.resolve_break_target(name)?;
        if pcs.is_empty() {
            return Err(format!("no code locations for `{name}`"));
        }
        let pc = pcs[0];
        let id = self.next_bp_id;
        self.next_bp_id += 1;
        let info = BreakpointInfo {
            id,
            pc,
            label: format!("{label} (pc {pc})"),
        };
        self.breakpoints.push(Breakpoint {
            id,
            pc,
            label: info.label.clone(),
            source: None,
        });
        self.sync_vm_breakpoints();
        Ok(info)
    }

    pub fn replace_function_breakpoints(
        &mut self,
        names: &[&str],
    ) -> Result<Vec<BreakpointInfo>, String> {
        self.breakpoints.retain(|b| b.source.is_some());
        // Empty replacement still must drop cleared fn BPs from the VM.
        if names.is_empty() {
            self.sync_vm_breakpoints();
            return Ok(Vec::new());
        }
        names
            .iter()
            .map(|n| self.set_function_breakpoint(n))
            .collect()
    }

    pub fn set_function_breakpoints(
        &mut self,
        names: &[&str],
    ) -> Result<Vec<BreakpointInfo>, String> {
        names
            .iter()
            .map(|n| self.set_function_breakpoint(n))
            .collect()
    }

    pub fn break_at(&mut self, arg: &str) -> Result<BreakpointInfo, String> {
        let (pcs, label) = self.resolve_break_target(arg)?;
        if pcs.is_empty() {
            return Err(format!("no code locations for `{arg}`"));
        }
        let pc = pcs[0];
        let id = self.next_bp_id;
        self.next_bp_id += 1;
        let info = BreakpointInfo {
            id,
            pc,
            label: format!("{label} (pc {pc})"),
        };
        self.breakpoints.push(Breakpoint {
            id,
            pc,
            label: info.label.clone(),
            source: None,
        });
        self.sync_vm_breakpoints();
        Ok(info)
    }

    pub fn start(&mut self) -> StopReason {
        self.machine.debug_reset();
        if self.machine.debug_controller().is_none() {
            self.machine.attach_debug(DebugController::new());
        }
        self.sync_vm_breakpoints();
        self.started = true;
        let reason = self.run_from(self.machine.debug_ip());
        if matches!(reason, StopReason::Halt | StopReason::Panic) {
            self.started = false;
        }
        reason
    }

    pub fn continue_exec(&mut self) -> Result<StopReason, String> {
        if !self.started {
            return Err("not started".into());
        }
        self.prepare_resume();
        let reason = self.run_from(self.machine.debug_ip());
        if matches!(reason, StopReason::Halt | StopReason::Panic) {
            self.started = false;
        }
        Ok(reason)
    }

    pub fn stepi(&mut self) -> Result<StopReason, String> {
        if !self.started {
            return Err("not started".into());
        }
        let ip = self.machine.debug_ip();
        if let Some(dbg) = self.machine.debug_controller_mut() {
            dbg.set_stepi();
            if dbg.breakpoints().contains(&ip) {
                dbg.skip_breakpoint_once(ip);
            }
        }
        let reason = self.run_from(ip);
        if matches!(reason, StopReason::Halt | StopReason::Panic) {
            self.started = false;
        }
        Ok(reason)
    }

    pub fn step_in(&mut self) -> Result<StopReason, String> {
        self.step_line(false)
    }

    pub fn step_over(&mut self) -> Result<StopReason, String> {
        self.step_line(true)
    }

    pub fn step_out(&mut self) -> Result<StopReason, String> {
        if !self.started {
            return Err("not started".into());
        }
        let ip = self.machine.debug_ip();
        let depth = self.machine.debug_frame_depth();
        if depth == 0 {
            return Err("no frame to finish".into());
        }
        if let Some(dbg) = self.machine.debug_controller_mut() {
            dbg.set_finish(depth - 1);
            if dbg.breakpoints().contains(&ip) {
                dbg.skip_breakpoint_once(ip);
            }
        }
        let reason = self.run_from(ip);
        if matches!(reason, StopReason::Halt | StopReason::Panic) {
            self.started = false;
        }
        Ok(reason)
    }

    pub fn stack_frames(&self) -> Vec<StackFrameInfo> {
        let depth = self.machine.debug_frame_depth();
        if depth == 0 {
            return Vec::new();
        }
        // Top frame first for display; `index` is the machine frame id so DAP
        // scopes/variables can pass it straight to `locals_for_frame`.
        (0..depth)
            .rev()
            .map(|frame_idx| {
                let ip = self.machine.debug_frame_ip(frame_idx).unwrap_or(0);
                let name = symbol_at_pc(&self.artifacts.functions, ip)
                    .unwrap_or("<unknown>")
                    .to_string();
                let (path, line, column) = self
                    .machine
                    .resolve_pc_location(ip)
                    .map(|(p, l, c)| (Some(p), Some(l), Some(c)))
                    .unwrap_or((None, None, None));
                StackFrameInfo {
                    index: frame_idx,
                    name,
                    pc: ip,
                    path,
                    line,
                    column,
                }
            })
            .collect()
    }

    pub fn locals_for_frame(&self, frame_idx: usize) -> Result<Vec<LocalInfo>, String> {
        let depth = self.machine.debug_frame_depth();
        if depth == 0 {
            return Err("no active frame".into());
        }
        if frame_idx >= depth {
            return Err(format!("frame {frame_idx} out of range"));
        }
        let ip = self
            .machine
            .debug_frame_ip(frame_idx)
            .ok_or("no frame ip")?;
        let locals = locals_for_pc(self, ip).unwrap_or(&[]);
        let mut out = Vec::new();
        for (name, slot) in locals {
            let val = self
                .machine
                .debug_slot(frame_idx, *slot as usize)
                .map(|v| self.machine.debug_format_value(v))
                .unwrap_or_else(|| "<unavailable>".into());
            out.push(LocalInfo {
                name: name.clone(),
                slot: *slot as usize,
                value: val,
            });
        }
        Ok(out)
    }

    pub fn read_variable(&self, name: &str) -> Result<LocalInfo, String> {
        let depth = self.machine.debug_frame_depth();
        if depth == 0 {
            return Err("no active frame".into());
        }
        let frame = depth - 1;
        let slot = if let Some(n) = name.strip_prefix('$') {
            n.parse()
                .map_err(|_| format!("invalid slot `{name}` (usage: $N)"))?
        } else {
            resolve_local_slot(self, name)?
        };
        let val = self
            .machine
            .debug_slot(frame, slot)
            .ok_or_else(|| format!("slot ${slot} out of range"))?;
        let label = locals_for_pc(self, self.machine.debug_ip())
            .and_then(|locals| {
                locals
                    .iter()
                    .find(|(_, s)| *s as usize == slot)
                    .map(|(n, _)| n.clone())
            })
            .unwrap_or_default();
        Ok(LocalInfo {
            name: if label.is_empty() {
                format!("${slot}")
            } else {
                label
            },
            slot,
            value: self.machine.debug_format_value(val),
        })
    }

    pub fn current_ip(&self) -> usize {
        self.machine.debug_ip()
    }

    pub fn registers(&self) -> (usize, usize, usize) {
        let ip = self.machine.debug_ip();
        let depth = self.machine.debug_frame_depth();
        let sp = self
            .machine
            .debug_frame_sp(depth.saturating_sub(1))
            .unwrap_or(0);
        (ip, sp, depth)
    }

    pub fn list_source(&self) -> Result<(PathBuf, u32, Vec<(u32, bool, String)>), String> {
        if !self.started {
            return Err("not started".into());
        }
        let ip = self.machine.debug_ip();
        let (path, line, _) = self
            .machine
            .resolve_pc_location(ip)
            .ok_or("no source location at current PC")?;
        let resolved = resolve_path(&path, Some(&self.base_dir));
        let text = fs::read_to_string(&resolved)
            .map_err(|e| format!("cannot read {}: {e}", resolved.display()))?;
        let lines: Vec<&str> = text.lines().collect();
        let start = line.saturating_sub(5).max(1);
        let end = (line + 5).min(lines.len() as u32);
        let mut rows = Vec::new();
        for n in start..=end {
            let mark = n == line;
            let src = lines.get((n - 1) as usize).unwrap_or(&"").to_string();
            rows.push((n, mark, src));
        }
        Ok((resolved, line, rows))
    }

    pub fn breakpoint_id_at_pc(&self, pc: usize) -> Option<usize> {
        self.breakpoints.iter().find(|b| b.pc == pc).map(|b| b.id)
    }

    pub fn stop_symbol(&self) -> String {
        let ip = self.machine.debug_ip();
        symbol_at_pc(&self.artifacts.functions, ip)
            .unwrap_or("<prog>")
            .to_string()
    }

    pub fn stop_location(&self) -> Option<(String, u32, u32)> {
        self.machine.resolve_pc_location(self.machine.debug_ip())
    }

    fn step_line(&mut self, next: bool) -> Result<StopReason, String> {
        if !self.started {
            return Err("not started".into());
        }
        let ip = self.machine.debug_ip();
        let depth = self.machine.debug_frame_depth();
        let (file, line) = match self.machine.debug_pc_line(ip) {
            Some(fl) => fl,
            None => return self.stepi(),
        };
        if let Some(dbg) = self.machine.debug_controller_mut() {
            if next {
                dbg.set_next(file, line, depth);
            } else {
                dbg.set_step_line(file, line, depth);
            }
            if dbg.breakpoints().contains(&ip) {
                dbg.skip_breakpoint_once(ip);
            }
        }
        let reason = self.run_from(ip);
        if matches!(reason, StopReason::Halt | StopReason::Panic) {
            self.started = false;
        }
        Ok(reason)
    }

    fn run_from(&mut self, start_ip: usize) -> StopReason {
        self.machine.debug_run_until_raw(
            &self.artifacts.bytecode,
            &self.artifacts.constants,
            &self.artifacts.strings,
            self.static_slots,
            start_ip,
        )
    }

    fn prepare_resume(&mut self) {
        let ip = self.machine.debug_ip();
        if let Some(dbg) = self.machine.debug_controller_mut() {
            dbg.clear_step();
            if dbg.breakpoints().contains(&ip) {
                dbg.skip_breakpoint_once(ip);
            }
        }
    }

    fn sync_vm_breakpoints(&mut self) {
        if let Some(dbg) = self.machine.debug_controller_mut() {
            dbg.clear_breakpoints();
            for b in &self.breakpoints {
                dbg.add_breakpoint(b.pc);
            }
        }
    }

    fn resolve_break_target(&self, arg: &str) -> Result<(Vec<usize>, String), String> {
        if let Some((file, line_s)) = arg.rsplit_once(':')
            && !file.is_empty()
            && line_s.chars().all(|c| c.is_ascii_digit())
        {
            let line: u32 = line_s.parse().map_err(|_| "invalid line number")?;
            let pcs = self.line_index.pcs_for_line(Some(file), line, &self.entry);
            return Ok((pcs, format!("{file}:{line}")));
        }
        if arg.chars().all(|c| c.is_ascii_digit()) {
            let line: u32 = arg.parse().map_err(|_| "invalid line number")?;
            let pcs = self.line_index.pcs_for_line(None, line, &self.entry);
            return Ok((pcs, format!("{}:{line}", self.entry)));
        }
        let matched: Vec<&FnSym> = self
            .artifacts
            .functions
            .iter()
            .filter(|s| matches_fn_pat(&s.name, arg))
            .collect();
        if matched.is_empty() {
            return Err(format!("no function matching `{arg}`"));
        }
        let pcs: Vec<usize> = matched.iter().map(|s| s.entry_pc as usize).collect();
        let name = matched[0].name.clone();
        Ok((pcs, name))
    }
}

pub fn symbol_at_pc(functions: &[FnSym], pc: usize) -> Option<&str> {
    let mut best: Option<&FnSym> = None;
    for s in functions {
        let entry = s.entry_pc as usize;
        if entry <= pc && best.map(|b| entry >= b.entry_pc as usize).unwrap_or(true) {
            best = Some(s);
        }
    }
    best.map(|s| s.name.as_str())
}

fn locals_for_pc<'a>(session: &'a DebugSession, pc: usize) -> Option<&'a [(String, u32)]> {
    let name = symbol_at_pc(&session.artifacts.functions, pc)?;
    session
        .artifacts
        .functions
        .iter()
        .find(|s| s.name == name)
        .map(|s| s.locals.as_slice())
}

fn resolve_local_slot(session: &DebugSession, name: &str) -> Result<usize, String> {
    let ip = session.machine.debug_ip();
    let locals = locals_for_pc(session, ip).unwrap_or(&[]);
    let name_l = name.to_ascii_lowercase();
    for (n, slot) in locals {
        if n.eq_ignore_ascii_case(name) || n.to_ascii_lowercase() == name_l {
            return Ok(*slot as usize);
        }
    }
    let matches: Vec<_> = locals
        .iter()
        .filter(|(n, _)| n.to_ascii_lowercase().contains(&name_l))
        .collect();
    match matches.as_slice() {
        [(_, slot)] => Ok(*slot as usize),
        [] => Err(format!(
            "no local `{name}` in current frame (try `info locals` or `print $N`)"
        )),
        many => Err(format!(
            "ambiguous local `{name}`: {}",
            many.iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}
fn source_matches(stored: &Option<String>, requested: &str) -> bool {
    let Some(stored) = stored else {
        return false;
    };
    if stored == requested {
        return true;
    }
    let sp = Path::new(stored);
    let rp = Path::new(requested);
    sp == rp
        || sp.file_name() == rp.file_name()
        || requested.ends_with(stored)
        || stored.ends_with(requested)
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::DebugLoc;
    use machine::StopReason;

    fn fib_path() -> String {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../examples/fib.hy")
            .canonicalize()
            .expect("examples/fib.hy")
            .display()
            .to_string()
    }

    fn compile_fib() -> DebugSession {
        let config = ReportConfig::from_cli_flags(false, false).expect("report config");
        DebugSession::compile(
            config,
            &fib_path(),
            Box::new(std::io::sink()),
            HostGrants::deny_all(),
        )
        .expect("compile fib")
    }

    #[test]
    fn line_index_maps_known_loc() {
        let text = "fn main() {\n    return;\n}\n";
        let tmp = std::env::temp_dir().join(format!("coil_dbg_line_{}", std::process::id()));
        fs::write(&tmp, text).unwrap();
        let path = tmp.display().to_string();
        let mut debug = ProgramDebug {
            source_files: vec![path.clone()],
            debug_locs: vec![DebugLoc::unknown(); 5],
            fn_symbols: Vec::new(),
        };
        let ret_off = text.find("return").unwrap() as u32;
        debug.debug_locs[3] = DebugLoc {
            file: 0,
            start_byte: ret_off,
            end_byte: ret_off + 6,
        };
        let idx = LineIndex::build(&debug, None);
        let pcs = idx.pcs_for_line(Some(&path), 2, &path);
        assert!(pcs.contains(&3), "pcs={pcs:?}");
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn line_index_resolves_basename_hint() {
        let text = "fn main() {\n    return;\n}\n";
        let tmp = std::env::temp_dir().join(format!("coil_dbg_base_{}", std::process::id()));
        fs::write(&tmp, text).unwrap();
        let path = tmp.display().to_string();
        let mut debug = ProgramDebug {
            source_files: vec![path.clone()],
            debug_locs: vec![DebugLoc::unknown(); 4],
            fn_symbols: Vec::new(),
        };
        let ret_off = text.find("return").unwrap() as u32;
        debug.debug_locs[2] = DebugLoc {
            file: 0,
            start_byte: ret_off,
            end_byte: ret_off + 6,
        };
        let idx = LineIndex::build(&debug, None);
        let base = Path::new(&path).file_name().unwrap().to_str().unwrap();
        let pcs = idx.pcs_for_line(Some(base), 2, &path);
        assert!(pcs.contains(&2), "pcs={pcs:?}");
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn function_breakpoint_exposes_fib_locals_via_frame_id() {
        let mut session = compile_fib();
        session.set_function_breakpoint("fib").expect("break fib");
        let reason = session.start();
        assert!(
            matches!(reason, StopReason::Breakpoint { .. }),
            "reason={reason:?}"
        );
        let frames = session.stack_frames();
        assert!(
            frames.len() >= 2,
            "expected recursive/caller stack, got {frames:?}"
        );
        assert!(
            frames[0].name.contains("fib"),
            "top frame should be fib, got {:?}",
            frames[0].name
        );
        let locals = session
            .locals_for_frame(frames[0].index)
            .expect("locals for top DAP frame id");
        assert!(
            locals.iter().any(|l| l.name == "n"),
            "expected local n, got {locals:?}"
        );
        let n = session.read_variable("n").expect("print n");
        assert_eq!(n.name, "n");
        assert!(!n.value.is_empty());
    }

    #[test]
    fn replace_line_breakpoints_marks_unverified_and_clears_prior() {
        let mut session = compile_fib();
        let entry = session.entry.clone();
        let first = session.replace_line_breakpoints(&entry, &[1, 999_999]);
        assert_eq!(first.len(), 2);
        assert!(!first[1].verified);
        assert!(first[1].pc.is_none());
        let second = session.replace_line_breakpoints(&entry, &[]);
        assert!(second.is_empty());
        assert!(
            session.breakpoints().is_empty(),
            "empty replace should clear line BPs for source"
        );
    }

    #[test]
    fn replace_function_breakpoints_empty_syncs_vm_so_continue_finishes() {
        let mut session = compile_fib();
        session.set_function_breakpoint("fib").expect("break fib");
        let reason = session.start();
        assert!(matches!(reason, StopReason::Breakpoint { .. }));
        session
            .replace_function_breakpoints(&[])
            .expect("clear fn breakpoints");
        assert!(session.breakpoints().is_empty());
        let reason = session.continue_exec().expect("continue");
        assert!(
            matches!(reason, StopReason::Halt),
            "empty fn replace must sync VM; reason={reason:?}"
        );
    }

    #[test]
    fn replace_function_breakpoints_replaces_fn_only() {
        let mut session = compile_fib();
        let entry = session.entry.clone();
        let line = session.replace_line_breakpoints(&entry, &[17]);
        let line_verified = line.iter().any(|r| r.verified);
        session
            .replace_function_breakpoints(&["fib"])
            .expect("fn breakpoints");
        assert!(
            session
                .breakpoints()
                .iter()
                .any(|b| b.label.contains("fib")),
            "expected fib BP"
        );
        session
            .replace_function_breakpoints(&[])
            .expect("clear fn breakpoints");
        let remaining = session.breakpoints();
        if line_verified {
            assert!(
                remaining.iter().any(|b| b.label.contains(':')),
                "line BP should survive fn replace, got {remaining:?}"
            );
        } else {
            assert!(remaining.is_empty(), "got {remaining:?}");
        }
    }

    #[test]
    fn continue_before_start_errors() {
        let mut session = compile_fib();
        let err = session.continue_exec().expect_err("not started");
        assert!(err.contains("not started"), "err={err}");
    }

    #[test]
    fn unknown_function_breakpoint_errors() {
        let mut session = compile_fib();
        let err = session
            .set_function_breakpoint("no_such_fn_zz")
            .expect_err("missing fn");
        assert!(
            err.contains("no function") || err.contains("no code"),
            "err={err}"
        );
    }

    #[test]
    fn source_matches_basename_and_suffix() {
        assert!(source_matches(
            &Some("examples/fib.hy".into()),
            "examples/fib.hy"
        ));
        assert!(source_matches(
            &Some("/abs/examples/fib.hy".into()),
            "fib.hy"
        ));
        assert!(source_matches(
            &Some("fib.hy".into()),
            "/workspace/examples/fib.hy"
        ));
        assert!(!source_matches(&None, "fib.hy"));
        assert!(!source_matches(&Some("other.hy".into()), "fib.hy"));
    }

    #[test]
    fn resolve_path_joins_base_dir() {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../examples");
        let resolved = resolve_path("fib.hy", Some(&base));
        assert!(
            resolved.exists(),
            "expected fib.hy under examples, got {}",
            resolved.display()
        );
    }
}
