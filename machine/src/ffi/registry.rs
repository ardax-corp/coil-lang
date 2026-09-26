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
}

pub trait NativeFn: Send + Sync {
    fn name(&self) -> &str;
    fn signature(&self) -> &FfiSignature;
    fn invoke(&self, heap: &mut Heap, args: &[Value]) -> Result<Option<Value>, FfiError>;
    fn host_op(&self) -> HostOp {
        HostOp::Ordinary
    }
}

pub struct HostClosureFn {
    signature: FfiSignature,
    /// When set, accept `min_args..=max_args` instead of exact `signature.arity()`.
    arity_range: Option<(usize, usize)>,
    host_op: HostOp,
    func: HostClosure,
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
        }
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
}

impl Natives {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, native: Arc<dyn NativeFn>) -> usize {
        let id = self.by_id.len();
        let name = native.name().to_string();
        let by_name = Arc::make_mut(&mut self.by_name);
        if by_name.is_empty() {
            // The standard host table is ~140 natives; skip the rehash ladder.
            by_name.reserve(192);
        }
        by_name.insert(name, Arc::clone(&native));
        Arc::make_mut(&mut self.by_id).push(native);
        id
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn NativeFn>> {
        self.by_name.get(name).cloned()
    }

    pub fn get_by_id(&self, id: usize) -> Option<&dyn NativeFn> {
        self.by_id.get(id).map(|n| n.as_ref())
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
