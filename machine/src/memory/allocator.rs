//! Unused arena allocator sketch (not wired into the live VM).
//! Live headers use the mapped slab in `slab.rs` (docs/internals/heap-identity.md).

// use std::{
//     alloc::Layout, borrow::{Borrow, BorrowMut}, cell::RefCell, marker::PhantomData, ops::{Deref, DerefMut}
// };
//
// use crate::garbage::{GcSized, Rc};
//
// pub struct ArenaAllocated<T>(*mut Rc<T>);
//
// impl<T> ArenaAllocated<T> {
//     pub fn new(ptr: *mut Rc<T>) -> Self {
//         Self(ptr)
//     }
//
//     pub fn eq(lhs: Self, rhs: Self) -> bool {
//         lhs.0.eq(&rhs.0)
//     }
//
//     pub fn ptr(&self) -> *mut Rc<T> {
//         self.0
//     }
// }
//
// impl<T: GcSized> GcSized for ArenaAllocated<T> {
//     fn size(&self) -> usize {
//         self.deref().size()
//     }
// }
//
// impl<T> Deref for ArenaAllocated<T> {
//     type Target = Rc<T>;
//
//     fn deref(&self) -> &Self::Target {
//         unsafe { &*self.0 }
//     }
// }
//
// impl<T> DerefMut for ArenaAllocated<T> {
//     fn deref_mut(&mut self) -> &mut Self::Target {
//         unsafe { &mut *self.0 }
//     }
// }
//
// impl<T> Copy for ArenaAllocated<T> {}
// impl<T> Clone for ArenaAllocated<T> {
//     fn clone(&self) -> Self {
//         *self
//     }
// }
//
// impl<T> Borrow<Rc<T>> for ArenaAllocated<T> {
//     fn borrow(&self) -> &Rc<T> {
//         self.deref()
//     }
// }
//
// impl<T> BorrowMut<Rc<T>> for ArenaAllocated<T> {
//     fn borrow_mut(&mut self) -> &mut Rc<T> {
//         unsafe { self.0.as_mut().expect("Unable to obtain mutable reference") }
//     }
// }
//
// #[derive(Clone)]
// pub struct Chunk<T: GcSized, const B: usize> {
//     data: *mut u8,
//     size: usize,
//     count: usize,
//     layout: Layout,
//     // freed: ArrayVec<usize, B>,
//     _phantom: PhantomData<[T; B]>,
// }
//
// // impl <T: GcSized, const B: usize>Clone for Chunk<T, B> {
// //     fn clone(&self) -> Self {
// //         Self {
// //             clones: self.clones + 1,
// //             data: self.data,
// //             size: self.size,
// //             layout: self.layout,
// //             count: self.count,
// //             _phantom: self._phantom,
// //         }
// //     }
// // }
//
// #[derive(Clone)]
// pub struct Allocator<T: GcSized, const B: usize> {
//     head: std::rc::Rc<RefCell<Chunk<T, B>>>,
// }
//
// impl<T: GcSized, const B: usize> Default for Chunk<T, B> {
//     fn default() -> Self {
//         Self::new()
//     }
// }
//
// impl<T: GcSized, const B: usize> Chunk<T, B> {
//     pub fn new() -> Self {
//         let size = std::mem::size_of::<T>();
//         let align = std::mem::align_of::<T>();
//
//         match std::alloc::Layout::from_size_align(size * B, align) {
//             Ok(layout) => unsafe {
//                 let data = std::alloc::alloc(layout);
//
//
//                 Chunk {
//                     data,
//                     size,
//                     layout,
//                     count: 0,
//                     // freed: ArrayVec::default(),
//                     _phantom: PhantomData,
//                 }
//             },
//             Err(e) => {
//                 panic!("Encountered allocation error: {}", e);
//             }
//         }
//     }
//
//     pub fn alloc(&mut self, value: T) -> ArenaAllocated<T> {
//         let offset = // if likely(self.freed.is_empty()) {
//             (self.count + self.size + self.layout.align() - 1) & !(self.layout.align() - 1);
//         // } else {
//             // *self.freed.pop()
//         // };
//
//         unsafe {
//             let ptr = self.data.add(self.count);
//             self.count = offset;
//             std::ptr::write(ptr as *mut Rc<T>, Rc::new(value));
//
//             ArenaAllocated(ptr as _)
//         }
//     }
//
//     #[inline]
//     pub fn free(&mut self, value: ArenaAllocated<T>) {
//         value.dec();
//     }
// }
//
// impl<T: GcSized, const B: usize> Drop for Chunk<T, B> {
//     fn drop(&mut self) {
//         unsafe {
//             std::alloc::dealloc(self.data, self.layout);
//         }
//     }
// }
//
// impl<T: GcSized, const B: usize> Allocator<T, B> {
//     pub fn default() -> Self {
//         Allocator { head: std::rc::Rc::new(RefCell::new(Chunk::new())) }
//     }
// }
//
// impl<T: GcSized, const B: usize> Allocator<T, B> {
//     #[inline]
//     pub fn alloc(&mut self, value: T) -> ArenaAllocated<T> {
//         (*self.head).borrow_mut().alloc(value)
//     }
//
//     pub fn free(&mut self, value: ArenaAllocated<T>) {
//         if value.dec() == 0 {
//             #[cfg(debug_assertions)]
//             eprintln!("Cleaning: {}", value.ptr().addr());
//             (*self.head).borrow_mut().free(value);
//         }
//     }
// }
//
// #[cfg(test)]
// mod tests {
//     use crate::{Allocator, ArenaAllocated, String, garbage::GcSized};
//
//     #[test]
//     fn test_usage() {
//         let mut allocator: Allocator<String, 4> = Allocator::default();
//         let str = "Hello, World".to_string();
//         let x: ArenaAllocated<String> = allocator.alloc(str.clone().into());
//         let y: ArenaAllocated<String> = allocator.alloc("Hello, Boss!".into());
//
//         assert_eq!(str, x.as_ref().to_string());
//         assert_eq!(str, x.as_ref().to_string());
//         assert_eq!(str, x.as_ref().to_string());
//         assert_eq!(str, x.as_ref().to_string());
//         assert_eq!(str, x.as_ref().to_string());
//         assert_eq!("Hello, Boss!", y.as_ref().to_string());
//         assert_eq!(str, x.as_ref().to_string());
//         assert_eq!(str, x.as_ref().to_string());
//         assert_eq!(str, x.as_ref().to_string());
//
//         assert_eq!(96, x.size());
//     }
// }
