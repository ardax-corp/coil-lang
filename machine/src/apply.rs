//! What a call of a function word does, as `CallIndirect` takes it
//! (`call_indirect`), for code that calls bodies without
//! the runtime's operand stack: which body runs on which words, or the new
//! function value a partial application makes.

use common::Value;

use crate::memory::{Heap, Member, Object, ObjFn};

/// What calling a function word comes to.
pub(crate) enum Apply {
    /// Run the body at `pc` on `args` (captures and dictionaries in
    /// place); then, when `extras` is not empty, call its result with them
    /// (an over-application).
    Call { pc: usize, args: Vec<Value>, extras: Vec<Value> },
    /// A partial application: this new function value.
    Value(Value),
    /// The word is no function (a compiler bug): the panic message.
    Bad(String),
}

/// Call `raw` on `args`, the last `dicts` of them instance dictionaries.
/// `code_len` bounds a raw code offset. A partial application or a rest
/// array allocates on `heap`; nothing collects meanwhile.
pub(crate) fn apply(heap: &mut Heap, raw: Value, args: &[Value], dicts: usize, code_len: usize) -> Apply {
    let fn_obj = if raw.is_zero() {
        None
    } else {
        match heap.find_object_by_addr(raw.bits()) {
            Some(Object::Fn(gc)) => Some(gc),
            _ => None,
        }
    };
    let (values, app_dicts) = args.split_at(args.len() - dicts);
    let Some(gc) = fn_obj else {
        if (raw.as_int() as u64) < code_len as u64 {
            return Apply::Call { pc: raw.as_int() as usize, args: args.to_vec(), extras: Vec::new() };
        }
        let what = if heap.find_object_by_addr(raw.bits()).is_some() { "a heap object that is not a function" } else { "not a code offset" };
        return Apply::Bad(format!("internal error: CallIndirect target {:#x} is {what} (compiler bug)", raw.bits()));
    };
    let base = gc.as_ref();
    if let Some(captured) = &base.dicts {
        // A `PolyFn` takes the arguments it declares (0: unrecorded, all)
        // and its captured dictionary slots, each the captured evidence or
        // else the call's; its result takes the rest.
        let arity = base.arity as usize;
        let take = if arity > 0 && values.len() > arity { arity } else { values.len() };
        let mut call = values[..take].to_vec();
        // The call's word of its type's variables, last (#2621): it counts
        // for a value made at a type of as many variables.
        let given = app_dicts.last().map_or(0, |w| w.bits());
        let count = (given >> 63 == 0).then_some((given >> common::KINDS_CALL_COUNT_SHIFT) as u32 & 0x3F);
        for (k, slot) in captured.iter().enumerate() {
            call.push(match slot {
                // A bit of the `kinds` word before it, from the call's.
                Some(Member::Value(v)) if let Some((n, to, from)) = common::kinds_from_call_parts(v.bits()) => {
                    if count == Some(n)
                        && given >> from & 1 == 1
                        && let Some(word) = call.last_mut()
                    {
                        *word = Value::from(word.bits() | 1 << to);
                    }
                    continue;
                }
                // A dictionary the call passes (#2636).
                Some(Member::Value(v)) if let Some(j) = common::dict_from_call_parts(v.bits()) => app_dicts.get(j as usize).copied().unwrap_or_default(),
                Some(Member::Value(v)) => *v,
                Some(Member::Object(o)) => Value::from(o.addr()),
                None => app_dicts.get(k).copied().unwrap_or_default(),
            });
        }
        return Apply::Call { pc: base.entry as usize, args: call, extras: values[take..].to_vec() };
    }
    // A monomorphic function takes no dictionaries.
    let arity = base.arity as usize;
    if base.filled_mask == 0 && !base.is_rest && values.len() == arity {
        let mut call = base.captures.clone();
        call.extend_from_slice(values);
        return Apply::Call { pc: base.entry as usize, args: call, extras: Vec::new() };
    }
    // The new arguments fill the next holes, in declaration order.
    let mut slots: Vec<Option<Value>> = vec![None; arity];
    let mut old = base.captured_args.iter();
    for (slot, v) in slots.iter_mut().enumerate() {
        if base.filled_mask & (1u64 << slot) != 0 {
            *v = old.next().copied();
        }
    }
    let mut filled_mask = base.filled_mask;
    let mut fresh = values.iter();
    for (slot, v) in slots.iter_mut().enumerate() {
        if filled_mask & (1u64 << slot) != 0 {
            continue;
        }
        let Some(&a) = fresh.next() else { break };
        *v = Some(a);
        filled_mask |= 1u64 << slot;
    }
    let remaining: Vec<Value> = fresh.copied().collect();
    let captured_args: Vec<Value> = slots
        .iter()
        .enumerate()
        .filter_map(|(slot, v)| (filled_mask & (1u64 << slot) != 0).then_some(*v).flatten())
        .collect();
    if (filled_mask.count_ones() as usize) < arity {
        let partial = ObjFn {
            entry: base.entry,
            arity: base.arity,
            is_rest: base.is_rest,
            filled_mask,
            captured_args,
            captures: base.captures.clone(),
            dicts: None,
        };
        let (object, _) = heap.alloc(partial, Object::Fn);
        return Apply::Value(Value::from(object.addr()));
    }
    let mut call = base.captures.clone();
    call.extend(captured_args);
    if base.is_rest {
        // The extras go in one array, unless the one extra is an array.
        let given = remaining.len() == 1
            && !remaining[0].is_zero()
            && matches!(heap.find_object_by_addr(remaining[0].bits()), Some(Object::Array(_)));
        let rest = if given {
            remaining[0]
        } else {
            let (object, _) = heap.alloc(crate::memory::ObjArray::new(remaining), Object::Array);
            Value::from(object.addr())
        };
        call.push(rest);
        return Apply::Call { pc: base.entry as usize, args: call, extras: Vec::new() };
    }
    Apply::Call { pc: base.entry as usize, args: call, extras: remaining }
}
