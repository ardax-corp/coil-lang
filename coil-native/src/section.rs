//! Reading the MIR section of a `.ccoil`. The format is
//! `compiler::native_mir::archive`.

#[cfg(test)]
use std::collections::HashMap;

pub use compiler::native_mir::archive::{decode, encode};

#[cfg(test)]
mod tests {
    use super::*;
    use compiler::native_mir::{BlockId, Label, NativeBody, MirBinOp, MirBlock, MirConst, MirFunc, MirInst, MirTy, Terminator, ValueId};
    use machine::native::{NATIVE_STACK_BUDGET, NativeCtx};

    /// A runner has no interpreter: a section with a body native code refuses stops
    /// it, naming the body, and an empty one says what to do.
    #[test]
    fn a_runner_refuses_what_it_cannot_compile() {
        let mut refused = body();
        refused.mir.emit_refusal = Some("no native code");
        let pcs = HashMap::from([("f".to_string(), 4u32)]);
        let why = crate::install_section(&encode(&[refused], &pcs, &[])).expect_err("a refused body");
        assert!(why.contains("`f` cannot compile to native code"), "{why}");
        let why = crate::install_section(&[]).expect_err("no section");
        assert!(why.contains("no native MIR"), "{why}");
    }

    /// `fn f(int a, int b) -> int { return a + b * 2; }`
    fn body() -> NativeBody {
        let mut f = MirFunc::new("f");
        f.blocks.clear();
        f.types = vec![MirTy::I64; 5];
        f.params = vec![ValueId(0), ValueId(1)];
        f.ret_ty = Some(MirTy::I64);
        let mut b = MirBlock::new(BlockId(0));
        b.insts.push(MirInst::Const { dest: ValueId(2), c: MirConst::I64(2) });
        b.insts.push(MirInst::Bin { dest: ValueId(3), op: MirBinOp::Mul, ty: MirTy::I64, lhs: ValueId(1), rhs: ValueId(2) });
        b.insts.push(MirInst::Bin { dest: ValueId(4), op: MirBinOp::Add, ty: MirTy::I64, lhs: ValueId(0), rhs: ValueId(3) });
        b.term = Some(Terminator::Return { lo: Some(ValueId(4)), hi: None });
        f.blocks.push(b);
        NativeBody { name: "f".into(), entry: Some(Label(7)), mir: f }
    }

    #[test]
    fn section_round_trips_and_runs() {
        let pcs = HashMap::from([("f".to_string(), 42u32)]);
        let bytes = encode(&[body()], &pcs, &[]);
        let (bodies, back_pcs) = decode(&bytes).expect("decode");
        assert_eq!(back_pcs, pcs);
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0].entry, Some(Label(7)));
        assert_eq!(bodies[0].mir.blocks, body().mir.blocks);
        assert_eq!(bodies[0].mir.params, body().mir.params);

        let program = crate::jit(&bodies, &back_pcs).expect("jit");
        assert_eq!(program.entries.len(), 1);
        assert_eq!(program.entries[0].pc, 42);
        let mut ctx = NativeCtx::new(0);
        let marker = 0u8;
        ctx.stack_limit = (std::ptr::addr_of!(marker) as u64).saturating_sub(NATIVE_STACK_BUDGET);
        let args = [5u64, 10];
        // SAFETY: compiled for two argument words.
        let out = unsafe { (program.entries[0].entry)(&mut ctx, args.as_ptr()) };
        assert_eq!((ctx.bail, out), (0, 25));
    }

    #[test]
    fn corrupt_sections_are_refused() {
        let bytes = encode(&[body()], &HashMap::new(), &[]);
        assert!(decode(&bytes[..bytes.len() - 1]).is_err());
        assert!(decode(b"CNMIR\x02").is_err());
        assert!(decode(b"nope").is_err());
        let mut long = bytes.clone();
        long.push(0);
        assert!(decode(&long).is_err());
    }
}
