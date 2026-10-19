//! Call lowering extracted from `do_compile` (stack-margin style).

use super::*;

impl Compiler {

    /// `true` when `name`'s two-word classification is safe to ignore here
    /// (i.e. it stays one word, the common case). `false` when `name`
    /// returns a known two-word layout: this call site takes its address
    /// (`CodePtr` / `MakePolyFn` / FFI callback / partial application),
    /// which needs the one-word ABI (task cut: `CallIndirect`, PolyFn,
    /// FFI, coroutines keep boxed `ObjEnum`). Records a diagnostic instead
    /// of silently mis-encoding the target as a unary entry.
    pub(super) fn deny_two_word_address_of(
        &mut self,
        name: &str,
        range: std::ops::Range<usize>,
    ) -> bool {
        let Some(kind) = self.two_word_return_kind(name) else {
            return true;
        };
        let layout = match &kind {
            PairKind::Product => "(T, T)",
            PairKind::Range { inclusive: false } => "__range",
            PairKind::Range { inclusive: true } => "__range_inc",
            _ => kind.enum_name().unwrap_or_default(),
        };
        let mut message = Message::error(
            ErrorCode::CodegenError,
            format!(
                "`{name}` returns a known two-word `{layout}` layout and cannot be used as a function value"
            ),
            range.clone(),
        );
        message.push(DiagLabel::new(
            "direct calls are fine; taking its address (assigning it, passing it as a callback, or partially applying it) is not supported for this return layout".to_string(),
            range,
        ));
        self.messages.push(message);
        false
    }

    /// The entry label a `CALL` of `name` reaches, with that call's side
    /// effects (`on_module`: marking a builtin `Show` thunk used) but
    /// without emitting it: `None` when the `CALL` would not be an entry
    /// op (no such function, or a body whose offset has no label). Takes
    /// [`Self::raw_instance_call`].
    pub(super) fn named_entry_target(&mut self, name: &str, on_module: bool) -> Option<crate::il::Label> {
        let adapter = self.raw_instance_redirect(name);
        let name = adapter.as_deref().unwrap_or(name);
        if let Some(&offset) = self.functions.get(name) {
            self.bytecode.entry_label_for_offset(offset)
        } else {
            let label = self.fn_entry_labels.get(name).copied()?;
            if on_module && self.builtin_show_thunks.iter().any(|(fqn, _, _)| fqn == name) {
                self.builtin_show_used.insert(name.to_string());
            }
            Some(label)
        }
    }

    /// The adapter an entry of raw instance method `name` goes through
    /// unless the call site said it passes raw arguments
    /// ([`Self::raw_instance_call`], taken here).
    pub(super) fn raw_instance_redirect(&mut self, name: &str) -> Option<String> {
        let raw_call = std::mem::take(&mut self.raw_instance_call);
        (!raw_call && self.raw_instance_methods.contains(name)).then(|| Self::dict_adapter_name(name))
    }

    pub(super) fn packed_entry_byte_ret(
        kind: crate::il::EntryKind,
        arity: u32,
        offset: u32,
        ret_words: u32,
    ) -> Byte {
        let inst = match kind {
            crate::il::EntryKind::Call => Instruction::CALL,
            crate::il::EntryKind::TailCall => Instruction::TailCall,
            crate::il::EntryKind::MakeCoro => Instruction::MakeCoro,
            crate::il::EntryKind::CodePtr => Instruction::CodePtr,
            crate::il::EntryKind::MakePolyFn => Instruction::MakePolyFn,
        };
        match kind {
            crate::il::EntryKind::CodePtr | crate::il::EntryKind::MakePolyFn => {
                Byte::new(inst).with_operand_u32(offset)
            }
            crate::il::EntryKind::Call => {
                Byte::new(inst).with_call_packed_ret(arity, offset, ret_words)
            }
            _ => Byte::new(inst).with_call_packed(arity, offset),
        }
    }

    pub(super) fn missing_call_target(&mut self, name: &str, range: std::ops::Range<usize>) {
        let mut message = Message::error(
            ErrorCode::CodegenError,
            format!("missing function entry `{name}`"),
            range.clone(),
        );
        message.push(DiagLabel::new(
            format!("no bound or reserved entry for `{name}`"),
            range,
        ));
        self.messages.push(message);
    }

}
