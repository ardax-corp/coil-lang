//! Shared debug session engine for REPL and DAP front-ends.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{ProgramDebug, byte_to_position};
use compiler::{DissectArtifacts, FnSym, HostGrants, Pipeline, matches_fn_pat};
use coil_mir_interp::debug::Debugger;
use machine::StopReason;
use reporting::ReportConfig;

type SourceLine = (u32, bool, String);
type ListedSource = (PathBuf, u32, Vec<SourceLine>);

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

/// First frame id of suspended tasks' saved frames (live frames use their
/// machine index, far below).
pub const SAVED_FRAME_BASE: usize = 1 << 24;

/// Frame id of saved frame `k` (bottom first) of suspended task `task`.
pub fn saved_frame_id(task: u64, k: usize) -> usize {
    SAVED_FRAME_BASE + (task as usize) * 4096 + k
}

/// Named local in the current frame.
#[derive(Clone, Debug)]
pub struct LocalInfo {
    pub name: String,
    /// Frame slot of a single-slot value (`None` for split layouts).
    pub slot: Option<usize>,
    pub value: String,
}

#[derive(Clone, Debug)]
struct Breakpoint {
    id: usize,
    pc: usize,
    label: String,
    /// Source path hint for DAP per-file breakpoint replacement.
    source: Option<String>,
    /// `break <loc> if <cond>`: stop only when it holds.
    condition: Option<Condition>,
}

/// `<local | $N> <op> <integer | true | false>`, evaluated on the stopped
/// frame's slot as an integer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Condition {
    pub operand: String,
    pub op: CmpOp,
    pub value: i64,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Condition {
    pub fn parse(text: &str) -> Result<Self, String> {
        let usage = || format!("unsupported condition `{text}` (use: <name|$N> <op> <int|true|false>)");
        let text = text.trim();
        let (operand, op, rest) = [
            ("==", CmpOp::Eq),
            ("!=", CmpOp::Ne),
            ("<=", CmpOp::Le),
            (">=", CmpOp::Ge),
            ("<", CmpOp::Lt),
            (">", CmpOp::Gt),
        ]
        .into_iter()
        .find_map(|(sym, op)| text.split_once(sym).map(|(l, r)| (l.trim(), op, r.trim())))
        .ok_or_else(usage)?;
        if operand.is_empty() {
            return Err(usage());
        }
        let value = match rest {
            "true" => 1,
            "false" => 0,
            n => n.parse::<i64>().map_err(|_| usage())?,
        };
        Ok(Self {
            operand: operand.to_string(),
            op,
            value,
            text: text.to_string(),
        })
    }

    fn holds(&self, lhs: i64) -> bool {
        match self.op {
            CmpOp::Eq => lhs == self.value,
            CmpOp::Ne => lhs != self.value,
            CmpOp::Lt => lhs < self.value,
            CmpOp::Le => lhs <= self.value,
            CmpOp::Gt => lhs > self.value,
            CmpOp::Ge => lhs >= self.value,
        }
    }
}

/// Line → PCs index for one source file (path as stored in ProgramDebug).
#[derive(Default)]
pub struct LineIndex {
    by_file: HashMap<String, HashMap<u32, Vec<usize>>>,
    by_basename: HashMap<String, String>,
    /// Canonical path → stored key: DAP clients send absolute paths while
    /// the archive stores them as compiled (often relative).
    by_canonical: HashMap<PathBuf, String>,
}

impl LineIndex {
    pub fn build(debug: &ProgramDebug, base_dir: Option<&Path>) -> Self {
        let mut idx = LineIndex::default();
        let mut texts: HashMap<u32, String> = HashMap::new();
        // Each op of a line has a location: a line's breakpoint is the first
        // op of each run of them, so it stops once per pass over the line.
        let mut last = None;
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
                if let Ok(canonical) = resolved.canonicalize() {
                    idx.by_canonical.insert(canonical, path.clone());
                }
                fs::read_to_string(resolved).unwrap_or_default()
            });
            if text.is_empty() {
                continue;
            }
            let line = byte_to_position(text, loc.start_byte as usize).line;
            if last.replace((loc.file, line)) == Some((loc.file, line)) {
                continue;
            }
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
            } else if let Some(stored) = Path::new(hint)
                .canonicalize()
                .ok()
                .and_then(|c| self.by_canonical.get(&c))
            {
                stored.clone()
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

/// `(file, line, column)` of each op of `debug`.
fn op_lines(debug: &ProgramDebug, base_dir: Option<&Path>) -> Vec<Option<(u32, u32, u32)>> {
    let mut texts: HashMap<u32, String> = HashMap::new();
    debug
        .debug_locs
        .iter()
        .map(|loc| {
            if !loc.is_known() {
                return None;
            }
            let text = texts.entry(loc.file).or_insert_with(|| {
                let path = debug.source_files.get(loc.file as usize).map(|p| resolve_path(p, base_dir));
                path.and_then(|p| fs::read_to_string(p).ok()).unwrap_or_default()
            });
            if text.is_empty() {
                return None;
            }
            let at = byte_to_position(text, loc.start_byte as usize);
            Some((loc.file, at.line, at.column))
        })
        .collect()
}

/// Put the debugger's op numbers where `artifacts` holds entry ids.
fn number_ops(artifacts: &mut DissectArtifacts, locs: Vec<common::DebugLoc>, entries: &[(String, u32)]) {
    let starts: HashMap<&str, u32> = entries.iter().map(|(name, pc)| (name.as_str(), *pc)).collect();
    artifacts.functions.retain_mut(|f| {
        let start = starts.get(f.name.as_str()).copied();
        f.body_id = start.unwrap_or(0);
        start.is_some()
    });
    artifacts.functions.sort_by_key(|f| f.body_id);
    artifacts.debug.debug_locs = locs;
    artifacts.debug.fn_symbols = entries.iter().map(|(name, pc)| common::FnDebugSym { name: name.clone(), body_id: *pc }).collect();
}

/// A program ready to run on the interpreter.
#[derive(Clone)]
struct Launch {
    program: Arc<machine::ThreadProgram>,
    code: Option<Arc<machine::native::NativeCode>>,
    base_dir: Option<PathBuf>,
    search_paths: Vec<PathBuf>,
    gate: machine::DloadGate,
    struct_layouts: Vec<machine::CStructLayout>,
    shared_print: Option<Arc<std::sync::Mutex<Vec<u8>>>>,
}

impl Launch {
    fn new(pipeline: &Pipeline, artifacts: &DissectArtifacts, entry: &Path) -> Self {
        let program = Arc::new(machine::ThreadProgram {
            startup: Arc::new(pipeline.startup()),
            main: pipeline.main_offset(),
            constants: Arc::new(pipeline.constants().to_vec()),
            strings: Arc::new(artifacts.strings.clone()),
            static_slot_count: pipeline.static_slot_count(),
            debug: pipeline.program_debug_full(),
            class_word_kinds: machine::class_kind_table(&pipeline.class_word_kinds()),
            static_word_kinds: Arc::new(pipeline.static_word_kinds()),
        });
        let mut gate = machine::DloadGate::from_consumer_trusted(&pipeline.dload_native_pins(), pipeline.dload_trusted_stems());
        for stem in pipeline.extra_dload_stems() {
            gate.grant_stem(stem);
        }
        for (stem, path) in pipeline.extra_dload_grants() {
            let _ = gate.grant_file(stem, path);
        }
        Self {
            program,
            code: None,
            base_dir: entry.parent().map(Path::to_path_buf),
            search_paths: pipeline.ffi_search_path_bufs(),
            gate,
            struct_layouts: pipeline.archived_struct_layouts().iter().map(machine::CStructLayout::from_archive).collect(),
            shared_print: None,
        }
    }

    fn with_code(mut self, program: coil_mir_interp::InterpProgram, _code_len: usize) -> Self {
        let complete = program.refused.is_empty();
        let code = machine::native::NativeCode::new(program.entries, program.sites).with_complete(complete).with_keep(program.keep);
        self.code = Some(Arc::new(code));
        self
    }

    /// Run the program on this thread; whether it panicked.
    fn run(self) -> bool {
        if let Some(code) = self.code {
            machine::native::install_shared_for_next_run(code);
        }
        let start = machine::NativeStart {
            base_dir: self.base_dir,
            ffi_search_paths: self.search_paths,
            dload_gate: Some(self.gate),
            struct_layouts: self.struct_layouts,
            shared_print: self.shared_print,
            host_natives: machine::standard_host_natives(),
        };
        machine::run_natively(self.program, start).unwrap_or(true)
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
    line_index: LineIndex,
    /// What runs the program, and reads it at a stop.
    dbg: Debugger,
    /// The program as a run starts it.
    launch: Launch,
    /// Each body's MIR, by name, for `disassemble`.
    mir_text: HashMap<String, String>,
    /// `(file, line, column)` of each op.
    op_lines: Vec<Option<(u32, u32, u32)>>,
    breakpoints: Vec<Breakpoint>,
    next_bp_id: usize,
    started: bool,
    /// The program panicked: its frames stay for `bt` / `print`, but it
    /// cannot resume.
    stopped_at_panic: bool,
    pub base_dir: PathBuf,
}

impl DebugSession {
    pub fn compile(
        config: ReportConfig,
        filename: &str,
        reporter: Box<dyn std::io::Write + Send>,
        grants: HostGrants,
        extra_roots: Vec<PathBuf>,
    ) -> Result<Self, ()> {
        let mut pipeline = Pipeline::with_reporter(config, reporter);
        pipeline.set_debugger_attached(true);
        pipeline.set_opt_level(compiler::OptLevel::None);
        pipeline.set_host_grants(grants);
        let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        pipeline.bind_project_roots_with_default(dir, extra_roots);
        compiler::native_mir::start_raw_capture();
        let mut artifacts = match pipeline.compile_dissect(filename) {
            Ok(a) => a,
            Err(_) => {
                let _ = compiler::native_mir::take_native_capture();
                let _ = pipeline.finish_reporting();
                return Err(());
            }
        };
        let bodies = compiler::native_mir::take_native_capture();
        let entry_path = PathBuf::from(filename);
        let base_dir = entry_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let launch = Launch::new(&pipeline, &artifacts, &entry_path);
        let mir_text = bodies.iter().map(|b| (b.name.clone(), b.mir.to_string())).collect();
        let pcs = launch.program.debug.fn_symbols.iter().map(|s| (s.name.clone(), s.body_id)).collect();
        let debug_program = coil_mir_interp::debug::prepare_debug(bodies, &pcs);
        let launch = launch.with_code(debug_program.program, 0);
        number_ops(&mut artifacts, debug_program.locs, &debug_program.entries);
        let op_lines = op_lines(&artifacts.debug, Some(&base_dir));
        let line_index = LineIndex::build(&artifacts.debug, Some(&base_dir));
        let dbg = Debugger::new(op_lines.iter().map(|l| l.map(|(f, l, _)| (f, l))).collect());
        let _ = pipeline.finish_reporting();

        Ok(Self {
            entry: filename.to_string(),
            artifacts,
            line_index,
            dbg,
            launch,
            mir_text,
            op_lines,
            breakpoints: Vec::new(),
            next_bp_id: 1,
            started: false,
            stopped_at_panic: false,
            base_dir,
        })
    }

    #[cfg(test)]
    pub fn started(&self) -> bool {
        self.started
    }

    /// Frames can be inspected: running, or stopped by a panic.
    pub fn inspectable(&self) -> bool {
        self.started || self.stopped_at_panic
    }

    pub fn panicked(&self) -> bool {
        self.stopped_at_panic
    }

    /// Capture program stdout/stderr writes into `buf` (keeps DAP stdio clean).
    pub fn set_print_capture(&mut self, buf: std::sync::Arc<std::sync::Mutex<Vec<u8>>>) {
        self.launch.shared_print = Some(buf);
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
                condition: None,
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

    pub fn set_function_breakpoint(&mut self, name: &str) -> Result<BreakpointInfo, String> {
        self.break_at_if(name, None)
    }

    pub fn replace_function_breakpoints(
        &mut self,
        names: &[&str],
    ) -> Result<Vec<BreakpointInfo>, String> {
        self.breakpoints.retain(|b| b.source.is_some());
        // Empty replacement still must drop cleared fn BPs from the runtime.
        if names.is_empty() {
            self.sync_vm_breakpoints();
            return Ok(Vec::new());
        }
        names
            .iter()
            .map(|n| self.set_function_breakpoint(n))
            .collect()
    }

    /// `break <arg> [if <condition>]`.
    pub fn break_at_if(
        &mut self,
        arg: &str,
        condition: Option<Condition>,
    ) -> Result<BreakpointInfo, String> {
        let (pcs, label) = self.resolve_break_target(arg)?;
        if pcs.is_empty() {
            return Err(format!("no code locations for `{arg}`"));
        }
        let pc = pcs[0];
        let id = self.next_bp_id;
        self.next_bp_id += 1;
        let suffix = condition
            .as_ref()
            .map(|c| format!(" if {}", c.text))
            .unwrap_or_default();
        let info = BreakpointInfo {
            id,
            pc,
            label: format!("{label} (pc {pc}){suffix}"),
        };
        self.breakpoints.push(Breakpoint {
            id,
            pc,
            label: info.label.clone(),
            source: None,
            condition,
        });
        self.sync_vm_breakpoints();
        Ok(info)
    }

    pub fn start(&mut self) -> StopReason {
        self.begin_run(false)
    }

    /// Start and stop before the first MIR step (DAP `stopOnEntry`).
    ///
    /// Uses a temporary PC-0 breakpoint so the runtime actually builds a frame;
    /// a fake pause left `stackTrace` / step empty.
    pub fn start_paused_at_entry(&mut self) -> StopReason {
        self.begin_run(true)
    }

    fn begin_run(&mut self, stop_on_entry: bool) -> StopReason {
        self.sync_vm_breakpoints();
        self.dbg.with_controller(|c| {
            c.clear_step();
            if stop_on_entry {
                c.stop_at_next();
            }
        });
        self.started = true;
        self.stopped_at_panic = false;
        let launch = self.launch.clone();
        let reason = self.dbg.start(move || launch.run());
        let reason = self.past_false_conditions(reason);
        self.note_end(&reason);
        reason
    }

    /// The program ended, or cannot go on.
    fn note_end(&mut self, reason: &StopReason) {
        if matches!(reason, StopReason::Halt | StopReason::Panic) {
            self.started = false;
            self.stopped_at_panic = matches!(reason, StopReason::Panic);
        }
        if let (StopReason::Panic, Some(message)) = (reason, self.dbg.panic_message()) {
            let at = self.nearest_source_location(self.current_ip());
            let at = at.map(|(file, line, column)| format!(" at {file}:{line}:{column}")).unwrap_or_default();
            eprintln!("panic: {message}{at}");
        }
    }

    /// Go on from a stop to the next one.
    fn resume(&mut self) -> StopReason {
        let reason = self.dbg.resume();
        let reason = self.past_false_conditions(reason);
        self.note_end(&reason);
        reason
    }

    pub fn continue_exec(&mut self) -> Result<StopReason, String> {
        if !self.started {
            return Err("not started".into());
        }
        self.dbg.with_controller(|c| c.clear_step());
        Ok(self.resume())
    }

    pub fn stepi(&mut self) -> Result<StopReason, String> {
        if !self.started {
            return Err("not started".into());
        }
        self.dbg.with_controller(|c| c.stop_at_next());
        Ok(self.resume())
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
        let depth = self.dbg.depth();
        if depth == 0 {
            return Err("no frame to finish".into());
        }
        self.dbg.with_controller(|c| c.set_finish(depth - 1));
        Ok(self.resume())
    }

    /// The MIR of function `name`, as `disassemble` shows it.
    pub fn mir_of(&self, name: &str) -> Option<&str> {
        self.mir_text.get(name).map(String::as_str)
    }

    pub fn stack_frames(&self) -> Vec<StackFrameInfo> {
        self.live_frames(0..self.dbg.depth())
    }

    /// The scheduler's tasks (empty without child tasks): the running one
    /// and the suspended ones, which a DAP client shows as threads.
    pub fn tasks(&self) -> Vec<machine::DebugTask> {
        self.dbg.tasks()
    }

    /// Frames of `task`, top first. Live frames keep their machine index;
    /// a suspended task's saved frames get `saved_frame_id` ids (no locals).
    pub fn task_frames(&self, task: &machine::DebugTask) -> Vec<StackFrameInfo> {
        match &task.frames {
            machine::DebugTaskFrames::Live(range) => self.live_frames(range.clone()),
            machine::DebugTaskFrames::Saved(pcs) => pcs
                .iter()
                .enumerate()
                .rev()
                .map(|(k, &pc)| self.frame_info(saved_frame_id(task.id, k), pc))
                .collect(),
        }
    }

    fn frame_info(&self, index: usize, ip: usize) -> StackFrameInfo {
        let name = symbol_at_pc(&self.artifacts.functions, ip)
            .unwrap_or("<unknown>")
            .to_string();
        let (path, line, column) = self
            .nearest_source_location(ip)
            .map(|(p, l, c)| (Some(p), Some(l), Some(c)))
            .unwrap_or((None, None, None));
        StackFrameInfo {
            index,
            name,
            pc: ip,
            path,
            line,
            column,
        }
    }

    fn live_frames(&self, frames: std::ops::Range<usize>) -> Vec<StackFrameInfo> {
        // Top frame first for display; `index` is the frame's id, so DAP
        // scopes/variables can pass it straight to `locals_for_frame`.
        frames
            .rev()
            .map(|frame_idx| self.frame_info(frame_idx, self.dbg.frame_op(frame_idx).unwrap_or(0)))
            .collect()
    }

    pub fn locals_for_frame(&self, frame_idx: usize) -> Result<Vec<LocalInfo>, String> {
        let depth = self.dbg.depth();
        if depth == 0 {
            return Err("no active frame".into());
        }
        if frame_idx >= depth {
            return Err(format!("frame {frame_idx} out of range"));
        }
        let pc = self.frame_lookup_pc(frame_idx).ok_or("no frame ip")?;
        let vars = self.visible_vars(pc);
        if vars.is_empty() {
            return Ok(self.legacy_locals(frame_idx, pc));
        }
        let renderer = self.renderer();
        Ok(vars
            .into_iter()
            .map(|var| LocalInfo {
                name: var.name.clone(),
                slot: var_slot(var, pc as u32),
                value: renderer.var(var, frame_idx, pc as u32),
            })
            .collect())
    }

    /// Name → slot table for functions compiled without debug variables.
    fn legacy_locals(&self, frame_idx: usize, pc: usize) -> Vec<LocalInfo> {
        locals_for_pc(self, pc)
            .unwrap_or(&[])
            .iter()
            .map(|(name, slot)| LocalInfo {
                name: name.clone(),
                slot: Some(*slot as usize),
                value: self
                    .dbg
                    .local(frame_idx, *slot)
                    .map(|v| self.dbg.format(v))
                    .unwrap_or_else(|| "<unavailable>".into()),
            })
            .collect()
    }

    /// Op used to look up a frame's variables: where it stands.
    fn frame_lookup_pc(&self, frame_idx: usize) -> Option<usize> {
        self.dbg.frame_op(frame_idx)
    }

    /// Source position `(file, byte)` of `pc`, else the nearest located PC
    /// before it in the same function.
    fn source_offset(&self, pc: usize) -> Option<(u32, u32)> {
        let entry = self
            .artifacts
            .functions
            .iter()
            .filter(|f| f.body_id as usize <= pc)
            .map(|f| f.body_id as usize)
            .max()
            .unwrap_or(0);
        let located = |p: usize| {
            let loc = self.artifacts.debug.debug_locs.get(p)?;
            loc.is_known().then_some((loc.file, loc.start_byte))
        };
        // The prologue (before the first statement) has no location: it
        // belongs to the start of the body.
        let end = self
            .artifacts
            .functions
            .iter()
            .map(|f| f.body_id as usize)
            .filter(|&e| e > pc)
            .min()
            .unwrap_or(self.artifacts.debug.debug_locs.len());
        (entry..=pc).rev().find_map(located).or_else(|| (pc..end).find_map(located))
    }

    /// Variables in scope at `pc`, innermost binding per name, in
    /// declaration order.
    fn visible_vars(&self, pc: usize) -> Vec<&compiler::debug_vars::DebugVar> {
        let Some(name) = symbol_at_pc(&self.artifacts.functions, pc) else {
            return Vec::new();
        };
        let Some(sym) = self.artifacts.functions.iter().find(|s| s.name == name) else {
            return Vec::new();
        };
        let Some((file, offset)) = self.source_offset(pc) else {
            return Vec::new();
        };
        let mut best: std::collections::HashMap<&str, &compiler::debug_vars::DebugVar> =
            std::collections::HashMap::new();
        for var in sym.vars.iter().filter(|v| v.in_scope(file, offset)) {
            best.entry(var.name.as_str())
                .and_modify(|cur| {
                    if var.scope.0 >= cur.scope.0 {
                        *cur = var;
                    }
                })
                .or_insert(var);
        }
        let mut vars: Vec<_> = best.into_values().collect();
        vars.sort_by_key(|v| (v.scope.0, v.name.clone()));
        vars
    }

    fn renderer(&self) -> crate::render::Renderer<'_> {
        crate::render::Renderer {
            dbg: &self.dbg,
            classes: &self.artifacts.classes,
            enums: &self.artifacts.enums,
        }
    }

    /// `print` argument: `$N` (raw slot), a variable, or a path through
    /// fields and indices (`p.x`, `xs[2]`, `a.b[1].c`).
    pub fn read_variable(&self, expr: &str) -> Result<LocalInfo, String> {
        let depth = self.dbg.depth();
        if depth == 0 {
            return Err("no active frame".into());
        }
        let frame = depth - 1;
        let pc = self.frame_lookup_pc(frame).ok_or("no frame ip")?;
        if let Some(n) = expr.strip_prefix('$') {
            let slot: usize = n
                .parse()
                .map_err(|_| format!("invalid slot `{expr}` (usage: $N)"))?;
            let val = self
                .dbg
                .local(frame, slot as u32)
                .ok_or_else(|| format!("slot ${slot} out of range"))?;
            return Ok(LocalInfo {
                name: expr.to_string(),
                slot: Some(slot),
                value: self.dbg.format(val),
            });
        }
        let (base, path) = split_path(expr)?;
        let vars = self.visible_vars(pc);
        if vars.is_empty() {
            let slot = resolve_local_slot(self, base)?;
            let val = self
                .dbg
                .local(frame, slot as u32)
                .ok_or_else(|| format!("slot ${slot} out of range"))?;
            return Ok(LocalInfo {
                name: base.to_string(),
                slot: Some(slot),
                value: self.dbg.format(val),
            });
        }
        let var = vars
            .iter()
            .find(|v| v.name == base)
            .or_else(|| vars.iter().find(|v| v.name.eq_ignore_ascii_case(base)))
            .copied()
            .ok_or_else(|| format!("no local `{base}` in current frame (try `info locals` or `print $N`)"))?;
        let renderer = self.renderer();
        let value = if path.is_empty() {
            renderer.var(var, frame, pc as u32)
        } else {
            crate::render::eval_path(&renderer, var, frame, pc as u32, &path)?
        };
        Ok(LocalInfo {
            name: expr.to_string(),
            slot: var_slot(var, pc as u32),
            value,
        })
    }

    pub fn current_ip(&self) -> usize {
        self.dbg.depth().checked_sub(1).and_then(|top| self.dbg.frame_op(top)).unwrap_or(0)
    }

    pub fn registers(&self) -> (usize, usize, usize) {
        let ip = self.current_ip();
        let depth = self.dbg.depth();
        let sp = self.dbg.frame_base(depth.saturating_sub(1)).unwrap_or(0);
        (ip, sp, depth)
    }

    pub fn list_source(&self) -> Result<ListedSource, String> {
        if !self.started {
            return Err("not started".into());
        }
        let ip = self.current_ip();
        let (path, line) = if let Some((p, l, _)) = self.nearest_source_location(ip) {
            (p, l)
        } else {
            self.list_fn_decl_fallback()?
        };
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

    /// When `debug_locs` miss this function, show the `fn name` decl in source.
    fn list_fn_decl_fallback(&self) -> Result<(String, u32), String> {
        let name = self.stop_symbol();
        let short = name.rsplit("::").next().unwrap_or(name.as_str());
        if short.is_empty() || short.starts_with('<') {
            return Err(
                "no source location near current PC (debug_locs sparse; try `disas` / `bt`)"
                    .into(),
            );
        }
        let mut files = vec![self.entry.clone()];
        for f in &self.artifacts.debug.source_files {
            files.push(f.clone());
        }
        for file in files {
            let resolved = resolve_path(&file, Some(&self.base_dir));
            let Ok(text) = fs::read_to_string(&resolved) else {
                continue;
            };
            if let Some(line) = find_fn_decl_line(&text, short) {
                return Ok((resolved.display().to_string(), line));
            }
        }
        Err(
            "no source location near current PC (debug_locs sparse; try `disas` / `bt`)"
                .into(),
        )
    }

    pub fn breakpoint_id_at_pc(&self, pc: usize) -> Option<usize> {
        self.breakpoints.iter().find(|b| b.pc == pc).map(|b| b.id)
    }

    pub fn stop_symbol(&self) -> String {
        let ip = self.current_ip();
        symbol_at_pc(&self.artifacts.functions, ip)
            .unwrap_or("<prog>")
            .to_string()
    }

    pub fn stop_location(&self) -> Option<(String, u32, u32)> {
        self.resolve_pc_location(self.current_ip())
    }

    fn step_line(&mut self, next: bool) -> Result<StopReason, String> {
        if !self.started {
            return Err("not started".into());
        }
        let depth = self.dbg.depth();
        let Some((file, line, _)) = self.op_lines.get(self.current_ip()).copied().flatten() else {
            return self.stepi();
        };
        self.dbg.with_controller(|c| {
            if next {
                c.set_next(file, line, depth);
            } else {
                c.set_step_line(file, line, depth);
            }
        });
        Ok(self.resume())
    }

    /// Go on past breakpoints whose condition does not hold.
    fn past_false_conditions(&mut self, mut reason: StopReason) -> StopReason {
        while let StopReason::Breakpoint { pc } = reason
            && self.condition_fails_at(pc)
        {
            self.dbg.with_controller(|c| c.clear_step());
            reason = self.dbg.resume();
        }
        reason
    }

    /// True when the breakpoint at `pc` has a condition that is false now.
    /// An operand that cannot be read stops (fail open: the user looks).
    fn condition_fails_at(&self, pc: usize) -> bool {
        let Some(cond) = self
            .breakpoints
            .iter()
            .find(|b| b.pc == pc)
            .and_then(|b| b.condition.as_ref())
        else {
            return false;
        };
        let depth = self.dbg.depth();
        if depth == 0 {
            return false;
        }
        let slot = match cond.operand.strip_prefix('$') {
            Some(n) => n.parse().ok(),
            None => resolve_local_slot(self, &cond.operand).ok(),
        };
        match slot.and_then(|slot| self.dbg.local(depth - 1, slot as u32)) {
            Some(v) => !cond.holds(v.as_int()),
            None => false,
        }
    }

    fn sync_vm_breakpoints(&mut self) {
        let pcs: Vec<usize> = self.breakpoints.iter().map(|b| b.pc).collect();
        self.dbg.with_controller(|c| {
            c.clear_breakpoints();
            for pc in pcs {
                c.add_breakpoint(pc);
            }
        });
    }

    /// `(path, line, column)` of op `pc`.
    fn resolve_pc_location(&self, pc: usize) -> Option<(String, u32, u32)> {
        let (file, line, column) = self.op_lines.get(pc).copied().flatten()?;
        let path = self.artifacts.debug.source_files.get(file as usize)?;
        Some((resolve_path(path, Some(&self.base_dir)).display().to_string(), line, column))
    }

    /// Exact loc at `ip`, else nearest known loc in the **same function**.
    ///
    /// Does not walk into callees / stdlib (a raw ±64 scan did).
    pub fn nearest_source_location(&self, ip: usize) -> Option<(String, u32, u32)> {
        if let Some(loc) = self.resolve_pc_location(ip) {
            return Some(loc);
        }
        let (start, end) = self.function_pc_range(ip);
        for cand in ip + 1..end {
            if let Some(loc) = self.resolve_pc_location(cand) {
                return Some(loc);
            }
        }
        for cand in (start..ip).rev() {
            if let Some(loc) = self.resolve_pc_location(cand) {
                return Some(loc);
            }
        }
        None
    }

    fn function_pc_range(&self, ip: usize) -> (usize, usize) {
        let mut starts: Vec<usize> = self
            .artifacts
            .functions
            .iter()
            .map(|s| s.body_id as usize)
            .collect();
        starts.sort_unstable();
        starts.dedup();
        let len = self.artifacts.debug.debug_locs.len();
        let start = starts
            .iter()
            .copied()
            .rev()
            .find(|&s| s <= ip)
            .unwrap_or(0);
        let end = starts
            .iter()
            .copied()
            .find(|&s| s > ip)
            .unwrap_or(len);
        (start, end)
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
        let pcs: Vec<usize> = matched.iter().map(|s| s.body_id as usize).collect();
        let name = matched[0].name.clone();
        Ok((pcs, name))
    }
}

/// Slot shown next to a variable: where its value is now (single-slot
/// variables), else its first component slot.
fn var_slot(var: &compiler::debug_vars::DebugVar, pc: u32) -> Option<usize> {
    use compiler::debug_vars::DebugVarLoc;
    match &var.loc {
        DebugVarLoc::Slot(home) => Some(var.slot_at(pc).unwrap_or(*home) as usize),
        _ => None,
    }
}

/// One step of a `print` path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathStep {
    Field(String),
    Index(usize),
}

/// `a.b[1]` → (`a`, [Field(b), Index(1)]).
fn split_path(expr: &str) -> Result<(&str, Vec<PathStep>), String> {
    let expr = expr.trim();
    let base_end = expr.find(['.', '[']).unwrap_or(expr.len());
    let base = &expr[..base_end];
    if base.is_empty() {
        return Err(format!("bad expression `{expr}`"));
    }
    let mut steps = Vec::new();
    let mut rest = &expr[base_end..];
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('.') {
            let end = r.find(['.', '[']).unwrap_or(r.len());
            if end == 0 {
                return Err(format!("bad expression `{expr}`"));
            }
            steps.push(PathStep::Field(r[..end].to_string()));
            rest = &r[end..];
        } else if let Some(r) = rest.strip_prefix('[') {
            let close = r.find(']').ok_or_else(|| format!("missing `]` in `{expr}`"))?;
            let index = r[..close]
                .trim()
                .parse()
                .map_err(|_| format!("index must be a non-negative integer in `{expr}`"))?;
            steps.push(PathStep::Index(index));
            rest = &r[close + 1..];
        } else {
            return Err(format!("bad expression `{expr}`"));
        }
    }
    Ok((base, steps))
}

pub fn symbol_at_pc(functions: &[FnSym], pc: usize) -> Option<&str> {
    let mut best: Option<&FnSym> = None;
    for s in functions {
        let entry = s.body_id as usize;
        if entry <= pc && best.map(|b| entry >= b.body_id as usize).unwrap_or(true) {
            best = Some(s);
        }
    }
    best.map(|s| s.name.as_str())
}

fn locals_for_pc(session: &DebugSession, pc: usize) -> Option<&[(String, u32)]> {
    let name = symbol_at_pc(&session.artifacts.functions, pc)?;
    session
        .artifacts
        .functions
        .iter()
        .find(|s| s.name == name)
        .map(|s| s.locals.as_slice())
}

fn resolve_local_slot(session: &DebugSession, name: &str) -> Result<usize, String> {
    let ip = session.current_ip();
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
fn find_fn_decl_line(text: &str, name: &str) -> Option<u32> {
    let needles = [
        format!("fn {name}"),
        format!("macro {name}"),
        format!("derive {name}"),
        format!("attr {name}"),
    ];
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        for needle in &needles {
            if trimmed.starts_with(needle.as_str())
                && trimmed
                    .as_bytes()
                    .get(needle.len())
                    .is_none_or(|c| !c.is_ascii_alphanumeric() && *c != b'_')
            {
                return Some((i + 1) as u32);
            }
        }
    }
    None
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
            .join("../examples/fib.coil")
            .canonicalize()
            .expect("examples/fib.coil")
            .display()
            .to_string()
    }

    fn compile_entry(path: &str, grants: HostGrants) -> Result<DebugSession, ()> {
        let config = ReportConfig::from_cli_flags(false, false).expect("report config");
        DebugSession::compile(
            config,
            path,
            Box::new(std::io::sink()),
            grants,
            Pipeline::workspace_language_extra_roots(),
        )
    }

    fn compile_fib() -> DebugSession {
        compile_entry(&fib_path(), HostGrants::deny_all()).expect("compile fib")
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
            "empty fn replace must sync the runtime; reason={reason:?}"
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
            &Some("examples/fib.coil".into()),
            "examples/fib.coil"
        ));
        assert!(source_matches(
            &Some("/abs/examples/fib.coil".into()),
            "fib.coil"
        ));
        assert!(source_matches(
            &Some("fib.coil".into()),
            "/workspace/examples/fib.coil"
        ));
        assert!(!source_matches(&None, "fib.coil"));
        assert!(!source_matches(&Some("other.coil".into()), "fib.coil"));
    }

    #[test]
    fn resolve_path_joins_base_dir() {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../examples");
        let resolved = resolve_path("fib.coil", Some(&base));
        assert!(
            resolved.exists(),
            "expected fib.coil under examples, got {}",
            resolved.display()
        );
    }

    #[test]
    fn start_paused_at_entry_has_frames_and_continue_finishes() {
        let mut session = compile_fib();
        let reason = session.start_paused_at_entry();
        assert!(
            matches!(
                reason,
                StopReason::Breakpoint { .. } | StopReason::Step | StopReason::Next
            ),
            "reason={reason:?}"
        );
        assert!(session.started());
        assert!(
            !session.stack_frames().is_empty(),
            "stopOnEntry must build a frame"
        );
        let reason = session.continue_exec().expect("continue");
        assert!(
            matches!(reason, StopReason::Halt),
            "reason={reason:?}"
        );
    }

    #[test]
    fn stepi_step_next_finish_and_restart() {
        let mut session = compile_fib();
        session.set_function_breakpoint("fib").expect("break fib");
        assert!(matches!(session.start(), StopReason::Breakpoint { .. }));
        let ip0 = session.current_ip();
        let stepi = session.stepi().expect("stepi");
        assert!(
            matches!(stepi, StopReason::Step | StopReason::Breakpoint { .. }),
            "stepi={stepi:?}"
        );
        assert_ne!(session.current_ip(), ip0, "stepi must advance PC");
        let _ = session.step_in().expect("step");
        let _ = session.step_over().expect("next");
        let finish = session.step_out().expect("finish");
        assert!(
            matches!(
                finish,
                StopReason::Finish | StopReason::Step | StopReason::Next | StopReason::Breakpoint { .. }
            ),
            "finish={finish:?}"
        );
        session
            .replace_function_breakpoints(&[])
            .expect("clear fn bp");
        let again = session.start();
        assert!(
            matches!(again, StopReason::Halt),
            "restart after delete should finish; reason={again:?}"
        );
    }

    #[test]
    fn list_uses_nearest_debug_loc() {
        let mut session = compile_fib();
        session.set_function_breakpoint("fib").expect("break fib");
        assert!(matches!(session.start(), StopReason::Breakpoint { .. }));
        let listed = session.list_source();
        assert!(
            listed.is_ok(),
            "list should snap to nearest loc, err={listed:?}"
        );
        let (path, line, rows) = listed.unwrap();
        assert!(path.to_string_lossy().contains("fib.coil"), "path={path:?}");
        assert!(line >= 1);
        assert!(rows.iter().any(|(_, mark, _)| *mark));
    }

    #[test]
    fn host_grants_apply_to_debug_compile() {
        let dir = std::env::temp_dir().join(format!("coil_dbg_grant_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("attach.coil");
        fs::write(
            &path,
            "use io::{stdout};\nfn main() { let _ = stdout().attach(0, 0, 0, 0, 0); }\n",
        )
        .unwrap();
        let denied = compile_entry(path.to_str().unwrap(), HostGrants::deny_all());
        assert!(denied.is_err(), "ungranted attach must fail typecheck");
        let mut grants = HostGrants::deny_all();
        grants.allow_attach = true;
        compile_entry(path.to_str().unwrap(), grants).expect("granted attach");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn condition_parse_forms() {
        let c = Condition::parse("n >= 3").unwrap();
        assert_eq!((c.operand.as_str(), c.op, c.value), ("n", CmpOp::Ge, 3));
        let c = Condition::parse("$2 == true").unwrap();
        assert_eq!((c.operand.as_str(), c.op, c.value), ("$2", CmpOp::Eq, 1));
        let c = Condition::parse("x != -1").unwrap();
        assert_eq!((c.op, c.value), (CmpOp::Ne, -1));
        assert!(Condition::parse("n").is_err());
        assert!(Condition::parse("== 3").is_err());
        assert!(Condition::parse("n == abc").is_err());
    }

    #[test]
    fn find_fn_decl_line_sees_macros() {
        let text = "/// docs\nmacro twice(Expr e) -> Code { return e; }\nfn main() {}\n";
        assert_eq!(find_fn_decl_line(text, "twice"), Some(2));
        assert_eq!(find_fn_decl_line(text, "main"), Some(3));
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
            debug_lines: Vec::new(),
            inline_scopes: Vec::new(),
        };
        let ret_off = text.find("return").unwrap() as u32;
        debug.debug_locs[2] = DebugLoc::span(0, ret_off, ret_off + 6);
        let idx = LineIndex::build(&debug, None);
        let base = Path::new(&path).file_name().unwrap().to_str().unwrap();
        let pcs = idx.pcs_for_line(Some(base), 2, &path);
        assert!(pcs.contains(&2), "pcs={pcs:?}");
        let _ = fs::remove_file(&tmp);
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
            debug_lines: Vec::new(),
            inline_scopes: Vec::new(),
        };
        let ret_off = text.find("return").unwrap() as u32;
        debug.debug_locs[3] = DebugLoc::span(0, ret_off, ret_off + 6);
        let idx = LineIndex::build(&debug, None);
        let pcs = idx.pcs_for_line(Some(&path), 2, &path);
        assert!(pcs.contains(&3), "pcs={pcs:?}");
        let _ = fs::remove_file(&tmp);
    }
}
