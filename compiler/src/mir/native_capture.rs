//! Hand each body's MIR to a native backend (`coil-native`).
//!
//! While a capture is open (`coil`), the pipeline
//! copies every body's MIR, as lowered from HIR. The backend reads the bodies it can compile and maps them to the
//! entry ids by name.

use std::cell::RefCell;

use std::collections::HashMap;

use super::MirFunc;
use crate::bodies::Label;

/// One function body's MIR.
#[derive(Clone, Debug)]
pub struct NativeBody {
    /// Function name, as in the compiler's entry table.
    pub name: String,
    /// Label `CALL`s of this body target (`MirInst::Call::target`).
    pub entry: Option<Label>,
    pub mir: MirFunc,
}

/// An open capture.
struct Capture {
    bodies: Vec<NativeBody>,
    /// Keep each body as lowered, with no MIR pass run on it.
    raw: bool,
}

thread_local! {
    static NATIVE_CAPTURE: RefCell<Option<Capture>> = const { RefCell::new(None) };
}

/// Start capturing for a backend that optimizes (drops an earlier capture).
pub fn start_native_capture() {
    NATIVE_CAPTURE.with(|c| *c.borrow_mut() = Some(Capture { bodies: Vec::new(), raw: false }));
}

/// Start capturing bodies as lowered from HIR, with no MIR pass run on them
/// (drops an earlier capture): what the MIR interpreter runs, so that its
/// output is the reference the optimized engines are compared against. A
/// bug in a pass cannot then hide on both sides of the comparison.
pub fn start_raw_capture() {
    NATIVE_CAPTURE.with(|c| *c.borrow_mut() = Some(Capture { bodies: Vec::new(), raw: true }));
}

/// Run `f` with the capture closed: a program compiled inside another
/// (a macro's expansion program) has bodies and labels of its own, which
/// the outer program's capture must not take.
pub fn without_native_capture<R>(f: impl FnOnce() -> R) -> R {
    let saved = NATIVE_CAPTURE.with(|c| c.borrow_mut().take());
    let out = f();
    NATIVE_CAPTURE.with(|c| *c.borrow_mut() = saved);
    out
}

/// Any capture is open: the compile hands its bodies over.
pub(crate) fn capture_open() -> bool {
    NATIVE_CAPTURE.with(|c| c.borrow().is_some())
}

/// End the capture and return its bodies.
pub fn take_native_capture() -> Vec<NativeBody> {
    finished(NATIVE_CAPTURE.with(|c| c.borrow_mut().take()).map(|c| c.bodies).unwrap_or_default())
}

/// The bodies captured so far, as [`take_native_capture`] would return
/// them, with the capture left open: compile-time evaluation runs in the
/// middle of a compile.
pub fn peek_native_capture() -> Vec<NativeBody> {
    finished(NATIVE_CAPTURE.with(|c| c.borrow().as_ref().map(|c| c.bodies.clone())).unwrap_or_default())
}

/// `bodies` as an engine runs them.
fn finished(mut bodies: Vec<NativeBody>) -> Vec<NativeBody> {
    super::native_expand::apply_all(bodies.iter_mut().map(|b| &mut b.mir));
    for body in &bodies {
        super::specialize::check_expanded(&body.mir);
    }
    bodies
}

/// A raw capture for compile-time evaluation, closed when this drops, so a
/// compile that fails or panics leaves none open. With a capture already
/// open this does nothing, and evaluation reads that one.
pub struct ComptimeCapture {
    owner: bool,
}

impl ComptimeCapture {
    pub fn open() -> Self {
        let owner = !capture_open();
        if owner {
            NATIVE_CAPTURE.with(|c| *c.borrow_mut() = Some(Capture { bodies: Vec::new(), raw: true }));
        }
        Self { owner }
    }

    /// End the capture and return its bodies.
    pub fn take(mut self) -> Vec<NativeBody> {
        self.owner = false;
        take_native_capture()
    }
}

impl Drop for ComptimeCapture {
    fn drop(&mut self) {
        if self.owner {
            NATIVE_CAPTURE.with(|c| *c.borrow_mut() = None);
        }
    }
}

/// Record `bodies` when a capture is open. Unless it is a raw one
/// ([`start_raw_capture`]), each comes after the MIR passes
/// ([`super::specialize::optimize_mir`]) at `opts`' level: native code
/// gets the same loop rewrites (vector loops, byte scans, bulk copies and
/// fills, which call the runtime's kernels). A generator, a body with a
/// cleanup pad, a debugger's or a coverage build's keep their MIR as
/// lowered.
pub fn capture_native_bodies<'a>(bodies: impl Iterator<Item = (&'a str, Option<Label>, &'a MirFunc)>, opts: &crate::opt::OptimizeOptions) {
    NATIVE_CAPTURE.with(|c| {
        if let Some(capture) = c.borrow_mut().as_mut() {
            let raw = capture.raw;
            capture.bodies.extend(bodies.map(|(name, entry, mir)| {
                let mut mir = mir.clone();
                super::specialize::check_lowered(&mir);
                if !raw && optimizable(&mir) {
                    super::specialize::optimize_mir(&mut mir, opts);
                }
                NativeBody { name: name.to_string(), entry, mir }
            }));
        }
    });
}

/// Whether native code takes `func` through the dense passes.
fn optimizable(func: &MirFunc) -> bool {
    let yields = func.blocks.iter().flat_map(|b| &b.insts).any(|i| matches!(i, super::MirInst::Yield { .. }));
    !yields && func.cleanup.is_none() && !func.debug_keep && !func.keep_lines && func.emit_refusal.is_none()
}

/// A [`super::MirInst::ByteScan`] or [`super::MirInst::VecLoop`] as the
/// the vector scan / loop runs it, over a frame of word slots native code
/// fills: slot 0 is the loop's `dest` (a reduce's or push's start value
/// goes in first), slot 1 its block position (`pos`), and `inputs[i]` is
/// in slot `2 + i`.
#[derive(Clone, Debug)]
pub struct VectorOp {
    pub inputs: Vec<super::ValueId>,
    /// The start value slot 0 takes (`VecLoop` reduce / push).
    pub start: Option<super::ValueId>,
    /// The body words, each `opcode | operand << 32`.
    pub body: Vec<u64>,
    /// The op's first operand byte: the body length with the scan's /
    /// loop's flags (`common::simd::SCAN_*` / `LOOP_POS`).
    pub flags: u8,
}

/// `inst` as a [`VectorOp`]: `None` for another inst or a body the runtime's
/// encoding cannot hold.
pub fn vector_op(inst: &super::MirInst) -> Option<VectorOp> {
    use super::inst::VecLoopKind;
    let mut inputs: Vec<super::ValueId> = Vec::new();
    for v in inst.operands() {
        if !inputs.contains(&v) {
            inputs.push(v);
        }
    }
    if inputs.len() + 2 > usize::from(u8::MAX) {
        return None;
    }
    let len = inputs.iter().chain(inst.dests().collect::<Vec<_>>().iter()).map(|v| v.index() + 1).max().unwrap_or(0);
    let mut regs = vec![0u8; len];
    for (i, v) in inputs.iter().enumerate() {
        regs[v.index()] = (i + 2) as u8;
    }
    let words = |body: Vec<common::LaneWord>| body.iter().map(|b| b.to_bits()).collect();
    match inst {
        super::MirInst::ByteScan { inclusive, backward, streams, scalars, expr, .. } => {
            let body = super::vector_body::byte_scan_body(expr, streams, scalars, &regs)?;
            let mut flags = body.len() as u8;
            if *inclusive {
                flags |= common::simd::SCAN_INCLUSIVE;
            }
            if *backward {
                flags |= common::simd::SCAN_BACKWARD;
            }
            Some(VectorOp { inputs, start: None, body: words(body), flags })
        }
        super::MirInst::VecLoop { pos, kind, .. } => {
            let p = if pos.is_some() { 1 } else { 0 };
            let body = super::vector_body::vec_loop_body(kind, &regs, 0, p)?;
            let start = match kind {
                VecLoopKind::Reduce { init, .. } | VecLoopKind::Push { out: init, .. } => Some(*init),
                VecLoopKind::Store { .. } => None,
            };
            let flags = body.len() as u8 | if pos.is_some() { common::simd::LOOP_POS } else { 0 };
            Some(VectorOp { inputs, start, body: words(body), flags })
        }
        _ => None,
    }
}

/// What `bodies` load through FFI: whether any calls into a shared library,
/// and the names `dload` is given as literals, in order, once each. `strings`
/// is the program's string table.
pub fn ffi_use(bodies: &[NativeBody], strings: &[String]) -> (bool, Vec<String>) {
    let mut uses = false;
    let mut names: Vec<String> = Vec::new();
    for b in bodies {
        let literals: std::collections::HashMap<super::ValueId, u32> = b
            .mir
            .blocks
            .iter()
            .flat_map(|blk| &blk.insts)
            .filter_map(|i| match i {
                super::MirInst::String { dest, idx } => Some((*dest, *idx)),
                _ => None,
            })
            .collect();
        for inst in b.mir.blocks.iter().flat_map(|blk| &blk.insts) {
            let super::MirInst::Ffi { op, args, .. } = inst else { continue };
            uses = true;
            if *op != super::MirFfiOp::Load {
                continue;
            }
            let name = args.first().and_then(|a| literals.get(a)).and_then(|&s| strings.get(s as usize));
            if let Some(name) = name
                && !names.contains(name)
            {
                names.push(name.clone());
            }
        }
    }
    (uses, names)
}

/// A `Call` of a captured body whose arguments are all constants.
#[derive(Clone, Debug)]
pub struct ConstCall {
    /// Index of its body in the capture, and its block and op there.
    at: (usize, usize, usize),
    /// The body it is in.
    pub body: String,
    pub target: Label,
    pub args: Vec<ConstArg>,
    pub loc: common::DebugLoc,
}

/// A constant argument: a word, or an index in the string table.
#[derive(Clone, Copy, Debug)]
pub enum ConstArg {
    Word(u64),
    Text(u32),
}

/// What a [`ConstCall`] computed.
pub enum ConstResult {
    Word(u64),
    Text(u32),
}

/// The one-word `Call`s of the open capture with constant arguments.
pub fn capture_const_calls() -> Vec<ConstCall> {
    NATIVE_CAPTURE.with(|c| {
        let c = c.borrow();
        let Some(capture) = c.as_ref() else { return Vec::new() };
        let mut out = Vec::new();
        for (bi, b) in capture.bodies.iter().enumerate() {
            let consts: HashMap<super::ValueId, ConstArg> = b
                .mir
                .blocks
                .iter()
                .flat_map(|blk| &blk.insts)
                .filter_map(|i| match i {
                    super::MirInst::Const { dest, c } => Some((*dest, ConstArg::Word(const_word(*c)))),
                    super::MirInst::String { dest, idx } => Some((*dest, ConstArg::Text(*idx))),
                    _ => None,
                })
                .collect();
            for (blk_i, blk) in b.mir.blocks.iter().enumerate() {
                for (i, inst) in blk.insts.iter().enumerate() {
                    let super::MirInst::Call { dest, dest_hi: None, target, args } = inst else { continue };
                    let Some(args) = args.iter().map(|a| consts.get(a).copied()).collect::<Option<Vec<_>>>() else { continue };
                    out.push(ConstCall { at: (bi, blk_i, i), body: b.name.clone(), target: *target, args, loc: b.mir.op_loc(*dest) });
                }
            }
        }
        out
    })
}

fn const_word(c: super::MirConst) -> u64 {
    match c {
        super::MirConst::I64(v) => v as u64,
        super::MirConst::F64(bits) => bits,
        super::MirConst::Bool(b) => u64::from(b),
    }
}

/// Replace each call with what it computed, in the open capture.
pub fn replace_const_calls(results: &[(ConstCall, ConstResult)]) {
    NATIVE_CAPTURE.with(|c| {
        let mut c = c.borrow_mut();
        let Some(capture) = c.as_mut() else { return };
        for (call, result) in results {
            let (bi, blk, i) = call.at;
            let mir = &mut capture.bodies[bi].mir;
            let super::MirInst::Call { dest, .. } = mir.blocks[blk].insts[i] else { continue };
            let by_type = match (result, mir.ty(dest)) {
                (ConstResult::Text(idx), _) => Some(super::MirInst::String { dest, idx: *idx }),
                (ConstResult::Word(w), super::MirTy::F64) => Some(super::MirInst::Const { dest, c: super::MirConst::F64(*w) }),
                (ConstResult::Word(w), super::MirTy::Bool) => Some(super::MirInst::Const { dest, c: super::MirConst::Bool(*w != 0) }),
                (ConstResult::Word(w), super::MirTy::I64) => Some(super::MirInst::Const { dest, c: super::MirConst::I64(*w as i64) }),
                _ => None,
            };
            if let Some(inst) = by_type {
                mir.blocks[blk].insts[i] = inst;
            }
        }
    });
}
