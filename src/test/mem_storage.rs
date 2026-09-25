//! In-memory storage driver for tests.

use crate::io_buffers::{IoVector, IoVectorMut};
use crate::storage::drivers::CommonStorageHelper;
use crate::storage::{PreallocateMode, Storage};
use std::fmt::{self, Display, Formatter};
use std::io;
#[cfg(feature = "async")]
use std::mem;
#[cfg(feature = "async")]
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// State shared between a [`MemStorage`] object and the test.
#[derive(Debug, Default)]
pub struct MemStorageState {
    /// Storage contents.
    pub data: Mutex<Vec<u8>>,

    /// While set, writes on `hold_range` are not performed immediately, but entered into `held`.
    #[cfg(feature = "async")]
    pub hold_writes: AtomicBool,

    /// Specifies which writes to hold; hold all writes if `None`.
    #[cfg(feature = "async")]
    pub hold_range: Mutex<Option<Range<u64>>>,

    /// Writes that have been issued, but not yet performed (offset and data).
    #[cfg(feature = "async")]
    pub held: Mutex<Vec<(u64, Vec<u8>)>>,

    /// If set, the next write fails (and its offset is entered into `failed`), independently of
    /// `hold_range`.
    pub fail_next_write: AtomicBool,

    /// Offsets of writes that have failed.
    pub failed: Mutex<Vec<u64>>,
}

impl MemStorageState {
    /// Whether a write of `len` bytes to `offset` is to be held.
    #[cfg(feature = "async")]
    fn should_hold(&self, offset: u64, len: u64) -> bool {
        if !self.hold_writes.load(Ordering::Relaxed) {
            return false;
        }

        self.hold_range
            .lock()
            .unwrap()
            .as_ref()
            .is_none_or(|range| offset < range.end && offset + len > range.start)
    }

    /// Write `buf` to `offset`, growing the storage if necessary.
    fn write(&self, offset: u64, buf: &[u8]) {
        let start = offset as usize;
        let end = start + buf.len();
        let mut data = self.data.lock().unwrap();
        if data.len() < end {
            data.resize(end, 0);
        }
        data[start..end].copy_from_slice(buf);
    }

    /// Perform all held writes, even if their futures have been dropped.
    #[cfg(feature = "async")]
    pub fn complete_held_writes(&self) {
        let held = mem::take(&mut *self.held.lock().unwrap());
        for (offset, data) in held {
            self.write(offset, &data);
        }
    }
}

/// In-memory storage.
///
/// Supports alignment requirements, injecting write errors, a fixed size, and (in async mode)
/// holding back writes, which then remain in flight when their futures are dropped.
#[derive(Debug)]
pub struct MemStorage {
    /// State shared with the test.
    state: Arc<MemStorageState>,

    /// Memory and request alignment.
    align: usize,

    /// If set, resizing fails.
    fixed_size: bool,

    /// Helper for implementing [`Storage`].
    helper: CommonStorageHelper,
}

impl MemStorage {
    /// Create a storage object using the given shared state.
    pub fn new(state: Arc<MemStorageState>, fixed_size: bool) -> Self {
        MemStorage {
            state,
            align: 1,
            fixed_size,
            helper: Default::default(),
        }
    }

    /// Set the memory and request alignment.
    pub fn with_align(mut self, align: usize) -> Self {
        self.align = align;
        self
    }
}

impl Display for MemStorage {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "mem[align={}, fixed_size={:?}]",
            self.align, self.fixed_size
        )
    }
}

#[maybe_async::maybe_async(AFIT)]
impl Storage for MemStorage {
    fn mem_align(&self) -> usize {
        self.align
    }

    fn req_align(&self) -> usize {
        self.align
    }

    fn size(&self) -> io::Result<u64> {
        Ok(self.state.data.lock().unwrap().len() as u64)
    }

    async unsafe fn pure_readv(&self, mut bufv: IoVectorMut<'_>, offset: u64) -> io::Result<()> {
        let data = self.state.data.lock().unwrap();
        let offset = offset as usize;
        let len = bufv.len() as usize;
        let mut bounce = vec![0u8; len];
        let copy_len = data.len().saturating_sub(offset).min(len);
        if copy_len > 0 {
            bounce[..copy_len].copy_from_slice(&data[offset..offset + copy_len]);
        }
        bufv.copy_from_slice(&bounce);
        Ok(())
    }

    async unsafe fn pure_writev(&self, bufv: IoVector<'_>, offset: u64) -> io::Result<()> {
        let mut bounce = vec![0u8; bufv.len() as usize];
        bufv.copy_into_slice(&mut bounce);

        if self.state.fail_next_write.swap(false, Ordering::Relaxed) {
            self.state.failed.lock().unwrap().push(offset);
            return Err(io::Error::other("Injected write error"));
        }

        #[cfg(feature = "async")]
        if self.state.should_hold(offset, bounce.len() as u64) {
            self.state.held.lock().unwrap().push((offset, bounce));
            // We intend to be dropped, so hold indefinitely (while internally,
            // [`MemStorageState::complete_held_writes()`] will write the data eventually)
            return std::future::pending().await;
        }

        self.state.write(offset, &bounce);
        Ok(())
    }

    async fn flush(&self) -> io::Result<()> {
        Ok(())
    }

    async fn sync(&self) -> io::Result<()> {
        Ok(())
    }

    async fn settle_writes(&self) -> io::Result<()> {
        #[cfg(feature = "async")]
        self.state.complete_held_writes();
        Ok(())
    }

    async unsafe fn invalidate_cache(&self) -> io::Result<()> {
        Ok(())
    }

    fn get_storage_helper(&self) -> &CommonStorageHelper {
        &self.helper
    }

    async fn resize(&self, new_size: u64, _prealloc_mode: PreallocateMode) -> io::Result<()> {
        if self.fixed_size {
            return Err(io::Error::other("Resize not supported"));
        }
        self.state.data.lock().unwrap().resize(new_size as usize, 0);
        Ok(())
    }
}
