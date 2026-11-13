//! Host natives registered with explicit signatures.

use std::sync::Arc;

use common::Value;

use crate::memory::{FfiType, Heap};

use super::signature::{FfiError, FfiSignature};

type HostClosure =
    Arc<dyn Fn(&mut Heap, &[Value]) -> Result<Option<Value>, FfiError> + Send + Sync>;

/// Discriminant for HostInvoke specials. Checked instead of `name().to_string()`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HostOp {
    #[default]
    Ordinary,
    Collect,
    RegisterFinalizer,
    /// Task scheduler native (`task_*`): the runtime runs it, since it can switch
    /// tasks. Ids [`common::TASK_SCOPE_OPEN_ID`]..=[`common::TASK_YIELD_ID`]
    /// and [`common::TASK_CANCEL_ID`]..=[`common::TASK_SHIELD_EXIT_ID`]
    /// and [`common::TASK_COND_NEW_ID`]..=[`common::TASK_COND_NOTIFY_ID`],
    /// and [`common::CONTRACT_FAIL_ID`] (it reads the caller's frame).
    Task,
    /// `unwind_resume`: the end of a `defer` cleanup pad; the runtime unwinder
    /// takes the frame back ([`common::UNWIND_RESUME_ID`]).
    Unwind,
    /// `gc_region_enter` / `gc_region_exit` around a `region` block: the runtime
    /// runs a minor collection ([`common::GC_REGION_ENTER_ID`]).
    RegionEnter,
    RegionExit,
    /// `startup_restore`: the driver restores the snapshot at a pool index
    /// ([`common::STARTUP_RESTORE_ID`]).
    Restore,
}

pub trait NativeFn: Send + Sync {
    fn name(&self) -> &str;
    fn signature(&self) -> &FfiSignature;
    fn invoke(&self, heap: &mut Heap, args: &[Value]) -> Result<Option<Value>, FfiError>;
    fn host_op(&self) -> HostOp {
        HostOp::Ordinary
    }
    /// The plain function behind a native that always returns a value and
    /// never parks, opens descriptors or switches tasks. The runtime calls it
    /// straight from the dispatch loop instead of through [`Self::invoke`].
    fn direct(&self) -> Option<crate::HostValueFn> {
        None
    }

    /// [`Self::direct`] of a native that first checks its `Stream`
    /// argument is not unsent (`host_natives::stream_first`): the plain
    /// function behind that check, which native code makes itself.
    fn guarded_direct(&self) -> Option<crate::HostValueFn> {
        None
    }

    /// A slot of a host table without this native: an AOT executable links
    /// only the natives its program calls (`host_natives_of_groups`).
    fn is_absent(&self) -> bool {
        false
    }
}

/// How native code calls host native `id` ([`Natives::host_call`]).
#[derive(Clone, Copy, Default)]
pub struct HostCall {
    /// The plain function (`None`: the native needs the runtime).
    pub func: Option<crate::HostValueFn>,
    /// It takes a `Stream` first, which must not be unsent.
    pub guarded: bool,
    /// `gc_collect`: a full collection on the runtime, with native code's
    /// words rooted around the call.
    pub collect: bool,
    /// An ordinary native that needs the runtime (it may park the task, open
    /// descriptors, ...), a task op, a `region` hook (a minor collection) or
    /// the finalizer registry's `gc_register_finalizer`: native code runs it
    /// as a runtime call.
    pub on_machine: bool,
}

impl HostCall {
    pub fn of(n: &dyn NativeFn) -> Self {
        if n.is_absent() {
            return Self::default();
        }
        let guarded = crate::host_natives::stream_first(n.name());
        let func = if guarded { n.guarded_direct() } else { n.direct() };
        let collect = n.name() == crate::gc_handles::GC_COLLECT_NATIVE;
        let on_machine = func.is_none()
            && !collect
            && matches!(
                n.host_op(),
                HostOp::Ordinary | HostOp::Task | HostOp::RegionEnter | HostOp::RegionExit | HostOp::RegisterFinalizer | HostOp::Restore
            );
        Self { func, guarded, collect, on_machine }
    }
}

pub struct HostClosureFn {
    signature: FfiSignature,
    /// When set, accept `min_args..=max_args` instead of exact `signature.arity()`.
    arity_range: Option<(usize, usize)>,
    host_op: HostOp,
    func: HostClosure,
    direct: Option<crate::HostValueFn>,
}

impl HostClosureFn {
    pub fn new<F>(signature: FfiSignature, func: F) -> Self
    where
        F: Fn(&mut Heap, &[Value]) -> Result<Option<Value>, FfiError> + Send + Sync + 'static,
    {
        Self {
            signature,
            arity_range: None,
            host_op: HostOp::Ordinary,
            func: Arc::new(func),
            direct: None,
        }
    }

    /// A native that is a plain [`crate::HostValueFn`]: it always returns a
    /// value, and never parks, opens descriptors or switches tasks, so the
    /// runtime may call it directly ([`NativeFn::direct`]).
    pub fn new_direct(signature: FfiSignature, func: crate::HostValueFn) -> Self {
        let mut native = Self::new(signature, move |heap, args| Ok(Some(func(heap, args))));
        native.direct = Some(func);
        native
    }

    pub fn with_host_op(mut self, host_op: HostOp) -> Self {
        self.host_op = host_op;
        self
    }

    /// Host native that accepts a variable number of arguments (inclusive range).
    pub fn new_with_arity_range<F>(
        signature: FfiSignature,
        min_args: usize,
        max_args: usize,
        func: F,
    ) -> Self
    where
        F: Fn(&mut Heap, &[Value]) -> Result<Option<Value>, FfiError> + Send + Sync + 'static,
    {
        Self {
            signature,
            arity_range: Some((min_args, max_args)),
            host_op: HostOp::Ordinary,
            func: Arc::new(func),
            direct: None,
        }
    }

    pub fn unary_i64(
        name: impl Into<String>,
        func: impl Fn(i64) -> i64 + Send + Sync + 'static,
    ) -> Self {
        let signature = FfiSignature::from_parts(name, vec![FfiType::Int], FfiType::Int).unwrap();
        Self::new(signature, move |_heap, args| {
            Ok(Some(Value::from(func(args[0].as_int()))))
        })
    }
}

impl NativeFn for HostClosureFn {
    fn name(&self) -> &str {
        &self.signature.name
    }

    fn signature(&self) -> &FfiSignature {
        &self.signature
    }

    fn host_op(&self) -> HostOp {
        self.host_op
    }

    fn direct(&self) -> Option<crate::HostValueFn> {
        // A special host op runs in the runtime, never as a plain call.
        if self.host_op == HostOp::Ordinary { self.direct } else { None }
    }

    fn invoke(&self, heap: &mut Heap, args: &[Value]) -> Result<Option<Value>, FfiError> {
        let ok = if let Some((min, max)) = self.arity_range {
            args.len() >= min && args.len() <= max
        } else {
            args.len() == self.signature.arity()
        };
        if !ok {
            return Err(FfiError::ArityMismatch {
                expected: self.signature.arity(),
                got: args.len(),
            });
        }
        (self.func)(heap, args)
    }
}

/// Registered host natives. Both tables sit behind `Arc` so a worker or
/// re-entrant `execute` can share the registry without re-hashing every name;
/// `register` copies on write only when a clone is still alive.
#[derive(Default, Clone)]
pub struct Natives {
    by_name: Arc<std::collections::HashMap<String, Arc<dyn NativeFn>>>,
    by_id: Arc<Vec<Arc<dyn NativeFn>>>,
    /// [`NativeFn::direct`] of each id, read by the dispatch loop.
    direct: Arc<Vec<Option<crate::HostValueFn>>>,
    /// How native code calls each id.
    calls: Arc<Vec<HostCall>>,
}

impl Natives {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, native: Arc<dyn NativeFn>) -> usize {
        let id = self.by_id.len();
        if !native.is_absent() {
            let name = native.name().to_string();
            let by_name = Arc::make_mut(&mut self.by_name);
            if by_name.is_empty() {
                // The standard host table is ~210 natives; skip the rehash ladder.
                by_name.reserve(256);
            }
            by_name.insert(name, Arc::clone(&native));
        }
        Arc::make_mut(&mut self.calls).push(HostCall::of(native.as_ref()));
        Arc::make_mut(&mut self.direct).push(native.direct());
        Arc::make_mut(&mut self.by_id).push(native);
        id
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn NativeFn>> {
        self.by_name.get(name).cloned()
    }

    pub fn get_by_id(&self, id: usize) -> Option<&dyn NativeFn> {
        self.by_id.get(id).map(|n| n.as_ref())
    }

    /// The direct function of native `id` ([`NativeFn::direct`]), if any.
    #[inline(always)]
    pub fn direct_by_id(&self, id: usize) -> Option<crate::HostValueFn> {
        self.direct.get(id).copied().flatten()
    }

    /// How native code calls native `id`.
    #[inline(always)]
    pub fn host_call(&self, id: usize) -> Option<HostCall> {
        self.calls.get(id).copied()
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Share the registered natives (stable ids) with a worker thread.
    pub fn clone_registry(&self) -> Self {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_closure_invokes_with_signature() {
        let native = HostClosureFn::unary_i64("dbl", |x| x * 2);
        let mut heap = Heap::default();
        let args = [Value::from(21_i64)];
        let ret = native.invoke(&mut heap, &args).unwrap().unwrap();
        assert_eq!(ret.as_int(), 42);
    }

    #[test]
    fn direct_natives_are_listed_by_id_and_invoke_the_same_function() {
        fn twice(_heap: &mut Heap, args: &[Value]) -> Value {
            Value::from(args[0].as_int() * 2)
        }
        let sig = FfiSignature::from_parts("twice", vec![FfiType::Int], FfiType::Int).unwrap();
        let mut reg = Natives::new();
        let plain = reg.register(Arc::new(HostClosureFn::unary_i64("plain", |x| x)));
        let direct = reg.register(Arc::new(HostClosureFn::new_direct(sig.clone(), twice)));
        let special = reg.register(Arc::new(HostClosureFn::new_direct(sig, twice).with_host_op(HostOp::Collect)));
        assert!(reg.direct_by_id(plain).is_none());
        assert!(reg.direct_by_id(special).is_none(), "a special host op never runs as a plain call");
        assert!(reg.direct_by_id(99).is_none());
        let mut heap = Heap::default();
        let args = [Value::from(21_i64)];
        let f = reg.direct_by_id(direct).expect("direct native");
        assert_eq!(f(&mut heap, &args).as_int(), 42);
        let via_invoke = reg.get_by_id(direct).unwrap().invoke(&mut heap, &args).unwrap().unwrap();
        assert_eq!(via_invoke.as_int(), 42);
    }

    #[test]
    fn registry_assigns_stable_ids() {
        let mut reg = Natives::new();
        let id0 = reg.register(Arc::new(HostClosureFn::unary_i64("a", |x| x)));
        let id1 = reg.register(Arc::new(HostClosureFn::unary_i64("b", |x| x + 1)));
        assert_eq!(id0, 0);
        assert_eq!(id1, 1);
        assert!(reg.get("a").is_some());
        assert_eq!(reg.get_by_id(1).unwrap().name(), "b");
    }

    #[test]
    fn cloned_registry_is_shared_and_copy_on_write() {
        let mut reg = Natives::new();
        reg.register(Arc::new(HostClosureFn::unary_i64("a", |x| x)));
        let worker = reg.clone_registry();
        assert!(Arc::ptr_eq(&reg.by_id, &worker.by_id), "clone shares tables");
        reg.register(Arc::new(HostClosureFn::unary_i64("b", |x| x)));
        assert_eq!(worker.len(), 1, "later registration does not leak into a clone");
        assert!(worker.get("b").is_none());
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn arity_range_accepts_min_through_max() {
        let sig = FfiSignature::from_parts("packed_like", vec![FfiType::Int; 3], FfiType::Int)
            .unwrap();
        let native = HostClosureFn::new_with_arity_range(sig, 2, 3, |_heap, args| {
            Ok(Some(Value::from(args.len() as i64)))
        });
        let mut heap = Heap::default();
        assert_eq!(
            native
                .invoke(&mut heap, &[Value::from(1_i64), Value::from(2_i64)])
                .unwrap()
                .unwrap()
                .as_int(),
            2
        );
        assert_eq!(
            native
                .invoke(
                    &mut heap,
                    &[Value::from(1_i64), Value::from(2_i64), Value::from(3_i64)]
                )
                .unwrap()
                .unwrap()
                .as_int(),
            3
        );
        let err = native
            .invoke(&mut heap, &[Value::from(1_i64)])
            .unwrap_err();
        assert!(matches!(
            err,
            FfiError::ArityMismatch {
                expected: 3,
                got: 1
            }
        ));
    }
}
