//! The word sequences of a `VecLoop` and a `ByteScan`, as the runtime's lane
//! code (`coil_native_vector`) reads them.

use common::{LaneOp, LaneWord, dense};

use super::inst::{MirCmpOp, ValueId};

/// A `VecLoop`'s `VLoop` body: each tree node into the next free vector
/// register (freed again after the node), a reduce's `VReduce` into the
/// loop's `dest` slot `d`; slot `p` holds the block position for an index.
pub(crate) fn vec_loop_body(kind: &super::inst::VecLoopKind, regs: &[u8], d: u8, p: u8) -> Option<Vec<LaneWord>> {
    use super::inst::{VecExpr, VecLoopKind};
    use common::simd;

    struct Ctx<'a> {
        out: Vec<LaneWord>,
        next: u8,
        regs: &'a [u8],
        p: u8,
        /// [`Ctx::shared`]'s registers by node, the readers each node has
        /// left, and the registers free again.
        memo: std::collections::HashMap<VecExpr, u8>,
        uses: std::collections::HashMap<VecExpr, usize>,
        free: Vec<u8>,
    }

    impl Ctx<'_> {
        /// One store's value or the fold's term in a register: each
        /// distinct node once ([`Ctx::shared`]), or, when that needs more
        /// than the registers, as the tree ([`Ctx::eval`]).
        fn value(&mut self, e: &VecExpr) -> Option<u8> {
            let at = self.out.len();
            self.next = 0;
            self.memo.clear();
            self.uses.clear();
            self.free.clear();
            self.count(e, false);
            if let Some(r) = self.shared(e, true) {
                return Some(r);
            }
            self.out.truncate(at);
            self.next = 0;
            self.eval(e)
        }

        /// How many parents read each shared node (`owned`: the node a
        /// `SELECT` / `VFma` writes into, computed again for each).
        fn count(&mut self, e: &VecExpr, owned: bool) {
            if !owned {
                let n = self.uses.entry(e.clone()).or_insert(0);
                *n += 1;
                if *n > 1 {
                    return;
                }
            }
            match e {
                VecExpr::Splat { .. } | VecExpr::Index | VecExpr::Load { .. } => {}
                VecExpr::Bin { lhs, rhs, .. } => {
                    self.count(lhs, false);
                    self.count(rhs, false);
                }
                VecExpr::Unary { src, .. } => self.count(src, false),
                VecExpr::Select { mask, then, els } => {
                    self.count(mask, true);
                    self.count(then, false);
                    self.count(els, false);
                }
                VecExpr::Fma { a, b, c, .. } => {
                    self.count(c, true);
                    self.count(a, false);
                    self.count(b, false);
                }
            }
        }

        /// A register for [`Ctx::shared`]: a freed one first.
        fn sreg(&mut self) -> Option<u8> {
            self.free.pop().or_else(|| self.reg())
        }

        /// A parent has read shared node `e`; its register is free after
        /// its last reader.
        fn release(&mut self, e: &VecExpr) {
            if let Some(n) = self.uses.get_mut(e) {
                *n -= 1;
                if *n == 0
                    && let Some(r) = self.memo.remove(e)
                {
                    self.free.push(r);
                }
            }
        }

        /// `e` with a node seen before in this value read from its
        /// register, which is reused once its last reader ran. The node a
        /// `VBin SELECT` / `VFma` writes into is computed again for it, not
        /// shared (`memo` false).
        fn shared(&mut self, e: &VecExpr, memo: bool) -> Option<u8> {
            if memo && let Some(&r) = self.memo.get(e) {
                return Some(r);
            }
            let r = match e {
                VecExpr::Splat { v, ty } => {
                    let r = self.sreg()?;
                    let k = if *ty == dense::TY_F64 { simd::SPLAT_F64 } else { simd::SPLAT_I64 };
                    self.word(LaneOp::VBin, k, r, self.slot(*v), 0);
                    r
                }
                VecExpr::Index => {
                    let (r, p) = (self.sreg()?, self.sreg()?);
                    self.word(LaneOp::VBin, simd::IOTA_I64, r, 0, 0);
                    self.word(LaneOp::VBin, simd::SPLAT_I64, p, self.p, 0);
                    self.word(LaneOp::VBin, simd::IADD64, r, r, p);
                    self.free.push(p);
                    r
                }
                VecExpr::Load { array, off, ty } => {
                    let r = self.sreg()?;
                    self.word(LaneOp::VLoad, *ty, r, self.slot(*array), self.slot(*off));
                    r
                }
                VecExpr::Bin { kind, lhs, rhs } => {
                    let x = self.shared(lhs, true)?;
                    let y = self.shared(rhs, true)?;
                    self.release(lhs);
                    self.release(rhs);
                    let r = self.sreg()?;
                    self.word(LaneOp::VBin, *kind, r, x, y);
                    r
                }
                VecExpr::Unary { kind, src } => {
                    let x = self.shared(src, true)?;
                    self.release(src);
                    let r = self.sreg()?;
                    self.word(LaneOp::VBin, *kind, r, x, 0);
                    r
                }
                VecExpr::Select { mask, then, els } => {
                    let m = self.shared(mask, false)?;
                    let x = self.shared(then, true)?;
                    let y = self.shared(els, true)?;
                    self.word(LaneOp::VBin, simd::SELECT, m, x, y);
                    self.release(then);
                    self.release(els);
                    m
                }
                VecExpr::Fma { ty, a, b, c } => {
                    let z = self.shared(c, false)?;
                    let x = self.shared(a, true)?;
                    let y = self.shared(b, true)?;
                    self.word(LaneOp::VFma, *ty, z, x, y);
                    self.release(a);
                    self.release(b);
                    z
                }
            };
            if memo {
                self.memo.insert(e.clone(), r);
            }
            Some(r)
        }

        fn reg(&mut self) -> Option<u8> {
            let r = self.next;
            if usize::from(r) >= simd::NREGS {
                return None;
            }
            self.next += 1;
            Some(r)
        }

        fn word(&mut self, op: LaneOp, kind: u8, dest: u8, a: u8, b: u8) {
            self.out.push(LaneWord::new(op).with_abc(kind, dest, a, b));
        }

        fn slot(&self, v: ValueId) -> u8 {
            self.regs[v.index()]
        }

        fn eval(&mut self, e: &VecExpr) -> Option<u8> {
            let base = self.next;
            let r = match e {
                VecExpr::Splat { v, ty } => {
                    let r = self.reg()?;
                    let k = if *ty == dense::TY_F64 { simd::SPLAT_F64 } else { simd::SPLAT_I64 };
                    self.word(LaneOp::VBin, k, r, self.slot(*v), 0);
                    r
                }
                // `p + k`: the lane offsets plus the position in `d`.
                VecExpr::Index => {
                    let (r, p) = (self.reg()?, self.reg()?);
                    self.word(LaneOp::VBin, simd::IOTA_I64, r, 0, 0);
                    self.word(LaneOp::VBin, simd::SPLAT_I64, p, self.p, 0);
                    self.word(LaneOp::VBin, simd::IADD64, r, r, p);
                    r
                }
                VecExpr::Load { array, off, ty } => {
                    let r = self.reg()?;
                    self.word(LaneOp::VLoad, *ty, r, self.slot(*array), self.slot(*off));
                    r
                }
                VecExpr::Bin { kind, lhs, rhs } => {
                    let x = self.eval(lhs)?;
                    let y = self.eval(rhs)?;
                    self.word(LaneOp::VBin, *kind, x, x, y);
                    x
                }
                VecExpr::Unary { kind, src } => {
                    let x = self.eval(src)?;
                    self.word(LaneOp::VBin, *kind, x, x, 0);
                    x
                }
                // `VBin SELECT` picks into its dest, which holds the mask.
                VecExpr::Select { mask, then, els } => {
                    let m = self.eval(mask)?;
                    let x = self.eval(then)?;
                    let y = self.eval(els)?;
                    self.word(LaneOp::VBin, simd::SELECT, m, x, y);
                    m
                }
                // `VFma` adds into its dest: `c` first, so it lands at `base`.
                VecExpr::Fma { ty, a, b, c } => {
                    let z = self.eval(c)?;
                    let x = self.eval(a)?;
                    let y = self.eval(b)?;
                    self.word(LaneOp::VFma, *ty, z, x, y);
                    z
                }
            };
            debug_assert_eq!(r, base);
            self.next = base + 1;
            Some(r)
        }
    }

    let mut cx = Ctx {
        out: Vec::new(),
        next: 0,
        regs,
        p,
        memo: std::collections::HashMap::new(),
        uses: std::collections::HashMap::new(),
        free: Vec::new(),
    };
    match kind {
        // A map-and-fold body stores first, as the scalar loop does
        // (`vec_loop` takes only bodies whose fold comes after the stores).
        VecLoopKind::Reduce { ty, fold, term, stores, .. } => {
            for (array, off, ty, value) in stores {
                let v = cx.value(value)?;
                cx.word(LaneOp::VStore, *ty, v, cx.slot(*array), cx.slot(*off));
            }
            let v = cx.value(term)?;
            cx.word(LaneOp::VReduce, *ty, d, v, *fold);
        }
        VecLoopKind::Store { stores } => {
            for (array, off, ty, value) in stores {
                let v = cx.value(value)?;
                cx.word(LaneOp::VStore, *ty, v, cx.slot(*array), cx.slot(*off));
            }
        }
        // The array is in `d` (moved there in front of the loop).
        VecLoopKind::Push { ty, value, .. } => {
            let v = cx.value(value)?;
            cx.word(LaneOp::VStore, *ty | simd::STORE_PUSH, v, d, 0);
        }
    }
    (cx.out.len() <= usize::from(common::simd::LOOP_LEN)).then_some(cx.out)
}

/// A `ByteScan`'s `VScan` body: each stream loaded once into the top
/// vector registers (stream `s` in `v7 - s`), then byte-lane `VBin`s, each
/// tree node into a register counted up from `v0`, so the mask ends in
/// `v0`. A `VLoad`'s index slot holds the stream's offset from the scan
/// position.
pub(crate) fn byte_scan_body(
    expr: &super::inst::LaneExpr,
    streams: &[(ValueId, ValueId)],
    scalars: &[ValueId],
    regs: &[u8],
) -> Option<Vec<LaneWord>> {
    use super::inst::{LaneExpr, LaneVal};
    use common::simd;

    struct Ctx<'a> {
        out: Vec<LaneWord>,
        next: u8,
        /// The lowest stream register: nodes stay below it.
        top: u8,
        scalars: &'a [ValueId],
        regs: &'a [u8],
    }

    impl Ctx<'_> {
        fn reg(&mut self) -> Option<u8> {
            let r = self.next;
            if r >= self.top {
                return None;
            }
            self.next += 1;
            Some(r)
        }

        fn vbin(&mut self, kind: u8, dest: u8, a: u8, b: u8) {
            self.out.push(LaneWord::new(LaneOp::VBin).with_abc(kind, dest, a, b));
        }

        fn stream(&self, s: u8) -> Option<u8> {
            let r = (simd::NREGS - 1).checked_sub(usize::from(s))? as u8;
            (r >= self.top).then_some(r)
        }

        fn eval(&mut self, e: &LaneExpr) -> Option<u8> {
            let pred = |op: MirCmpOp| match op {
                MirCmpOp::Lt => dense::CMP_LT,
                MirCmpOp::Le => dense::CMP_LE,
                MirCmpOp::Gt => dense::CMP_GT,
                MirCmpOp::Ge => dense::CMP_GE,
                MirCmpOp::Eq => dense::CMP_EQ,
                MirCmpOp::Ne => dense::CMP_NE,
            };
            let base = self.next;
            let r = match e {
                // `x == x` / `x != x` on a loaded lane.
                LaneExpr::Const(b) => {
                    let (r, x) = (self.reg()?, self.stream(0)?);
                    let k = if *b { dense::CMP_EQ } else { dense::CMP_NE };
                    self.vbin(simd::B_CMP + k, r, x, x);
                    r
                }
                LaneExpr::Cmp { op, lhs, rhs } => match (*lhs, *rhs) {
                    (LaneVal::Stream(a), LaneVal::Stream(b)) => {
                        let (r, x, y) = (self.reg()?, self.stream(a)?, self.stream(b)?);
                        self.vbin(simd::B_CMP + pred(*op), r, x, y);
                        r
                    }
                    (LaneVal::Stream(a), LaneVal::Scalar(k)) => {
                        let (r, x) = (self.reg()?, self.stream(a)?);
                        let slot = self.regs[self.scalars.get(usize::from(k))?.index()];
                        self.vbin(simd::BS_CMP + pred(*op), r, slot, x);
                        r
                    }
                    (LaneVal::Scalar(_), LaneVal::Stream(_)) => {
                        let flipped = match op {
                            MirCmpOp::Lt => MirCmpOp::Gt,
                            MirCmpOp::Le => MirCmpOp::Ge,
                            MirCmpOp::Gt => MirCmpOp::Lt,
                            MirCmpOp::Ge => MirCmpOp::Le,
                            o => *o,
                        };
                        return self.eval(&LaneExpr::Cmp { op: flipped, lhs: *rhs, rhs: *lhs });
                    }
                    (LaneVal::Scalar(_), LaneVal::Scalar(_)) => return None,
                },
                LaneExpr::And(a, b) | LaneExpr::Or(a, b) => {
                    let x = self.eval(a)?;
                    let y = self.eval(b)?;
                    let kind = if matches!(e, LaneExpr::And(..)) { simd::M_AND } else { simd::M_OR };
                    self.vbin(kind, x, x, y);
                    x
                }
                LaneExpr::Not(a) => {
                    let x = self.eval(a)?;
                    self.vbin(simd::M_NOT, x, x, x);
                    x
                }
            };
            // Registers past the result are free again.
            debug_assert_eq!(r, base);
            self.next = base + 1;
            Some(r)
        }
    }

    let top = simd::NREGS.checked_sub(streams.len())? as u8;
    let mut cx = Ctx { out: Vec::new(), next: 0, top, scalars, regs };
    for (s, &(arr, off)) in streams.iter().enumerate() {
        let r = cx.stream(s as u8)?;
        cx.out.push(LaneWord::new(LaneOp::VLoad).with_abc(
            simd::TY_U8,
            r,
            regs[arr.index()],
            regs[off.index()],
        ));
    }
    cx.eval(expr)?;
    (cx.out.len() <= simd::SCAN_MAX_BODY).then_some(cx.out)
}
