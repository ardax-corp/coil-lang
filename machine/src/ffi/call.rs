//! libffi call preparation and invocation.

use std::ffi::{c_char, c_void, CStr, CString};

use common::Value;
use libffi::middle::{Arg, Cif, CodePtr, Type};

use crate::memory::{CStructLayout, FfiType, Heap, Member, ObjString, Object};

use super::signature::{FfiError, FfiSignature};

pub struct PreparedCall {
    pub cif: Cif,
    pub addr: CodePtr,
}

pub struct InvokeContext {
    heap: *mut Heap,
    struct_layouts: *const CStructLayout,
    struct_layouts_len: usize,
}

impl InvokeContext {
    pub fn new(heap: *mut Heap, struct_layouts: &[CStructLayout]) -> Self {
        Self {
            heap,
            struct_layouts: struct_layouts.as_ptr(),
            struct_layouts_len: struct_layouts.len(),
        }
    }

    fn heap(&mut self) -> &mut Heap {
        // SAFETY: VM is single-threaded; reentrant callbacks may borrow the heap
        // while libffi is active, so this cannot overlap with `&mut Heap` borrows
        // held across the native call.
        unsafe { &mut *self.heap }
    }

    fn layouts(&self) -> &[CStructLayout] {
        unsafe { std::slice::from_raw_parts(self.struct_layouts, self.struct_layouts_len) }
    }
}

fn ffi_type_to_libffi(ty: FfiType, layouts: &[CStructLayout]) -> Result<Type, FfiError> {
    match ty {
        FfiType::Int => Ok(Type::i64()),
        FfiType::Float => Ok(Type::f64()),
        FfiType::String | FfiType::Ptr | FfiType::Callback(_) => Ok(Type::pointer()),
        FfiType::Void => Ok(Type::void()),
        FfiType::Bool => Ok(Type::u8()),
        FfiType::Int8 => Ok(Type::i8()),
        FfiType::Int16 => Ok(Type::i16()),
        FfiType::Int32 => Ok(Type::i32()),
        FfiType::UInt8 => Ok(Type::u8()),
        FfiType::UInt16 => Ok(Type::u16()),
        FfiType::UInt32 => Ok(Type::u32()),
        FfiType::UInt64 => Ok(Type::u64()),
        FfiType::Struct(id) => {
            let layout = layouts
                .get(id as usize)
                .ok_or_else(|| FfiError::Unsupported(format!("unknown struct layout id {id}")))?;
            let fields: Result<Vec<Type>, FfiError> = layout
                .fields
                .iter()
                .map(|(_, fty)| ffi_type_to_libffi(*fty, layouts))
                .collect();
            Ok(Type::structure(fields?))
        }
    }
}

pub fn prepare_cif(
    sig: &FfiSignature,
    layouts: &[CStructLayout],
) -> Result<PreparedCall, FfiError> {
    let arg_types: Result<Vec<Type>, FfiError> = sig
        .args
        .iter()
        .copied()
        .map(|t| ffi_type_to_libffi(t, layouts))
        .collect();
    let ret_type = ffi_type_to_libffi(sig.ret, layouts)?;
    let cif = Cif::new(arg_types?, ret_type);
    Ok(PreparedCall {
        cif,
        addr: CodePtr::from_ptr(std::ptr::null_mut()),
    })
}

/// Prepare a per-invoke CIF for a C varargs call (`ffi_prep_cif_var`).
pub fn prepare_variadic_cif(
    arg_types: &[FfiType],
    nfixed: usize,
    ret: FfiType,
    layouts: &[CStructLayout],
) -> Result<Cif, FfiError> {
    if nfixed > arg_types.len() {
        return Err(FfiError::ArityMismatch {
            expected: nfixed,
            got: arg_types.len(),
        });
    }
    let libffi_args: Result<Vec<Type>, FfiError> = arg_types
        .iter()
        .copied()
        .map(|t| ffi_type_to_libffi(t, layouts))
        .collect();
    let ret_type = ffi_type_to_libffi(ret, layouts)?;
    Ok(Cif::new_variadic(libffi_args?, nfixed, ret_type))
}

/// Default C argument promotions for the `...` region.
pub fn promote_variadic_arg_type(ty: FfiType) -> FfiType {
    match ty {
        FfiType::Bool | FfiType::Int8 | FfiType::Int16 | FfiType::UInt8 | FfiType::UInt16 => {
            FfiType::Int
        }
        // Float is already f64 in our ABI mapping (matches `float` → `double` promotion).
        other => other,
    }
}

pub fn prepare_cif_for_symbol(
    sig: &FfiSignature,
    library: &libloading::Library,
    symbol: &str,
    layouts: &[CStructLayout],
) -> Result<PreparedCall, FfiError> {
    // Variadic: resolve symbol now; CIF is rebuilt per invoke. We still
    // prepare a fixed-prefix CIF as a placeholder so `PreparedCall` stays
    // uniform (call path ignores it when `sig.variadic`).
    let mut prepared = prepare_cif(sig, layouts)?;
    prepared.addr = resolve_symbol(library, symbol)?;
    Ok(prepared)
}

pub fn resolve_symbol(library: &libloading::Library, symbol: &str) -> Result<CodePtr, FfiError> {
    type FnPtr = unsafe extern "C" fn();
    let sym_bytes: &[u8] = symbol.as_bytes();
    let sym: libloading::Symbol<FnPtr> = unsafe {
        library
            .get(sym_bytes)
            .map_err(|_| FfiError::SymbolNotFound {
                name: symbol.to_string(),
            })?
    };
    let ptr: *mut c_void = unsafe { std::mem::transmute(sym.into_raw()) };
    Ok(CodePtr::from_ptr(ptr))
}

fn read_c_string_ptr(heap: &Heap, value: &Value) -> *const c_char {
    let raw = value.raw() as u64;
    if raw == 0 {
        return std::ptr::null();
    }
    heap.cstr_from_addr(raw).unwrap_or(std::ptr::null())
}

fn member_to_value(member: &Member) -> Value {
    match member {
        Member::Value(v) => Value::from(*v),
        Member::Object(o) => Value::from(o.addr()),
    }
}

fn instance_field(heap: &mut Heap, addr: u64, fname: &str) -> Result<Value, FfiError> {
    let obj = heap
        .find_object_by_addr(addr)
        .ok_or_else(|| FfiError::Unsupported("struct value not found on heap".into()))?;
    match obj {
        Object::Instance(gc) => {
            let key = heap.intern(fname.to_string());
            gc.as_ref()
                .get(key)
                .map(|member| member_to_value(&member))
                .ok_or_else(|| FfiError::Unsupported(format!("missing field `{fname}`")))
        }
        _ => Err(FfiError::Unsupported(
            "struct argument must be a record/dict instance".into(),
        )),
    }
}

fn append_field_bytes(
    out: &mut Vec<u8>,
    val: &Value,
    fty: FfiType,
    heap: &mut Heap,
    layouts: &[CStructLayout],
) -> Result<(), FfiError> {
    match fty {
        FfiType::Int => out.extend_from_slice(&val.as_int().to_ne_bytes()),
        FfiType::Int8 => out.push(val.as_int() as i8 as u8),
        FfiType::Int16 => out.extend_from_slice(&(val.as_int() as i16).to_ne_bytes()),
        FfiType::Int32 => out.extend_from_slice(&(val.as_int() as i32).to_ne_bytes()),
        FfiType::UInt8 => out.push(val.as_int() as u8),
        FfiType::UInt16 => out.extend_from_slice(&(val.as_int() as u16).to_ne_bytes()),
        FfiType::UInt32 => out.extend_from_slice(&(val.as_int() as u32).to_ne_bytes()),
        FfiType::UInt64 => out.extend_from_slice(&(val.as_int() as u64).to_ne_bytes()),
        FfiType::Float => out.extend_from_slice(&val.as_float().to_ne_bytes()),
        FfiType::Bool => out.push(if val.as_bool() { 1 } else { 0 }),
        FfiType::Struct(id) => {
            let sub = layouts
                .get(id as usize)
                .ok_or_else(|| FfiError::Unsupported(format!("unknown nested struct id {id}")))?;
            pack_struct(heap, val, sub, layouts, out)?;
        }
        _ => {
            return Err(FfiError::Unsupported(format!(
                "field type `{fty:?}` not supported in struct pack"
            )));
        }
    }
    Ok(())
}

fn pack_struct(
    heap: &mut Heap,
    value: &Value,
    layout: &CStructLayout,
    layouts: &[CStructLayout],
    out: &mut Vec<u8>,
) -> Result<(), FfiError> {
    out.clear();
    let addr = value.raw() as u64;
    for (fname, fty) in &layout.fields {
        let val = instance_field(heap, addr, fname)?;
        append_field_bytes(out, &val, *fty, heap, layouts)?;
    }
    Ok(())
}

fn field_byte_size(fty: FfiType, layouts: &[CStructLayout]) -> Result<usize, FfiError> {
    Ok(match fty {
        FfiType::Int | FfiType::UInt64 | FfiType::Float => 8,
        FfiType::Int32 | FfiType::UInt32 => 4,
        FfiType::Int16 | FfiType::UInt16 => 2,
        FfiType::Int8 | FfiType::UInt8 | FfiType::Bool => 1,
        FfiType::Struct(id) => {
            let layout = layouts
                .get(id as usize)
                .ok_or_else(|| FfiError::Unsupported(format!("unknown nested struct id {id}")))?;
            struct_byte_size(layout, layouts)?
        }
        other => {
            return Err(FfiError::Unsupported(format!(
                "field type `{other:?}` not supported in struct size"
            )));
        }
    })
}

fn struct_byte_size(layout: &CStructLayout, layouts: &[CStructLayout]) -> Result<usize, FfiError> {
    let mut total = 0usize;
    for (_, fty) in &layout.fields {
        total += field_byte_size(*fty, layouts)?;
    }
    Ok(total)
}

fn read_field_bytes(
    buf: &[u8],
    offset: usize,
    fty: FfiType,
    heap: &mut Heap,
    layouts: &[CStructLayout],
) -> Result<(Value, usize), FfiError> {
    let size = field_byte_size(fty, layouts)?;
    if offset + size > buf.len() {
        return Err(FfiError::Unsupported(
            "struct return buffer too small".into(),
        ));
    }
    let slice = &buf[offset..offset + size];
    let val = match fty {
        FfiType::Int => Value::from(i64::from_ne_bytes(slice.try_into().unwrap())),
        FfiType::UInt64 => Value::from(u64::from_ne_bytes(slice.try_into().unwrap()) as i64),
        FfiType::Float => Value::from(f64::from_ne_bytes(slice.try_into().unwrap())),
        FfiType::Int32 => Value::from(i32::from_ne_bytes(slice.try_into().unwrap()) as i64),
        FfiType::UInt32 => Value::from(u32::from_ne_bytes(slice.try_into().unwrap()) as i64),
        FfiType::Int16 => Value::from(i16::from_ne_bytes(slice.try_into().unwrap()) as i64),
        FfiType::UInt16 => Value::from(u16::from_ne_bytes(slice.try_into().unwrap()) as i64),
        FfiType::Int8 => Value::from(slice[0] as i8 as i64),
        FfiType::UInt8 => Value::from(slice[0] as i64),
        FfiType::Bool => Value::from(slice[0] != 0),
        FfiType::Struct(id) => {
            let sub = layouts
                .get(id as usize)
                .ok_or_else(|| FfiError::Unsupported(format!("unknown nested struct id {id}")))?;
            unpack_struct(heap, sub, layouts, slice)?
        }
        other => {
            return Err(FfiError::Unsupported(format!(
                "field type `{other:?}` not supported in struct unpack"
            )));
        }
    };
    Ok((val, size))
}

fn unpack_struct(
    heap: &mut Heap,
    layout: &CStructLayout,
    layouts: &[CStructLayout],
    buf: &[u8],
) -> Result<Value, FfiError> {
    use crate::memory::ObjInstance;
    let (obj, mut gc) = heap.alloc(ObjInstance::default(), Object::Instance);
    let mut offset = 0usize;
    for (fname, fty) in &layout.fields {
        let (val, nbytes) = read_field_bytes(buf, offset, *fty, heap, layouts)?;
        offset += nbytes;
        let key = heap.intern(fname.clone());
        let member = match heap.find_object_by_addr(val.raw() as u64) {
            Some(o) => Member::Object(o),
            None => Member::Value(val),
        };
        gc.as_mut().set(key, member);
    }
    Ok(Value::from(obj.addr()))
}

fn array_buffer_from_value(
    heap: &Heap,
    value: &Value,
    bufs: &mut Vec<Vec<i64>>,
) -> Result<(*mut c_void, Option<u64>), FfiError> {
    let addr = value.raw() as u64;
    if let Some(obj) = heap.find_object_by_addr(addr) {
        let elements = match obj {
            Object::Array(gc) => gc.as_ref().elements.clone(),
            Object::Tuple(gc) => gc.as_ref().elements.clone(),
            _ => Vec::new(),
        };
        if !elements.is_empty() {
            let mut buf: Vec<i64> = elements.iter().map(|v| v.as_int()).collect();
            let ptr = buf.as_mut_ptr() as *mut c_void;
            bufs.push(buf);
            return Ok((ptr, Some(addr)));
        }
    }
    Ok((value.raw() as *mut c_void, None))
}

fn copy_array_buffers_back(heap: &mut Heap, targets: &[(u64, usize)], bufs: &[Vec<i64>]) {
    for &(addr, buf_idx) in targets {
        let Some(buf) = bufs.get(buf_idx) else {
            continue;
        };
        heap.update_array_elements(addr, buf);
    }
}

pub fn invoke_via_libffi(
    prepared: &PreparedCall,
    sig: &FfiSignature,
    args: &[Value],
    // Full per-arg FFI types for variadic calls (length == `args.len()`),
    // before default promotions on the `...` tail. Ignored when not variadic.
    variadic_arg_types: Option<&[FfiType]>,
    ctx: &mut InvokeContext,
    _callback_closures: &mut Vec<*mut c_void>,
) -> Result<Option<Value>, FfiError> {
    let nfixed = sig.arity();
    let effective_types: Vec<FfiType> = if sig.variadic {
        if args.len() < nfixed {
            return Err(FfiError::ArityMismatch {
                expected: nfixed,
                got: args.len(),
            });
        }
        let tags = variadic_arg_types.ok_or_else(|| {
            FfiError::Unsupported("variadic FFI invoke requires per-argument type tags".into())
        })?;
        if tags.len() != args.len() {
            return Err(FfiError::ArityMismatch {
                expected: args.len(),
                got: tags.len(),
            });
        }
        tags.iter()
            .enumerate()
            .map(|(i, ty)| {
                if i < nfixed {
                    // Prefer the declared fixed type when present.
                    sig.args.get(i).copied().unwrap_or(*ty)
                } else {
                    promote_variadic_arg_type(*ty)
                }
            })
            .collect()
    } else {
        if args.len() != nfixed {
            return Err(FfiError::ArityMismatch {
                expected: nfixed,
                got: args.len(),
            });
        }
        sig.args.clone()
    };

    // For variadic calls, build a fresh CIF; fixed-arity uses the declare-time CIF.
    let variadic_cif;
    let cif: &Cif = if sig.variadic {
        variadic_cif = prepare_variadic_cif(&effective_types, nfixed, sig.ret, ctx.layouts())?;
        &variadic_cif
    } else {
        &prepared.cif
    };

    // libffi 5's `Arg<'arg>` borrows the marshalling storage, so we cannot
    // push into a Vec while an Arg still holds a reference into it. Phase 1
    // fills typed storage; phase 2 builds Args from the finished buffers.
    enum ArgSlot {
        I64(usize),
        I8(usize),
        I16(usize),
        I32(usize),
        U8(usize),
        U16(usize),
        U32(usize),
        U64(usize),
        F64(usize),
        Ptr(usize),
    }

    let mut i64_storage: Vec<i64> = Vec::new();
    let mut i8_storage: Vec<i8> = Vec::new();
    let mut i16_storage: Vec<i16> = Vec::new();
    let mut i32_storage: Vec<i32> = Vec::new();
    let mut u8_storage: Vec<u8> = Vec::new();
    let mut u16_storage: Vec<u16> = Vec::new();
    let mut u32_storage: Vec<u32> = Vec::new();
    let mut u64_storage: Vec<u64> = Vec::new();
    let mut f64_storage: Vec<f64> = Vec::new();
    let mut str_storage: Vec<CString> = Vec::new();
    let mut ptr_storage: Vec<*mut c_void> = Vec::new();
    let mut array_buffers: Vec<Vec<i64>> = Vec::new();
    let mut array_copy_back: Vec<(u64, usize)> = Vec::new();
    let mut struct_bufs: Vec<Vec<u8>> = Vec::new();
    let mut slots: Vec<ArgSlot> = Vec::with_capacity(effective_types.len());

    fn value_is_stream(heap: &Heap, value: &Value) -> bool {
        let v = crate::io::peel_one_boxed(heap, *value);
        matches!(
            heap.find_object_by_addr(v.raw() as u64),
            Some(Object::Stream(_))
        )
    }

    fn int_from_value(heap: &Heap, value: &Value) -> Result<i64, FfiError> {
        if value_is_stream(heap, value) {
            return Err(FfiError::Unsupported(
                "FFI integer types do not accept a Stream (no silent fd coercion)".into(),
            ));
        }
        Ok(value.as_int())
    }

    for (i, (ty, value)) in effective_types.iter().zip(args.iter()).enumerate() {
        match ty {
            FfiType::Int => {
                slots.push(ArgSlot::I64(i64_storage.len()));
                i64_storage.push(int_from_value(ctx.heap(), value)?);
            }
            FfiType::Int8 => {
                slots.push(ArgSlot::I8(i8_storage.len()));
                i8_storage.push(int_from_value(ctx.heap(), value)? as i8);
            }
            FfiType::Int16 => {
                slots.push(ArgSlot::I16(i16_storage.len()));
                i16_storage.push(int_from_value(ctx.heap(), value)? as i16);
            }
            FfiType::Int32 => {
                slots.push(ArgSlot::I32(i32_storage.len()));
                i32_storage.push(int_from_value(ctx.heap(), value)? as i32);
            }
            FfiType::UInt8 => {
                slots.push(ArgSlot::U8(u8_storage.len()));
                u8_storage.push(int_from_value(ctx.heap(), value)? as u8);
            }
            FfiType::UInt16 => {
                slots.push(ArgSlot::U16(u16_storage.len()));
                u16_storage.push(int_from_value(ctx.heap(), value)? as u16);
            }
            FfiType::UInt32 => {
                slots.push(ArgSlot::U32(u32_storage.len()));
                u32_storage.push(int_from_value(ctx.heap(), value)? as u32);
            }
            FfiType::UInt64 => {
                slots.push(ArgSlot::U64(u64_storage.len()));
                u64_storage.push(int_from_value(ctx.heap(), value)? as u64);
            }
            FfiType::Float => {
                slots.push(ArgSlot::F64(f64_storage.len()));
                f64_storage.push(value.as_float());
            }
            FfiType::Bool => {
                slots.push(ArgSlot::U8(u8_storage.len()));
                u8_storage.push(if value.as_bool() { 1 } else { 0 });
            }
            FfiType::String => {
                let heap = ctx.heap();
                let ptr = read_c_string_ptr(heap, value);
                if ptr.is_null() {
                    str_storage.push(CString::new("").unwrap());
                } else {
                    let s = unsafe { CStr::from_ptr(ptr) };
                    str_storage.push(CString::new(s.to_bytes()).unwrap_or_default());
                }
                slots.push(ArgSlot::Ptr(ptr_storage.len()));
                ptr_storage.push(str_storage.last().unwrap().as_ptr() as *mut c_void);
            }
            FfiType::Ptr => {
                let (ptr, heap_addr) =
                    array_buffer_from_value(ctx.heap(), value, &mut array_buffers)?;
                if let Some(addr) = heap_addr {
                    array_copy_back.push((addr, array_buffers.len() - 1));
                }
                slots.push(ArgSlot::Ptr(ptr_storage.len()));
                ptr_storage.push(ptr);
            }
            FfiType::Callback(_) => {
                slots.push(ArgSlot::Ptr(ptr_storage.len()));
                ptr_storage.push(value.raw() as *mut c_void);
            }
            FfiType::Struct(id) => {
                let layouts: Vec<CStructLayout> = ctx.layouts().to_vec();
                let layout = layouts
                    .get(*id as usize)
                    .ok_or_else(|| FfiError::Unsupported(format!("unknown struct layout id {id}")))?
                    .clone();
                let mut buf = Vec::new();
                pack_struct(ctx.heap(), value, &layout, &layouts, &mut buf)?;
                struct_bufs.push(buf);
                slots.push(ArgSlot::Ptr(ptr_storage.len()));
                ptr_storage.push(struct_bufs.last().unwrap().as_ptr() as *mut c_void);
            }
            FfiType::Void => return Err(FfiError::VoidArgument { index: i }),
        }
    }

    let ffi_args: Vec<Arg> = slots
        .iter()
        .map(|slot| match *slot {
            ArgSlot::I64(i) => Arg::new(&i64_storage[i]),
            ArgSlot::I8(i) => Arg::new(&i8_storage[i]),
            ArgSlot::I16(i) => Arg::new(&i16_storage[i]),
            ArgSlot::I32(i) => Arg::new(&i32_storage[i]),
            ArgSlot::U8(i) => Arg::new(&u8_storage[i]),
            ArgSlot::U16(i) => Arg::new(&u16_storage[i]),
            ArgSlot::U32(i) => Arg::new(&u32_storage[i]),
            ArgSlot::U64(i) => Arg::new(&u64_storage[i]),
            ArgSlot::F64(i) => Arg::new(&f64_storage[i]),
            ArgSlot::Ptr(i) => Arg::new(&ptr_storage[i]),
        })
        .collect();

    match sig.ret {
        FfiType::Void => {
            unsafe {
                cif.call::<()>(prepared.addr, &ffi_args);
            }
            copy_array_buffers_back(ctx.heap(), &array_copy_back, &array_buffers);
            Ok(None)
        }
        FfiType::Int | FfiType::Int32 | FfiType::Int16 | FfiType::Int8 => {
            let ret = unsafe { cif.call::<i64>(prepared.addr, &ffi_args) };
            copy_array_buffers_back(ctx.heap(), &array_copy_back, &array_buffers);
            Ok(Some(Value::from(ret)))
        }
        FfiType::UInt8 | FfiType::UInt16 | FfiType::UInt32 | FfiType::UInt64 => {
            let ret = unsafe { cif.call::<u64>(prepared.addr, &ffi_args) };
            copy_array_buffers_back(ctx.heap(), &array_copy_back, &array_buffers);
            Ok(Some(Value::from(ret as i64)))
        }
        FfiType::Float => {
            let ret = unsafe { cif.call::<f64>(prepared.addr, &ffi_args) };
            copy_array_buffers_back(ctx.heap(), &array_copy_back, &array_buffers);
            Ok(Some(Value::from(ret)))
        }
        FfiType::Bool => {
            let ret = unsafe { cif.call::<u8>(prepared.addr, &ffi_args) };
            copy_array_buffers_back(ctx.heap(), &array_copy_back, &array_buffers);
            Ok(Some(Value::from(ret != 0)))
        }
        FfiType::String => {
            let ret = unsafe { cif.call::<*mut c_char>(prepared.addr, &ffi_args) };
            copy_array_buffers_back(ctx.heap(), &array_copy_back, &array_buffers);
            if ret.is_null() {
                Ok(Some(Value::from(0u64)))
            } else {
                let s = unsafe { CStr::from_ptr(ret) };
                let data = s.to_string_lossy();
                let (obj, _gc) = ctx
                    .heap()
                    .alloc(ObjString::from(data.as_ref()), Object::String);
                Ok(Some(Value::from(obj.addr())))
            }
        }
        FfiType::Ptr => {
            let ret = unsafe { cif.call::<*mut c_void>(prepared.addr, &ffi_args) };
            copy_array_buffers_back(ctx.heap(), &array_copy_back, &array_buffers);
            Ok(Some(Value::from(ret as u64)))
        }
        // Opaque function pointer — same representation as Ptr. Re-invoking
        // requires a host/`declare` of the pointed-to symbol; no trampoline
        // is built from a returned callback address in this phase.
        FfiType::Callback(_) => {
            let ret = unsafe { cif.call::<*mut c_void>(prepared.addr, &ffi_args) };
            copy_array_buffers_back(ctx.heap(), &array_copy_back, &array_buffers);
            Ok(Some(Value::from(ret as u64)))
        }
        FfiType::Struct(id) => {
            let layouts: Vec<CStructLayout> = ctx.layouts().to_vec();
            let layout = layouts
                .get(id as usize)
                .ok_or_else(|| FfiError::Unsupported(format!("unknown struct layout id {id}")))?
                .clone();
            let nbytes = struct_byte_size(&layout, &layouts)?;
            // libffi may write at least a full register; pad the buffer.
            let buf_len = nbytes.max(std::mem::size_of::<usize>());
            let mut ret_buf = vec![0u8; buf_len];
            unsafe {
                libffi::raw::ffi_call(
                    cif.as_raw_ptr(),
                    Some(*prepared.addr.as_safe_fun()),
                    ret_buf.as_mut_ptr() as *mut c_void,
                    ffi_args.as_ptr() as *mut *mut c_void,
                );
            }
            copy_array_buffers_back(ctx.heap(), &array_copy_back, &array_buffers);
            let val = unpack_struct(ctx.heap(), &layout, &layouts, &ret_buf[..nbytes])?;
            Ok(Some(val))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::FfiSignatureBuilder;
    use crate::ffi::{library_candidates, DloadGate};

    fn open_libc_ungated() -> Option<std::sync::Arc<libloading::Library>> {
        for c in library_candidates("c", None, &[]) {
            if let Ok(lib) = unsafe { libloading::Library::new(&c) } {
                return Some(std::sync::Arc::new(lib));
            }
        }
        None
    }

    fn resolve_granted(
        stem: &str,
        path: &std::path::Path,
    ) -> Result<std::sync::Arc<libloading::Library>, crate::ffi::FfiError> {
        let mut gate = DloadGate::deny_all();
        gate.grant_file(stem, path)?;
        crate::ffi::resolve_library(path.to_str().unwrap(), None, &[], &gate)
    }

    extern "C" fn add_two(a: i64, b: i64) -> i64 {
        a + b
    }

    #[test]
    fn prepare_cif_accepts_int_int_to_int() {
        let sig = FfiSignature::from_parts("add", vec![FfiType::Int, FfiType::Int], FfiType::Int)
            .unwrap();
        assert!(prepare_cif(&sig, &[]).is_ok());
    }

    #[test]
    fn invoke_rust_fn_via_libffi() {
        let sig = FfiSignature::from_parts("add", vec![FfiType::Int, FfiType::Int], FfiType::Int)
            .unwrap();
        let mut prepared = prepare_cif(&sig, &[]).unwrap();
        prepared.addr = CodePtr::from_ptr(add_two as *mut c_void);
        let mut heap = Heap::default();
        let args = [Value::from(40_i64), Value::from(2_i64)];
        let mut ctx = InvokeContext::new(&mut heap, &[]);
        let mut closures = Vec::new();
        let ret = invoke_via_libffi(&prepared, &sig, &args, None, &mut ctx, &mut closures)
            .unwrap()
            .unwrap();
        assert_eq!(ret.as_int(), 42);
    }

    #[test]
    fn invoke_void_return_pushes_nothing() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static HITS: AtomicUsize = AtomicUsize::new(0);
        extern "C" fn touch(v: i64) {
            HITS.fetch_add(v as usize, Ordering::SeqCst);
        }
        let sig = FfiSignature::from_parts("touch", vec![FfiType::Int], FfiType::Void).unwrap();
        let mut prepared = prepare_cif(&sig, &[]).unwrap();
        prepared.addr = CodePtr::from_ptr(touch as *mut c_void);
        let mut heap = Heap::default();
        let args = [Value::from(3_i64)];
        let mut ctx = InvokeContext::new(&mut heap, &[]);
        let mut closures = Vec::new();
        let ret = invoke_via_libffi(&prepared, &sig, &args, None, &mut ctx, &mut closures).unwrap();
        assert!(ret.is_none());
        assert_eq!(HITS.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn invoke_float_round_trip() {
        extern "C" fn mul(a: f64, b: f64) -> f64 {
            a * b
        }
        let sig =
            FfiSignature::from_parts("mul", vec![FfiType::Float, FfiType::Float], FfiType::Float)
                .unwrap();
        let mut prepared = prepare_cif(&sig, &[]).unwrap();
        prepared.addr = CodePtr::from_ptr(mul as *mut c_void);
        let mut heap = Heap::default();
        let args = [Value::from(2.5_f64), Value::from(4.0_f64)];
        let mut ctx = InvokeContext::new(&mut heap, &[]);
        let mut closures = Vec::new();
        let ret = invoke_via_libffi(&prepared, &sig, &args, None, &mut ctx, &mut closures)
            .unwrap()
            .unwrap();
        assert!((ret.as_float() - 10.0).abs() < f64::EPSILON);
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn invoke_libc_strlen_via_libffi() {
        let lib = match open_libc_ungated() {
            Some(l) => l,
            None => {
                if std::env::var_os("CI").is_some() {
                    panic!("FFI soft-skip forbidden in CI: libc not reachable via dlopen");
                }
                eprintln!("skipping: libc not reachable via dlopen");
                return;
            }
        };
        let sig = FfiSignature::from_parts("strlen", vec![FfiType::String], FfiType::Int).unwrap();
        let prepared = match prepare_cif_for_symbol(&sig, &lib, "strlen", &[]) {
            Ok(p) => p,
            Err(e) => {
                if std::env::var_os("CI").is_some() {
                    panic!("FFI soft-skip forbidden in CI: {e}");
                }
                eprintln!("skipping: {e}");
                return;
            }
        };
        let mut heap = Heap::default();
        let (obj, _gc) = heap.alloc(ObjString::from("hello"), Object::String);
        let args = [Value::from(obj.addr())];
        let mut ctx = InvokeContext::new(&mut heap, &[]);
        let mut closures = Vec::new();
        let ret = invoke_via_libffi(&prepared, &sig, &args, None, &mut ctx, &mut closures)
            .unwrap()
            .unwrap();
        assert_eq!(ret.as_int(), 5);
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn apply_cb_rust_callback() {
        extern "C" fn doubler(x: i64) -> i64 {
            x * 2
        }
        let lib_name = crate::ffi::platform_shared_lib_filename("sum");
        let lib_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../examples")
            .join(&lib_name);
        if !lib_path.exists() {
            if std::env::var_os("CI").is_some() {
                panic!("FFI soft-skip forbidden in CI: {lib_name} not built");
            }
            eprintln!("skipping: {lib_name} not built");
            return;
        }
        let lib = match resolve_granted("sum", &lib_path) {
            Ok(l) => l,
            Err(e) => {
                if std::env::var_os("CI").is_some() {
                    panic!("FFI soft-skip forbidden in CI: {e}");
                }
                eprintln!("skipping: {e}");
                return;
            }
        };
        let sig = FfiSignature::from_parts(
            "apply_cb",
            vec![FfiType::Callback(0), FfiType::Int],
            FfiType::Int,
        )
        .unwrap();
        let prepared = prepare_cif_for_symbol(&sig, &lib, "apply_cb", &[]).unwrap();
        let mut heap = Heap::default();
        let args = [
            Value::from(doubler as *const () as u64),
            Value::from(21_i64),
        ];
        let mut ctx = InvokeContext::new(&mut heap, &[]);
        let mut closures = Vec::new();
        let ret = invoke_via_libffi(&prepared, &sig, &args, None, &mut ctx, &mut closures)
            .unwrap()
            .unwrap();
        assert_eq!(ret.as_int(), 42);
    }

    #[test]
    fn libffi_closure_int_to_int_trampoline_only() {
        use libffi::middle::{Cif, Type};
        use std::ffi::c_void;
        use std::sync::atomic::{AtomicI64, Ordering};
        static LAST: AtomicI64 = AtomicI64::new(0);
        unsafe extern "C" fn tramp(
            _cif: &libffi::low::ffi_cif,
            result: &mut i64,
            args: *const *const c_void,
            _userdata: &(),
        ) {
            let arg_ptr = *args;
            let arg = *(arg_ptr as *const i64);
            LAST.store(arg, Ordering::SeqCst);
            *result = arg * 2;
        }
        let cif = Cif::new(vec![Type::i64()], Type::i64());
        let ud = ();
        let closure = libffi::middle::Closure::new(cif, tramp, &ud);
        type Cb = unsafe extern "C" fn(i64) -> i64;
        let cb: Cb = unsafe { std::mem::transmute(*closure.code_ptr()) };
        assert_eq!(unsafe { cb(21) }, 42);
        assert_eq!(LAST.load(Ordering::SeqCst), 21);
    }

    #[test]
    fn invoke_fails_on_wrong_arity() {
        let sig = FfiSignature::from_parts("add", vec![FfiType::Int, FfiType::Int], FfiType::Int)
            .unwrap();
        let mut prepared = prepare_cif(&sig, &[]).unwrap();
        prepared.addr = CodePtr::from_ptr(add_two as *mut c_void);
        let mut heap = Heap::default();
        let args = [Value::from(1_i64)];
        let mut ctx = InvokeContext::new(&mut heap, &[]);
        let mut closures = Vec::new();
        let err =
            invoke_via_libffi(&prepared, &sig, &args, None, &mut ctx, &mut closures).unwrap_err();
        assert!(matches!(
            err,
            FfiError::ArityMismatch {
                expected: 2,
                got: 1
            }
        ));
    }

    #[test]
    fn promote_variadic_arg_type_widens_narrow_ints() {
        assert_eq!(promote_variadic_arg_type(FfiType::Bool), FfiType::Int);
        assert_eq!(promote_variadic_arg_type(FfiType::Int8), FfiType::Int);
        assert_eq!(promote_variadic_arg_type(FfiType::Int16), FfiType::Int);
        assert_eq!(promote_variadic_arg_type(FfiType::UInt8), FfiType::Int);
        assert_eq!(promote_variadic_arg_type(FfiType::UInt16), FfiType::Int);
        assert_eq!(promote_variadic_arg_type(FfiType::Float), FfiType::Float);
        assert_eq!(promote_variadic_arg_type(FfiType::String), FfiType::String);
        assert_eq!(promote_variadic_arg_type(FfiType::Int), FfiType::Int);
    }

    #[test]
    fn variadic_invoke_requires_at_least_nfixed_args() {
        let sig = FfiSignatureBuilder::new("printf")
            .arg(FfiType::String)
            .ret(FfiType::Int)
            .variadic()
            .build()
            .unwrap();
        let mut prepared = prepare_cif(&sig, &[]).unwrap();
        prepared.addr = CodePtr::from_ptr(add_two as *mut c_void);
        let mut heap = Heap::default();
        let args: [Value; 0] = [];
        let mut ctx = InvokeContext::new(&mut heap, &[]);
        let mut closures = Vec::new();
        let err = invoke_via_libffi(&prepared, &sig, &args, Some(&[]), &mut ctx, &mut closures)
            .unwrap_err();
        assert!(matches!(
            err,
            FfiError::ArityMismatch {
                expected: 1,
                got: 0
            }
        ));
    }

    #[test]
    fn variadic_invoke_requires_type_tags() {
        let sig = FfiSignatureBuilder::new("printf")
            .arg(FfiType::String)
            .ret(FfiType::Int)
            .variadic()
            .build()
            .unwrap();
        let mut prepared = prepare_cif(&sig, &[]).unwrap();
        prepared.addr = CodePtr::from_ptr(add_two as *mut c_void);
        let mut heap = Heap::default();
        let (obj, _gc) = heap.alloc(ObjString::from("hi"), Object::String);
        let args = [Value::from(obj.addr())];
        let mut ctx = InvokeContext::new(&mut heap, &[]);
        let mut closures = Vec::new();
        let err =
            invoke_via_libffi(&prepared, &sig, &args, None, &mut ctx, &mut closures).unwrap_err();
        assert!(matches!(err, FfiError::Unsupported(_)));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn variadic_invoke_libc_snprintf_formats_int() {
        use std::ffi::CStr;

        let lib = match open_libc_ungated() {
            Some(l) => l,
            None => {
                if std::env::var_os("CI").is_some() {
                    panic!("FFI soft-skip forbidden in CI: libc not reachable via dlopen");
                }
                eprintln!("skipping: libc not reachable via dlopen");
                return;
            }
        };

        // Fixed prefix: Ptr (buf), Int (size), String (fmt); then variadic Int.
        let sig = FfiSignatureBuilder::new("snprintf")
            .arg(FfiType::Ptr)
            .arg(FfiType::Int)
            .arg(FfiType::String)
            .ret(FfiType::Int)
            .variadic()
            .build()
            .unwrap();
        let prepared = match prepare_cif_for_symbol(&sig, &lib, "snprintf", &[]) {
            Ok(p) => p,
            Err(e) => {
                if std::env::var_os("CI").is_some() {
                    panic!("FFI soft-skip forbidden in CI: {e}");
                }
                eprintln!("skipping: {e}");
                return;
            }
        };

        let mut heap = Heap::default();
        let mut buf = vec![0u8; 64];
        let (fmt_obj, _gc) = heap.alloc(ObjString::from("hello %i"), Object::String);
        let args = [
            Value::from(buf.as_mut_ptr() as u64),
            Value::from(64_i64),
            Value::from(fmt_obj.addr()),
            Value::from(42_i64),
        ];
        let tags = [FfiType::Ptr, FfiType::Int, FfiType::String, FfiType::Int];
        let mut ctx = InvokeContext::new(&mut heap, &[]);
        let mut closures = Vec::new();
        let ret = invoke_via_libffi(&prepared, &sig, &args, Some(&tags), &mut ctx, &mut closures)
            .unwrap()
            .unwrap();
        assert!(ret.as_int() > 0);
        let s = unsafe { CStr::from_ptr(buf.as_ptr() as *const c_char) };
        assert_eq!(s.to_string_lossy(), "hello 42");
    }
    /// COI-234: `declare(..., Int)` must reject a Stream. Do not inspect an fd.
    #[test]
    fn invoke_int_rejects_stream_argument() {
        extern "C" fn sink(_v: i64) {}
        let sig = FfiSignature::from_parts("sink", vec![FfiType::Int], FfiType::Void).unwrap();
        let mut prepared = prepare_cif(&sig, &[]).unwrap();
        prepared.addr = CodePtr::from_ptr(sink as *mut c_void);
        let mut heap = Heap::default();
        let stream = crate::io::stream_stdout(&mut heap).expect("stdout stream");
        let args = [stream];
        let mut ctx = InvokeContext::new(&mut heap, &[]);
        let mut closures = Vec::new();
        let result = invoke_via_libffi(&prepared, &sig, &args, None, &mut ctx, &mut closures);
        assert!(
            result.is_err(),
            "FFI Int must not coerce a Stream (no silent fd())"
        );
    }
}
