//! Feature-agnostic synchronization primitives
//!
//! In async mode, re-exports `tokio::sync` types.  In sync mode, re-exports `parking_lot` types.

#[cfg(feature = "async")]
mod async_impl;
#[cfg(feature = "sync")]
mod sync_impl;

#[cfg(feature = "async")]
#[allow(unused_imports)]
pub(crate) use tokio::sync::{
    Mutex, MutexGuard, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock, RwLockReadGuard,
    RwLockWriteGuard,
};

#[cfg(feature = "async")]
#[allow(unused_imports)]
pub(crate) use async_impl::OwnedRwLock;

#[cfg(feature = "sync")]
#[allow(unused_imports)]
pub(crate) use parking_lot::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

#[cfg(feature = "sync")]
#[allow(unused_imports)]
pub(crate) use sync_impl::{OwnedRwLock, OwnedRwLockReadGuard, OwnedRwLockWriteGuard};

/// Either an owned read guard or a borrowed write guard on an [`OwnedRwLock`].
///
/// The guards are only held, never accessed.
#[derive(Debug)]
pub(crate) enum RwLockEitherGuard<'a, T> {
    /// Owned read guard.
    Read(#[allow(dead_code)] OwnedRwLockReadGuard<T>),

    /// Borrowed write guard.
    Write(#[allow(dead_code)] &'a OwnedRwLockWriteGuard<T>),
}

impl<T> From<OwnedRwLockReadGuard<T>> for RwLockEitherGuard<'_, T> {
    fn from(guard: OwnedRwLockReadGuard<T>) -> Self {
        RwLockEitherGuard::Read(guard)
    }
}

impl<'a, T> From<&'a OwnedRwLockWriteGuard<T>> for RwLockEitherGuard<'a, T> {
    fn from(guard: &'a OwnedRwLockWriteGuard<T>) -> Self {
        RwLockEitherGuard::Write(guard)
    }
}
