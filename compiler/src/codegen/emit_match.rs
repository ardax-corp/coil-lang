//! Match lowering extracted from `do_compile` (stack-margin style).

use super::*;

impl Compiler {
    #[inline(never)]
    pub(super) fn compile_match_expr<'compiler>(
        &mut self,
        scrutinee: &Output<'compiler>,
        arms: &[MatchArm<'compiler>],
    ) -> CodeBuf {
        if self.try_compile_frame_local_match(scrutinee, arms) {
            return CodeBuf::new();
        }
        if (self.compiling_pair_mode
            || matches!(scrutinee.1.as_ref(), Expression::Call { .. }))
            && self.expr_is_pair_producer(scrutinee)
            && self.expr_pair_enum_kind(scrutinee).is_some()
            && self.try_compile_pair_match(scrutinee, arms)
        {
            return CodeBuf::new();
        }
        if self.expr_is_niche_option(scrutinee) {
            if self.try_compile_niche_option_match(scrutinee, arms) {
                return CodeBuf::new();
            }

            // Existing pattern lowering requires an ObjEnum. Force direct
            // constructors onto the boxed path before entering it.
            let previous = self.force_heap_option;
            self.force_heap_option = true;
            let result = self.compile_match_expr_boxed(scrutinee, arms);
            self.force_heap_option = previous;
            return result;
        }
        self.compile_match_expr_boxed(scrutinee, arms)
    }

    pub(super) fn try_compile_pair_match<'compiler>(
        &mut self,
        scrutinee: &Output<'compiler>,
        arms: &[MatchArm<'compiler>],
    ) -> bool {
        if arms.len() != 2 {
            return false;
        }
        let Some(first) = Self::pair_match_arm_info(&self.checker, &arms[0]) else {
            return false;
        };
        let Some(second) = Self::pair_match_arm_info(&self.checker, &arms[1]) else {
            return false;
        };
        if first.0 == second.0 {
            return false;
        }

        self.bytecode
            .push_seek(self.context.variables.len() as u32);
        let previous_pair_context = self.pair_value_context;
        self.pair_value_context = true;
        let mut scrutinee_bc = self.do_compile(scrutinee);
        self.pair_value_context = previous_pair_context;
        self.bytecode.append(&mut scrutinee_bc);

        let mut bb = BlockBuilder::new();
        let fallback = bb.fresh_label(self.bytecode.il_mut());
        let end = bb.fresh_label(self.bytecode.il_mut());
        self.bytecode.push(Byte::new(Instruction::DUPLICATE));
        self.bytecode.push_const(first.0 as i32);
        self.bytecode.push(Byte::new(Instruction::EQ));
        bb.emit_jump_to_hinted(
            fallback,
            BbJumpKind::JumpIfFalse,
            FuseHint::nofuse_value_under_jmp(),
            self.bytecode.il_mut(),
        );
        self.bytecode.push_pop(); // tag
        let first_slot = self.alloc_pair_match_binding(first.1);
        if let Some(slot) = first_slot {
            self.bytecode.push_store_pop(slot);
        } else {
            self.bytecode.push_pop(); // payload
        }
        self.compile_pair_match_body(&arms[0], first.1, first_slot);
        bb.emit_jump_to(end, BbJumpKind::Unconditional, self.bytecode.il_mut());

        bb.bind_label(fallback, self.bytecode.il_mut());
        self.bytecode.push_pop(); // second tag
        let second_slot = self.alloc_pair_match_binding(second.1);
        if let Some(slot) = second_slot {
            self.bytecode.push_store_pop(slot);
        } else {
            self.bytecode.push_pop(); // payload
        }
        self.compile_pair_match_body(&arms[1], second.1, second_slot);
        bb.bind_label(end, self.bytecode.il_mut());
        true
    }

    fn pair_match_arm_info<'a>(
        checker: &Checker,
        arm: &'a MatchArm<'a>,
    ) -> Option<(u32, Option<&'a str>)> {
        let Pattern::Constructor {
            enum_name,
            variant_name,
            payload,
        } = &arm.pattern.1
        else {
            return None;
        };
        if !common::is_builtin_option_enum(enum_name)
            && !common::is_builtin_result_enum(enum_name)
        {
            return None;
        }
        let tag = checker.tag_for(enum_name, variant_name)?;
        let binding = match payload {
            PatternPayload::Unit => None,
            PatternPayload::Tuple(parts) if parts.len() == 1 => match &parts[0].1 {
                Pattern::Binding { name } => Some(*name),
                Pattern::Wildcard => None,
                _ => return None,
            },
            _ => return None,
        };
        Some((tag, binding))
    }

    fn alloc_pair_match_binding(&mut self, binding: Option<&str>) -> Option<u32> {
        let name = binding?;
        let slot = self.context.variables.len() as u32;
        self.context
            .variables
            .intern(format!("__pair_match{}", slot));
        self.record_debug_local(name, slot);
        Some(slot)
    }

    fn compile_pair_match_body(
        &mut self,
        arm: &MatchArm<'_>,
        binding: Option<&str>,
        slot: Option<u32>,
    ) {
        let mut inner = HashMap::new();
        if let (Some(name), Some(slot)) = (binding, slot) {
            inner.insert(name.to_string(), slot);
        }
        let saved_bindings = self.push_match_bindings(inner);
        let mut body_bc = self.do_compile(&arm.body);
        self.bytecode.append(&mut body_bc);
        self.context.match_bindings = saved_bindings;
    }

    /// Local ObjEnum that the checker proved never leaves this frame:
    /// `[payload, tag]` in slots / on the stack, no `MakeEnum`.
    fn try_compile_frame_local_match<'compiler>(
        &mut self,
        scrutinee: &Output<'compiler>,
        arms: &[MatchArm<'compiler>],
    ) -> bool {
        if arms.is_empty() {
            return false;
        }
        let mut peeled = scrutinee;
        loop {
            match peeled.1.as_ref() {
                Expression::Group(inner) | Expression::Expr(inner) => peeled = inner,
                Expression::Fragment(items) if items.len() == 1 => peeled = &items[0],
                _ => break,
            }
        }
        let ident = match peeled.1.as_ref() {
            Expression::Identifier(n) => Some(*n),
            _ => None,
        };
        let from_ident = ident.and_then(|n| self.unboxed_enum_info(n));
        let from_fact = self.node_is_frame_local(scrutinee) || self.node_is_frame_local(peeled);
        if from_ident.is_none() && !from_fact {
            return false;
        }
        if matches!(peeled.1.as_ref(), Expression::Identifier(_)) && from_ident.is_none() {
            return false;
        }
        if matches!(peeled.1.as_ref(), Expression::Construct { .. }) && !from_fact {
            return false;
        }
        if !matches!(
            peeled.1.as_ref(),
            Expression::Identifier(_) | Expression::Construct { .. }
        ) {
            return false;
        }

        let mut arm_info: Vec<(u32, usize, Option<&str>)> = Vec::new();
        let mut wildcard: Option<usize> = None;
        for (index, arm) in arms.iter().enumerate() {
            match &arm.pattern.1 {
                Pattern::Constructor {
                    enum_name,
                    variant_name,
                    payload,
                } => {
                    let Some(tag) = self.checker.tag_for(enum_name, variant_name) else {
                        return false;
                    };
                    let arity = self.checker.arity_for(enum_name, variant_name).unwrap_or(0);
                    if arity > 1 {
                        return false;
                    }
                    let binding = match payload {
                        PatternPayload::Unit => None,
                        PatternPayload::Tuple(parts) if parts.len() == 1 => match &parts[0].1 {
                            Pattern::Binding { name } => Some(*name),
                            Pattern::Wildcard => None,
                            _ => return false,
                        },
                        PatternPayload::Record(fields) if fields.len() == 1 => {
                            match &fields[0].pattern.1 {
                                Pattern::Binding { name } => Some(*name),
                                Pattern::Wildcard => None,
                                _ => return false,
                            }
                        }
                        PatternPayload::Tuple(parts) if parts.is_empty() => None,
                        _ => return false,
                    };
                    arm_info.push((tag, index, binding));
                }
                Pattern::Wildcard => wildcard = Some(index),
                Pattern::Binding { name } => {
                    arm_info.push((u32::MAX, index, Some(*name)));
                }
            }
        }
        if arm_info.is_empty() && wildcard.is_none() {
            return false;
        }

        self.bytecode
            .push_seek(self.context.variables.len() as u32);
        self.unbox_enum_context += 1;
        let mut scrutinee_bc = self.do_compile(scrutinee);
        self.unbox_enum_context -= 1;
        self.bytecode.append(&mut scrutinee_bc);

        let mut bb = BlockBuilder::new();
        let end = bb.fresh_label(self.bytecode.il_mut());
        let n_dispatch = arm_info.len();
        for (i, (tag, arm_idx, binding)) in arm_info.iter().enumerate() {
            let is_last = i + 1 == n_dispatch && wildcard.is_none();
            let miss = if is_last {
                None
            } else {
                Some(bb.fresh_label(self.bytecode.il_mut()))
            };
            if *tag != u32::MAX && !is_last {
                self.bytecode.push(Byte::new(Instruction::DUPLICATE));
                self.bytecode.push_const(*tag as i32);
                self.bytecode.push(Byte::new(Instruction::EQ));
                bb.emit_jump_to_hinted(
                    miss.expect("miss label"),
                    BbJumpKind::JumpIfFalse,
                    FuseHint::nofuse_value_under_jmp(),
                    self.bytecode.il_mut(),
                );
                self.bytecode.push_pop();
            } else if *tag != u32::MAX && is_last {
                self.bytecode.push_pop();
            } else if *tag == u32::MAX {
                self.bytecode.push_pop();
            }
            let slot = binding.map(|name| {
                let slot = self.context.variables.len() as u32;
                self.context
                    .variables
                    .intern(format!("__unbox_match{slot}"));
                self.record_debug_local(name, slot);
                slot
            });
            if let Some(slot) = slot {
                self.bytecode.push_store_pop(slot);
            } else {
                self.bytecode.push_pop();
            }
            self.compile_pair_match_body(&arms[*arm_idx], *binding, slot);
            let more = i + 1 < n_dispatch || wildcard.is_some();
            if more {
                bb.emit_jump_to(end, BbJumpKind::Unconditional, self.bytecode.il_mut());
            }
            if let Some(m) = miss {
                bb.bind_label(m, self.bytecode.il_mut());
            }
        }
        if let Some(w) = wildcard {
            self.bytecode.push_pop();
            self.bytecode.push_pop();
            self.compile_pair_match_body(&arms[w], None, None);
        }
        bb.bind_label(end, self.bytecode.il_mut());
        if let Some((payload, tag_slot)) = from_ident {
            let last = self.node_id_of(peeled).is_some_and(|id| {
                self.typed_sidecar.is_frame_local_last_use(id)
            }) || self.node_id_of(scrutinee).is_some_and(|id| {
                self.typed_sidecar.is_frame_local_last_use(id)
            });
            if last {
                self.bytecode.push_const(0);
                self.bytecode.push_store_pop(payload);
                self.bytecode.push_const(0);
                self.bytecode.push_store_pop(tag_slot);
            }
        }
        true
    }

    /// Compile a niche-option match when both arms only inspect the null
    /// sentinel and optionally bind the payload.
    fn try_compile_niche_option_match<'compiler>(
        &mut self,
        scrutinee: &Output<'compiler>,
        arms: &[MatchArm<'compiler>],
    ) -> bool {
        if arms.len() != 2 || !self.expr_is_niche_option(scrutinee) {
            return false;
        }

        let mut some: Option<(usize, Option<&str>)> = None;
        let mut fallback: Option<(usize, Option<&str>)> = None;
        for (index, arm) in arms.iter().enumerate() {
            match &arm.pattern.1 {
                Pattern::Constructor {
                    enum_name,
                    variant_name,
                    payload: PatternPayload::Tuple(parts),
                } if common::is_builtin_option_enum(enum_name)
                    && *variant_name == "Some"
                    && parts.len() == 1 =>
                {
                    let binding = match &parts[0].1 {
                        Pattern::Binding { name } => Some(*name),
                        Pattern::Wildcard => None,
                        _ => return false,
                    };
                    some = Some((index, binding));
                }
                Pattern::Constructor {
                    enum_name,
                    variant_name,
                    payload: PatternPayload::Unit,
                } if common::is_builtin_option_enum(enum_name)
                    && *variant_name == "None" =>
                {
                    fallback = Some((index, None));
                }
                Pattern::Wildcard => {
                    fallback = Some((index, None));
                }
                Pattern::Binding { name } => {
                    fallback = Some((index, Some(*name)));
                }
                _ => return false,
            }
        }
        let Some((some_index, some_binding)) = some else {
            return false;
        };
        let Some((fallback_index, fallback_binding)) = fallback else {
            return false;
        };

        self.bytecode
            .push_seek(self.context.variables.len() as u32);
        let previous_niche_context = self.force_niche_option;
        self.force_niche_option = true;
        let mut scrutinee_bc = self.do_compile(scrutinee);
        self.force_niche_option = previous_niche_context;
        self.bytecode.append(&mut scrutinee_bc);

        let mut bb = BlockBuilder::new();
        let fallback_label = bb.fresh_label(self.bytecode.il_mut());
        let end_label = bb.fresh_label(self.bytecode.il_mut());

        // Keep the niche value for the selected arm while testing a duplicate.
        self.bytecode.push(Byte::new(Instruction::DUPLICATE));
        self.bytecode.push(Byte::new(Instruction::LogNot));
        bb.emit_jump_to(
            fallback_label,
            BbJumpKind::JumpIfTrue,
            self.bytecode.il_mut(),
        );

        let some_slot = some_binding.map(|name| {
            let slot = self.context.variables.len() as u32;
            self.context
                .variables
                .intern(format!("__niche_match{}", slot));
            self.record_debug_local(name, slot);
            slot
        });
        if let Some(slot) = some_slot {
            self.bytecode.push_store_pop(slot);
        } else {
            self.bytecode.push_pop();
        }
        self.compile_niche_match_body(&arms[some_index], some_binding, some_slot);
        bb.emit_jump_to(end_label, BbJumpKind::Unconditional, self.bytecode.il_mut());

        bb.bind_label(fallback_label, self.bytecode.il_mut());
        let fallback_slot = fallback_binding.map(|name| {
            let slot = self.context.variables.len() as u32;
            self.context
                .variables
                .intern(format!("__niche_match{}", slot));
            self.record_debug_local(name, slot);
            slot
        });
        if let Some(slot) = fallback_slot {
            self.bytecode.push_store_pop(slot);
        } else {
            self.bytecode.push_pop();
        }
        self.compile_niche_match_body(&arms[fallback_index], fallback_binding, fallback_slot);

        bb.bind_label(end_label, self.bytecode.il_mut());
        true
    }

    fn compile_niche_match_body(
        &mut self,
        arm: &MatchArm<'_>,
        binding: Option<&str>,
        slot: Option<u32>,
    ) {
        let mut inner = HashMap::new();
        if let (Some(name), Some(slot)) = (binding, slot) {
            inner.insert(name.to_string(), slot);
        }
        let saved_bindings = self.push_match_bindings(inner);
        if let Some(slot) = slot {
            self.record_debug_local(binding.unwrap_or(""), slot);
        }
        let mut body_bc = self.do_compile(&arm.body);
        self.bytecode.append(&mut body_bc);
        self.context.match_bindings = saved_bindings;
    }

    #[inline(never)]
    pub(super) fn compile_match_expr_boxed<'compiler>(
        &mut self,
        scrutinee: &Output<'compiler>,
        arms: &[MatchArm<'compiler>],
    ) -> CodeBuf {
        let mut bytecode = CodeBuf::new();
        if arms.is_empty() {
            bytecode.append(&mut self.do_compile(scrutinee));
            bytecode.push_pop();
        } else {
            let mut bb = BlockBuilder::new();
            let end_label = bb.fresh_label(self.bytecode.il_mut());

            let tag_groups = group_arms_by_outer_tag(arms, &self.checker);
            // Forward pass emits JUMP_IF_MATCH for every non-last
            // group, and also for the last group when any group is
            // multi-arm. Allocate labels for those targets — not
            // merely for `!is_last && Constructor` in source order
            // (that missed the last group's first arm when Err
            // followed two Ok arms, panicking at emit time).
            let any_multi_arm_group = tag_groups.iter().any(|g| g.arm_indices.len() > 1);
            let mut arm_labels: Vec<Option<crate::block_builder::Label>> =
                vec![None; arms.len()];
            for (g_idx, group) in tag_groups.iter().enumerate() {
                let is_last_group = g_idx == tag_groups.len() - 1;
                if !is_last_group || any_multi_arm_group {
                    let first_arm_idx = group.arm_indices[0];
                    arm_labels[first_arm_idx] =
                        Some(bb.fresh_label(self.bytecode.il_mut()));
                }
            }

            // Reset shared stack/locals cursor to the live-local height
            // before evaluating the scrutinee. Prior STORE high-water
            // (e.g. `let v = match` in a loop) would otherwise make
            // JumpIfMatch push payloads past `payload_base`.
            let seek_base = self.context.variables.len() as u32;
            self.bytecode.push_seek(seek_base);

            // Compile scrutinee before choosing payload_base —
            // HostInvoke arg staging (`alloc_temp_slot`) grows
            // `variables`, and bindings must start *after* those
            // temps or Unpack/JumpIfMatch collide with them
            // (e.g. `match try_recv(rx)` after print→write_all).
            let mut scrutinee_bc = self.do_compile(scrutinee);
            self.bytecode.append(&mut scrutinee_bc);
            if self.force_heap_option
                && self.expr_is_niche_option(scrutinee)
                && !Self::is_option_construct(scrutinee)
            {
                self.bytecode
                    .push(Byte::new(Instruction::OptionNicheToHeap));
            }

            // First payload slot after locals + scrutinee temps.
            // JumpIfMatch/Unpack push payloads onto the stack
            // above those locals, so bindings must start here —
            // not at the historical hardcoded slot 1.
            let payload_base = self.context.variables.len() as u32;

            // Forward pass: outer-tag dispatch + last-arm scrutinee consumer.
            for (g_idx, group) in tag_groups.iter().enumerate() {
                let is_last_group = g_idx == tag_groups.len() - 1;
                if !is_last_group || any_multi_arm_group {
                    let first_arm_idx = group.arm_indices[0];
                    let label = arm_labels[first_arm_idx]
                        .expect("non-last group's first arm must have a Label");
                    bb.emit_jump_to(
                        label,
                        BbJumpKind::JumpIfMatch {
                            tag: group.tag,
                            arity: 0,
                        },
                        self.bytecode.il_mut(),
                    );
                } else {
                    // Last group in a match with NO
                    // multi-arm groups — emit the
                    // scrutinee-consumer for the
                    // last arm in source order (the
                    // last element of the last
                    // group's `arm_indices`). This
                    // matches the pre-grouped
                    // behavior: the very last arm
                    // is reached by fall-through
                    // from every preceding
                    // JUMP_IF_MATCH miss, so the
                    // scrutinee is still on the
                    // stack and must be consumed
                    // (UNPACK for Constructor, POP
                    // for Wildcard, STORE 1 for
                    // Binding).
                    let last_arm_idx = *group
                        .arm_indices
                        .last()
                        .expect("last group must have at least one arm");
                    let last_arm = &arms[last_arm_idx];
                    match &last_arm.pattern.1 {
                        Pattern::Constructor {
                            enum_name,
                            variant_name,
                            ..
                        } => {
                            let arity = self
                                .checker
                                .arity_for(enum_name, variant_name)
                                .expect(
                                    "Match arm constructor: typechecker should have registered the arity",
                                );
                            self.bytecode.push(
                                Byte::new(Instruction::Unpack)
                                    .with_operand_u32(arity as u32),
                            );
                        }
                        Pattern::Wildcard => {
                            // Wildcard arm — POP the
                            // scrutinee.
                            self.bytecode.push_pop();
                        }
                        Pattern::Binding { name } => {
                            // Binding arm — scrutinee already sits at
                            // `payload_base` (shared stack/locals). No
                            // STORE opcode; reverse pass records the
                            // binding slot.
                            let _ = name;
                        }
                    }
                }
            }

            // Step 3.5: For multi-arm groups WITH
            // runtime tests, emit the inner-pattern
            // test chain. This sits between the
            // forward pass (JUMP_IF_MATCH
            // dispatch + scrutinee-consumer) and the
            // reverse pass (binding + body emission).
            //
            // Why this pass is needed: when two or
            // more arms share the same OUTER variant
            // tag but differ on an INNER sub-pattern
            // (e.g. `Result::Ok(Option::Some(v))` vs
            // `Result::Ok(Option::None)`), a single
            // `JUMP_IF_MATCH` on the outer tag can't
            // disambiguate between them — both arms
            // match the outer tag. The inner-pattern
            // test chain adds a second dispatch step
            // (a runtime test on the inner payload)
            // to pick the right arm.
            //
            // Layout for a 3-arm group
            // `[arm_0, arm_1, arm_2]` sharing the
            // outer tag:
            //
            //   [REBIND arm_0_label here]
            //   POP/STORE for arm_0's sub-patterns
            //   JMP → pass_label_0
            //   POP/STORE for arm_1's sub-patterns
            //   JMP → pass_label_1
            //   POP/STORE for arm_2's sub-patterns
            //   (no JMP — pass_label is None)
            //   → arm_2 body (fall-through)
            //   JMP → end_label
            //   [bind pass_label_1 here] arm_1 body
            //   JMP → end_label
            //   [bind pass_label_0 here] arm_0 body
            //   [end_label: RETURN]
            //
            // The REBIND of `arm_0_label` redirects
            // the outer `JUMP_IF_MATCH` (emitted in
            // the forward pass) from landing at the
            // first arm's BODY to landing at the
            // START of the test chain. Each non-last
            // arm's `JMP → pass_label_N` then routes
            // a successful test to the arm's body
            // (bound later in the reverse pass). The
            // last arm's test chain falls through to
            // its body (no JMP needed).
            //
            // Multi-arm groups WITHOUT runtime tests
            // (every sub-pattern is `Wildcard` /
            // `Binding`, no nested `Constructor`) are
            // unaffected — the existing
            // first-arm-wins behavior is preserved.
            // Single-arm groups are also unaffected.
            let mut pass_labels: HashMap<usize, Option<crate::block_builder::Label>> =
                HashMap::new();
            let mut test_chain_first_arms: std::collections::HashSet<usize> =
                std::collections::HashSet::new();
            // All arms that participate in a test chain
            // group . The reverse pass uses
            // this set to decide whether to skip
            // POP/STORE/UNPACK emission in
            // `emit_pattern_binding` — the test chain
            // pass already consumed the values, so the
            // reverse pass should NOT re-emit them.
            // `test_chain_first_arms` (above) only tracks
            // the FIRST arm of each group (for label
            // re-binding); `test_chain_arms` tracks ALL
            // arms in all test chain groups.
            let mut test_chain_arms: std::collections::HashSet<usize> =
                std::collections::HashSet::new();
            // Per-arm binding map populated by
            // `emit_inner_test` for arms in test chain
            // groups. Keyed by arm_idx → name → slot.
            // The reverse pass consults this map to
            // install `self.context.match_bindings` for
            // test chain arms, instead of re-emitting
            // binding code (which would double-pop /
            // double-store the payload values).
            let mut match_bindings_per_arm: HashMap<usize, HashMap<String, u32>> =
                HashMap::new();

            for group in &tag_groups {
                // Only groups with multiple arms AND
                // at least one arm with a runtime test
                // trigger the new test-chain
                // emission.
                if group.arm_indices.len() <= 1 {
                    continue;
                }
                let has_runtime_test = group
                    .arm_indices
                    .iter()
                    .any(|&i| arm_has_runtime_test(&arms[i]));
                if !has_runtime_test {
                    continue;
                }

                let first_arm_idx = group.arm_indices[0];
                let first_arm_label = arm_labels[first_arm_idx]
                    .expect("non-last group's first arm must have a Label");

                // REBIND the first arm's label so the
                // outer JUMP_IF_MATCH lands at the
                // test chain start, not at the arm
                // body. `bind_label` is idempotent —
                // calling it again would re-patch the
                // JUMP_IF_MATCH, which is exactly
                // what we want here.
                bb.bind_label(first_arm_label, self.bytecode.il_mut());
                test_chain_first_arms.insert(first_arm_idx);
                for &arm_idx in &group.arm_indices {
                    test_chain_arms.insert(arm_idx);
                }

                // Emit the test chain for each arm in
                // source order. Every arm gets a
                // `pass_label` JMP to its body —
                // including the last arm in the group.
                // Fall-through after the last arm is
                // only safe when that arm's body is
                // emitted immediately after the test
                // chain (i.e. the group is source-last).
                // With a later tag group (e.g. Ok/Ok
                // then Err), fall-through would land in
                // the wrong body's bytecode.
                for (rank, &arm_idx) in group.arm_indices.iter().enumerate() {
                    let is_last_in_group = rank == group.arm_indices.len() - 1;

                    let pass_label = Some(bb.fresh_label(self.bytecode.il_mut()));

                    // `fail_label` is the NEXT arm's
                    // body label (so the runtime
                    // test can dispatch to the next
                    // arm's test chain on failure).
                    // For the LAST arm in the group
                    // (and for any arm whose NEXT
                    // sibling has no body label —
                    // e.g. it's the last arm of the
                    // entire match and was reached
                    // by fall-through), fall back to
                    // `end_label` so the jump is at
                    // least well-formed (the
                    // placeholder implementation
                    // currently doesn't emit a JMP to
                    // fail_label, but the operand
                    // still needs to be consistent
                    // with the placeholder value).
                    let fail_label = if !is_last_in_group {
                        let next_arm_idx = group.arm_indices[rank + 1];
                        arm_labels[next_arm_idx].unwrap_or(end_label)
                    } else {
                        end_label
                    };

                    pass_labels.insert(arm_idx, pass_label);

                    // Get the arm's payload (only
                    // Constructor arms are candidates
                    // for runtime tests, by the
                    // definition of
                    // `arm_has_runtime_test`).
                    let (enum_name, variant_name, payload) = match &arms[arm_idx].pattern.1 {
                        Pattern::Constructor {
                            enum_name,
                            variant_name,
                            payload,
                            ..
                        } => (*enum_name, *variant_name, payload),
                        _ => continue,
                    };

                    emit_inner_test(
                        arm_idx,
                        &self.checker,
                        enum_name,
                        variant_name,
                        payload,
                        &mut match_bindings_per_arm,
                        &mut self.bytecode,
                        &mut bb,
                        pass_label,
                        fail_label,
                        payload_base,
                    );
                }
            }

            // Step 4-8: emit each arm's binding code
            // and body. We go in REVERSE source order
            // so the bytecode layout is:
            //   [last arm body]
            //   [JMP end (skip remaining)]
            //   [second-to-last arm body]
            //   [JMP end]
            //   ...
            //   [first arm body]
            //
            // Each non-first arm body is preceded by
            // JMP-to-end so it doesn't fall through
            // into the next body.

            // We process arms in reverse order so the
            // LAST arm body comes first in the
            // bytecode, then non-last arms with
            // JMP-to-end after each.
            for i in (0..arms.len()).rev() {
                let arm = &arms[i];
                let is_first = i == 0;

                // If this arm has a pre-allocated
                // `Label` (it's a non-last constructor
                // arm), bind it to the current bytecode
                // position. This patches the
                // JUMP_IF_MATCH placeholder emitted in
                // the forward pass.
                //
                // The `Label` is `Copy`, so the
                // immutable borrow of `arm_labels`
                // ends after this `if let` expression,
                // and the mutable borrow of `bb` (via
                // `bind_label`) starts fresh. No
                // borrow conflict.
                //
                // Exception: for the FIRST arm of a
                // test-chain group, the label was
                // already REBOUND by the test chain
                // pass to the test-chain start. We
                // MUST NOT bind it again here — that
                // would redirect the outer
                // JUMP_IF_MATCH from the test-chain
                // start back to the arm body,
                // bypassing the test chain entirely.
                // The reverse pass for this arm binds
                // `pass_label_0` instead (the
                // forward-fallthrough target emitted
                // by `emit_inner_test`).
                if !test_chain_first_arms.contains(&i)
                    && let Some(label) = arm_labels[i]
                {
                    bb.bind_label(label, self.bytecode.il_mut());
                }

                // For arms in test chain groups,
                // bind the test chain's
                // `pass_label` to the start of
                // this arm's body. Every test-chain
                // arm (including the last) gets a
                // pass_label so dispatch works when
                // another tag group follows.
                if let Some(Some(label)) = pass_labels.get(&i) {
                    bb.bind_label(*label, self.bytecode.il_mut());
                }

                // Per-arm binding slots (`payload_base` = first
                // payload). Payload order follows declaration
                // order; record patterns may list fields in any
                // source order.
                let mut arm_bindings: HashMap<String, u32> = HashMap::new();
                let mut next_slot: u32 = payload_base;
                // Test-chain arms: payload already on stack from
                // the forward pass — use `consume_values = false`
                // to record bindings without re-emitting UNPACK/POP.
                let in_test_chain = test_chain_arms.contains(&i);
                if let Some(bindings) = match_bindings_per_arm.get(&i) {
                    // This arm is in a test chain
                    // group AND the test chain recorded
                    // bindings (Wildcard/Binding
                    // sub-patterns at the OUTER level).
                    // Use the recorded bindings and skip
                    // the reverse-pass binding code
                    // entirely.
                    arm_bindings = bindings.clone();
                } else if in_test_chain {
                    // Test chain arm without recorded
                    // bindings — the test chain emitted
                    // JUMP_IF_MATCH for nested
                    // Constructor sub-patterns (no
                    // STORE). Walk the pattern to RECORD
                    // the bindings in `arm_bindings`
                    // (the body needs them for
                    // `Identifier` lookups), but with
                    // `consume_values = false` so we
                    // don't re-emit the bytecode (the
                    // test chain handled the values).
                    match &arm.pattern.1 {
                        Pattern::Binding { name } => {
                            arm_bindings.insert(name.to_string(), payload_base);
                        }
                        Pattern::Constructor {
                            enum_name,
                            variant_name,
                            ..
                        } => {
                            // Test-chain arm: the test
                            // chain pass already emitted
                            // POP / STORE / JUMP_IF_MATCH
                            // for the OUTER level. Walk
                            // the pattern with
                            // `consume_values = false` to
                            // RECORD the bindings (the
                            // body needs them for
                            // `Identifier` lookups) but
                            // skip the redundant bytecode
                            // emission. The function
                            // handles Tuple (UNPACK skip
                            // + sub-pattern walk) and
                            // Record (decl-order walk +
                            // sub-pattern walk) internally.
                            let decl_order =
                                self.checker.payload_tys_for(enum_name, variant_name);
                            emit_pattern_binding(
                                &self.checker,
                                &mut arm_bindings,
                                &mut next_slot,
                                &arm.pattern.1,
                                &decl_order,
                                &mut self.bytecode,
                                false,
                                true, // is_outer = true (forward pass handled UNPACK/JUMP_IF_MATCH)
                            );
                        }
                        Pattern::Wildcard => {}
                    }
                } else {
                    // Not in a test chain: emit binding
                    // code at the outer level (consume
                    // the values via POP/STORE/UNPACK).
                    match &arm.pattern.1 {
                        Pattern::Binding { name } => {
                            // Binding arm: the forward pass
                            // already emitted STORE at
                            // `payload_base` for the
                            // scrutinee. Record the binding
                            // here so the body's
                            // `Identifier` lookup finds it.
                            arm_bindings.insert(name.to_string(), payload_base);
                        }
                        Pattern::Constructor {
                            enum_name,
                            variant_name,
                            ..
                        } => {
                            // Non-test-chain arm: emit full
                            // binding code at the outer
                            // level (consume the values via
                            // POP/STORE/UNPACK). The
                            // function handles Tuple (emit
                            // UNPACK + sub-pattern walk) and
                            // Record (decl-order walk + per-
                            // field recursion — including
                            // unbounded-depth nested record
                            // patterns) internally.
                            let decl_order =
                                self.checker.payload_tys_for(enum_name, variant_name);
                            emit_pattern_binding(
                                &self.checker,
                                &mut arm_bindings,
                                &mut next_slot,
                                &arm.pattern.1,
                                &decl_order,
                                &mut self.bytecode,
                                true,
                                true, // is_outer = true (forward pass handled UNPACK/JUMP_IF_MATCH)
                            );
                        }
                        Pattern::Wildcard => {
                            // No bindings — the forward pass
                            // already emitted POP for the
                            // scrutinee.
                        }
                    }
                } // close `else` for test chain arms

                // Install this arm's bindings on top of any enclosing
                // match so nested `match` bodies can still load outer
                // pattern names. Inner names shadow.
                let saved_bindings = self.push_match_bindings(arm_bindings);
                let binding_slots: Vec<(String, u32)> = self
                    .context
                    .match_bindings
                    .as_ref()
                    .map(|m| m.iter().map(|(n, s)| (n.clone(), *s)).collect())
                    .unwrap_or_default();
                let max_binding_slot = binding_slots.iter().map(|(_, s)| *s).max();
                for (name, slot) in &binding_slots {
                    self.record_debug_local(name, *slot);
                }
                // JumpIfMatch/Unpack leave payloads at these slots via
                // stack/locals overlap. Reserve them in `variables` so
                // arm-body temps (`alloc_temp_slot`) cannot STORE over
                // the bindings.
                if let Some(max_slot) = max_binding_slot {
                    while (self.context.variables.len() as u32) <= max_slot {
                        let pad = format!("__match{}", self.context.variables.len());
                        let _ = self.context.variables.intern(pad);
                    }
                }

                // Per-arm binding types override the flat
                // `codegen_var_types` side-table so Access on
                // a reused binding name (`p.y` vs `p.h`) sees
                // this arm's payload type, not the last arm's.
                let mut arm_binding_tys = HashMap::new();
                collect_pattern_binding_types(
                    &self.checker,
                    &arm.pattern.1,
                    &mut arm_binding_tys,
                );
                if let Pattern::Binding { name } = &arm.pattern.1 {
                    if let Some(ty) = self.checker.codegen_var_type(name) {
                        arm_binding_tys.insert(name.to_string(), ty.clone());
                    }
                }
                self.mono_codegen_var_types.push(arm_binding_tys);

                // Emit the arm body unless it is the sole bound name
                // (`Ok(x) => x`): JumpIfMatch already left the payload
                // on the stack at the binding slot.
                if !Self::match_arm_body_is_identity_binding(&arm.pattern.1, &arm.body) {
                    if self.match_tail_call {
                        let mut arm_bc = CodeBuf::new();
                        if !self.try_emit_tail_call_expr(&arm.body, &mut arm_bc) {
                            arm_bc = self.do_compile(&arm.body);
                        }
                        self.bytecode.append(&mut arm_bc);
                    } else {
                        let mut body_bc = self.do_compile(&arm.body);
                        self.bytecode.append(&mut body_bc);
                    }
                }

                self.mono_codegen_var_types.pop();

                self.context.match_bindings = saved_bindings;

                // For non-first arms, emit a
                // JMP-to-end placeholder targeting
                // `end_label`. This is patched when we
                // bind `end_label` below.
                if !is_first {
                    bb.emit_jump_to(
                        end_label,
                        BbJumpKind::Unconditional,
                        self.bytecode.il_mut(),
                    );
                }
            }

            // Value-join bind so fuse-select / invert-guard do not eat the
            // match (replaces the dummy DUPLICATE;POP barrier). Omitted when
            // StorePop consumes the match immediately.
            if self.suppress_match_fusion_barrier {
                bb.bind_label(end_label, self.bytecode.il_mut());
            } else {
                bb.bind_join_label(end_label, self.bytecode.il_mut());
            }

            // Validate: every label that had a
            // pending jump is bound.
        }
        bytecode
    }
}
