//! Feature-agnostic synchronization primitives
//!
//! In async mode, re-exports `tokio::sync` types.  In sync mode, provides `std::sync` wrappers
//! with a matching API.

#[cfg(feature = "sync")]
mod sync_impl;

#[cfg(feature = "async")]
#[allow(unused_imports)]
pub(crate) use tokio::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

#[cfg(feature = "sync")]
#[allow(unused_imports)]
pub(crate) use std::sync::{MutexGuard, RwLockReadGuard, RwLockWriteGuard};
#[cfg(feature = "sync")]
#[allow(unused_imports)]
pub(crate) use sync_impl::{Mutex, RwLock};

/// Either an owned read guard or a borrowed write guard on an `RwLock`.
///
/// The guards are only held, never accessed.
#[derive(Debug)]
pub(crate) enum RwLockEitherGuard<'a, T> {
    /// Owned read guard.
    Read(#[allow(dead_code)] RwLockReadGuard<'a, T>),

    /// Borrowed write guard.
    Write(#[allow(dead_code)] &'a RwLockWriteGuard<'a, T>),
}

impl<'a, T> From<RwLockReadGuard<'a, T>> for RwLockEitherGuard<'a, T> {
    fn from(guard: RwLockReadGuard<'a, T>) -> Self {
        RwLockEitherGuard::Read(guard)
    }
}

impl<'a, T> From<&'a RwLockWriteGuard<'a, T>> for RwLockEitherGuard<'a, T> {
    fn from(guard: &'a RwLockWriteGuard<'a, T>) -> Self {
        RwLockEitherGuard::Write(guard)
    }
}
