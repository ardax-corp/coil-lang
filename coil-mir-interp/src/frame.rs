//! An activation's frames.

use std::ffi::c_void;

use compiler::native_mir::MirFunc;
use machine::native::{NativeCtx, SITES_SCAN};

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
    /// Under a debugger: each frame's locals.
    pub(crate) locals: Vec<crate::debug::FrameLocals>,
    /// It stopped at a yield or a wait: its frames stay ([`crate::exec::entry`]).
    pub(crate) parked: bool,
    /// The top frame runs its op again, not the one after it.
    pub(crate) rerun: bool,
    /// The op parked at takes the resume's word as its `send` and runs again
    /// (`yield from`).
    pub(crate) rerun_op: bool,
    /// What the resumer sent to an op that runs again.
    pub(crate) send: Option<u64>,
    site_record: [u64; 4],
}

impl Activation {
    pub(crate) fn new(ctx: *mut NativeCtx) -> Self {
        let (frames, regs, scratch) = POOL.with(|p| p.borrow_mut().pop()).unwrap_or_default();
        Self { ctx, frames, regs, scratch, locals: Vec::new(), parked: false, rerun: false, rerun_op: false, send: None, site_record: [0; 4] }
    }

    /// Link this activation's call-site record into the context, for as
    /// long as it runs. It must not move from here on.
    ///
    /// # Safety
    /// `self.ctx` is live; [`Self::unlink`] runs before `self` goes.
    pub(crate) unsafe fn link(&mut self) {
        let data = std::ptr::from_mut(self) as u64;
        let ctx = unsafe { &mut *self.ctx };
        self.site_record = [ctx.calls, SITES_SCAN, scan_sites as *const () as u64, data];
        ctx.calls = self.site_record.as_ptr() as u64;
    }

    /// # Safety
    /// As [`Self::link`].
    pub(crate) unsafe fn unlink(&mut self) {
        let ctx = unsafe { &mut *self.ctx };
        ctx.calls = self.site_record[0];
    }

    pub(crate) fn release(mut self) {
        self.frames.clear();
        self.regs.clear();
        self.scratch.clear();
        self.locals.clear();
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

/// [`machine::native::SiteScanFn`] of an [`Activation`].
unsafe fn scan_sites(data: *mut c_void, out: &mut Vec<u32>) {
    let act = data.cast::<Activation>();
    // SAFETY: the record is linked while the activation runs, and it keeps
    // its frames as they are while anything can reach here. Raw accesses:
    // the interpreter's loop holds its own pointers into them.
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
