//! Mark-and-sweep heap: intrusive object list, string interning, and GC.

use std::collections::{HashMap, HashSet};

use super::AddrHashBuilder;

#[cfg(feature = "crypto")]
use crate::crypto_hasher_state::ObjCryptoHasher;

const GC_NEXT_THRESHOLD: usize = 1024 * 1024;
const GC_GROWTH_FACTOR: usize = 2;

/// Managed heap. Objects are linked in an intrusive list for traversal.
/// `Gc<T>` handles are copyable; the VM controls when objects become unreachable.
pub struct Heap {
    alloc_bytes: usize,
    gc_next_threshold: usize,
    gc_growth_factor: usize,
    strings: Table<()>,
    head: Option<Object>,
    /// O(1) lookup of live objects by address (updated on alloc/sweep).
    addr_index: HashMap<u64, Object, AddrHashBuilder>,
    /// Immortal arity-0 enum singletons keyed by tag (never swept).
    immortal_enums: HashMap<u32, Object, AddrHashBuilder>,
    /// Reused mark-set across collections (avoids alloc per GC).
    gc_mark_set: HashSet<u64, AddrHashBuilder>,
    /// Reused gray worklist / root buffers across collections.
    gc_gray: Vec<Object>,
    gc_root_objects: Vec<Object>,
    gc_roots: Vec<u64>,
    gc_dangling_strings: Vec<RefString>,
}

impl Default for Heap {
    fn default() -> Self {
        Self {
            alloc_bytes: 0,
            gc_next_threshold: GC_NEXT_THRESHOLD,
            gc_growth_factor: GC_GROWTH_FACTOR,
            strings: Table::default(),
            head: None,
            addr_index: HashMap::default(),
            immortal_enums: HashMap::default(),
            gc_mark_set: HashSet::default(),
            gc_gray: Vec::new(),
            gc_root_objects: Vec::new(),
            gc_roots: Vec::new(),
            gc_dangling_strings: Vec::new(),
        }
    }
}

impl Heap {
    /// Look up a heap string by address and return a NUL-terminated C string
    /// for FFI. The returned pointer is leaked for the duration of the call.
    #[must_use]
    pub fn cstr_from_addr(&self, addr: u64) -> Option<*const std::os::raw::c_char> {
        if let Some(crate::memory::Object::String(gc)) = self.find_object_by_addr(addr) {
            let s: std::ffi::CString = std::ffi::CString::new(gc.as_ref().data.as_bytes()).ok()?;
            let boxed: &'static std::ffi::CString = Box::leak(Box::new(s));
            return Some(boxed.as_ptr());
        }
        None
    }

    /// Allocates an object and returns its handle. The object is pushed to the
    /// front of the list of allocated objects.
    pub fn alloc<T: GcSized, F>(&mut self, data: T, map: F) -> (Object, Gc<T>)
    where
        F: Fn(Gc<T>) -> Object,
    {
        let boxed = Box::new(GcData::new(self.head, data));
        let content = Gc::new(boxed);
        let object = map(content);
        let size = object.size();
        self.head = Some(object);
        self.alloc_bytes += size;
        self.addr_index.insert(object.addr(), object);
        crate::vm::note_heap_alloc();

        (object, content)
    }

    /// Interns a string and returns its handle. The same reference is returned
    /// for two equal strings.
    pub fn intern(&mut self, data: String) -> RefString {
        let hash = ObjString::hash(&data);
        if let Some(s) = self.strings.find(&data, hash) {
            return s;
        }
        self.intern_new(data, hash)
    }

    /// Intern a borrowed string without allocating when it is already cached.
    pub fn intern_str(&mut self, data: &str) -> RefString {
        let hash = ObjString::hash(data);
        if let Some(s) = self.strings.find(data, hash) {
            return s;
        }
        self.intern_new(data.to_owned(), hash)
    }

    /// Register an existing string object in the intern table when needed.
    pub fn intern_ref(&mut self, string: RefString) -> RefString {
        let data = string.as_ref();
        if let Some(s) = self.strings.find(&data.data, data.hash) {
            return s;
        }
        self.strings.insert(string, ());
        string
    }

    fn intern_new(&mut self, data: String, hash: u32) -> RefString {
        let obj_string = ObjString { data, hash };
        let (_, s) = self.alloc(obj_string, Object::String);
        self.strings.insert(s, ());
        s
    }

    /// Allocate a loaded FFI library as `Object::Library`.
    pub fn alloc_library(
        &mut self,
        library: std::sync::Arc<crate::ffi::Library>,
    ) -> (Object, crate::memory::Gc<ObjLibrary>) {
        let obj_lib = ObjLibrary {
            library,
            signatures: Vec::new(),
            by_name: std::collections::HashMap::new(),
            closures: Vec::new(),
        };
        self.alloc(obj_lib, Object::Library)
    }

    /// Allocate an enum value, reusing the immortal object for unit variants.
    pub fn alloc_enum_value(&mut self, tag: u32, payload: Vec<Member>) -> common::Value {
        let object = if payload.is_empty() {
            self.immortal_unit_enum(tag)
        } else {
            self.alloc(ObjEnum { tag, payload }, Object::Enum).0
        };
        common::Value::from(object.addr())
    }

    /// Releases all objects that aren't marked. This method also removes
    /// interned strings when no object is referencing them.
    ///
    /// ## Safety
    ///
    /// The caller must ensure that all reachable pointers have been marked.
    /// Otherwise, we'll deallocate objects that are in use and leave dangling
    /// pointers.
    pub unsafe fn sweep(&mut self) {
        let mut prev_obj: Option<Object> = None;
        let mut curr_obj = self.head;

        let mut dangling_strings = std::mem::take(&mut self.gc_dangling_strings);
        dangling_strings.clear();
        for (k, ()) in self.strings.iter() {
            if !k.is_marked() {
                dangling_strings.push(k);
            }
        }
        for s in dangling_strings.drain(..) {
            self.strings.remove(s);
        }

        while let Some(curr_ref) = curr_obj {
            let next = curr_ref.get_next();
            if curr_ref.is_marked() {
                curr_ref.unmark();
                prev_obj = curr_obj;
                curr_obj = next;
            } else {
                self.addr_index.remove(&curr_ref.addr());
                unsafe { self.dealloc(curr_ref) };
                curr_obj = next;
                if let Some(prev_ref) = prev_obj {
                    prev_ref.set_next(next);
                } else {
                    self.head = curr_obj;
                }
            }
        }

        self.gc_next_threshold = self.alloc_bytes * self.gc_growth_factor;
        self.gc_dangling_strings = dangling_strings;
    }

    /// Returns the number of bytes that are being allocated.
    pub const fn size(&self) -> usize {
        self.alloc_bytes
    }

    /// Number of live heap objects (for GC pressure after `HostInvoke`).
    #[inline]
    pub fn live_object_count(&self) -> usize {
        self.addr_index.len()
    }

    /// True when live heap bytes exceed the collection threshold. [`Self::sweep`]
    /// rescales the threshold to `live * GC_GROWTH_FACTOR`, so collection cost
    /// stays proportional to the live set rather than to the allocation count.
    #[inline]
    pub fn should_collect(&self) -> bool {
        self.alloc_bytes > self.gc_next_threshold
    }

    /// Lower the byte threshold so the next [`Self::should_collect`] check
    /// fires (test helper for GC stress).
    #[cfg(any(test, feature = "debugger"))]
    pub fn set_gc_threshold_for_test(&mut self, bytes: usize) {
        self.gc_next_threshold = bytes;
    }

    /// Adjust tracked heap bytes after an in-place grow/shrink of a managed
    /// object's internal Rust allocation (for example `ObjArray.elements`).
    pub fn account_resize(&mut self, old_size: usize, new_size: usize) {
        if new_size >= old_size {
            self.alloc_bytes += new_size - old_size;
        } else {
            self.alloc_bytes -= old_size - new_size;
        }
    }

    /// Deallocates an object.
    ///
    /// ## Safety
    ///
    /// + The caller must ensure that no other piece of code will ever use this
    ///   reference. Otherwise, we'll risk dereferencing a dangling pointer.
    /// + Before calling this method, the caller must ensure that the object was
    ///   removed from the linked list of heap-allocated objects.
    unsafe fn dealloc(&mut self, object: Object) {
        let size = object.size();
        self.alloc_bytes -= size;

        match object {
            Object::String(s) => {
                s.release();
            }
            Object::Instance(i) => {
                i.release();
            }
            Object::Enum(e) => {
                e.release();
            }
            Object::Library(l) => {
                l.release();
            }
            Object::Tuple(t) => {
                t.release();
            }
            Object::Array(a) => {
                a.release();
            }
            Object::Coroutine(c) => {
                c.release();
            }
            Object::Boxed(b) => {
                b.release();
            }
            Object::Root(r) => {
                r.release();
            }
            Object::Weak(w) => {
                w.release();
            }
            Object::PolyFn(p) => {
                p.release();
            }
            Object::Fn(f) => {
                f.release();
            }
            Object::Stream(s) => {
                // Closing the fd happens in ObjStream::drop via release.
                s.release();
            }
            Object::Thread(t) => {
                t.release();
            }
            Object::Sender(s) => {
                s.release();
            }
            Object::Receiver(r) => {
                r.release();
            }
            Object::Mutex(m) => {
                m.release();
            }
            Object::RwLock(l) => {
                l.release();
            }
            #[cfg(feature = "crypto")]
            Object::CryptoHasher(h) => {
                h.release();
            }
        }
    }

    pub fn trace(&mut self, values: &[u64]) {
        self.gc_mark_set.clear();
        self.gc_mark_set.extend(values.iter().copied());
        let mut gray = std::mem::take(&mut self.gc_gray);
        gray.clear();
        let mut current = self.head;

        while let Some(reference) = current {
            if !reference.is_marked() && self.gc_mark_set.contains(&reference.addr()) {
                reference.mark(&mut gray);
            }

            current = reference.get_next();
        }
        gray.clear();
        self.gc_gray = gray;
    }

    /// Clear [`Object::Weak`] handles whose referents were not marked.
    ///
    /// Must run after the mark phase and before [`Self::sweep`] so upgrades
    /// never observe a recycled address (ABA).
    pub fn clear_dead_weaks(&self) {
        let mut current = self.head;
        while let Some(obj) = current {
            if let Object::Weak(gc) = obj {
                let weak = gc.as_ref();
                if !weak.cleared.get() {
                    let target = weak.target.get();
                    let addr = target.raw() as u64;
                    if addr != 0
                        && let Some(referent) = self.find_object_by_addr(addr)
                        && !referent.is_marked()
                    {
                        weak.cleared.set(true);
                        weak.target.set(Value::from(0i64));
                    }
                }
            }
            current = obj.get_next();
        }
    }

    /// Take the reusable GC root address buffer (caller must restore via [`Self::restore_gc_roots`]).
    /// Immortal arity-0 enum singletons are always seeded as roots.
    pub fn take_gc_roots(&mut self) -> Vec<u64> {
        let mut roots = std::mem::take(&mut self.gc_roots);
        roots.clear();
        for obj in self.immortal_enums.values() {
            roots.push(obj.addr());
        }
        roots
    }

    /// Return a shared arity-0 enum for `tag`, allocating once per tag.
    pub fn immortal_unit_enum(&mut self, tag: u32) -> Object {
        if let Some(obj) = self.immortal_enums.get(&tag) {
            return *obj;
        }
        let obj_enum = crate::memory::ObjEnum {
            tag,
            payload: Vec::new(),
        };
        let (object, _) = self.alloc(obj_enum, Object::Enum);
        self.immortal_enums.insert(tag, object);
        object
    }

    pub fn restore_gc_roots(&mut self, roots: Vec<u64>) {
        self.gc_roots = roots;
    }

    pub fn take_gc_worklists(&mut self) -> (Vec<Object>, Vec<Object>) {
        let mut gray = std::mem::take(&mut self.gc_gray);
        let mut root_objects = std::mem::take(&mut self.gc_root_objects);
        gray.clear();
        root_objects.clear();
        (gray, root_objects)
    }

    pub fn restore_gc_worklists(&mut self, gray: Vec<Object>, root_objects: Vec<Object>) {
        self.gc_gray = gray;
        self.gc_root_objects = root_objects;
    }

    /// Head of the intrusive object list (for address lookup).
    pub fn head_for_lookup(&self) -> Option<Object> {
        self.head
    }

    /// Find a heap object by its address (O(1) via addr index).
    pub fn find_object_by_addr(&self, addr: u64) -> Option<Object> {
        self.addr_index.get(&addr).copied()
    }

    #[cfg(feature = "crypto")]
    pub fn with_crypto_hasher<R>(
        &mut self,
        addr: u64,
        f: impl FnOnce(&mut ObjCryptoHasher) -> R,
    ) -> Option<R> {
        if let Some(Object::CryptoHasher(gc)) = self.find_object_by_addr(addr) {
            return Some(f(gc.payload_mut()));
        }
        None
    }

    /// Write back scratch-buffer values into a live `ObjArray`.
    pub fn update_array_elements(&mut self, addr: u64, values: &[i64]) {
        if let Some(Object::Array(mut gc)) = self.find_object_by_addr(addr) {
            let arr = gc.as_mut();
            for (i, &v) in values.iter().enumerate() {
                if i < arr.elements.len() {
                    arr.elements[i] = Value::from(v);
                }
            }
        }
    }

    /// True if `addr` is a live heap object.
    pub fn contains_addr(&self, addr: *mut u8) -> bool {
        self.addr_index.contains_key(&(addr as u64))
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        for object in &*self {
            unsafe {
                self.dealloc(object);
            }
        }

        debug_assert_eq!(0, self.alloc_bytes);
    }
}

impl IntoIterator for &Heap {
    type Item = Object;

    type IntoIter = HeapIter;

    fn into_iter(self) -> Self::IntoIter {
        Self::IntoIter { next: self.head }
    }
}

/// An iterator through all currently allocated objects.
pub struct HeapIter {
    next: Option<Object>,
}

impl Iterator for HeapIter {
    type Item = Object;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(node) = self.next {
            self.next = node.get_next();
            return Some(node);
        }
        None
    }
}

#[cfg(debug_assertions)]
use std::fmt::Debug;

use std::{
    cell::Cell,
    error, fmt, mem,
    ops::{self, BitXor, Deref},
    ptr::NonNull,
};

pub type RefString = Gc<ObjString>;
pub type RefInstance = Gc<ObjInstance>;
pub type RefEnum = Gc<ObjEnum>;
pub type RefLibrary = Gc<ObjLibrary>;
pub type RefCoroutine = Gc<ObjCoroutine>;

/// Lifecycle of a heap-allocated coroutine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoroState {
    /// Created but never resumed, or suspended at a `yield`.
    Suspended,
    /// Body returned; further `resume` is a no-op (returns default).
    Done,
}

/// An enumeration of all potential errors that occur when working with objects.
#[derive(Debug)]
pub enum Error {
    InvalidCast,
}

impl error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCast => write!(f, "Invalid cast."),
        }
    }
}

pub type RefBoxed = Gc<ObjBoxed>;
pub type RefRoot = Gc<ObjRoot>;
pub type RefWeak = Gc<ObjWeak>;
pub type RefPolyFn = Gc<ObjPolyFn>;
pub type RefFn = Gc<ObjFn>;
pub type RefStream = Gc<ObjStream>;
pub type RefThread = Gc<ObjThread>;
pub type RefSender = Gc<ObjSender>;
pub type RefReceiver = Gc<ObjReceiver>;
pub type RefThreadMutex = Gc<ObjThreadMutex>;
pub type RefRwLock = Gc<ObjRwLock>;
#[cfg(feature = "crypto")]
pub type RefCryptoHasher = Gc<ObjCryptoHasher>;

/// Kind of host-backed IO stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamKind {
    Stdin,
    Stdout,
    Stderr,
    File,
    Tcp,
    TcpListener,
    /// Datagram socket (`io::net::udp::bind` / `connect`).
    Udp,
    /// Package IO attached in place (`Stream.attach` / leftover enable shim).
    Attached,
}

#[derive(Clone, Copy)]
pub enum Object {
    String(RefString),
    Instance(RefInstance),
    Enum(RefEnum),
    Library(RefLibrary),
    Tuple(crate::memory::Gc<ObjTuple>),
    Array(crate::memory::Gc<ObjArray>),
    Coroutine(RefCoroutine),
    Boxed(RefBoxed),
    /// Strong GC pin: marks `payload` while this handle is reachable.
    Root(RefRoot),
    /// Non-rooting handle; cleared when the referent is unmarked.
    Weak(RefWeak),
    PolyFn(RefPolyFn),
    Fn(RefFn),
    Stream(RefStream),
    Thread(RefThread),
    Sender(RefSender),
    Receiver(RefReceiver),
    Mutex(RefThreadMutex),
    RwLock(RefRwLock),
    #[cfg(feature = "crypto")]
    CryptoHasher(RefCryptoHasher),
}

impl Object {
    /// Mark the current object reference and put it in `grey_objects` if its has not been marked.
    pub fn mark(&self, grey_objects: &mut Vec<Self>) {
        let marked = match self {
            Self::String(s) => s.mark(),
            Self::Instance(i) => i.mark(),
            Self::Enum(e) => e.mark(),
            Self::Library(l) => l.mark(),
            Self::Tuple(t) => t.mark(),
            Self::Array(a) => a.mark(),
            Self::Coroutine(c) => c.mark(),
            Self::Boxed(b) => b.mark(),
            Self::Root(r) => r.mark(),
            Self::Weak(w) => w.mark(),
            Self::PolyFn(p) => p.mark(),
            Self::Fn(f) => f.mark(),
            Self::Stream(s) => s.mark(),
            Self::Thread(t) => t.mark(),
            Self::Sender(s) => s.mark(),
            Self::Receiver(r) => r.mark(),
            Self::Mutex(m) => m.mark(),
            Self::RwLock(l) => l.mark(),
            #[cfg(feature = "crypto")]
            Self::CryptoHasher(h) => h.mark(),
                    };
        if marked {
            grey_objects.push(*self);
        }
    }

    /// Unmark the object.
    pub fn unmark(&self) {
        match self {
            Self::String(s) => s.unmark(),
            Self::Instance(i) => i.unmark(),
            Self::Enum(e) => e.unmark(),
            Self::Library(l) => l.unmark(),
            Self::Tuple(t) => t.unmark(),
            Self::Array(a) => a.unmark(),
            Self::Coroutine(c) => c.unmark(),
            Self::Boxed(b) => b.unmark(),
            Self::Root(r) => r.unmark(),
            Self::Weak(w) => w.unmark(),
            Self::PolyFn(p) => p.unmark(),
            Self::Fn(f) => f.unmark(),
            Self::Stream(s) => s.unmark(),
            Self::Thread(t) => t.unmark(),
            Self::Sender(s) => s.unmark(),
            Self::Receiver(r) => r.unmark(),
            Self::Mutex(m) => m.unmark(),
            Self::RwLock(l) => l.unmark(),
            #[cfg(feature = "crypto")]
            Self::CryptoHasher(h) => h.unmark(),
                    }
    }

    /// Return whether the object is marked.
    #[must_use]
    pub fn is_marked(&self) -> bool {
        match self {
            Self::String(s) => s.is_marked(),
            Self::Instance(i) => i.is_marked(),
            Self::Enum(e) => e.is_marked(),
            Self::Library(l) => l.is_marked(),
            Self::Tuple(t) => t.is_marked(),
            Self::Array(a) => a.is_marked(),
            Self::Coroutine(c) => c.is_marked(),
            Self::Boxed(b) => b.is_marked(),
            Self::Root(r) => r.is_marked(),
            Self::Weak(w) => w.is_marked(),
            Self::PolyFn(p) => p.is_marked(),
            Self::Fn(f) => f.is_marked(),
            Self::Stream(s) => s.is_marked(),
            Self::Thread(t) => t.is_marked(),
            Self::Sender(s) => s.is_marked(),
            Self::Receiver(r) => r.is_marked(),
            Self::Mutex(m) => m.is_marked(),
            Self::RwLock(l) => l.is_marked(),
            #[cfg(feature = "crypto")]
            Self::CryptoHasher(h) => h.is_marked(),
                    }
    }

    /// Mark direct heap references held by this object.
    pub fn mark_references(&self, grey_objects: &mut Vec<Self>) {
        match self {
            Self::String(_) => {}
            Self::Instance(i) => i.as_ref().fields.iter().for_each(|(k, v)| {
                k.mark();

                if let Member::Object(i) = v {
                    i.mark(grey_objects);
                }
            }),
            Self::Enum(e) => {
                for member in &e.as_ref().payload {
                    if let Member::Object(o) = member {
                        o.mark(grey_objects);
                    }
                }
            }
            Self::Library(_) => {}
            // Array/Tuple store raw `Value` element pointers (not `Member`).
            // Transitive marking walks those addresses in
            // `Machine::gc_collect`'s grey-stack loop via
            // `mark_aggregate_elements`. Coroutine `saved_stack` /
            // `yield_from` are rooted in the same place.
            Self::Tuple(_) => {}
            Self::Array(_) => {}
            Self::Coroutine(_) => {}
            Self::Boxed(b) => {
                if let Member::Object(o) = &b.as_ref().payload {
                    o.mark(grey_objects);
                }
            }
            Self::Root(r) => {
                if let Member::Object(o) = &r.as_ref().payload {
                    o.mark(grey_objects);
                }
            }
            Self::Weak(_) => {}
            Self::PolyFn(p) => {
                for captured in p.as_ref().captured_dicts.iter().flatten() {
                    if let Member::Object(o) = captured {
                        o.mark(grey_objects);
                    }
                }
            }
            Self::Fn(_) => {
                // `captures` / `captured_args` are raw `Value`s (like Array
                // elements); Machine::gc_collect traces them via the root set
                // when the ObjFn itself is reachable.
            }
            Self::Stream(_) => {}
            Self::Thread(_) => {}
            Self::Sender(_) => {}
            Self::Receiver(_) => {}
            Self::Mutex(_) => {}
            Self::RwLock(_) => {}
            #[cfg(feature = "crypto")]
            Self::CryptoHasher(_) => {}
                    }
    }

    /// Get the next object reference in the linked list.
    #[must_use]
    pub fn get_next(&self) -> Option<Self> {
        match self {
            Self::String(s) => s.get_next(),
            Self::Instance(i) => i.get_next(),
            Self::Enum(e) => e.get_next(),
            Self::Library(l) => l.get_next(),
            Self::Tuple(t) => t.get_next(),
            Self::Array(a) => a.get_next(),
            Self::Coroutine(c) => c.get_next(),
            Self::Boxed(b) => b.get_next(),
            Self::Root(r) => r.get_next(),
            Self::Weak(w) => w.get_next(),
            Self::PolyFn(p) => p.get_next(),
            Self::Fn(f) => f.get_next(),
            Self::Stream(s) => s.get_next(),
            Self::Thread(t) => t.get_next(),
            Self::Sender(s) => s.get_next(),
            Self::Receiver(r) => r.get_next(),
            Self::Mutex(m) => m.get_next(),
            Self::RwLock(l) => l.get_next(),
            #[cfg(feature = "crypto")]
            Self::CryptoHasher(h) => h.get_next(),
                    }
    }

    /// Set the next object reference in the linked list.
    pub fn set_next(&self, next: Option<Self>) {
        match self {
            Self::String(s) => s.set_next(next),
            Self::Instance(i) => i.set_next(next),
            Self::Enum(e) => e.set_next(next),
            Self::Library(l) => l.set_next(next),
            Self::Tuple(t) => t.set_next(next),
            Self::Array(a) => a.set_next(next),
            Self::Coroutine(c) => c.set_next(next),
            Self::Boxed(b) => b.set_next(next),
            Self::Root(r) => r.set_next(next),
            Self::Weak(w) => w.set_next(next),
            Self::PolyFn(p) => p.set_next(next),
            Self::Fn(f) => f.set_next(next),
            Self::Stream(s) => s.set_next(next),
            Self::Thread(t) => t.set_next(next),
            Self::Sender(s) => s.set_next(next),
            Self::Receiver(r) => r.set_next(next),
            Self::Mutex(m) => m.set_next(next),
            Self::RwLock(l) => l.set_next(next),
            #[cfg(feature = "crypto")]
            Self::CryptoHasher(h) => h.set_next(next),
                    }
    }

    #[must_use]
    pub fn addr(&self) -> u64 {
        match self {
            Self::String(s) => s.as_ptr() as u64,
            Self::Instance(i) => i.as_ptr() as u64,
            Self::Enum(e) => e.as_ptr() as u64,
            Self::Library(l) => l.as_ptr() as u64,
            Self::Tuple(t) => t.as_ptr() as u64,
            Self::Array(a) => a.as_ptr() as u64,
            Self::Coroutine(c) => c.as_ptr() as u64,
            Self::Boxed(b) => b.as_ptr() as u64,
            Self::Root(r) => r.as_ptr() as u64,
            Self::Weak(w) => w.as_ptr() as u64,
            Self::PolyFn(p) => p.as_ptr() as u64,
            Self::Fn(f) => f.as_ptr() as u64,
            Self::Stream(s) => s.as_ptr() as u64,
            Self::Thread(t) => t.as_ptr() as u64,
            Self::Sender(s) => s.as_ptr() as u64,
            Self::Receiver(r) => r.as_ptr() as u64,
            Self::Mutex(m) => m.as_ptr() as u64,
            Self::RwLock(l) => l.as_ptr() as u64,
            #[cfg(feature = "crypto")]
            Self::CryptoHasher(h) => h.as_ptr() as u64,
                    }
    }
}

impl GcSized for Object {
    fn size(&self) -> usize {
        match self {
            Self::String(s) => s.size(),
            Self::Instance(i) => i.size(),
            Self::Enum(e) => e.size(),
            Self::Library(l) => l.size(),
            Self::Tuple(t) => t.size(),
            Self::Array(a) => a.size(),
            Self::Coroutine(c) => c.size(),
            Self::Boxed(b) => b.size(),
            Self::Root(r) => r.size(),
            Self::Weak(w) => w.size(),
            Self::PolyFn(p) => p.size(),
            Self::Fn(f) => f.size(),
            Self::Stream(s) => s.size(),
            Self::Thread(t) => t.size(),
            Self::Sender(s) => s.size(),
            Self::Receiver(r) => r.size(),
            Self::Mutex(m) => m.size(),
            Self::RwLock(l) => l.size(),
            #[cfg(feature = "crypto")]
            Self::CryptoHasher(h) => h.size(),
                    }
    }
}

impl fmt::Display for Object {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::String(s) => write!(f, "{}", s.as_ref()),
            Self::Instance(_) => write!(f, "0x{:08x}", self.addr()),
            Self::Enum(_) => write!(f, "0x{:08x}", self.addr()),
            Self::Library(_) => write!(f, "0x{:08x}", self.addr()),
            Self::Tuple(t) => write!(f, "{}", t.as_ref()),
            Self::Array(a) => write!(f, "{}", a.as_ref()),
            Self::Coroutine(c) => write!(f, "{}", c.as_ref()),
            Self::Boxed(_) => write!(f, "<boxed 0x{:08x}>", self.addr()),
            Self::Root(_) => write!(f, "<root 0x{:08x}>", self.addr()),
            Self::Weak(_) => write!(f, "<weak 0x{:08x}>", self.addr()),
            Self::PolyFn(_) => write!(f, "<polyfn 0x{:08x}>", self.addr()),
            Self::Fn(_) => write!(f, "<fn 0x{:08x}>", self.addr()),
            Self::Stream(_) => write!(f, "<stream 0x{:08x}>", self.addr()),
            Self::Thread(_) => write!(f, "<thread 0x{:08x}>", self.addr()),
            Self::Sender(_) => write!(f, "<sender 0x{:08x}>", self.addr()),
            Self::Receiver(_) => write!(f, "<receiver 0x{:08x}>", self.addr()),
            Self::Mutex(_) => write!(f, "<mutex 0x{:08x}>", self.addr()),
            Self::RwLock(_) => write!(f, "<rwlock 0x{:08x}>", self.addr()),
            #[cfg(feature = "crypto")]
            Self::CryptoHasher(_) => write!(f, "<crypto_hasher 0x{:08x}>", self.addr()),
                    }
    }
}

impl Object {
    /// C string pointer for FFI; non-strings return null.
    pub fn as_cstr(&self) -> *const std::os::raw::c_char {
        match self {
            Self::String(s) => s.data.data.as_ptr() as *const std::os::raw::c_char,
            Self::Instance(_)
            | Self::Enum(_)
            | Self::Library(_)
            | Self::Tuple(_)
            | Self::Array(_)
            | Self::Coroutine(_)
            | Self::Boxed(_)
            | Self::Root(_)
            | Self::Weak(_)
            | Self::PolyFn(_)
            | Self::Fn(_)
            | Self::Stream(_)
            | Self::Thread(_)
            | Self::Sender(_)
            | Self::Receiver(_)
            | Self::Mutex(_)
            | Self::RwLock(_) => std::ptr::null(),
                        #[cfg(feature = "crypto")]
            Self::CryptoHasher(_) => std::ptr::null(),
        }
    }
}

#[derive(Clone, Copy)]
pub enum Member {
    Value(Value),
    Object(Object),
}

pub struct ObjInstance {
    fields: Table<Member>,
    /// Compile-time class identity (`0` = none / dict / legacy `INIT`).
    pub type_id: u32,
    /// Set when `drop` has run (GC or explicit); drop must not run twice.
    pub finalized: bool,
}

impl ObjInstance {
    #[must_use]
    pub fn default() -> Self {
        Self {
            fields: Table::default(),
            type_id: 0,
            finalized: false,
        }
    }

    #[must_use]
    pub fn with_type_id(type_id: u32) -> Self {
        Self {
            fields: Table::default(),
            type_id,
            finalized: false,
        }
    }

    pub fn set(&mut self, key: RefString, value: Member) {
        self.fields.insert(key, value);
    }

    pub fn get(&self, key: RefString) -> Option<Member> {
        self.fields.get(key)
    }

    /// Iterate live `(key, value)` entries in table order (DictEntries).
    pub fn iter_fields(&self) -> impl Iterator<Item = (RefString, Member)> + '_ {
        self.fields.iter()
    }
}

impl GcSized for ObjInstance {
    fn size(&self) -> usize {
        // `Table` entry storage uses Rust's global allocator, not the VM heap.
        std::mem::size_of::<Self>()
    }
}

/// Heap-allocated enum variant (`tag` + flat `Member` payload).
pub struct ObjEnum {
    pub tag: u32,
    pub payload: Vec<Member>,
}

impl GcSized for ObjEnum {
    fn size(&self) -> usize {
        std::mem::size_of::<Self>() + self.payload.capacity() * std::mem::size_of::<Member>()
    }
}

/// The content of a heap-allocated string object.
pub struct ObjString {
    pub data: String,
    pub hash: u32,
}

impl ObjString {
    #[must_use]
    pub fn hash(s: &str) -> u32 {
        let mut hash = 2_166_136_261;
        for b in s.bytes() {
            hash = hash.bitxor(u32::from(b));
            hash = hash.wrapping_mul(16_777_619);
        }
        hash
    }
}

impl GcSized for ObjString {
    fn size(&self) -> usize {
        mem::size_of::<Self>() + mem::size_of_val(&*self.data)
    }
}

impl From<&str> for ObjString {
    fn from(value: &str) -> Self {
        let data = String::from(value);
        let hash = Self::hash(value);
        Self { data, hash }
    }
}

pub struct ObjTuple {
    pub elements: Vec<Value>,
}

pub struct ObjArray {
    pub elements: Vec<Value>,
}

/// Suspended async function state: saved stack segment + call frames.
pub struct ObjCoroutine {
    pub state: CoroState,
    pub resume_ip: usize,
    /// Stack segment (args + locals + operands) relative to segment base 0.
    pub saved_stack: Vec<Value>,
    /// Bitmask of `saved_stack` slots that hold heap pointers (for precise GC).
    /// When zero, GC conservatively scans every slot.
    pub saved_live_mask: u64,
    /// `(ip, sp_offset)` pairs; `sp_offset` is relative to the coroutine segment base.
    pub saved_frames: Vec<(usize, usize)>,
    /// Value from the resumer's `resume h with v` (delivered at the next binding yield).
    pub pending_send: Value,
    /// Active `yield from` delegate, if any.
    pub yield_from: Option<RefCoroutine>,
    /// Outer continuation IP when the delegate completes.
    pub yield_from_resume_ip: usize,
    /// Registered IO reactor waiter while this coro cooperatively awaits readiness.
    pub io_wait: Option<crate::io_reactor::WaitToken>,
}

/// Heap-allocated boxed value for the generics runtime.
pub struct ObjBoxed {
    /// `ValueTag` discriminant stored as a raw `u16`.
    pub tag: u16,
    /// The wrapped payload.
    pub payload: Member,
}

/// Explicit strong GC pin: keeps `payload` alive while this object is reachable.
pub struct ObjRoot {
    /// Rooted value; cleared to `Member::Value(0)` after [`crate::gc_handles`] unroot.
    pub payload: Member,
}

/// Non-rooting handle to a value; cleared when the referent is unmarked.
pub struct ObjWeak {
    /// Referent (immediate or heap address). Not traced by mark-sweep.
    pub target: Cell<Value>,
    /// Set when the referent dies (or after an explicit clear).
    pub cleared: Cell<bool>,
}

/// Heap-allocated polymorphic function descriptor.
pub struct ObjPolyFn {
    /// Bytecode entry offset of the monomorphised body.
    pub entry: u32,
    /// Number of type parameters expected (reserved for future use).
    pub type_arity: u8,
    /// Dictionary evidence captured when this value escaped a constrained
    /// scope. `None` leaves the position for application-time evidence.
    pub captured_dicts: Vec<Option<Member>>,
}

/// First-class monomorphic function / partial / explicit-capture lambda.
pub struct ObjFn {
    /// Bytecode entry of the body.
    pub entry: u32,
    /// Fixed arity, or rest `nfixed` when `is_rest`.
    pub arity: u32,
    /// Trailing rest parameter packs extra args into `[T]`.
    pub is_rest: bool,
    /// Bitmask of which fixed param slots are already filled (partial apply).
    pub filled_mask: u64,
    /// Values for filled param slots (decl order among filled bits).
    pub captured_args: Vec<Value>,
    /// Explicit `use (x, y)` capture snapshot (leading frame locals).
    pub captures: Vec<Value>,
}

/// Host-backed non-blocking IO stream (file / stdio / TCP / UDP / attached).
pub struct ObjStream {
    pub handle: Option<crate::io_handle::NativeHandle>,
    pub kind: StreamKind,
    pub closed: bool,
    /// Soft deadline for sync read adapters / handshake reads (`None` = wait forever).
    pub read_timeout: Option<std::time::Duration>,
    /// Soft deadline for sync write adapters / handshake writes (`None` = wait forever).
    pub write_timeout: Option<std::time::Duration>,
    /// Package session pointer + C vtable (`Stream.attach`).
    pub attached: Option<crate::stream_attach::AttachedIo>,
}

impl Drop for ObjStream {
    fn drop(&mut self) {
        if let Some(slot) = self.attached.take() {
            // shutdown when the fd is still here, then free.
            slot.shutdown_then_free(self.handle.as_mut());
        }
        // NativeHandle closes on drop; clear explicitly for clarity.
        self.handle.take();
        self.closed = true;
    }
}

impl GcSized for ObjStream {
    fn size(&self) -> usize {
        std::mem::size_of::<Self>()
    }
}

impl fmt::Display for ObjStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<stream {:?}>", self.kind)
    }
}

/// Join handle for a spawned OS thread (host `JoinState` lives outside the VM heap).
pub struct ObjThread {
    pub state: std::sync::Arc<crate::thread::JoinState>,
}

impl GcSized for ObjThread {
    fn size(&self) -> usize {
        std::mem::size_of::<Self>()
    }
}

impl fmt::Display for ObjThread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<thread>")
    }
}

pub struct ObjSender {
    pub inner: std::sync::Arc<crate::thread::ChannelInner>,
}

impl GcSized for ObjSender {
    fn size(&self) -> usize {
        std::mem::size_of::<Self>()
    }
}

impl fmt::Display for ObjSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<sender>")
    }
}

pub struct ObjReceiver {
    pub inner: std::sync::Arc<crate::thread::ChannelInner>,
}

impl GcSized for ObjReceiver {
    fn size(&self) -> usize {
        std::mem::size_of::<Self>()
    }
}

impl fmt::Display for ObjReceiver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<receiver>")
    }
}

pub struct ObjThreadMutex {
    pub inner: std::sync::Arc<crate::thread::MutexInner>,
}

impl GcSized for ObjThreadMutex {
    fn size(&self) -> usize {
        std::mem::size_of::<Self>()
    }
}

impl fmt::Display for ObjThreadMutex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<mutex>")
    }
}

pub struct ObjRwLock {
    pub inner: std::sync::Arc<crate::thread::RwLockInner>,
}

impl GcSized for ObjRwLock {
    fn size(&self) -> usize {
        std::mem::size_of::<Self>()
    }
}

impl fmt::Display for ObjRwLock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<rwlock>")
    }
}

impl GcSized for ObjTuple {
    fn size(&self) -> usize {
        mem::size_of::<Self>() + self.elements.capacity() * mem::size_of::<Value>()
    }
}

impl GcSized for ObjArray {
    fn size(&self) -> usize {
        mem::size_of::<Self>() + self.elements.capacity() * mem::size_of::<Value>()
    }
}

impl GcSized for ObjCoroutine {
    fn size(&self) -> usize {
        // `saved_stack` / `saved_frames` use Rust's allocator, not the VM
        // heap byte counter (same contract as `ObjInstance`).
        mem::size_of::<Self>()
    }
}

impl GcSized for ObjBoxed {
    fn size(&self) -> usize {
        mem::size_of::<Self>()
    }
}

impl GcSized for ObjRoot {
    fn size(&self) -> usize {
        mem::size_of::<Self>()
    }
}

impl GcSized for ObjWeak {
    fn size(&self) -> usize {
        mem::size_of::<Self>()
    }
}

impl GcSized for ObjPolyFn {
    fn size(&self) -> usize {
        mem::size_of::<Self>()
    }
}

impl GcSized for ObjFn {
    fn size(&self) -> usize {
        // Vec payloads use Rust's allocator (same as ObjArray elements).
        mem::size_of::<Self>()
    }
}

impl fmt::Display for ObjTuple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "({})",
            self.elements
                .iter()
                .map(|v| format!("{}", v.as_int()))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl fmt::Display for ObjArray {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}]",
            self.elements
                .iter()
                .map(|v| format!("{}", v.as_int()))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl fmt::Display for ObjCoroutine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<coroutine {:?}>", self.state)
    }
}

impl fmt::Display for ObjString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.data)
    }
}

/// Loaded shared library plus cached FFI signatures.
pub struct ObjLibrary {
    pub library: std::sync::Arc<crate::ffi::Library>,
    pub signatures: Vec<RegisteredFunction>,
    pub by_name: std::collections::HashMap<String, usize>,
    /// libffi closures registered for callbacks (keeps trampolines alive).
    pub closures: Vec<crate::ffi::OwnedClosure>,
}

/// C signature metadata for an FFI function.
#[derive(Clone, Debug)]
pub struct FunctionSig {
    pub name: String,
    /// Fixed-prefix arity (`nfixed` when [`Self::variadic`]).
    pub arity: usize,
    pub arg_types: Vec<FfiType>,
    pub ret_type: FfiType,
    /// C-style varargs — CIF rebuilt per invoke with `Cif::new_variadic`.
    pub variadic: bool,
}

impl FunctionSig {
    pub fn from_ffi_signature(sig: &crate::ffi::FfiSignature) -> Self {
        Self {
            name: sig.name.clone(),
            arity: sig.arity(),
            arg_types: sig.args.clone(),
            ret_type: sig.ret,
            variadic: sig.variadic,
        }
    }
}

/// A declared FFI function with a prepared libffi call interface.
pub struct RegisteredFunction {
    pub sig: FunctionSig,
    pub prepared: crate::ffi::PreparedCall,
}

impl RegisteredFunction {
    pub fn ffi_signature(&self) -> crate::ffi::FfiSignature {
        crate::ffi::FfiSignature {
            name: self.sig.name.clone(),
            args: self.sig.arg_types.clone(),
            ret: self.sig.ret_type,
            variadic: self.sig.variadic,
        }
    }
}

/// C ABI type tags for FFI marshalling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FfiType {
    Int,
    Float,
    String,
    Void,
    Bool,
    Int8,
    Int16,
    Int32,
    UInt8,
    UInt16,
    UInt32,
    UInt64,
    Ptr,
    Callback(u32),
    Struct(u32),
}

impl FfiType {
    pub fn from_tag(tag: u32, aux: u32) -> Self {
        use common::tag as t;
        match tag {
            x if x == t::FLOAT => Self::Float,
            x if x == t::STRING => Self::String,
            x if x == t::VOID => Self::Void,
            x if x == t::BOOL => Self::Bool,
            x if x == t::INT8 => Self::Int8,
            x if x == t::INT16 => Self::Int16,
            x if x == t::INT32 => Self::Int32,
            x if x == t::UINT8 => Self::UInt8,
            x if x == t::UINT16 => Self::UInt16,
            x if x == t::UINT32 => Self::UInt32,
            x if x == t::UINT64 => Self::UInt64,
            x if x == t::PTR => Self::Ptr,
            x if x == t::CALLBACK => Self::Callback(aux),
            x if x == t::STRUCT => Self::Struct(aux),
            _ => Self::Int,
        }
    }

    pub fn tag(&self) -> u32 {
        use common::tag as t;
        match self {
            Self::Int => t::INT,
            Self::Float => t::FLOAT,
            Self::String => t::STRING,
            Self::Void => t::VOID,
            Self::Bool => t::BOOL,
            Self::Int8 => t::INT8,
            Self::Int16 => t::INT16,
            Self::Int32 => t::INT32,
            Self::UInt8 => t::UINT8,
            Self::UInt16 => t::UINT16,
            Self::UInt32 => t::UINT32,
            Self::UInt64 => t::UINT64,
            Self::Ptr => t::PTR,
            Self::Callback(_) => t::CALLBACK,
            Self::Struct(_) => t::STRUCT,
        }
    }

    pub fn aux(&self) -> u32 {
        match self {
            Self::Callback(id) | Self::Struct(id) => *id,
            _ => 0,
        }
    }

    pub fn is_void(self) -> bool {
        matches!(self, Self::Void)
    }
}

/// C-layout struct descriptor for pass-by-value FFI.
#[derive(Clone, Debug)]
pub struct CStructLayout {
    pub name: String,
    pub fields: Vec<(String, FfiType)>,
}

impl GcSized for ObjLibrary {
    fn size(&self) -> usize {
        mem::size_of::<Self>() + mem::size_of_val(&*self.library)
    }
}

impl fmt::Display for ObjLibrary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "<library at 0x{:x}, {} function(s)>",
            std::sync::Arc::as_ptr(&self.library) as u64,
            self.signatures.len()
        )
    }
}

pub trait GcSized {
    fn size(&self) -> usize;
}

pub struct GcData<T> {
    marked: Cell<bool>,
    next: Cell<Option<Object>>,
    data: T,
}

impl<T> GcData<T> {
    pub const fn new(next: Option<Object>, data: T) -> Self {
        Self {
            marked: Cell::new(false),
            next: Cell::new(next),
            data,
        }
    }

    pub const fn get_next(&self) -> Option<Object> {
        self.next.get()
    }

    pub fn set_next(&self, next: Option<Object>) {
        self.next.set(next);
    }

    pub const fn is_marked(&self) -> bool {
        self.marked.get()
    }

    pub fn mark(&self) -> bool {
        let is_not_marked = !self.marked.get();
        if is_not_marked {
            self.marked.set(true);
        }
        is_not_marked
    }

    pub fn unmark(&self) {
        self.marked.set(false);
    }
}

impl<T> AsRef<T> for GcData<T> {
    fn as_ref(&self) -> &T {
        &self.data
    }
}

impl<T> AsMut<T> for GcData<T> {
    fn as_mut(&mut self) -> &mut T {
        &mut self.data
    }
}

impl<T: GcSized> GcSized for GcData<T> {
    fn size(&self) -> usize {
        mem::size_of_val(&self.next) + mem::size_of_val(&self.marked) + self.data.size()
    }
}

impl<T: GcSized + Copy> GcSized for Cell<T> {
    fn size(&self) -> usize {
        self.get().size()
    }
}

pub struct Gc<T> {
    ptr: NonNull<GcData<T>>,
}

impl<T> Gc<T> {
    #[must_use]
    pub fn new(boxed: Box<GcData<T>>) -> Self {
        Self {
            ptr: NonNull::from(Box::leak(boxed)),
        }
    }

    pub fn release(self) {
        _ = unsafe { Box::from_raw(self.ptr.as_ptr()) };
    }

    #[must_use]
    pub fn ptr_eq(lhs: Self, rhs: Self) -> bool {
        lhs.ptr.eq(&rhs.ptr)
    }

    #[must_use]
    pub const fn as_ptr(&self) -> *const GcData<T> {
        self.ptr.as_ptr()
    }

    /// Mutable access to the inner payload (single-threaded VM only).
    pub fn payload_mut(&self) -> &mut T {
        unsafe {
            let ptr = self.ptr.as_ptr().cast::<GcData<T>>();
            (*ptr).as_mut()
        }
    }
}

impl<T: GcSized> GcSized for Gc<T> {
    fn size(&self) -> usize {
        self.deref().size()
    }
}

impl<T> ops::Deref for Gc<T> {
    type Target = GcData<T>;

    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T> ops::DerefMut for Gc<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.ptr.as_mut() }
    }
}

impl<T> Copy for Gc<T> {}
impl<T> Clone for Gc<T> {
    fn clone(&self) -> Self {
        *self
    }
}

// Open-addressing hash table keyed by interned strings.

use std::{alloc, cell::UnsafeCell, marker::PhantomData};

use common::Value;

pub struct Table<V>(UnsafeCell<Store<V>>);

impl<V> Default for Table<V> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<V> Table<V> {
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self(UnsafeCell::new(Store::new()))
    }

    #[inline]
    pub fn len(&self) -> usize {
        let store = unsafe { &*self.0.get() };
        store.lives
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        let store = unsafe { &*self.0.get() };
        store.cap
    }

    #[inline]
    pub fn get(&self, key: RefString) -> Option<V>
    where
        V: Copy,
    {
        let store = unsafe { &*self.0.get() };
        store.get(key)
    }

    #[inline]
    pub fn find(&self, s: &str, hash: u32) -> Option<RefString> {
        let store = unsafe { &*self.0.get() };
        store.find(s, hash)
    }

    #[inline]
    pub fn insert(&self, key: RefString, val: V) -> Option<V> {
        let store = unsafe { &mut *self.0.get() };
        store.insert(key, val)
    }

    #[inline]
    pub fn remove(&self, key: RefString) -> Option<V> {
        let store = unsafe { &mut *self.0.get() };
        store.remove(key)
    }

    #[inline]
    pub fn iter(&self) -> Iter<'_, V>
    where
        V: Copy,
    {
        let store = unsafe { &*self.0.get() };
        store.into_iter()
    }
}

pub struct Iter<'store, V> {
    ptr: NonNull<Entry<V>>,
    idx: usize,
    cap: usize,
    marker: PhantomData<&'store Store<V>>,
}

impl<V> Iterator for Iter<'_, V>
where
    V: Copy,
{
    type Item = (RefString, V);

    fn next(&mut self) -> Option<Self::Item> {
        while self.idx < self.cap {
            let entry = unsafe { &*self.ptr.as_ptr().add(self.idx) };
            self.idx += 1;
            if let Entry::Live(x) = entry {
                return Some((x.key, x.val));
            }
        }
        None
    }
}

struct Store<V> {
    lives: usize,
    deads: usize,
    cap: usize,
    ptr: NonNull<Entry<V>>,
}

impl<V> Drop for Store<V> {
    fn drop(&mut self) {
        if self.cap > 0 {
            let entries = NonNull::slice_from_raw_parts(self.ptr, self.cap);
            unsafe {
                NonNull::drop_in_place(entries);
                Self::dealloc(self.ptr, self.cap);
            }
        }
    }
}

impl<'store, V> IntoIterator for &'store Store<V>
where
    V: Copy,
{
    type Item = (RefString, V);

    type IntoIter = Iter<'store, V>;

    fn into_iter(self) -> Self::IntoIter {
        Self::IntoIter {
            ptr: self.ptr,
            idx: 0,
            cap: self.cap,
            marker: PhantomData,
        }
    }
}

impl<V> Store<V> {
    const fn new() -> Self {
        Self {
            ptr: NonNull::dangling(),
            cap: 0,
            lives: 0,
            deads: 0,
        }
    }

    fn get(&self, key: RefString) -> Option<V>
    where
        V: Copy,
    {
        if self.lives == 0 {
            return None;
        }
        let entry_ptr = unsafe { Self::probe(self.cap, self.ptr, key) };
        let entry = unsafe { entry_ptr.as_ref() };
        let Entry::Live(e) = entry else {
            return None;
        };
        Some(e.val)
    }

    fn find(&self, s: &str, hash: u32) -> Option<RefString> {
        if self.lives == 0 {
            return None;
        }
        let mut index = hash as usize & (self.cap - 1);
        loop {
            let entry_ptr = unsafe { self.ptr.add(index) };
            let entry = unsafe { entry_ptr.as_ref() };
            match entry {
                Entry::Free => return None,
                Entry::Live(entry) if coil_simd::bytes::eq(entry.key.as_ref().data.as_bytes(), s.as_bytes()) => {
                    return Some(entry.key);
                }
                _ => {}
            }
            index = (index + 1) & (self.cap - 1);
        }
    }

    fn insert(&mut self, key: RefString, val: V) -> Option<V> {
        if self.lives + self.deads >= self.cap * 3 / 4 {
            self.resize();
        }
        let mut entry_ptr = unsafe { Self::probe(self.cap, self.ptr, key) };
        let entry = unsafe { entry_ptr.as_mut() };
        match mem::replace(entry, Entry::Live(EntryInner { key, val })) {
            Entry::Free => {
                self.lives += 1;
                None
            }
            Entry::Dead => {
                self.lives += 1;
                self.deads -= 1;
                None
            }
            Entry::Live(e) => Some(e.val),
        }
    }

    fn remove(&mut self, key: RefString) -> Option<V> {
        if self.lives == 0 {
            return None;
        }
        let mut entry_ptr = unsafe { Self::probe(self.cap, self.ptr, key) };
        let entry = unsafe { entry_ptr.as_mut() };
        let Entry::Live(entry_old) = mem::replace(entry, Entry::Dead) else {
            return None;
        };
        self.lives -= 1;
        self.deads += 1;
        Some(entry_old.val)
    }

    unsafe fn probe(cap: usize, ptr: NonNull<Entry<V>>, key: RefString) -> NonNull<Entry<V>> {
        let mut dead = None;
        let mut index = key.as_ref().hash as usize & (cap - 1);
        loop {
            let entry_ptr = unsafe { ptr.add(index) };
            match unsafe { entry_ptr.as_ref() } {
                Entry::Free => {
                    return dead.unwrap_or(entry_ptr);
                }
                Entry::Dead if dead.is_none() => {
                    dead = Some(entry_ptr);
                }
                Entry::Live(e) if Gc::ptr_eq(e.key, key) => {
                    return entry_ptr;
                }
                _ => {}
            }
            index = (index + 1) & (cap - 1);
        }
    }

    fn resize(&mut self) {
        let new_cap = self
            .cap
            .checked_mul(2)
            .expect("capacity does not overflow")
            .max(8);

        let new_ptr = Self::alloc(new_cap);
        if self.cap > 0 {
            for i in 0..self.cap {
                let old_entry_ptr = unsafe { self.ptr.add(i) };
                if let Entry::Live(e) = unsafe { old_entry_ptr.as_ref() } {
                    let new_entry_ptr = unsafe { Self::probe(new_cap, new_ptr, e.key) };
                    unsafe {
                        NonNull::swap(old_entry_ptr, new_entry_ptr);
                    }
                }
            }
            unsafe {
                Self::dealloc(self.ptr, self.cap);
            }
        }
        self.deads = 0;
        self.cap = new_cap;
        self.ptr = new_ptr;
    }

    fn layout(cap: usize) -> alloc::Layout {
        alloc::Layout::array::<Entry<V>>(cap).expect("a valid array layout")
    }

    fn alloc(cap: usize) -> NonNull<Entry<V>> {
        let layout = Self::layout(cap);
        let nullable = unsafe { alloc::alloc(layout) };
        let Some(ptr) = NonNull::new(nullable.cast()) else {
            alloc::handle_alloc_error(layout);
        };
        for i in 0..cap {
            unsafe {
                ptr.add(i).write(Entry::Free);
            }
        }
        ptr
    }

    unsafe fn dealloc(ptr: NonNull<Entry<V>>, cap: usize) {
        unsafe {
            std::alloc::dealloc(ptr.as_ptr().cast(), Self::layout(cap));
        }
    }
}

enum Entry<V> {
    Free,
    Dead,
    Live(EntryInner<V>),
}

struct EntryInner<V> {
    key: RefString,
    val: V,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live_object_addrs(heap: &Heap) -> std::collections::HashSet<u64> {
        let mut addrs = std::collections::HashSet::new();
        for obj in heap {
            addrs.insert(obj.addr());
        }
        addrs
    }

    #[test]
    fn enum_gc_marks_payload_pointers() {
        let mut heap = Heap::default();

        // 1. Allocate the inner object (a string).
        let (string_obj, string_ref) = heap.alloc(ObjString::from("inner"), Object::String);
        let string_addr = string_obj.addr();
        let string_member = Member::Object(string_obj);

        // 2. Allocate the enum with the string in its payload.
        let enum_value = ObjEnum {
            tag: 0,
            payload: vec![string_member],
        };
        let (enum_obj, _enum_ref) = heap.alloc(enum_value, Object::Enum);
        let enum_addr = enum_obj.addr();

        // 3. Mark the enum as a root and propagate the mark to its
        //    payload (which holds the string pointer).
        let mut gray = Vec::new();
        heap.trace(&[enum_addr]);
        enum_obj.mark_references(&mut gray);

        // 4. Sweep — anything not marked is deallocated.
        unsafe { heap.sweep() };

        // 5. Both objects must still be alive.
        let live = live_object_addrs(&heap);
        assert!(
            live.contains(&string_addr),
            "string at 0x{:x} was collected despite being reachable from enum payload",
            string_addr
        );
        assert!(
            live.contains(&enum_addr),
            "enum at 0x{:x} was collected despite being a GC root",
            enum_addr
        );
        // Sanity: the string ref is still dereferenceable.
        let _ = string_ref.as_ref();
    }

    #[test]
    fn enum_gc_marks_nested_enum_payloads() {
        let mut heap = Heap::default();

        // Inner enum: empty payload.
        let (inner_obj, _inner_ref) = heap.alloc(
            ObjEnum {
                tag: 1,
                payload: vec![],
            },
            Object::Enum,
        );
        let inner_addr = inner_obj.addr();

        // Outer enum: payload contains the inner enum as a
        // `Member::Object`.
        let outer = ObjEnum {
            tag: 0,
            payload: vec![Member::Object(inner_obj)],
        };
        let (outer_obj, _outer_ref) = heap.alloc(outer, Object::Enum);
        let outer_addr = outer_obj.addr();

        // Mark outer as root, propagate through its payload to mark
        // the inner enum.
        let mut gray = Vec::new();
        heap.trace(&[outer_addr]);
        outer_obj.mark_references(&mut gray);

        // Drain the grey stack — each newly-marked object should
        // also have its references traced. For the inner enum
        // (empty payload) this is a no-op, but we still call it to
        // exercise the arm.
        while let Some(obj) = gray.pop() {
            obj.mark_references(&mut gray);
        }

        // Sweep.
        unsafe { heap.sweep() };

        // Both must survive.
        let live = live_object_addrs(&heap);
        assert!(
            live.contains(&inner_addr),
            "inner enum at 0x{:x} was collected despite being reachable from outer enum payload",
            inner_addr
        );
        assert!(
            live.contains(&outer_addr),
            "outer enum at 0x{:x} was collected despite being a GC root",
            outer_addr
        );
    }

    #[test]
    fn find_object_by_addr_hit_and_miss() {
        let mut heap = Heap::default();
        let (obj, _) = heap.alloc(ObjString::from("hi"), Object::String);
        let addr = obj.addr();
        assert!(matches!(
            heap.find_object_by_addr(addr),
            Some(Object::String(_))
        ));
        assert!(heap.find_object_by_addr(addr.wrapping_add(1)).is_none());
    }

    #[test]
    fn borrowed_intern_reuses_existing_string_without_heap_growth() {
        let mut heap = Heap::default();
        let first = heap.intern("literal".to_owned());
        let size = heap.size();
        let second = heap.intern_str("literal");

        assert!(Gc::ptr_eq(first, second));
        assert_eq!(heap.size(), size);
        assert_eq!(
            heap.into_iter()
                .filter(|obj| matches!(obj, Object::String(_)))
                .count(),
            1
        );
    }

    #[test]
    fn intern_ref_registers_an_existing_string_without_copying() {
        let mut heap = Heap::default();
        let (object, string) = heap.alloc(ObjString::from("raw"), Object::String);
        let resolved = heap.intern_ref(string);
        let found = heap.intern_str("raw");

        assert_eq!(object.addr(), resolved.as_ptr() as u64);
        assert!(Gc::ptr_eq(resolved, found));
    }

    #[test]
    fn sweep_reuses_dangling_string_scratch() {
        let mut heap = Heap::default();
        let _ = heap.intern("first".to_owned());
        unsafe { heap.sweep() };
        let capacity = heap.gc_dangling_strings.capacity();
        assert!(capacity >= 1);

        let _ = heap.intern("second".to_owned());
        unsafe { heap.sweep() };
        assert_eq!(heap.gc_dangling_strings.capacity(), capacity);
    }

    /// Byte-threshold GC: under threshold → no collect; after a rooted sweep the
    /// threshold scales with live bytes so a single surviving object does not
    /// trigger on every subsequent alloc.
    #[test]
    fn should_collect_uses_byte_threshold_rescaled_by_sweep() {
        let mut heap = Heap::default();
        assert!(
            !heap.should_collect(),
            "fresh heap must sit under the default threshold"
        );

        let (keep, _) = heap.alloc(ObjString::from("keep"), Object::String);
        let keep_addr = keep.addr();
        // Force a collection, then root `keep` so it survives.
        heap.set_gc_threshold_for_test(0);
        assert!(heap.should_collect());
        heap.trace(&[keep_addr]);
        keep.mark_references(&mut Vec::new());
        unsafe { heap.sweep() };

        assert!(
            !heap.should_collect(),
            "after sweep, threshold must be live*growth so one survivor is quiet"
        );
        let quiet_size = heap.size();
        // Grow past the rescaled threshold without roots — should_collect again.
        while !heap.should_collect() {
            let _ = heap.alloc(ObjString::from("pressure"), Object::String);
            // Guard against runaway if rescale broke (would never trip).
            assert!(
                heap.size() < quiet_size.saturating_mul(8).max(4096),
                "alloc_bytes grew without tripping should_collect"
            );
        }
    }

    /// Immortal arity-0 enums are seeded as GC roots and must not be swept,
    /// even when nothing else references them.
    #[test]
    fn immortal_unit_enums_survive_sweep_as_roots() {
        let mut heap = Heap::default();
        let immortal = heap.immortal_unit_enum(7);
        let immortal_addr = immortal.addr();
        let again = heap.immortal_unit_enum(7);
        assert_eq!(immortal_addr, again.addr());

        let (junk, _) = heap.alloc(ObjString::from("junk"), Object::String);
        let junk_addr = junk.addr();

        let roots = heap.take_gc_roots();
        assert!(
            roots.contains(&immortal_addr),
            "take_gc_roots must seed immortal enum addresses"
        );
        heap.trace(&roots);
        unsafe { heap.sweep() };
        heap.restore_gc_roots(roots);

        assert!(
            heap.find_object_by_addr(immortal_addr).is_some(),
            "immortal enum must survive an otherwise empty-root sweep"
        );
        assert!(
            heap.find_object_by_addr(junk_addr).is_none(),
            "unrooted junk must still be collected"
        );
        assert_eq!(
            heap.immortal_unit_enum(7).addr(),
            immortal_addr,
            "post-sweep lookup must reuse the same singleton"
        );
    }

    #[test]
    fn alloc_enum_value_reuses_unit_singletons() {
        let mut heap = Heap::default();
        let first = heap.alloc_enum_value(3, Vec::new());
        let second = heap.alloc_enum_value(3, Vec::new());

        assert_eq!(first.raw(), second.raw());
        assert_eq!(
            heap.into_iter()
                .filter(|obj| matches!(obj, Object::Enum(_)))
                .count(),
            1
        );
    }

    #[test]
    fn find_object_by_addr_clears_after_sweep() {
        let mut heap = Heap::default();
        let (obj, _) = heap.alloc(ObjString::from("gone"), Object::String);
        let addr = obj.addr();
        assert!(heap.find_object_by_addr(addr).is_some());
        // No roots → sweep removes the object and its addr_index entry.
        unsafe { heap.sweep() };
        assert!(
            heap.find_object_by_addr(addr).is_none(),
            "swept object must leave the O(1) addr index"
        );
    }

    #[test]
    fn cstr_from_addr_string_hit_and_type_miss() {
        let mut heap = Heap::default();
        let (s_obj, _) = heap.alloc(ObjString::from("coil"), Object::String);
        let (arr_obj, _) = heap.alloc(
            ObjArray {
                elements: vec![Value::from(1i64)],
            },
            Object::Array,
        );

        let ptr = heap
            .cstr_from_addr(s_obj.addr())
            .expect("string addr must yield a cstr");
        let got = unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_str()
            .expect("utf8");
        assert_eq!(got, "coil");

        assert!(
            heap.cstr_from_addr(arr_obj.addr()).is_none(),
            "non-string live addr must miss"
        );
        assert!(heap.cstr_from_addr(0).is_none());
    }

    #[test]
    fn cstr_from_addr_rejects_embedded_nul() {
        let mut heap = Heap::default();
        let (obj, _) = heap.alloc(ObjString::from("a\0b"), Object::String);
        assert!(
            heap.cstr_from_addr(obj.addr()).is_none(),
            "embedded NUL cannot become a CString"
        );
    }

    #[test]
    fn update_array_elements_writes_and_truncates_excess() {
        let mut heap = Heap::default();
        let (obj, gc) = heap.alloc(
            ObjArray {
                elements: vec![Value::from(0i64), Value::from(0i64)],
            },
            Object::Array,
        );
        let addr = obj.addr();
        heap.update_array_elements(addr, &[10, 20, 30]);
        assert_eq!(gc.as_ref().elements[0].as_int(), 10);
        assert_eq!(gc.as_ref().elements[1].as_int(), 20);
        assert_eq!(gc.as_ref().elements.len(), 2);
    }

    #[test]
    fn update_array_elements_noops_missing_or_wrong_type() {
        let mut heap = Heap::default();
        let (s_obj, _) = heap.alloc(ObjString::from("x"), Object::String);
        heap.update_array_elements(s_obj.addr(), &[1]);
        heap.update_array_elements(0, &[1]);
        assert!(matches!(
            heap.find_object_by_addr(s_obj.addr()),
            Some(Object::String(_))
        ));
    }

    #[cfg(feature = "crypto")]
    #[test]
    fn with_crypto_hasher_rejects_wrong_type() {
        let mut heap = Heap::default();
        let (obj, _) = heap.alloc(ObjString::from("not-hasher"), Object::String);
        assert!(
            heap.with_crypto_hasher(obj.addr(), |_| panic!("must not run"))
                .is_none()
        );
        assert!(
            heap.with_crypto_hasher(0, |_| panic!("must not run"))
                .is_none()
        );
    }

    #[test]
    fn gc_scratch_buffers_round_trip_take_restore() {
        let mut heap = Heap::default();
        let mut roots = heap.take_gc_roots();
        roots.push(1);
        roots.push(2);
        heap.restore_gc_roots(roots);

        let (mut gray, mut root_objects) = heap.take_gc_worklists();
        assert!(gray.is_empty());
        assert!(root_objects.is_empty());
        // Capacity may be retained after clear; restore must not panic.
        gray.reserve(4);
        root_objects.reserve(4);
        heap.restore_gc_worklists(gray, root_objects);

        let roots2 = heap.take_gc_roots();
        // Prior contents were cleared on take; buffer is reusable.
        assert!(roots2.is_empty());
        heap.restore_gc_roots(roots2);
    }

    #[test]
    fn repeated_trace_reuses_mark_set_without_leaking_prior_roots() {
        let mut heap = Heap::default();
        let (keep, _) = heap.alloc(ObjString::from("keep"), Object::String);
        let (drop_me, _) = heap.alloc(ObjString::from("drop"), Object::String);
        let keep_addr = keep.addr();
        let drop_addr = drop_me.addr();

        // First collection: only `keep` is a root.
        heap.trace(&[keep_addr]);
        let mut gray = Vec::new();
        keep.mark_references(&mut gray);
        unsafe { heap.sweep() };

        let live = live_object_addrs(&heap);
        assert!(live.contains(&keep_addr));
        assert!(!live.contains(&drop_addr));

        // Second collection with empty roots must not resurrect drop_me via a
        // stale mark-set entry from the previous trace.
        let (orphan, _) = heap.alloc(ObjString::from("orphan"), Object::String);
        let orphan_addr = orphan.addr();
        heap.trace(&[]);
        unsafe { heap.sweep() };
        let live = live_object_addrs(&heap);
        assert!(
            !live.contains(&orphan_addr),
            "empty-root trace must not keep prior mark-set addresses alive"
        );
        // `keep` was unmarked after sweep and not re-rooted — also gone.
        assert!(!live.contains(&keep_addr));
    }
}
