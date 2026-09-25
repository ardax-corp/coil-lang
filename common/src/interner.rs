//! Deduping interner backed by a small inline `ArrayVec`.

use std::{collections::HashMap, hash::Hash};

use crate::{ArrayVec, ArrayVecIter, likely, promise};

#[derive(Default, Clone)]
pub struct Interner<T: Default + Eq> {
    storage: ArrayVec<T, 64>,
    hash: HashMap<T, usize>,
}

impl<T: Default + Hash + Eq + Clone> Interner<T> {
    pub fn intern(&mut self, value: T) -> usize {
        if let Some(position) = self.hash.get(&value) {
            *position
        } else {
            let position = self.storage.len();
            self.storage.push(value.to_owned());
            self.hash.insert(value, position);

            position
        }
    }

    pub fn contains(&self, value: &T) -> bool {
        likely(self.hash.contains_key(value))
    }

    pub fn resolve(&self, key: usize) -> &T {
        promise!(key < self.storage.len());

        &self.storage[key]
    }

    pub fn key(&self, value: &T) -> Option<usize> {
        self.hash.get(value).copied()
    }

    pub fn iter(&self) -> ArrayVecIter<'_, T, 64> {
        self.storage.iter()
    }

    pub fn len(&self) -> usize {
        self.storage.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(debug_assertions)]
impl<T: std::fmt::Debug + Default + Eq> std::fmt::Debug for Interner<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.hash)
    }
}

#[cfg(test)]
mod tests {
    use crate::Interner;

    #[test]
    fn test_interning() {
        let mut interner = Interner::default();
        assert!(interner.is_empty());

        assert_eq!(0, interner.intern("Hello"));
        assert!(!interner.is_empty());
        assert_eq!(interner.len(), 1);
        assert_eq!(1, interner.intern("World"));
        assert_eq!(0, interner.intern("Hello"));
        assert_eq!(1, interner.intern("World"));

        assert_eq!("Hello", *interner.resolve(0));
        assert_eq!("World", *interner.resolve(1));

        assert!(interner.key(&"Hello").is_some());
        assert!(interner.key(&"World").is_some());

        assert_eq!(0, interner.key(&"Hello").unwrap());
        assert_eq!(1, interner.key(&"World").unwrap());
    }
}
