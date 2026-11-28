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
pub(crate) fn ffi_type_from_value(v: &Value, heap: &Heap) -> crate::memory::FfiType {
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

    /// The library behind handle `addr`: one this runtime loaded, or this
    /// runtime's copy of one its spawning runtime loaded, loaded and
    /// declared again on first use (#2902). Native code calls through the
    /// address it read at setup instead, so only the runtime's call path
    /// (the MIR interpreter, `invoke`) needs the copy.
    fn library_object(&mut self, addr: u64) -> Option<Object> {
        if let Some(&object) = self.ffi.userland_libraries.get(&addr) {
            return Some(object);
        }
        if let Some(local) = self.ffi.adopted.get(&addr) {
            return self.ffi.userland_libraries.get(local).copied();
        }
        let recipe = self.ffi.recipes.lock().ok()?.get(&addr).cloned()?;
        let lib_arc = crate::ffi::resolve_library(&recipe.path, self.ffi.base_dir.as_deref(), &self.ffi.search_paths, &self.ffi.dload_gate).ok()?;
        self.ffi.libraries.entry(recipe.path.clone()).or_insert_with(|| lib_arc.clone());
        let (mut object, lib) = self.heap.alloc_library(lib_arc);
        lib.payload_mut().static_symbols = crate::ffi::static_library(&recipe.path);
        for sig in recipe.signatures {
            register_signature_on_object(&mut object, sig, &self.ffi.struct_layouts).ok()?;
        }
        let local = object.addr();
        self.ffi.userland_libraries.insert(local, object);
        self.ffi.adopted.insert(addr, local);
        Some(object)
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
                if let Ok(mut recipes) = self.ffi.recipes.lock() {
                    recipes.insert(addr, crate::ffi::LibraryRecipe { path, signatures: Vec::new() });
                }
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
        let Some(mut owned) = self.library_object(lib_addr) else {
            return self.ffi_err(crate::ffi::FfiErrorKindTag::InvalidHandle, format!("FFI declare: library at 0x{lib_addr:x} is not loaded"));
        };
        let sig = crate::ffi::FfiSignature { name, args, ret, variadic };
        match register_signature_on_object(&mut owned, sig.clone(), &self.ffi.struct_layouts) {
            Ok(id) => {
                self.ffi.userland_libraries.insert(owned.addr(), owned);
                // A library this runtime loaded: its jobs declare the same
                // ids on their copies.
                if let Ok(mut recipes) = self.ffi.recipes.lock()
                    && let Some(recipe) = recipes.get_mut(&lib_addr)
                    && recipe.signatures.len() == id
                {
                    recipe.signatures.push(sig);
                }
                crate::io::alloc_result_ok(&mut self.heap, Value::from(id as i64))
            }
            Err(e) => self.ffi_error(e),
        }
    }

    /// `FfiInvoke`: call the declared function. `callback` makes the C
    /// trampoline for a callback argument (the function at that offset).
    pub(crate) fn ffi_invoke(&mut self, lib_addr: u64, function_id: usize, args: &[Value], arg_types: Option<&[crate::memory::FfiType]>, callback: &mut dyn FnMut(u32, u32) -> Result<crate::ffi::OwnedClosure, crate::ffi::FfiError>) -> Value {
        match self.ffi_call(lib_addr, function_id, args, arg_types, callback) {
            Ok(v) => crate::io::alloc_result_ok(&mut self.heap, v),
            Err((kind, message)) => self.ffi_err(kind, message),
        }
    }

    /// `FFI_OP_ADDR`: the C address of function `function_id` of the
    /// library at `lib`, as a `Result`.
    pub(crate) fn ffi_addr(&mut self, lib: Value, function_id: Value) -> Value {
        let addr = match self.library_object(lib.bits()) {
            Some(Object::Library(gc)) => gc.as_ref().signatures.get(function_id.as_int() as usize).map(|f| f.prepared.address()),
            _ => None,
        };
        match addr {
            Some(a) => crate::io::alloc_result_ok(&mut self.heap, Value::from(a as i64)),
            None => self.ffi_error(crate::ffi::FfiError::InvalidHandle("function id out of range".into())),
        }
    }

    /// `FfiLoad` with [`crate::FFI_LINKED_HOOK`]: the library at `path`
    /// from the executable's table of linked functions, with no file
    /// opened and no gate (it was checked when the executable linked).
    pub(crate) fn ffi_load_linked(&mut self, path: Value) -> Value {
        let path = match self.heap.find_object_by_addr(path.bits()) {
            Some(Object::String(gc)) => gc.as_ref().data.to_string(),
            _ => String::new(),
        };
        match crate::ffi::static_library(&path) {
            Some(lib) => {
                self.ffi.linked.push((lib, Vec::new()));
                crate::io::alloc_result_ok(&mut self.heap, Value::from(self.ffi.linked.len() as i64))
            }
            None => self.ffi_err(crate::ffi::FfiErrorKindTag::LibraryNotFound, format!("FFI library `{path}` is not linked into this executable")),
        }
    }

    /// `DeclareFFI` with [`crate::FFI_LINKED_HOOK`]: function `name` of
    /// linked library `lib`, by its address in the table.
    pub(crate) fn ffi_declare_linked(&mut self, lib: Value, name: Value) -> Value {
        let name = match self.heap.find_object_by_addr(name.bits()) {
            Some(Object::String(gc)) => gc.as_ref().data.to_string(),
            _ => String::new(),
        };
        let Some((table, declared)) = (lib.as_int() as usize).checked_sub(1).and_then(|i| self.ffi.linked.get_mut(i)) else {
            return self.ffi_err(crate::ffi::FfiErrorKindTag::InvalidHandle, "invalid library handle".into());
        };
        match table.get(&name) {
            Some(&addr) => {
                declared.push(addr);
                let id = declared.len() as i64 - 1;
                crate::io::alloc_result_ok(&mut self.heap, Value::from(id))
            }
            None => self.ffi_error(crate::ffi::FfiError::SymbolNotFound { name }),
        }
    }

    /// `FFI_OP_ADDR` with [`crate::FFI_LINKED_HOOK`].
    pub(crate) fn ffi_addr_linked(&mut self, lib: Value, function_id: Value) -> Value {
        let addr = (lib.as_int() as usize).checked_sub(1).and_then(|i| self.ffi.linked.get(i)).and_then(|(_, d)| d.get(function_id.as_int() as usize).copied());
        match addr {
            Some(a) => crate::io::alloc_result_ok(&mut self.heap, Value::from(a as i64)),
            None => self.ffi_error(crate::ffi::FfiError::InvalidHandle("function id out of range".into())),
        }
    }

    /// [`Runtime::ffi_invoke`]'s call: the C function's word (`0` for a
    /// `void` one), or its error's kind and message.
    pub(crate) fn ffi_call(&mut self, lib_addr: u64, function_id: usize, args: &[Value], arg_types: Option<&[crate::memory::FfiType]>, callback: &mut dyn FnMut(u32, u32) -> Result<crate::ffi::OwnedClosure, crate::ffi::FfiError>) -> Result<Value, (crate::ffi::FfiErrorKindTag, String)> {
        let error = |e: crate::ffi::FfiError| (crate::ffi::FfiErrorKindTag::from_ffi_error(&e), e.to_string());
        let lib = match self.library_object(lib_addr) {
            Some(Object::Library(gc)) => gc,
            Some(_) => return Err((crate::ffi::FfiErrorKindTag::InvalidHandle, "invalid library handle (not a loaded library)".into())),
            None => return Err(error(crate::ffi::FfiError::InvalidHandle("invalid library handle".into()))),
        };
        let lib_ref: &crate::memory::ObjLibrary = lib.as_ref();
        let Some(registered) = lib_ref.signatures.get(function_id) else {
            return Err(error(crate::ffi::FfiError::InvalidHandle("function id out of range".into())));
        };
        let sig = registered.ffi_signature();
        let mut owned_args;
        let mut args = args;
        if sig.args.iter().any(|ty| matches!(ty, crate::memory::FfiType::Callback(_))) {
            owned_args = args.to_vec();
            for (i, ty) in sig.args.iter().enumerate() {
                if let crate::memory::FfiType::Callback(aux) = *ty {
                    // One closure per function and signature, made once.
                    let key = (owned_args[i].as_int() as u32, aux);
                    let at = match self.ffi.closure_of.get(&key) {
                        Some(&at) => at,
                        None => match callback(key.0, key.1) {
                            Ok(closure) => {
                                self.ffi.closures.push(closure);
                                self.ffi.closure_of.insert(key, self.ffi.closures.len() - 1);
                                self.ffi.closures.len() - 1
                            }
                            Err(e) => return Err(error(e)),
                        },
                    };
                    owned_args[i] = Value::from(self.ffi.closures[at].code_ptr_usize() as u64);
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
        result.map(Option::unwrap_or_default).map_err(error)
    }
}
