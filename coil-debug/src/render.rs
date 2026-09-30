//! Render frame words with their compile-time types.

use compiler::debug_vars::{DebugClassTable, DebugEnumTable, DebugTy, DebugVar, DebugVarLoc};
use machine::{DebugObject, Machine};

/// How deep nested objects are expanded, and how many elements are shown.
const MAX_DEPTH: u32 = 3;
const MAX_ELEMS: usize = 16;

pub struct Renderer<'a> {
    pub machine: &'a Machine<256>,
    pub classes: &'a DebugClassTable,
    pub enums: &'a DebugEnumTable,
}

impl Renderer<'_> {
    /// Value of `var` in frame `frame` stopped at `pc`, or `<optimized out>`.
    pub fn var(&self, var: &DebugVar, frame: usize, pc: u32) -> String {
        let word = |slot: u32| self.machine.debug_slot(frame, slot as usize);
        match &var.loc {
            DebugVarLoc::Slot(_) => match var.slot_at(pc).and_then(word) {
                Some(w) => self.value(w, &var.ty, 0),
                None => "<optimized out>".into(),
            },
            DebugVarLoc::Fields { class, fields } => {
                let parts: Vec<String> = fields
                    .iter()
                    .enumerate()
                    .map(|(i, (name, _, ty))| match var.component_slot(i, pc).and_then(word) {
                        Some(w) => format!("{name}: {}", self.value(w, ty, 1)),
                        None => format!("{name}: <optimized out>"),
                    })
                    .collect();
                format!("{class} {{ {} }}", parts.join(", "))
            }
            DebugVarLoc::Elems { slots, elem } => {
                let parts: Vec<String> = (0..slots.len())
                    .map(|i| match var.component_slot(i, pc).and_then(word) {
                        Some(w) => self.value(w, elem, 1),
                        None => "<optimized out>".into(),
                    })
                    .collect();
                format!("[{}]", parts.join(", "))
            }
            DebugVarLoc::Pair {
                enum_name,
                payload_ty,
                ..
            } => {
                let (Some(p), Some(t)) = (
                    var.component_slot(0, pc).and_then(word),
                    var.component_slot(1, pc).and_then(word),
                ) else {
                    return "<optimized out>".into();
                };
                let variant = self.variant_name(enum_name, t.as_int() as u32);
                if self.variant_is_unit(enum_name, &variant) {
                    format!("{enum_name}::{variant}")
                } else {
                    format!("{enum_name}::{variant}({})", self.value(p, payload_ty, 1))
                }
            }
        }
    }

    /// One word as a value of `ty`.
    pub fn value(&self, word: common::Value, ty: &DebugTy, depth: u32) -> String {
        match ty {
            DebugTy::Int => word.as_int().to_string(),
            DebugTy::Float => format!("{:?}", word.as_float()),
            DebugTy::Bool => (word.as_int() != 0).to_string(),
            DebugTy::Byte => (word.as_int() & 0xFF).to_string(),
            DebugTy::Unit => "()".into(),
            DebugTy::Str => match self.machine.debug_object(word) {
                Some(DebugObject::Str(s)) => format!("{s:?}"),
                _ => "<invalid string>".into(),
            },
            DebugTy::Class(name) => self.class(word, name, depth),
            DebugTy::Array(elem) => match self.machine.debug_object(word) {
                Some(DebugObject::Array(items)) => self.list("[", "]", &items, |w| {
                    self.value(w, elem, depth + 1)
                }, depth),
                _ => self.untyped(word, depth),
            },
            DebugTy::Tuple(items) => match self.machine.debug_object(word) {
                Some(DebugObject::Tuple(words)) => {
                    let parts: Vec<String> = words
                        .iter()
                        .enumerate()
                        .map(|(i, w)| {
                            items
                                .get(i)
                                .map_or_else(|| self.untyped(*w, depth + 1), |t| self.value(*w, t, depth + 1))
                        })
                        .collect();
                    format!("({})", parts.join(", "))
                }
                _ => self.untyped(word, depth),
            },
            DebugTy::Enum(name, args) => self.enum_value(word, name, args, depth),
            DebugTy::Other(_) => self.untyped(word, depth),
        }
    }

    fn class(&self, word: common::Value, name: &str, depth: u32) -> String {
        if word.raw().is_null() {
            return "null".into();
        }
        match self.machine.debug_object(word) {
            Some(DebugObject::Instance { fields, .. }) => {
                if depth >= MAX_DEPTH {
                    return format!("{name} {{ … }}");
                }
                let layout = self.classes.get(name);
                let parts: Vec<String> = fields
                    .iter()
                    .enumerate()
                    .map(|(i, w)| match layout.and_then(|l| l.get(i)) {
                        Some((field, ty)) => format!("{field}: {}", self.value(*w, ty, depth + 1)),
                        None => self.untyped(*w, depth + 1),
                    })
                    .collect();
                format!("{name} {{ {} }}", parts.join(", "))
            }
            _ => self.untyped(word, depth),
        }
    }

    fn enum_value(&self, word: common::Value, name: &str, args: &[DebugTy], depth: u32) -> String {
        let arg = |i: usize| args.get(i).cloned().unwrap_or(DebugTy::Other("?".into()));
        let object = self.machine.debug_object(word);
        if let Some(DebugObject::Enum { tag, payload }) = &object {
            let variant = self.variant_name(name, *tag);
            if payload.is_empty() {
                return format!("{name}::{variant}");
            }
            // Builtin generics: payload type from the type arguments.
            let ty = match (name, variant.as_str()) {
                ("Option", "Some") | ("Result", "Ok") => arg(0),
                ("Result", "Err") => arg(1),
                _ => DebugTy::Other("?".into()),
            };
            let parts: Vec<String> = payload.iter().map(|w| self.value(*w, &ty, depth + 1)).collect();
            return format!("{name}::{variant}({})", parts.join(", "));
        }
        // Niche layouts (COI-92): `Option` of a heap object is `0` / the
        // object; heap-heap `Result` tags `Err` with bit 0.
        match name {
            "Option" if word.raw().is_null() => "Option::None".into(),
            "Option" => format!("Option::Some({})", self.value(word, &arg(0), depth + 1)),
            "Result" if word.raw() as u64 & 1 == 1 => {
                let inner = common::Value::from(word.raw() as u64 & !1);
                format!("Result::Err({})", self.value(inner, &arg(1), depth + 1))
            }
            "Result" => format!("Result::Ok({})", self.value(word, &arg(0), depth + 1)),
            // Scalar-backed (`#[repr(int)]`) enums: the word is the value.
            _ if object.is_none() => format!("{name}({})", word.as_int()),
            _ => self.untyped(word, depth),
        }
    }

    /// A word without a usable type: an object when one lives at that
    /// address, else an integer.
    fn untyped(&self, word: common::Value, depth: u32) -> String {
        match self.machine.debug_object(word) {
            Some(DebugObject::Str(s)) => format!("{s:?}"),
            Some(DebugObject::Array(items)) => {
                self.list("[", "]", &items, |w| self.untyped(w, depth + 1), depth)
            }
            Some(DebugObject::Tuple(items)) => {
                self.list("(", ")", &items, |w| self.untyped(w, depth + 1), depth)
            }
            Some(DebugObject::Enum { tag, payload }) if payload.is_empty() => format!("<variant {tag}>"),
            Some(DebugObject::Enum { tag, payload }) => format!(
                "<variant {tag}>{}",
                self.list("(", ")", &payload, |w| self.untyped(w, depth + 1), depth)
            ),
            Some(DebugObject::Instance { fields, .. }) => {
                self.list("{ ", " }", &fields, |w| self.untyped(w, depth + 1), depth)
            }
            Some(DebugObject::Boxed(inner)) => inner.as_int().to_string(),
            Some(DebugObject::Other(kind)) => format!("<{kind}>"),
            None => word.as_int().to_string(),
        }
    }

    fn list(
        &self,
        open: &str,
        close: &str,
        items: &[common::Value],
        each: impl Fn(common::Value) -> String,
        depth: u32,
    ) -> String {
        if depth >= MAX_DEPTH {
            return format!("{open}…{close}");
        }
        let mut parts: Vec<String> = items.iter().take(MAX_ELEMS).map(|w| each(*w)).collect();
        if items.len() > MAX_ELEMS {
            parts.push(format!("… {} more", items.len() - MAX_ELEMS));
        }
        format!("{open}{}{close}", parts.join(", "))
    }

    fn variant_name(&self, enum_name: &str, tag: u32) -> String {
        let builtin: &[&str] = match enum_name {
            "Option" => &["None", "Some"],
            "Result" => &["Ok", "Err"],
            _ => &[],
        };
        self.enums
            .get(enum_name)
            .and_then(|v| v.get(tag as usize).cloned())
            .or_else(|| builtin.get(tag as usize).map(|s| s.to_string()))
            .unwrap_or_else(|| format!("#{tag}"))
    }

    fn variant_is_unit(&self, enum_name: &str, variant: &str) -> bool {
        matches!((enum_name, variant), ("Option", "None"))
    }
}

/// Follow `.field` / `[index]` steps from `var` and render the result.
pub fn eval_path(
    r: &Renderer<'_>,
    var: &DebugVar,
    frame: usize,
    pc: u32,
    path: &[crate::session::PathStep],
) -> Result<String, String> {
    use crate::session::PathStep;
    let word = |slot: u32| {
        r.machine
            .debug_slot(frame, slot as usize)
            .ok_or_else(|| "<unavailable>".to_string())
    };
    let mut steps = path.iter();
    // The first step may address a component of a split layout directly.
    let (mut value, mut ty) = match (&var.loc, path.first()) {
        (DebugVarLoc::Fields { fields, class }, Some(PathStep::Field(f))) => {
            steps.next();
            let i = fields
                .iter()
                .position(|(name, _, _)| name == f)
                .ok_or_else(|| format!("`{class}` has no field `{f}`"))?;
            let Some(slot) = var.component_slot(i, pc) else {
                return Ok("<optimized out>".into());
            };
            (word(slot)?, fields[i].2.clone())
        }
        (DebugVarLoc::Elems { slots, elem }, Some(PathStep::Index(i))) => {
            steps.next();
            if *i >= slots.len() {
                return Err(format!("index {i} out of bounds (len {})", slots.len()));
            }
            let Some(slot) = var.component_slot(*i, pc) else {
                return Ok("<optimized out>".into());
            };
            (word(slot)?, elem.clone())
        }
        (DebugVarLoc::Slot(_), _) => {
            let slot = var
                .slot_at(pc)
                .ok_or_else(|| format!("`{}` is optimized out here", var.name))?;
            (word(slot)?, var.ty.clone())
        }
        _ => return Err(format!("cannot index into `{}` this way", var.name)),
    };
    for step in steps {
        (value, ty) = match (step, &ty, r.machine.debug_object(value)) {
            (PathStep::Field(f), DebugTy::Class(name), Some(DebugObject::Instance { fields, .. })) => {
                let layout = r.classes.get(name).ok_or_else(|| format!("unknown class `{name}`"))?;
                let idx = layout
                    .iter()
                    .position(|(n, _)| n == f)
                    .ok_or_else(|| format!("`{name}` has no field `{f}`"))?;
                let w = *fields.get(idx).ok_or("field out of range")?;
                (w, layout[idx].1.clone())
            }
            (PathStep::Index(i), DebugTy::Array(elem), Some(DebugObject::Array(items))) => {
                let w = *items
                    .get(*i)
                    .ok_or_else(|| format!("index {i} out of bounds (len {})", items.len()))?;
                (w, (**elem).clone())
            }
            (PathStep::Index(i), DebugTy::Tuple(tys), Some(DebugObject::Tuple(items))) => {
                let w = *items.get(*i).ok_or_else(|| format!("tuple has no element {i}"))?;
                (w, tys.get(*i).cloned().unwrap_or(DebugTy::Other("?".into())))
            }
            (step, ty, _) => {
                return Err(format!("cannot apply {step:?} to a value of type {}", ty.name()));
            }
        };
    }
    Ok(r.value(value, &ty, 0))
}
