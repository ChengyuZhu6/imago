//! Wrappers around `tokio::sync` lock types, where necessary.

use std::sync::Arc;
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard};

/// Read-write lock whose guards own the lock, so a guard can outlive its owner.
#[derive(Debug, Default)]
pub(crate) struct OwnedRwLock<T>(Arc<tokio::sync::RwLock<T>>);

impl<T> Clone for OwnedRwLock<T> {
    fn clone(&self) -> Self {
        OwnedRwLock(Arc::clone(&self.0))
    }
}

impl<T> OwnedRwLock<T> {
    /// Acquire an owned read lock.
    pub async fn read(&self) -> OwnedRwLockReadGuard<T> {
        Arc::clone(&self.0).read_owned().await
    }

    /// Acquire an owned write lock.
    pub async fn write(&self) -> OwnedRwLockWriteGuard<T> {
        Arc::clone(&self.0).write_owned().await
    }
}
