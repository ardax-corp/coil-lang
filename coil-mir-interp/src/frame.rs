//! An activation's frames and what the collector reads from them.

use std::collections::HashSet;
use std::ffi::c_void;

use compiler::native_mir::{MirFunc, MirInst, MirTy, Terminator, ValueId, heap_word, held_words, liveness, opaque_words, pair_words, pointer_words, ty_of};
use machine::native::{NativeCtx, ROOTS_SCAN, SITES_SCAN};

use crate::Body;

/// `Frame::ip` at a safepoint: the frame is at the start of its block, past
/// its φs, with no op running.
pub(crate) const AT_BLOCK_START: u32 = u32::MAX;

/// Registers past which a call is a stack overflow: 128 MiB of words.
pub(crate) const MAX_WORDS: usize = 1 << 24;

/// One call of a body.
pub(crate) struct Frame {
    pub(crate) body: *const Body,
    /// Its first register in [`Activation::regs`].
    pub(crate) base: usize,
    /// The block it runs (a position in `MirFunc::blocks`).
    pub(crate) block: u32,
    /// The op it runs, or [`AT_BLOCK_START`]. Below the top frame, the call.
    pub(crate) ip: u32,
    /// A cleanup pad's homes (its body's frame's copy); null: this frame's
    /// own, past its registers.
    pub(crate) homes: *mut u64,
}

/// The calls of one entry into the interpreter, innermost last.
pub(crate) struct Activation {
    pub(crate) ctx: *mut NativeCtx,
    pub(crate) frames: Vec<Frame>,
    pub(crate) regs: Vec<u64>,
    /// Argument words of the op running.
    pub(crate) scratch: Vec<u64>,
    root_record: [u64; 4],
    site_record: [u64; 4],
}

impl Activation {
    pub(crate) fn new(ctx: *mut NativeCtx) -> Self {
        let (frames, regs, scratch) = POOL.with(|p| p.borrow_mut().pop()).unwrap_or_default();
        Self { ctx, frames, regs, scratch, root_record: [0; 4], site_record: [0; 4] }
    }

    /// Link this activation's records into the context, for as long as it
    /// runs. It must not move from here on.
    ///
    /// # Safety
    /// `self.ctx` is live; [`Self::unlink`] runs before `self` goes.
    pub(crate) unsafe fn link(&mut self) {
        let data = std::ptr::from_mut(self) as u64;
        let ctx = unsafe { &mut *self.ctx };
        self.root_record = [ctx.roots, ROOTS_SCAN, scan_roots as *const () as u64, data];
        ctx.roots = self.root_record.as_ptr() as u64;
        self.site_record = [ctx.calls, SITES_SCAN, scan_sites as *const () as u64, data];
        ctx.calls = self.site_record.as_ptr() as u64;
    }

    /// # Safety
    /// As [`Self::link`].
    pub(crate) unsafe fn unlink(&mut self) {
        let ctx = unsafe { &mut *self.ctx };
        ctx.roots = self.root_record[0];
        ctx.calls = self.site_record[0];
    }

    pub(crate) fn release(mut self) {
        self.frames.clear();
        self.regs.clear();
        self.scratch.clear();
        let parts = (std::mem::take(&mut self.frames), std::mem::take(&mut self.regs), std::mem::take(&mut self.scratch));
        POOL.with(|p| {
            let mut pool = p.borrow_mut();
            if pool.len() < 8 {
                pool.push(parts);
            }
        });
    }
}

type Parts = (Vec<Frame>, Vec<u64>, Vec<u64>);

thread_local! {
    /// Storage of finished activations: a callback run per element should
    /// not allocate its frames each time.
    static POOL: std::cell::RefCell<Vec<Parts>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The words a collection must see at each point of a body, by position.
pub(crate) struct RootMaps {
    live_in: Vec<HashSet<ValueId>>,
    live_after: Vec<Vec<HashSet<ValueId>>>,
    held: Vec<Vec<HashSet<ValueId>>>,
    /// Non-heap values that hold an object for sure.
    pointers: HashSet<ValueId>,
    /// Values that may hold one: the collector pins, never rewrites.
    ambiguous: Vec<ValueId>,
    /// Homes holding a heap word, and in an opaque body the `int` ones.
    heap_homes: Vec<u16>,
    ambiguous_homes: Vec<u16>,
}

impl RootMaps {
    fn new(f: &MirFunc) -> Self {
        let (live_in, live_after) = liveness(f);
        let pointers = pointer_words(f);
        let mut ambiguous = opaque_words(f);
        ambiguous.extend(pair_words(f, &pointers));
        let heap_homes = (0..f.homes.len() as u16).filter(|&h| heap_word(f.homes[h as usize])).collect();
        let ambiguous_homes = if f.opaque_words { (0..f.homes.len() as u16).filter(|&h| f.homes[h as usize] == MirTy::I64).collect() } else { Vec::new() };
        Self { live_in, live_after, held: held_words(f), pointers, ambiguous, heap_homes, ambiguous_homes }
    }

    /// What lives at `ip` of block `bi`: past the φs at a safepoint; else
    /// what lives after the op and what it reads, which the call it makes
    /// may move.
    fn live_at(&self, f: &MirFunc, bi: usize, ip: u32) -> HashSet<ValueId> {
        let b = &f.blocks[bi];
        if ip == AT_BLOCK_START {
            let phis = b.insts.iter().take_while(|i| i.is_phi()).count();
            let mut live = if phis == 0 { self.live_in[bi].clone() } else { self.live_after[bi][phis - 1].clone() };
            live.extend(self.held[bi][phis].iter().copied());
            return live;
        }
        let ip = ip as usize;
        let Some(inst) = b.insts.get(ip) else {
            // At the terminator: nothing runs there that collects.
            let mut live: HashSet<ValueId> = self.held[bi].last().cloned().unwrap_or_default();
            if let Some(t) = &b.term {
                live.extend(term_uses(t));
            }
            return live;
        };
        let mut live = self.live_after[bi][ip].clone();
        live.extend(operands(inst));
        live.extend(self.held[bi][ip].iter().copied());
        live
    }
}

fn operands(inst: &MirInst) -> impl Iterator<Item = ValueId> {
    inst.operands().into_iter()
}

fn term_uses(t: &Terminator) -> Vec<ValueId> {
    match t {
        Terminator::Br { cond, .. } => vec![*cond],
        Terminator::JumpIfMatch { scrutinee, .. } => vec![*scrutinee],
        Terminator::Return { lo, hi } => lo.iter().chain(hi).copied().collect(),
        _ => Vec::new(),
    }
}

/// [`machine::native::RootScanFn`] of an [`Activation`].
unsafe fn scan_roots(data: *mut c_void, visit: &mut dyn FnMut(*mut u64, bool)) {
    let act = data.cast::<Activation>();
    // SAFETY: the record is linked while the activation runs, and it keeps
    // its frames as they are while anything can reach here. Raw accesses:
    // the interpreter's loop holds its own pointers into them.
    unsafe {
        let n = (&(*act).frames).len();
        let regs = (*act).regs.as_mut_ptr();
        for i in (0..n).rev() {
            let frame = &*(&(*act).frames).as_ptr().add(i);
            let body = &*frame.body;
            let f = &body.mir;
            let maps = body.roots.get_or_init(|| RootMaps::new(f));
            let live = maps.live_at(f, frame.block as usize, frame.ip);
            let at = regs.add(frame.base);
            // A stable order: a generator's parked words are written back
            // by position.
            let mut words: Vec<ValueId> = live.iter().copied().filter(|&v| heap_word(ty_of(f, v)) || maps.pointers.contains(&v)).collect();
            words.sort_unstable();
            for v in words {
                visit(at.add(v.index()), false);
            }
            for &v in maps.ambiguous.iter().filter(|v| live.contains(v)) {
                visit(at.add(v.index()), true);
            }
            // A pad's homes are the unwound frame's, in a record of the
            // runtime's: visited here too, they would be forwarded twice.
            if frame.homes.is_null() {
                let homes = frame_homes(frame, at, f);
                for &h in &maps.heap_homes {
                    visit(homes.add(usize::from(h)), false);
                }
                for &h in &maps.ambiguous_homes {
                    visit(homes.add(usize::from(h)), true);
                }
            }
        }
    }
}

/// [`machine::native::SiteScanFn`] of an [`Activation`].
unsafe fn scan_sites(data: *mut c_void, out: &mut Vec<u32>) {
    let act = data.cast::<Activation>();
    // SAFETY: as [`scan_roots`].
    unsafe {
        let n = (&(*act).frames).len();
        for i in (0..n).rev() {
            let frame = &*(&(*act).frames).as_ptr().add(i);
            let body = &*frame.body;
            if frame.ip != AT_BLOCK_START {
                out.push(body.site_at(frame.block, frame.ip));
            }
        }
    }
}

/// Where `frame`'s homes are: `regs` are its registers.
pub(crate) fn frame_homes(frame: &Frame, regs: *mut u64, f: &MirFunc) -> *mut u64 {
    if frame.homes.is_null() {
        // SAFETY: a frame's homes follow its registers.
        unsafe { regs.add(f.types.len()) }
    } else {
        frame.homes
    }
}
