//! The FFI ops on the [`Runtime`]: `FfiLoad`, `DeclareFFI` and
//! `FfiInvoke` return the `Result` value the op pushes, for the runtime and for
//! native code alike.

use common::Value;

use crate::CStructLayout;
use crate::memory::{Heap, Object};
use crate::runtime::Runtime;

/// An `FfiInvoke`'s operands, taken off the stack.
pub(crate) struct FfiCall {
    pub(crate) lib_addr: u64,
    pub(crate) function_id: usize,
    pub(crate) args: Vec<Value>,
    pub(crate) arg_types: Option<Vec<crate::memory::FfiType>>,
}

impl FfiCall {
    /// The call from its operand words: library, function id, argument
    /// tuple and, for a variadic call, the argument tags tuple.
    pub(crate) fn from_words(heap: &Heap, words: &[Value]) -> Self {
        let tuple = |v: Option<&Value>| match v.and_then(|v| heap.find_object_by_addr(v.bits())) {
            Some(Object::Enum(gc)) if gc.as_ref().is_tuple() => Some(gc.as_ref().elements().to_vec()),
            _ => None,
        };
        let arg_types = (words.len() > 3).then(|| tuple(words.get(3)).unwrap_or_default().iter().map(|v| ffi_type_from_value(v, heap)).collect());
        FfiCall {
            lib_addr: words.first().map_or(0, |v| v.bits()),
            function_id: words.get(1).map_or(0, |v| v.as_int() as usize),
            args: tuple(words.get(2)).unwrap_or_default(),
            arg_types,
        }
    }
}

/// An FFI type tag word (`ffi::Type`) as the type it names.
fn ffi_type_from_value(v: &Value, heap: &Heap) -> crate::memory::FfiType {
    let raw = v.bits();
    let (tag, aux) = if raw <= common::tag::LAST as u64 {
        (raw as u32, 0)
    } else if raw > 0xFFFF {
        ((raw & 0xFFFF) as u32, (raw >> 16) as u32)
    } else if let Some(crate::memory::Object::Enum(gc)) = heap.find_object_by_addr(raw) {
        (gc.as_ref().tag, 0)
    } else {
        (common::tag::INT, 0)
    };
    crate::memory::FfiType::from_tag(tag, aux)
}

/// Register a new FFI function on the given library `Object`.
fn register_signature_on_object(obj: &mut Object, sig: crate::ffi::FfiSignature, layouts: &[CStructLayout]) -> Result<usize, crate::ffi::FfiError> {
    if let crate::memory::Object::Library(gc) = obj {
        let obj_lib: &mut crate::memory::ObjLibrary = (**gc).as_mut();
        crate::ffi::register_on_library(obj_lib, sig, layouts)
    } else {
        Err(crate::ffi::FfiError::InvalidHandle("not a library object".into()))
    }
}

/// The FFI ops (`FfiLoad`, `DeclareFFI`, `FfiInvoke`) on the runtime: each
/// returns the `Result` value the op pushes.
impl Runtime {
    fn ffi_tuple_types(&self, tuple: Value) -> Vec<crate::memory::FfiType> {
        match self.heap.find_object_by_addr(tuple.bits()) {
            Some(Object::Enum(gc)) if gc.as_ref().is_tuple() => gc.as_ref().elements().iter().map(|v| ffi_type_from_value(v, &self.heap)).collect(),
            _ => Vec::new(),
        }
    }

    fn ffi_err(&mut self, kind: crate::ffi::FfiErrorKindTag, message: String) -> Value {
        crate::ffi::alloc_result_ffi_err(&mut self.heap, kind, message)
    }

    fn ffi_error(&mut self, err: crate::ffi::FfiError) -> Value {
        let kind = crate::ffi::FfiErrorKindTag::from_ffi_error(&err);
        self.ffi_err(kind, err.to_string())
    }

    /// `FfiLoad`: load the library at the string `path`.
    pub(crate) fn ffi_load(&mut self, path: Value) -> Value {
        let path = match self.heap.find_object_by_addr(path.bits()) {
            Some(Object::String(gc)) => gc.as_ref().data.to_string(),
            _ => String::new(),
        };
        match crate::ffi::resolve_library(&path, self.ffi.base_dir.as_deref(), &self.ffi.search_paths, &self.ffi.dload_gate) {
            Ok(lib_arc) => {
                self.ffi.libraries.entry(path.clone()).or_insert_with(|| lib_arc.clone());
                let (object, lib) = self.heap.alloc_library(lib_arc);
                lib.payload_mut().static_symbols = crate::ffi::static_library(&path);
                let addr = object.addr();
                self.ffi.userland_libraries.insert(addr, object);
                crate::io::alloc_result_ok(&mut self.heap, Value::from(addr))
            }
            Err(e) => self.ffi_error(e),
        }
    }

    /// `DeclareFFI` (operand `raw`): declare `name` in library `lib` with
    /// the argument tags tuple `args` and return tag `ret`.
    pub(crate) fn ffi_declare(&mut self, raw: u32, lib: Value, name: Value, args: Value, ret: Value) -> Value {
        let variadic = (raw & (1 << 16)) != 0;
        let ret = ffi_type_from_value(&ret, &self.heap);
        let args = self.ffi_tuple_types(args);
        let name = match self.heap.find_object_by_addr(name.bits()) {
            Some(Object::String(gc)) => gc.as_ref().data.to_string(),
            _ => String::new(),
        };
        let lib_addr = lib.bits();
        let Some(mut owned) = self.ffi.userland_libraries.get(&lib_addr).copied() else {
            return self.ffi_err(crate::ffi::FfiErrorKindTag::InvalidHandle, format!("FFI declare: library at 0x{lib_addr:x} is not loaded"));
        };
        let sig = crate::ffi::FfiSignature { name, args, ret, variadic };
        match register_signature_on_object(&mut owned, sig, &self.ffi.struct_layouts) {
            Ok(id) => {
                self.ffi.userland_libraries.insert(lib_addr, owned);
                crate::io::alloc_result_ok(&mut self.heap, Value::from(id as i64))
            }
            Err(e) => self.ffi_error(e),
        }
    }

    /// `FfiInvoke`: call the declared function. `callback` makes the C
    /// trampoline for a callback argument (the function at that offset).
    pub(crate) fn ffi_invoke(&mut self, lib_addr: u64, function_id: usize, args: &[Value], arg_types: Option<&[crate::memory::FfiType]>, callback: &mut dyn FnMut(u32) -> Result<crate::ffi::OwnedClosure, crate::ffi::FfiError>) -> Value {
        let lib = match self.ffi.userland_libraries.get(&lib_addr).copied() {
            Some(Object::Library(gc)) => gc,
            Some(_) => return self.ffi_err(crate::ffi::FfiErrorKindTag::InvalidHandle, "invalid library handle (not a loaded library)".into()),
            None => return self.ffi_error(crate::ffi::FfiError::InvalidHandle("invalid library handle".into())),
        };
        let lib_ref: &crate::memory::ObjLibrary = lib.as_ref();
        let Some(registered) = lib_ref.signatures.get(function_id) else {
            return self.ffi_error(crate::ffi::FfiError::InvalidHandle("function id out of range".into()));
        };
        let sig = registered.ffi_signature();
        let mut owned_args;
        let mut args = args;
        if sig.args.iter().any(|ty| matches!(ty, crate::memory::FfiType::Callback(_))) {
            owned_args = args.to_vec();
            for (i, ty) in sig.args.iter().enumerate() {
                if let crate::memory::FfiType::Callback(_) = ty {
                    let made = callback(owned_args[i].as_int() as u32);
                    match made {
                        Ok(closure) => {
                            owned_args[i] = Value::from(closure.code_ptr_usize() as u64);
                            self.ffi.closures.push(closure);
                        }
                        Err(e) => return self.ffi_error(e),
                    }
                }
            }
            args = &owned_args;
        }
        // No coil callback can run during the call without a closure:
        // packed byte arrays can be lent in place.
        let mut ctx = crate::ffi::InvokeContext::new(self.heap.get_mut() as *mut Heap, &self.ffi.struct_layouts).lending_arrays(self.ffi.closures.is_empty());
        let mut closure_ptrs = Vec::new();
        let roots_len = self.ffi_arg_roots.len();
        self.ffi_arg_roots.extend_from_slice(args);
        let result = crate::ffi::invoke_via_libffi(&registered.prepared, sig, args, arg_types, &mut ctx, &mut closure_ptrs);
        self.ffi_arg_roots.truncate(roots_len);
        match result {
            Ok(Some(v)) => crate::io::alloc_result_ok(&mut self.heap, v),
            Ok(None) => crate::io::alloc_result_ok(&mut self.heap, Value::default()),
            Err(e) => self.ffi_error(e),
        }
    }
}
