//! Stack-like vector: inline storage for `N` elements, then heap spill.

use std::ops::{Index, IndexMut};

use crate::{likely, promise, unlikely};

#[derive(Clone)]
pub struct ArrayVec<T: Default, const N: usize> {
    current: usize,
    storage: [T; N],
    expansion: Vec<T>,
}

impl<T: Default, const N: usize> FromIterator<T> for ArrayVec<T, N> {
    fn from_iter<X: IntoIterator<Item = T>>(iter: X) -> Self {
        let mut result = ArrayVec::default();
        iter.into_iter().for_each(|v| result.push(v));

        result
    }
}

impl<T: Default, const N: usize> Default for ArrayVec<T, N> {
    fn default() -> Self {
        Self {
            current: 0,
            storage: std::array::from_fn(|_| T::default()),
            expansion: Vec::default(),
        }
    }
}

impl<T: Default, const N: usize> ArrayVec<T, N> {
    pub fn iter<'iter>(&'iter self) -> ArrayVecIter<'iter, T, N> {
        ArrayVecIter::new(self)
    }

    fn grow(&mut self, cursor: usize) {
        let boundary = self.expansion.len();

        if unlikely(cursor >= boundary) {
            self.expansion.resize_with(boundary + N, T::default);
        }
    }

    #[inline]
    pub fn current(&self) -> &T {
        if likely(self.current < N) {
            promise!(self.current < N);

            &self.storage[self.current]
        } else {
            promise!((self.current - N) < self.expansion.len());
            &self.expansion[self.current - N]
        }
    }

    #[inline]
    pub fn current_mut(&mut self) -> &mut T {
        if likely(self.current < N) {
            promise!(self.current < N);

            &mut self.storage[self.current]
        } else {
            let index = self.current - N;
            self.grow(index);

            promise!(index < self.expansion.len());

            &mut self.expansion[index]
        }
    }

    #[inline]
    pub fn consume(&mut self) {
        self.current += 1;
    }

    #[inline]
    pub fn setup_current_and_advance<F>(&mut self, setup: F)
    where
        F: FnOnce(&mut T),
    {
        setup(self.current_mut());
        self.consume();
    }

    /// Hot CALL helper: rewrite the active frame, then push a fresh one.
    #[inline]
    pub fn rewrite_top_and_push<F, G>(&mut self, rewrite_top: F, setup_new: G)
    where
        F: FnOnce(&mut T),
        G: FnOnce(&mut T),
    {
        rewrite_top(self.get_mut());
        setup_new(self.current_mut());
        self.consume();
    }

    #[inline]
    pub fn seek(&mut self, value: usize) {
        self.current = value;
    }

    #[inline]
    pub fn push(&mut self, value: T) {
        let current = self.current;
        self.current += 1;

        if likely(current < N) {
            promise!(current < N);
            promise!(current < self.storage.len());

            self.storage[current] = value;
        } else {
            // `offset` is an index into `expansion` (0 on first spill).
            let offset = current - N;
            self.grow(offset);

            promise!(offset < self.expansion.len());

            self.expansion[offset] = value;
        }
    }

    #[inline]
    pub fn pop(&mut self) -> &T {
        promise!(self.current > 0);
        promise!(!self.storage.is_empty() || !self.expansion.is_empty());

        self.current -= 1;
        if likely(self.current < N) {
            &self.storage[self.current]
        } else {
            promise!(self.current >= N);
            promise!(self.current - N < self.expansion.len());
            &self.expansion[self.current - N]
        }
    }

    pub fn get(&self) -> &T {
        promise!(self.current > 0);
        let current = self.current - 1;

        if likely(current < N) {
            promise!(current < N);
            promise!(current < self.storage.len());

            &self.storage[current]
        } else {
            promise!(self.expansion.len() > current - N);
            &self.expansion[current - N]
        }
    }

    pub fn get_mut(&mut self) -> &mut T {
        promise!(self.current > 0);
        let current = self.current - 1;

        if likely(current < N) {
            promise!(current < N);
            &mut self.storage[current]
        } else {
            self.grow(current - N);

            promise!(self.expansion.len() > current - N);
            &mut self.expansion[current - N]
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.current
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.current == 0
    }

    #[inline]
    pub fn clear(&mut self) {
        self.current = 0;
    }
}

impl<T: Default, const N: usize> Index<usize> for ArrayVec<T, N> {
    type Output = T;
    fn index(&self, index: usize) -> &Self::Output {
        if likely(index < N) {
            promise!(index < N);
            &self.storage[index]
        } else {
            promise!(index - N < self.expansion.len());
            &self.expansion[index - N]
        }
    }
}

impl<T: Default, const N: usize> IndexMut<usize> for ArrayVec<T, N> {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        self.current = self.current.max(index + 1);
        if likely(index < N) {
            promise!(index < N);
            &mut self.storage[index]
        } else {
            self.grow(index - N);
            promise!(index - N < self.expansion.len());
            &mut self.expansion[index - N]
        }
    }
}

#[cfg(debug_assertions)]
impl<T: Default + std::fmt::Debug, const N: usize> std::fmt::Debug for ArrayVec<T, N> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?}",
            self.storage
                .iter()
                .chain(self.expansion.iter())
                .enumerate()
                .filter_map(|(idx, val)| {
                    if idx >= self.current {
                        return None;
                    }

                    Some(val.to_owned())
                })
                .collect::<Vec<_>>()
        )
    }
}

pub struct ArrayVecIter<'iter, T: Default, const N: usize> {
    cursor: usize,
    value: &'iter ArrayVec<T, N>,
}

impl<'iter, T: Default, const N: usize> ArrayVecIter<'iter, T, N> {
    pub fn seek(&mut self, cursor: usize) {
        self.cursor = cursor;
    }
}

impl<'iter, T: Default, const N: usize> ArrayVecIter<'iter, T, N> {
    pub fn new(value: &'iter ArrayVec<T, N>) -> Self {
        Self { cursor: 0, value }
    }
}

impl<'iter, T: Default, const N: usize> Iterator for ArrayVecIter<'iter, T, N> {
    type Item = &'iter T;
    fn next(&mut self) -> Option<Self::Item> {
        let mut value = None;

        if likely(self.cursor < self.value.len()) {
            promise!(self.cursor < self.value.len());
            value = Some(&self.value[self.cursor]);
        }

        self.cursor += 1;

        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_pop_within_inline_capacity() {
        let mut v = ArrayVec::<i32, 4>::default();
        assert!(v.is_empty());
        v.push(1);
        v.push(2);
        assert_eq!(v.len(), 2);
        assert_eq!(*v.pop(), 2);
        assert_eq!(*v.get(), 1);
    }

    #[test]
    fn spills_to_heap_past_inline_capacity() {
        let mut v = ArrayVec::<i32, 2>::default();
        for i in 0..6 {
            v.push(i);
        }
        assert_eq!(v.len(), 6);
        assert_eq!(v[0], 0);
        assert_eq!(v[5], 5);
        assert_eq!(*v.pop(), 5);
        assert_eq!(v.len(), 5);
    }

    #[test]
    fn index_mut_grows_len() {
        let mut v = ArrayVec::<i32, 2>::default();
        v[3] = 99;
        assert_eq!(v.len(), 4);
        assert_eq!(v[3], 99);
    }

    #[test]
    fn from_iter_and_iter_visit_all() {
        let v: ArrayVec<i32, 2> = (0..5).collect();
        let collected: Vec<_> = v.iter().copied().collect();
        assert_eq!(collected, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn clear_resets_len_but_keeps_values_addressable() {
        let mut v = ArrayVec::<i32, 2>::default();
        v.push(7);
        v.push(8);
        v.clear();
        assert!(v.is_empty());
        v.push(1);
        assert_eq!(*v.get(), 1);
    }

    #[test]
    fn setup_current_and_advance_writes_slot() {
        let mut v = ArrayVec::<i32, 2>::default();
        v.setup_current_and_advance(|slot| *slot = 42);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0], 42);
    }

    #[test]
    fn seek_repositions_current_cursor() {
        let mut v = ArrayVec::<i32, 4>::default();
        v.push(1);
        v.push(2);
        v.seek(0);
        assert_eq!(*v.current(), 1);
        *v.current_mut() = 9;
        assert_eq!(v[0], 9);
    }

    #[test]
    fn iter_seek_skips_prefix() {
        let v: ArrayVec<i32, 2> = (0..4).collect();
        let mut it = v.iter();
        it.seek(2);
        assert_eq!(it.next(), Some(&2));
        assert_eq!(it.next(), Some(&3));
        assert_eq!(it.next(), None);
    }
}
