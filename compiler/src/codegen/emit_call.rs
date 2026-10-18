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
        let Some(enum_name) = self.two_word_return_kind(name) else {
            return true;
        };
        let layout = if crate::typechecking::return_layout::is_two_word_product_kind(&enum_name) {
            "(T, T)".to_string()
        } else {
            enum_name
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

    /// Emit a `CALL` (or another entry kind) to function `name` onto the
    /// module buffer.
    pub(super) fn emit_named_entry_on_module(
        &mut self,
        name: &str,
        arity: u32,
        kind: crate::il::EntryKind,
    ) -> bool {
        match kind {
            crate::il::EntryKind::Call => {
                let two_word = self.two_word_return_kind(name);
                let ret_words = if two_word.is_some() { 2 } else { 1 };
                let ok = self.emit_named_entry_on_module_ret(name, arity, kind, ret_words);
                if ok
                    && !self.repr_now().unboxing()
                    && let Some(enum_name) = two_word
                {
                    let mut bytecode = std::mem::take(&mut self.bytecode);
                    self.emit_box_pair_after_call(&mut bytecode, &enum_name);
                    self.bytecode = bytecode;
                }
                ok
            }
            crate::il::EntryKind::CodePtr | crate::il::EntryKind::MakePolyFn => {
                if !self.deny_two_word_address_of(name, 0..0) {
                    return false;
                }
                self.emit_named_entry_on_module_ret(name, arity, kind, 1)
            }
            _ => self.emit_named_entry_on_module_ret(name, arity, kind, 1),
        }
    }

    /// Same as [`Self::emit_named_entry_on_module`] with an explicit `CALL`
    /// return width (`1` or `2` words). Non-`Call` kinds ignore `ret_words`.
    pub(super) fn emit_named_entry_on_module_ret(
        &mut self,
        name: &str,
        arity: u32,
        kind: crate::il::EntryKind,
        ret_words: u32,
    ) -> bool {
        let adapter = self.raw_instance_redirect(name);
        let name = adapter.as_deref().unwrap_or(name);
        if let Some(&offset) = self.functions.get(name) {
            self.bytecode.push(Self::packed_entry_byte_ret(
                kind,
                arity,
                offset as u32,
                ret_words,
            ));
            true
        } else if let Some(label) = self.fn_entry_labels.get(name).copied() {
            self.bytecode.il_mut().emit_entry_ret_at(
                kind,
                arity,
                label,
                DebugLoc::unknown(),
                ret_words,
            );
            if self.builtin_show_thunks.iter().any(|(fqn, _, _)| fqn == name) {
                self.builtin_show_used.insert(name.to_string());
            }
            true
        } else {
            false
        }
    }

    /// The entry label a `CALL` of `name` reaches, as a staged buffer's
    /// `CALL` (or, `on_module`,
    /// [`Self::emit_named_entry_on_module_ret`]) emits it, with that
    /// call's side effects but without emitting it: `None` when the `CALL`
    /// would not be an entry op (no such function, or a body whose offset
    /// has no label). Takes [`Self::raw_instance_call`] as they do.
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
