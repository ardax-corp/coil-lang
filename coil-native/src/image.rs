//! A program's compiled code as bytes, for the code cache: what each function
//! compiled to, with its relocations, and what the runtime needs beside it.
//! [`load`] hands the bytes back to a new module, so nothing is compiled, and
//! the module relocates them for this process as it would fresh code.

use cranelift_codegen::binemit::Reloc;
use cranelift_codegen::ir::LibCall;
use cranelift_jit::JITModule;
use cranelift_module::{FuncId, Linkage, Module, ModuleReloc, ModuleRelocTarget};
use machine::native::{NativeEntry, NativeEntryFn, NativeSite};
use sha2::{Digest, Sha256};

use crate::{FuncMaps, NativeProgram};

/// Bump when the layout changes; a reader refuses any other format.
const FORMAT: u32 = 3;
const MAGIC: &[u8; 8] = b"COILCODE";
const DIGEST: usize = 32;

/// The relocations native code uses, by their index here in the image.
const RELOCS: &[Reloc] = &[
    Reloc::Abs4,
    Reloc::Abs8,
    Reloc::X86PCRel4,
    Reloc::X86CallPCRel4,
    Reloc::X86CallPLTRel4,
    Reloc::X86GOTPCRel4,
    Reloc::Arm64Call,
    Reloc::Aarch64AdrGotPage21,
    Reloc::Aarch64AdrPrelPgHi21,
    Reloc::Aarch64AddAbsLo12Nc,
    Reloc::Aarch64Ld64GotLo12Nc,
];

const LINKAGES: &[Linkage] = &[Linkage::Import, Linkage::Local, Linkage::Preemptible, Linkage::Hidden, Linkage::Export];

const TARGET_FUNC: u8 = 0;
const TARGET_LIBCALL: u8 = 1;
const TARGET_OFFSET: u8 = 2;

/// One compiled function, as a module takes it.
pub(crate) struct Func {
    pub(crate) id: FuncId,
    pub(crate) align: u64,
    pub(crate) bytes: Vec<u8>,
    pub(crate) relocs: Vec<ModuleReloc>,
}

impl Func {
    /// Function `id`, which `ctx` just compiled.
    pub(crate) fn of(id: FuncId, ctx: &cranelift_codegen::Context) -> Option<Self> {
        let code = ctx.compiled_code()?;
        let relocs = code.buffer.relocs().iter().map(|r| ModuleReloc::from_mach_reloc(r, &ctx.func, id)).collect();
        Some(Self { id, align: u64::from(code.buffer.min_alignment), bytes: code.code_buffer().to_vec(), relocs })
    }
}

/// The compiled functions of a module, as it defines them.
#[derive(Default)]
pub(crate) struct Recorder {
    funcs: Vec<Func>,
}

impl Recorder {
    pub(crate) fn add(&mut self, func: Func) {
        self.funcs.push(func);
    }
}

#[derive(Default)]
struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, x: u8) {
        self.0.push(x);
    }
    fn u16(&mut self, x: u16) {
        self.0.extend_from_slice(&x.to_le_bytes());
    }
    fn u32(&mut self, x: u32) {
        self.0.extend_from_slice(&x.to_le_bytes());
    }
    fn u64(&mut self, x: u64) {
        self.0.extend_from_slice(&x.to_le_bytes());
    }
    fn len(&mut self, n: usize) {
        self.u32(n as u32);
    }
    fn bytes(&mut self, b: &[u8]) {
        self.len(b.len());
        self.0.extend_from_slice(b);
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.bytes.len()).ok_or("code image is truncated")?;
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().map_err(|_| "code image is truncated")?))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().map_err(|_| "code image is truncated")?))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().map_err(|_| "code image is truncated")?))
    }
    fn len(&mut self) -> Result<usize, String> {
        Ok(self.u32()? as usize)
    }
    fn bytes(&mut self) -> Result<&'a [u8], String> {
        let n = self.len()?;
        self.take(n)
    }
    fn text(&mut self) -> Result<String, String> {
        Ok(std::str::from_utf8(self.bytes()?).map_err(|_| "code image: bad text")?.to_string())
    }
}

fn write_reloc(w: &mut Writer, r: &ModuleReloc) -> Option<()> {
    w.u32(r.offset);
    w.u8(RELOCS.iter().position(|k| *k == r.kind)? as u8);
    w.u64(r.addend as u64);
    match &r.name {
        ModuleRelocTarget::User { namespace: 0, index } => {
            w.u8(TARGET_FUNC);
            w.u32(*index);
        }
        ModuleRelocTarget::LibCall(call) => {
            w.u8(TARGET_LIBCALL);
            w.u32(LibCall::all_libcalls().iter().position(|c| c == call)? as u32);
        }
        ModuleRelocTarget::FunctionOffset(id, offset) => {
            w.u8(TARGET_OFFSET);
            w.u32(id.as_u32());
            w.u32(*offset);
        }
        _ => return None,
    }
    Some(())
}

fn read_reloc(r: &mut Reader<'_>) -> Result<ModuleReloc, String> {
    let offset = r.u32()?;
    let kind = *RELOCS.get(usize::from(r.u8()?)).ok_or("code image: bad relocation")?;
    let addend = r.u64()? as i64;
    let name = match r.u8()? {
        TARGET_FUNC => ModuleRelocTarget::user(0, r.u32()?),
        TARGET_LIBCALL => ModuleRelocTarget::LibCall(*LibCall::all_libcalls().get(r.u32()? as usize).ok_or("code image: bad library call")?),
        TARGET_OFFSET => ModuleRelocTarget::FunctionOffset(FuncId::from_u32(r.u32()?), r.u32()?),
        _ => return Err("code image: bad relocation target".to_string()),
    };
    Ok(ModuleReloc { offset, kind, name, addend })
}

/// What [`encode`] stores of an entry, beside its function.
pub(crate) struct Entry {
    pub(crate) func: FuncId,
    pub(crate) entry: NativeEntry,
}

/// What a compile produced, as a module and its program describe it.
pub(crate) struct Parts<'a> {
    pub(crate) module: &'a JITModule,
    pub(crate) reserve: usize,
    pub(crate) recorder: Recorder,
    pub(crate) maps: &'a [(FuncId, FuncMaps)],
    pub(crate) entries: &'a [Entry],
    pub(crate) sites: &'a [NativeSite],
    pub(crate) compiled: &'a [String],
}

/// The image of `parts`; `None` for code it has no way to hold.
pub(crate) fn encode(parts: Parts<'_>) -> Option<Vec<u8>> {
    let mut w = Writer::default();
    w.u64(parts.reserve as u64);
    let decls: Vec<_> = parts.module.declarations().get_functions().collect();
    w.len(decls.len());
    for (_, decl) in decls {
        w.bytes(decl.name.as_deref()?.as_bytes());
        w.u8(LINKAGES.iter().position(|l| *l == decl.linkage)? as u8);
    }
    w.len(parts.recorder.funcs.len());
    for f in &parts.recorder.funcs {
        w.u32(f.id.as_u32());
        w.u64(f.align);
        w.bytes(&f.bytes);
        w.len(f.relocs.len());
        for r in &f.relocs {
            write_reloc(&mut w, r)?;
        }
    }
    w.len(parts.maps.len());
    for (id, (size, maps)) in parts.maps {
        w.u32(id.as_u32());
        w.u32(*size);
        w.len(maps.len());
        for (ra, offsets) in maps {
            w.u32(*ra);
            w.len(offsets.len());
            offsets.iter().for_each(|o| w.u32(*o));
        }
    }
    w.len(parts.entries.len());
    for e in parts.entries {
        let n = &e.entry;
        w.u32(e.func.as_u32());
        w.u32(n.pc);
        w.u16(n.arity);
        w.u16(n.reads);
        w.u8(n.ret_words);
        w.u8(u8::from(n.suspends) | u8::from(n.generator) << 1 | u8::from(n.cleanup) << 2);
        w.u32(n.twin);
    }
    w.len(parts.sites.len());
    for s in parts.sites {
        w.u32(s.body_id);
        w.u32(s.loc.file);
        w.u32(s.loc.start_byte);
        w.u32(s.loc.end_byte);
        w.u32(s.loc.scope);
    }
    w.len(parts.compiled.len());
    parts.compiled.iter().for_each(|name| w.bytes(name.as_bytes()));
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&FORMAT.to_le_bytes());
    out.extend_from_slice(&Sha256::digest(&w.0));
    out.extend_from_slice(&w.0);
    Some(out)
}

/// The program in `image`, mapped and relocated for this process. The digest
/// refuses a damaged file: running one would be a crash, not an error.
pub fn load(image: &[u8]) -> Result<NativeProgram, String> {
    let header = MAGIC.len() + 4 + DIGEST;
    let (head, payload) = image.split_at_checked(header).ok_or("code image is truncated")?;
    if &head[..MAGIC.len()] != MAGIC || head[MAGIC.len()..MAGIC.len() + 4] != FORMAT.to_le_bytes() {
        return Err("not a code image of this format".to_string());
    }
    if head[MAGIC.len() + 4..] != Sha256::digest(payload)[..] {
        return Err("code image is damaged".to_string());
    }
    let mut r = Reader { bytes: payload, at: 0 };
    let reserve = r.u64()? as usize;
    let mut module = crate::jit_module(reserve)?;
    let sig = module.make_signature();
    for _ in 0..r.len()? {
        let name = r.text()?;
        let linkage = *LINKAGES.get(usize::from(r.u8()?)).ok_or("code image: bad linkage")?;
        module.declare_function(&name, linkage, &sig).map_err(|e| e.to_string())?;
    }
    for _ in 0..r.len()? {
        let id = FuncId::from_u32(r.u32()?);
        let align = r.u64()?;
        let bytes = r.bytes()?;
        let relocs = (0..r.len()?).map(|_| read_reloc(&mut r)).collect::<Result<Vec<_>, _>>()?;
        module.define_function_bytes(id, align, bytes, &relocs).map_err(|e| e.to_string())?;
    }
    module.finalize_definitions().map_err(|e| e.to_string())?;
    let mut mapped = Vec::new();
    for _ in 0..r.len()? {
        let id = FuncId::from_u32(r.u32()?);
        let size = r.u32()?;
        let mut maps = Vec::new();
        for _ in 0..r.len()? {
            let ra = r.u32()?;
            let offsets = (0..r.len()?).map(|_| r.u32()).collect::<Result<Vec<_>, _>>()?;
            maps.push((ra, offsets));
        }
        let start = module.get_finalized_function(id) as u64;
        machine::native::register_stack_maps(start, size, maps);
        mapped.push((start, size));
    }
    let mut entries = Vec::new();
    for _ in 0..r.len()? {
        let func = FuncId::from_u32(r.u32()?);
        let (pc, arity, reads, ret_words, flags, twin) = (r.u32()?, r.u16()?, r.u16()?, r.u8()?, r.u8()?, r.u32()?);
        // SAFETY: `build_entry` gave this function the `NativeEntryFn` ABI.
        let entry: NativeEntryFn = unsafe { std::mem::transmute::<*const u8, NativeEntryFn>(module.get_finalized_function(func)) };
        entries.push(NativeEntry { pc, arity, suspends: flags & 1 != 0, reads, ret_words, generator: flags & 2 != 0, cleanup: flags & 4 != 0, entry, data: 0, twin });
    }
    let mut sites = Vec::new();
    for _ in 0..r.len()? {
        let body_id = r.u32()?;
        let (file, start_byte, end_byte, scope) = (r.u32()?, r.u32()?, r.u32()?, r.u32()?);
        sites.push(NativeSite { body_id, loc: common::DebugLoc { file, start_byte, end_byte, scope } });
    }
    let compiled = (0..r.len()?).map(|_| r.text()).collect::<Result<Vec<_>, _>>()?;
    Ok(NativeProgram { entries, compiled, refused: Vec::new(), sites, image: None, keep: crate::Code::keep(module, mapped) })
}
