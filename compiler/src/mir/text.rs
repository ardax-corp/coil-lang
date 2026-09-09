//! Print / parse a small CLIF-like text form for round-trip tests.

use super::func::{MirBlock, MirFunc};
use super::inst::{
    BlockId, MirAllocKind, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirDeoptKind, MirGcKind,
    MirInst, MirUnaryOp, Terminator, ValueId,
};
use super::ty::MirTy;

impl std::fmt::Display for MirFunc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "func @{}", self.name)?;
        write!(f, "(")?;
        for (i, p) in self.params.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{p}: {}", self.ty(*p))?;
        }
        write!(f, ")")?;
        if let Some(rt) = self.ret_ty {
            write!(f, " -> {rt}")?;
            if let Some(hi) = self.ret_hi_ty {
                write!(f, ", {hi}")?;
            }
        }
        writeln!(f, " {{")?;
        for block in &self.blocks {
            write_block(f, self, block)?;
        }
        write!(f, "}}")
    }
}

fn write_block(
    f: &mut std::fmt::Formatter<'_>,
    func: &MirFunc,
    block: &MirBlock,
) -> std::fmt::Result {
    writeln!(f, "{}:", block.id)?;
    for inst in &block.insts {
        write!(f, "    ")?;
        write_inst(f, func, inst)?;
        writeln!(f)?;
    }
    write!(f, "    ")?;
    match &block.term {
        Some(Terminator::Jump { dest }) => writeln!(f, "jump {dest}")?,
        Some(Terminator::Br {
            cond,
            taken,
            not_taken,
        }) => writeln!(f, "brif {cond}, {taken}, {not_taken}")?,
        Some(Terminator::JumpIfMatch {
            scrutinee,
            tag,
            payloads,
            taken,
            not_taken,
        }) => {
            write!(f, "jumpifmatch {scrutinee}, {tag}")?;
            for p in payloads {
                write!(f, ", {p}")?;
            }
            writeln!(f, ", {taken}, {not_taken}")?;
        }
        Some(Terminator::Return {
            lo: Some(v),
            hi: Some(h),
        }) => writeln!(f, "return {v}, {h}")?,
        Some(Terminator::Return {
            lo: Some(v),
            hi: None,
        }) => writeln!(f, "return {v}")?,
        Some(Terminator::Return { lo: None, .. }) => writeln!(f, "return")?,
        Some(Terminator::Unreachable) => writeln!(f, "unreachable")?,
        None => writeln!(f, "unreachable")?,
    }
    Ok(())
}

fn write_inst(f: &mut std::fmt::Formatter<'_>, func: &MirFunc, inst: &MirInst) -> std::fmt::Result {
    match inst {
        MirInst::Const { dest, c } => {
            write!(f, "{dest} = ")?;
            match *c {
                MirConst::I32(v) => write!(f, "iconst.i32 {v}"),
                MirConst::I64(v) => write!(f, "iconst.i64 {v}"),
                MirConst::F32(b) => write!(f, "fconst.f32 bits={b}"),
                MirConst::F64(b) => write!(f, "fconst.f64 bits={b}"),
                MirConst::Bool(v) => write!(f, "bconst {v}"),
            }
        }
        MirInst::Bin {
            dest,
            op,
            ty,
            lhs,
            rhs,
        } => write!(f, "{dest} = {} {lhs}, {rhs}", op.as_str(*ty)),
        MirInst::Cmp {
            dest,
            op,
            ty,
            lhs,
            rhs,
        } => write!(f, "{dest} = {} {lhs}, {rhs}", op.as_str(ty.is_float())),
        MirInst::Unary { dest, op, src } => {
            let name = match (op, func.ty(*src).is_float()) {
                (MirUnaryOp::Neg, false) => "ineg",
                (MirUnaryOp::Neg, true) => "fneg",
                (MirUnaryOp::Not, _) if func.ty(*src) == MirTy::Bool => "bnot",
                (MirUnaryOp::Not, _) => "lnot",
            };
            write!(f, "{dest} = {name} {src}")
        }
        MirInst::Cast {
            dest,
            kind,
            to,
            src,
        } => {
            let from = func.ty(*src);
            let name = match kind {
                MirCastKind::IntToFloat => format!("fcvt.{to}.{from}"),
                MirCastKind::Sext => format!("sext.{to}.{from}"),
            };
            write!(f, "{dest} = {name} {src}")
        }
        MirInst::HostInvoke {
            dest,
            native_id,
            args,
        } => {
            let name = super::host_allow::host_edge_spec(*native_id)
                .map(|s| s.name)
                .unwrap_or("unknown");
            write!(f, "{dest} = host.{name}")?;
            for (i, a) in args.iter().enumerate() {
                if i == 0 {
                    write!(f, " {a}")?;
                } else {
                    write!(f, ", {a}")?;
                }
            }
            Ok(())
        }
        MirInst::Call { dest, target, args } => {
            write!(f, "{dest} = call.{} {}", func.ty(*dest), target.0)?;
            for (i, a) in args.iter().enumerate() {
                if i == 0 {
                    write!(f, " {a}")?;
                } else {
                    write!(f, ", {a}")?;
                }
            }
            Ok(())
        }
        MirInst::MatchPayload {
            dest,
            scrutinee,
            index,
        } => write!(f, "{dest} = matchpayload {scrutinee}, {index}"),
        MirInst::FieldLoad {
            dest,
            object,
            base,
            index,
        } => write!(f, "{dest} = fieldload {object}, {base}, {index}"),
        MirInst::FieldStore {
            dest,
            src,
            base,
            index,
        } => write!(f, "{dest} = fieldstore {src}, {base}, {index}"),
        MirInst::Index {
            dest,
            array,
            index,
            unchecked,
        } => {
            let name = if *unchecked { "index.u" } else { "index" };
            write!(f, "{dest} = {name} {array}, {index}")
        }
        MirInst::StoreIndex {
            dest,
            array,
            index,
            value,
            unchecked,
        } => {
            let name = if *unchecked {
                "storeindex.u"
            } else {
                "storeindex"
            };
            write!(f, "{dest} = {name} {array}, {index}, {value}")
        }
        MirInst::ArrayLen { dest, array } => write!(f, "{dest} = arraylen {array}"),
        MirInst::Alloc { dest, kind, elems } => {
            write!(f, "{dest} = alloc.{}", kind.as_str())?;
            match *kind {
                MirAllocKind::Object { type_id, nfields } => {
                    write!(f, " {type_id}, {nfields}")?;
                }
                MirAllocKind::Enum { tag } => write!(f, " {tag}")?,
                MirAllocKind::Array | MirAllocKind::Tuple => {}
            }
            for (i, e) in elems.iter().enumerate() {
                if i == 0 && matches!(kind, MirAllocKind::Array | MirAllocKind::Tuple) {
                    write!(f, " {e}")?;
                } else {
                    write!(f, ", {e}")?;
                }
            }
            Ok(())
        }
        MirInst::GcBarrier { dest, kind, roots } => {
            write!(f, "{dest} = gcbarrier.{}", kind.as_str())?;
            for (i, r) in roots.iter().enumerate() {
                if i == 0 {
                    write!(f, " {r}")?;
                } else {
                    write!(f, ", {r}")?;
                }
            }
            Ok(())
        }
        MirInst::Deopt { dest, kind, .. } => write!(f, "{dest} = deopt.{}", kind.as_str()),
        MirInst::Phi { dest, ty, args } => {
            write!(f, "{dest} = phi.{ty} [")?;
            for (i, (b, v)) in args.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{b}: {v}")?;
            }
            write!(f, "]")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

/// Parse one [`MirFunc`] from the text form [`MirFunc`] prints.
pub fn parse_func(src: &str) -> Result<MirFunc, ParseError> {
    let mut p = Parser::new(src);
    p.parse_func()
}

struct Parser<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> Parser<'a> {
    fn new(s: &'a str) -> Self {
        Self { s, i: 0 }
    }

    fn parse_func(&mut self) -> Result<MirFunc, ParseError> {
        self.kw("func")?;
        self.expect('@')?;
        let name = self.ident()?;
        let mut func = MirFunc {
            name,
            params: Vec::new(),
            ret_ty: None,
            ret_hi_ty: None,
            ret_layout: super::layout::MirLayout::Word,
            entry: BlockId(0),
            blocks: Vec::new(),
            types: Vec::new(),
            slot_env: std::collections::HashMap::new(),
            gc_roots: Vec::new(),
        };
        self.expect('(')?;
        if !self.eat(')') {
            loop {
                let v = self.value()?;
                self.expect(':')?;
                let ty = self.ty()?;
                ensure_ty(&mut func.types, v, ty);
                func.params.push(v);
                if self.eat(')') {
                    break;
                }
                self.expect(',')?;
            }
        }
        if self.eat_str("->") {
            func.ret_ty = Some(self.ty()?);
            if self.eat(',') {
                func.ret_hi_ty = Some(self.ty()?);
                func.ret_layout = super::layout::MirLayout::TwoSlot;
            }
        }
        self.expect('{')?;
        while !self.eat('}') {
            let block = self.parse_block(&mut func.types)?;
            func.blocks.push(block);
        }
        if func.blocks.is_empty() {
            return Err(ParseError("no blocks".into()));
        }
        func.entry = func.blocks[0].id;
        if func.has_gc_edge() {
            super::gc::fill_live_roots(&mut func);
        }
        Ok(func)
    }

    fn parse_block(&mut self, types: &mut Vec<MirTy>) -> Result<MirBlock, ParseError> {
        let id = self.block()?;
        self.expect(':')?;
        let mut insts = Vec::new();
        let mut term = None;
        loop {
            if self.peek_block_header() || self.peek('}') || self.eof() {
                break;
            }
            if self.peek_term() {
                term = Some(self.parse_term()?);
                break;
            }
            insts.push(self.parse_inst(types)?);
        }
        Ok(MirBlock { id, insts, term })
    }

    fn parse_inst(&mut self, types: &mut Vec<MirTy>) -> Result<MirInst, ParseError> {
        let dest = self.value()?;
        self.expect('=')?;
        let op = self.ident_dots()?;
        if let Some(rest) = op.strip_prefix("iconst.") {
            let ty = MirTy::parse(rest).ok_or_else(|| ParseError(op.clone()))?;
            let n = self.int()?;
            let c = match ty {
                MirTy::I32 => MirConst::I32(n as i32),
                MirTy::I64 => MirConst::I64(n),
                _ => return Err(ParseError("iconst type".into())),
            };
            ensure_ty(types, dest, c.ty());
            return Ok(MirInst::Const { dest, c });
        }
        if let Some(rest) = op.strip_prefix("fconst.") {
            let ty = MirTy::parse(rest).ok_or_else(|| ParseError(op.clone()))?;
            self.kw("bits")?;
            self.expect('=')?;
            let bits = self.uint()?;
            let c = match ty {
                MirTy::F32 => MirConst::F32(bits as u32),
                MirTy::F64 => MirConst::F64(bits),
                _ => return Err(ParseError("fconst type".into())),
            };
            ensure_ty(types, dest, c.ty());
            return Ok(MirInst::Const { dest, c });
        }
        if op == "bconst" {
            let v = self.bool()?;
            ensure_ty(types, dest, MirTy::Bool);
            return Ok(MirInst::Const {
                dest,
                c: MirConst::Bool(v),
            });
        }
        if let Some((bop, float)) = MirBinOp::parse(&op) {
            let lhs = self.value()?;
            self.expect(',')?;
            let rhs = self.value()?;
            let ty = if float { MirTy::F64 } else { MirTy::I64 };
            let ty = match (peek_ty(types, lhs), peek_ty(types, rhs)) {
                (a, b) if a == b && a.is_specialized() => a,
                _ => ty,
            };
            if float && !ty.is_float() {
                return Err(ParseError("f-binop on int".into()));
            }
            ensure_ty(types, dest, ty);
            return Ok(MirInst::Bin {
                dest,
                op: bop,
                ty,
                lhs,
                rhs,
            });
        }
        if let Some((cop, float)) = MirCmpOp::parse(&op) {
            let lhs = self.value()?;
            self.expect(',')?;
            let rhs = self.value()?;
            let ty = if float {
                if peek_ty(types, lhs).is_float() {
                    peek_ty(types, lhs)
                } else {
                    MirTy::F64
                }
            } else if peek_ty(types, lhs).is_int() {
                peek_ty(types, lhs)
            } else {
                MirTy::I64
            };
            ensure_ty(types, dest, MirTy::Bool);
            return Ok(MirInst::Cmp {
                dest,
                op: cop,
                ty,
                lhs,
                rhs,
            });
        }
        if op == "lnot" || op == "matchpayload" || op == "fieldload" || op == "fieldstore" {
            if op == "lnot" {
                let src = self.value()?;
                ensure_ty(types, dest, MirTy::Bool);
                return Ok(MirInst::Unary {
                    dest,
                    op: MirUnaryOp::Not,
                    src,
                });
            }
            if op == "matchpayload" {
                let scrutinee = self.value()?;
                self.expect(',')?;
                let index = self.uint()? as u32;
                ensure_ty(types, dest, peek_ty(types, scrutinee));
                return Ok(MirInst::MatchPayload {
                    dest,
                    scrutinee,
                    index,
                });
            }
            let object = self.value()?;
            self.expect(',')?;
            let base = self.uint()? as u32;
            self.expect(',')?;
            let index = self.uint()? as u32;
            ensure_ty(types, dest, peek_ty(types, object));
            if op == "fieldload" {
                return Ok(MirInst::FieldLoad {
                    dest,
                    object,
                    base,
                    index,
                });
            }
            return Ok(MirInst::FieldStore {
                dest,
                src: object,
                base,
                index,
            });
        }
        if op == "index" || op == "index.u" {
            let array = self.value()?;
            self.expect(',')?;
            let index = self.value()?;
            ensure_ty(types, dest, MirTy::I64);
            return Ok(MirInst::Index {
                dest,
                array,
                index,
                unchecked: op.ends_with(".u"),
            });
        }
        if op == "storeindex" || op == "storeindex.u" {
            let array = self.value()?;
            self.expect(',')?;
            let index = self.value()?;
            self.expect(',')?;
            let value = self.value()?;
            ensure_ty(types, dest, peek_ty(types, value));
            return Ok(MirInst::StoreIndex {
                dest,
                array,
                index,
                value,
                unchecked: op.ends_with(".u"),
            });
        }
        if op == "arraylen" {
            let array = self.value()?;
            ensure_ty(types, dest, MirTy::I64);
            return Ok(MirInst::ArrayLen { dest, array });
        }
        if op == "ineg" || op == "fneg" || op == "bnot" {
            let src = self.value()?;
            let ty = if op == "bnot" {
                MirTy::Bool
            } else if op == "fneg" {
                if peek_ty(types, src).is_float() {
                    peek_ty(types, src)
                } else {
                    MirTy::F64
                }
            } else if peek_ty(types, src).is_int() {
                peek_ty(types, src)
            } else {
                MirTy::I64
            };
            ensure_ty(types, dest, ty);
            let uop = if op == "bnot" {
                MirUnaryOp::Not
            } else {
                MirUnaryOp::Neg
            };
            return Ok(MirInst::Unary { dest, op: uop, src });
        }
        if let Some(rest) = op.strip_prefix("fcvt.") {
            let mut parts = rest.split('.');
            let to =
                MirTy::parse(parts.next().unwrap_or("")).ok_or_else(|| ParseError(op.clone()))?;
            let src = self.value()?;
            ensure_ty(types, dest, to);
            return Ok(MirInst::Cast {
                dest,
                kind: MirCastKind::IntToFloat,
                to,
                src,
            });
        }
        if let Some(rest) = op.strip_prefix("sext.") {
            let mut parts = rest.split('.');
            let to =
                MirTy::parse(parts.next().unwrap_or("")).ok_or_else(|| ParseError(op.clone()))?;
            let src = self.value()?;
            ensure_ty(types, dest, to);
            return Ok(MirInst::Cast {
                dest,
                kind: MirCastKind::Sext,
                to,
                src,
            });
        }
        if let Some(rest) = op.strip_prefix("phi.") {
            let ty = MirTy::parse(rest).ok_or_else(|| ParseError(op.clone()))?;
            self.expect('[')?;
            let mut args = Vec::new();
            if !self.eat(']') {
                loop {
                    let b = self.block()?;
                    self.expect(':')?;
                    let v = self.value()?;
                    args.push((b, v));
                    if self.eat(']') {
                        break;
                    }
                    self.expect(',')?;
                }
            }
            ensure_ty(types, dest, ty);
            return Ok(MirInst::Phi { dest, ty, args });
        }
        if let Some(name) = op.strip_prefix("host.") {
            let spec = super::host_allow::host_edge_spec_by_name(name)
                .ok_or_else(|| ParseError(format!("unknown host {name}")))?;
            let mut args = Vec::new();
            if spec.args.is_empty() {
                // no operands
            } else {
                args.push(self.value()?);
                for _ in 1..spec.args.len() {
                    self.expect(',')?;
                    args.push(self.value()?);
                }
            }
            ensure_ty(types, dest, spec.ret);
            return Ok(MirInst::HostInvoke {
                dest,
                native_id: spec.id,
                args,
            });
        }
        if let Some(ty_s) = op.strip_prefix("call.") {
            let ty = MirTy::parse(ty_s).ok_or_else(|| ParseError(op.clone()))?;
            let target = crate::il::Label(self.uint()? as u32);
            let mut args = Vec::new();
            if self.peek('v') {
                args.push(self.value()?);
                while self.eat(',') {
                    args.push(self.value()?);
                }
            }
            ensure_ty(types, dest, ty);
            return Ok(MirInst::Call { dest, target, args });
        }
        if let Some(kind_s) = op.strip_prefix("alloc.") {
            let kind = match kind_s {
                "array" => MirAllocKind::Array,
                "tuple" => MirAllocKind::Tuple,
                "object" => {
                    let type_id = self.uint()? as u32;
                    self.expect(',')?;
                    let nfields = self.uint()? as u32;
                    MirAllocKind::Object { type_id, nfields }
                }
                "enum" => MirAllocKind::Enum {
                    tag: self.uint()? as u32,
                },
                _ => return Err(ParseError(format!("unknown alloc {kind_s}"))),
            };
            let mut elems = Vec::new();
            if self.peek('v') {
                elems.push(self.value()?);
                while self.eat(',') {
                    elems.push(self.value()?);
                }
            }
            ensure_ty(types, dest, MirTy::HeapRef);
            return Ok(MirInst::Alloc { dest, kind, elems });
        }
        if let Some(kind_s) = op.strip_prefix("gcbarrier.") {
            let kind = MirGcKind::parse(kind_s)
                .ok_or_else(|| ParseError(format!("unknown gc {kind_s}")))?;
            let mut roots = Vec::new();
            if self.peek('v') {
                roots.push(self.value()?);
                while self.eat(',') {
                    roots.push(self.value()?);
                }
            }
            ensure_ty(types, dest, MirTy::HeapRef);
            return Ok(MirInst::GcBarrier { dest, kind, roots });
        }
        if let Some(kind_s) = op.strip_prefix("deopt.") {
            let kind = MirDeoptKind::parse(kind_s)
                .ok_or_else(|| ParseError(format!("unknown deopt {kind_s}")))?;
            ensure_ty(types, dest, MirTy::Bool);
            return Ok(MirInst::Deopt {
                dest,
                kind,
                loc: common::DebugLoc::unknown(),
            });
        }
        Err(ParseError(format!("unknown op {op}")))
    }

    fn parse_term(&mut self) -> Result<Terminator, ParseError> {
        let k = self.ident()?;
        match k.as_str() {
            "jump" => Ok(Terminator::Jump {
                dest: self.block()?,
            }),
            "brif" => {
                let cond = self.value()?;
                self.expect(',')?;
                let taken = self.block()?;
                self.expect(',')?;
                let not_taken = self.block()?;
                Ok(Terminator::Br {
                    cond,
                    taken,
                    not_taken,
                })
            }
            "jumpifmatch" => {
                let scrutinee = self.value()?;
                self.expect(',')?;
                let tag = self.uint()? as u32;
                let mut payloads = Vec::new();
                self.expect(',')?;
                // payloads then taken, not_taken — last two are blocks.
                loop {
                    self.skip();
                    let start = self.i;
                    if self.peek('v') {
                        payloads.push(self.value()?);
                        if !self.eat(',') {
                            return Err(ParseError("jumpifmatch needs dest blocks".into()));
                        }
                        continue;
                    }
                    self.i = start;
                    let taken = self.block()?;
                    self.expect(',')?;
                    let not_taken = self.block()?;
                    return Ok(Terminator::JumpIfMatch {
                        scrutinee,
                        tag,
                        payloads,
                        taken,
                        not_taken,
                    });
                }
            }
            "return" => {
                if self.peek('v') {
                    let lo = self.value()?;
                    let hi = if self.eat(',') {
                        Some(self.value()?)
                    } else {
                        None
                    };
                    Ok(Terminator::Return { lo: Some(lo), hi })
                } else {
                    Ok(Terminator::Return { lo: None, hi: None })
                }
            }
            "unreachable" => Ok(Terminator::Unreachable),
            _ => Err(ParseError(format!("bad term {k}"))),
        }
    }

    fn peek_term(&mut self) -> bool {
        let save = self.i;
        self.skip();
        let ok = self.starts("jumpifmatch")
            || self.starts("jump")
            || self.starts("brif")
            || self.starts("return")
            || self.starts("unreachable");
        self.i = save;
        ok
    }

    fn peek_block_header(&mut self) -> bool {
        let save = self.i;
        self.skip();
        let ok = if self.starts("bb") {
            self.i += 2;
            while self.peek_char().is_some_and(|c| c.is_ascii_digit()) {
                self.i += 1;
            }
            self.skip();
            self.peek(':')
        } else {
            false
        };
        self.i = save;
        ok
    }

    fn kw(&mut self, w: &str) -> Result<(), ParseError> {
        self.skip();
        if self.starts(w) {
            let after = self.i + w.len();
            if after < self.s.len() && self.s.as_bytes()[after].is_ascii_alphanumeric() {
                return Err(ParseError(format!("expected {w}")));
            }
            self.i = after;
            Ok(())
        } else {
            Err(ParseError(format!("expected {w}")))
        }
    }

    fn eat_str(&mut self, w: &str) -> bool {
        self.skip();
        if self.starts(w) {
            self.i += w.len();
            true
        } else {
            false
        }
    }

    fn ident(&mut self) -> Result<String, ParseError> {
        self.skip();
        let start = self.i;
        if !self
            .peek_char()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        {
            return Err(ParseError("ident".into()));
        }
        self.i += 1;
        while self
            .peek_char()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            self.i += 1;
        }
        Ok(self.s[start..self.i].to_string())
    }

    fn ident_dots(&mut self) -> Result<String, ParseError> {
        self.skip();
        let start = self.i;
        if !self.peek_char().is_some_and(|c| c.is_ascii_alphabetic()) {
            return Err(ParseError("opcode".into()));
        }
        while self
            .peek_char()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        {
            self.i += 1;
        }
        Ok(self.s[start..self.i].to_string())
    }

    fn value(&mut self) -> Result<ValueId, ParseError> {
        self.skip();
        self.expect('v')?;
        Ok(ValueId(self.uint()? as u32))
    }

    fn block(&mut self) -> Result<BlockId, ParseError> {
        self.skip();
        if !self.starts("bb") {
            return Err(ParseError("bb".into()));
        }
        self.i += 2;
        Ok(BlockId(self.uint()? as u32))
    }

    fn ty(&mut self) -> Result<MirTy, ParseError> {
        let n = self.ident()?;
        MirTy::parse(&n).ok_or_else(|| ParseError(format!("type {n}")))
    }

    fn int(&mut self) -> Result<i64, ParseError> {
        self.skip();
        let neg = self.eat('-');
        let n = self.uint()? as i64;
        Ok(if neg { -n } else { n })
    }

    fn uint(&mut self) -> Result<u64, ParseError> {
        self.skip();
        let start = self.i;
        while self.peek_char().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        if start == self.i {
            return Err(ParseError("number".into()));
        }
        self.s[start..self.i]
            .parse()
            .map_err(|_| ParseError("number".into()))
    }

    fn bool(&mut self) -> Result<bool, ParseError> {
        if self.eat_str("true") {
            Ok(true)
        } else if self.eat_str("false") {
            Ok(false)
        } else {
            Err(ParseError("bool".into()))
        }
    }

    fn expect(&mut self, c: char) -> Result<(), ParseError> {
        self.skip();
        if self.eat(c) {
            Ok(())
        } else {
            Err(ParseError(format!("expected {c:?} at {}", self.i)))
        }
    }

    fn eat(&mut self, c: char) -> bool {
        self.skip();
        if self.peek(c) {
            self.i += c.len_utf8();
            true
        } else {
            false
        }
    }

    fn peek(&mut self, c: char) -> bool {
        self.skip();
        self.peek_char() == Some(c)
    }

    fn peek_char(&self) -> Option<char> {
        self.s[self.i..].chars().next()
    }

    fn starts(&self, w: &str) -> bool {
        self.s[self.i..].starts_with(w)
    }

    fn eof(&mut self) -> bool {
        self.skip();
        self.i >= self.s.len()
    }

    fn skip(&mut self) {
        loop {
            while self.peek_char().is_some_and(|c| c.is_whitespace()) {
                self.i += self.peek_char().unwrap().len_utf8();
            }
            if self.starts("//") {
                while self.peek_char().is_some_and(|c| c != '\n') {
                    self.i += 1;
                }
                continue;
            }
            break;
        }
    }
}

fn ensure_ty(types: &mut Vec<MirTy>, v: ValueId, ty: MirTy) {
    if v.index() >= types.len() {
        types.resize(v.index() + 1, MirTy::Bottom);
    }
    if types[v.index()] == MirTy::Bottom {
        types[v.index()] = ty;
    }
}

fn peek_ty(types: &[MirTy], v: ValueId) -> MirTy {
    types.get(v.index()).copied().unwrap_or(MirTy::Bottom)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::builder::MirBuilder;
    use crate::mir::inst::LocalId;

    #[test]
    fn round_trip_builder_func() {
        let mut b = MirBuilder::new("add1");
        let x = b.add_param(MirTy::I32).unwrap();
        let one = b.ins_const(MirConst::I32(1)).unwrap();
        let y = b.ins_binop(MirBinOp::Add, x, one).unwrap();
        b.ret(Some(y)).unwrap();
        let f = b.finish().unwrap();
        let text = f.to_string();
        let g = parse_func(&text).expect(&text);
        assert_eq!(g.name, f.name);
        assert_eq!(g.params.len(), f.params.len());
        assert_eq!(g.blocks.len(), f.blocks.len());
        assert_eq!(
            g.block(BlockId(0)).insts.len(),
            f.block(BlockId(0)).insts.len()
        );
        g.verify().unwrap();
        let again = parse_func(&g.to_string()).unwrap();
        assert_eq!(again, g);
    }

    #[test]
    fn round_trip_phi_text() {
        let src = r#"
func @join(v0: i64) -> i64 {
bb0:
    v1 = iconst.i64 0
    v2 = icmp.slt v0, v1
    brif v2, bb1, bb2
bb1:
    v3 = iconst.i64 1
    jump bb3
bb2:
    v4 = iconst.i64 2
    jump bb3
bb3:
    v5 = phi.i64 [bb1: v3, bb2: v4]
    return v5
}
"#;
        let f = parse_func(src).unwrap();
        f.verify().unwrap();
        let g = parse_func(&f.to_string()).unwrap();
        assert_eq!(f, g);
        let _ = LocalId(0);
    }
}
