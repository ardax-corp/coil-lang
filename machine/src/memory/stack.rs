//! Capacity-backed operand stack with an explicit cursor (`tell` / `seek`).
//!
//! Capacity is chosen per program from recursion-depth analysis (see
//! `compiler::typechecking::stack_bound`). Slots are pre-filled so hot-path
//! indexing stays `promise!` + `get_unchecked`, matching the old fixed array.

use std::ops::{Index, IndexMut, Range};

use common::promise;

pub struct Stack<T: Default> {
    stack: Vec<T>,
    cursor: usize,
}

impl<T: Default + Copy> Default for Stack<T> {
    fn default() -> Self {
        Self::with_capacity(crate::DEFAULT_OPERAND_STACK_SLOTS)
    }
}

impl<T: Default + Copy> Stack<T> {
    /// Pre-allocate `cap` slots (minimum 1).
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        let cap = cap.max(1);
        Self {
            stack: std::iter::repeat_with(T::default).take(cap).collect(),
            cursor: 0,
        }
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.stack.len()
    }

    /// Extend the backing storage to `cap` slots; the cursor and every slot
    /// keep their values. Never shrinks.
    pub fn grow_to(&mut self, cap: usize) {
        if cap > self.stack.len() {
            self.stack.resize_with(cap, T::default);
        }
    }

    #[inline]
    pub fn pop(&mut self) -> T {
        promise!(self.cursor > 0);
        self.cursor -= 1;
        // SAFETY: cursor was in `1..=capacity` before decrement.
        unsafe { *self.stack.get_unchecked(self.cursor) }
    }

    #[inline]
    pub fn push(&mut self, value: T) {
        promise!(self.cursor < self.stack.len());
        // SAFETY: promise! guarantees cursor < capacity.
        unsafe {
            *self.stack.get_unchecked_mut(self.cursor) = value;
        }
        self.cursor += 1;
    }

    #[inline]
    pub fn peek(&self) -> &T {
        promise!(self.cursor > 0);
        // SAFETY: cursor > 0 and ≤ capacity.
        unsafe { self.stack.get_unchecked(self.cursor - 1) }
    }

    #[inline]
    pub fn seek(&mut self, idx: usize) {
        promise!(idx <= self.stack.len());
        self.cursor = idx;
    }

    #[inline]
    pub fn top(&mut self) -> &mut T {
        promise!(self.cursor > 0);
        // SAFETY: cursor > 0 and ≤ capacity.
        unsafe { self.stack.get_unchecked_mut(self.cursor - 1) }
    }

    /// Duplicate TOS without going through `peek` + `push`.
    #[inline]
    pub fn duplicate(&mut self) {
        promise!(self.cursor > 0);
        promise!(self.cursor < self.stack.len());
        // SAFETY: both indices are in-bounds by the promises above.
        unsafe {
            let v = *self.stack.get_unchecked(self.cursor - 1);
            *self.stack.get_unchecked_mut(self.cursor) = v;
        }
        self.cursor += 1;
    }

    /// Copy `len` values from `src` to `dst` (forward; caller ensures non-overlap or `dst <= src`).
    #[inline]
    pub fn copy_slots(&mut self, dst: usize, src: usize, len: usize) {
        let cap = self.stack.len();
        promise!(dst + len <= cap);
        promise!(src + len <= cap);
        if dst == src || len == 0 {
            return;
        }
        // SAFETY: ranges fit in the buffer.
        unsafe {
            let ptr = self.stack.as_mut_ptr();
            std::ptr::copy(ptr.add(src), ptr.add(dst), len);
        }
    }

    #[inline]
    pub fn tell(&self) -> usize {
        self.cursor
    }

    #[inline]
    pub fn as_slice(&self) -> &[T] {
        &self.stack[..self.cursor]
    }

    /// The top `n` operands in push order (bottom-to-top). Lets aggregate
    /// builders read a whole argument window instead of popping one at a time;
    /// callers must clamp `n` to [`Self::tell`].
    #[inline]
    pub fn top_window(&self, n: usize) -> &[T] {
        promise!(n <= self.cursor);
        // SAFETY: `n <= cursor <= capacity`, and every slot is pre-filled.
        unsafe { self.stack.get_unchecked(self.cursor - n..self.cursor) }
    }

    /// Full backing storage (including slots below the cursor).
    /// Used by the GC to root live locals that share the operand stack.
    #[inline]
    pub fn buffer(&self) -> &[T] {
        &self.stack
    }
}

impl<T: Default> Index<Range<usize>> for Stack<T> {
    type Output = [T];

    fn index(&self, index: Range<usize>) -> &Self::Output {
        promise!(index.end <= self.stack.len());
        promise!(index.start <= index.end);
        // SAFETY: range is within the buffer.
        unsafe { self.stack.get_unchecked(index) }
    }
}

impl<T: Default + Copy> Index<usize> for Stack<T> {
    type Output = T;
    fn index(&self, index: usize) -> &Self::Output {
        promise!(index < self.stack.len());
        // SAFETY: promise! guarantees index < capacity.
        unsafe { self.stack.get_unchecked(index) }
    }
}

impl<T: Default + Copy> IndexMut<usize> for Stack<T> {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        promise!(index < self.stack.len());
        // SAFETY: promise! guarantees index < capacity.
        unsafe { self.stack.get_unchecked_mut(index) }
    }
}

#[cfg(debug_assertions)]
impl<T: Default + std::fmt::Debug> std::fmt::Debug for Stack<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", &self.stack[..self.cursor])
    }
}

#[cfg(test)]
mod tests {
    use super::Stack;

    #[test]
    fn duplicate_copies_tos_without_changing_original() {
        let mut s = Stack::<i64>::with_capacity(8);
        s.push(7);
        s.duplicate();
        assert_eq!(s.tell(), 2);
        assert_eq!(s.pop(), 7);
        assert_eq!(s.pop(), 7);
    }

    #[test]
    fn top_window_borrows_the_top_operands_in_push_order() {
        let mut s = Stack::<i64>::with_capacity(8);
        s.push(10);
        s.push(20);
        s.push(30);

        assert_eq!(s.top_window(2), &[20, 30]);
        assert_eq!(s.top_window(3), &[10, 20, 30]);
        assert!(s.top_window(0).is_empty());
        // Borrowing must not consume: aggregate builders seek afterwards.
        assert_eq!(s.tell(), 3);
    }

    #[test]
    fn copy_slots_moves_args_toward_frame_base() {
        let mut s = Stack::<i64>::with_capacity(8);
        s.push(10);
        s.push(20);
        s.push(30);
        s.push(40);
        s.copy_slots(0, 2, 2);
        assert_eq!(s[0], 30);
        assert_eq!(s[1], 40);
        assert_eq!(s[2], 30);
        assert_eq!(s[3], 40);
    }

    #[test]
    fn copy_slots_noop_when_dst_equals_src_or_len_zero() {
        let mut s = Stack::<i64>::with_capacity(8);
        s.push(1);
        s.push(2);
        s.copy_slots(0, 0, 2);
        s.copy_slots(0, 1, 0);
        assert_eq!(s[0], 1);
        assert_eq!(s[1], 2);
    }

    #[test]
    fn as_slice_excludes_slots_past_cursor_after_pop() {
        let mut s = Stack::<i64>::with_capacity(8);
        s.push(11);
        s.push(22);
        assert_eq!(s.as_slice(), &[11, 22]);
        let _ = s.pop();
        assert_eq!(s.as_slice(), &[11]);
        assert_eq!(s.buffer()[1], 22);
    }

    #[test]
    fn with_capacity_sets_backing_len() {
        let s = Stack::<i64>::with_capacity(64);
        assert_eq!(s.capacity(), 64);
        assert_eq!(s.tell(), 0);
    }

    #[test]
    fn with_capacity_zero_becomes_one() {
        let s = Stack::<i64>::with_capacity(0);
        assert_eq!(s.capacity(), 1);
    }
}
