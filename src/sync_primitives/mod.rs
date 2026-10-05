//! Feature-agnostic synchronization primitives
//!
//! In async mode, re-exports `tokio::sync` types.  In sync mode, re-exports `parking_lot` types.

#[cfg(feature = "async")]
#[allow(unused_imports)]
pub(crate) use tokio::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

#[cfg(feature = "sync")]
#[allow(unused_imports)]
pub(crate) use parking_lot::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

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
