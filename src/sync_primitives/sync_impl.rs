//! Wrappers around `parking_lot` lock types, where necessary.

use std::sync::Arc;

/// Guard for read access to an [`OwnedRwLock`], to which it has a strong reference.
pub(crate) type OwnedRwLockReadGuard<T> =
    parking_lot::ArcRwLockReadGuard<parking_lot::RawRwLock, T>;

/// Guard for write access to an [`OwnedRwLock`], to which it has a strong reference.
pub(crate) type OwnedRwLockWriteGuard<T> =
    parking_lot::ArcRwLockWriteGuard<parking_lot::RawRwLock, T>;

/// Read-write lock whose guards own the lock, so a guard can outlive its owner.
#[derive(Debug, Default)]
pub(crate) struct OwnedRwLock<T>(Arc<parking_lot::RwLock<T>>);

impl<T> Clone for OwnedRwLock<T> {
    fn clone(&self) -> Self {
        OwnedRwLock(Arc::clone(&self.0))
    }
}

impl<T> OwnedRwLock<T> {
    /// Acquire an owned read lock.
    pub fn read(&self) -> OwnedRwLockReadGuard<T> {
        self.0.read_arc()
    }

    /// Acquire an owned write lock.
    pub fn write(&self) -> OwnedRwLockWriteGuard<T> {
        self.0.write_arc()
    }
}
