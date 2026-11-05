//! Hand each body's MIR to a native backend (`coil-native`).
//!
//! While a capture is open (`coil --jit`), `IlModule::optimize_and_flatten`
//! copies every body's MIR, as lowered from HIR, before the VM tiers take
//! it. The backend reads the bodies it can compile and maps them to the
//! bytecode entry PCs by name.

use std::cell::RefCell;

use super::MirFunc;
use crate::il::Label;

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

pub fn native_capture_active() -> bool {
    NATIVE_CAPTURE.with(|c| c.borrow().is_some())
}

/// End the capture and return its bodies, each call of a twinned host
/// native now a call of its Coil twin ([`super::native_twins`]).
pub fn take_native_capture() -> Vec<NativeBody> {
    let mut bodies = NATIVE_CAPTURE.with(|c| c.borrow_mut().take()).map(|c| c.bodies).unwrap_or_default();
    let entries = bodies.iter().filter_map(|b| Some((b.name.clone(), b.entry?))).collect();
    super::native_twins::apply(bodies.iter_mut().map(|b| (b.name.as_str(), &mut b.mir)), &entries);
    super::native_expand::apply_all(bodies.iter_mut().map(|b| (b.name.as_str(), &mut b.mir)));
    bodies
}

/// Record `bodies` when a capture is open. Unless it is a raw one
/// ([`start_raw_capture`]), each comes after the MIR passes the dense tier
/// runs ([`super::specialize::optimize_mir`]) at `opts`' level: native code
/// gets the same loop rewrites (vector loops, byte scans, bulk copies and
/// fills, which call the runtime's kernels). A generator, a body with a
/// cleanup pad, a debugger's or a coverage build's keep their MIR as
/// lowered.
pub fn capture_native_bodies<'a>(bodies: impl Iterator<Item = (&'a str, Option<Label>, &'a MirFunc)>, opts: &crate::il::opt::OptimizeOptions) {
    NATIVE_CAPTURE.with(|c| {
        if let Some(capture) = c.borrow_mut().as_mut() {
            let raw = capture.raw;
            capture.bodies.extend(bodies.map(|(name, entry, mir)| {
                let mut mir = mir.clone();
                if !raw && optimizable(&mir) {
                    super::specialize::optimize_mir(&mut mir, opts, false);
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
/// VM's `VScan` / `VLoop` runs it, over a frame of word slots native code
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

/// `inst` as a [`VectorOp`]: `None` for another inst or a body the VM's
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
    let words = |body: Vec<common::Byte>| body.iter().map(|b| u64::from(*b.bytecode() as u8) | (u64::from(b.operand_u32()) << 32)).collect::<Vec<_>>();
    match inst {
        super::MirInst::ByteScan { inclusive, backward, streams, scalars, expr, .. } => {
            let body = super::emit::byte_scan_body(expr, streams, scalars, &regs)?;
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
            let body = super::emit::vec_loop_body(kind, &regs, 0, p)?;
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
