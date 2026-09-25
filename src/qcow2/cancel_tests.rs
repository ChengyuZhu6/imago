//! Tests for requests whose futures are dropped while the storage still has them in flight, and
//! for requests racing each other.
//!
//! Dropping a future does not necessarily stop the I/O it has issued (e.g. with io_uring, the
//! kernel will still perform it), so such requests may complete at any later point.

use super::*;
use crate::test::mem_storage::{MemStorage, MemStorageState};
use crate::{DenyImplicitOpenGate, Mapping};
use futures::FutureExt;
use std::future::Future;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};

/// Create a 512-byte-cluster image of the given size on a [`MemStorage`] with the given state.
async fn create_image(state: &Arc<MemStorageState>, size: u64) -> FormatAccess<MemStorage> {
    let qcow2 = Qcow2::<MemStorage>::create_builder(MemStorage::new(Arc::clone(state), false))
        .cluster_size(512)
        .size(size)
        .create_open(DenyImplicitOpenGate::default(), |image| {
            Ok(Qcow2::<MemStorage>::builder(image)
                .backing(None)
                .write(true))
        })
        .await
        .unwrap();

    FormatAccess::new(qcow2)
}

/// Return the host offset `offset` is mapped to; panic if it is not mapped to raw data.
async fn host_offset(image: &FormatAccess<MemStorage>, offset: u64) -> u64 {
    match image.get_mapping(offset, 1).await.unwrap().0 {
        Mapping::Raw { offset, .. } => offset,
        mapping => panic!("Expected raw data mapping at {offset:#x}, got {mapping:?}"),
    }
}

/// A write whose future is dropped while it is still in flight must not overwrite data in a
/// cluster that has been freed (by discarding it) and reallocated in the meantime.
///
/// `discard_to_zero()` takes a mutable reference so no concurrent request can hold a mapping to
/// the discarded cluster, but that does not cover requests whose futures have been dropped.
#[tokio::test]
async fn test_dropped_write_vs_discard_and_realloc() {
    let state = Arc::new(MemStorageState::default());
    let mut image = create_image(&state, 64 * 1024).await;

    // Allocate guest cluster 0
    image.write(&[0x11u8; 512][..], 0).await.unwrap();
    let freed_host_offset = host_offset(&image, 0).await;

    // Issue a write to guest cluster 0, and drop its future while it is still in flight
    state.hold_writes.store(true, Ordering::Relaxed);
    assert!(image.write(&[0x22u8; 512][..], 0).now_or_never().is_none());
    assert_eq!(state.held.lock().unwrap().len(), 1);
    state.hold_writes.store(false, Ordering::Relaxed);

    // Free guest cluster 0’s host cluster
    image.discard_to_zero(0, 512).await.unwrap();

    // Allocate guest cluster 1, which must reuse that host cluster for this test to be meaningful
    let new_data = [0x33u8; 512];
    image.write(&new_data[..], 512).await.unwrap();
    assert_eq!(host_offset(&image, 512).await, freed_host_offset);

    // Now the dropped write completes (unless it has already been settled before)
    state.complete_held_writes();

    let mut buf = [0u8; 512];
    image.read(&mut buf[..], 512).await.unwrap();
    assert!(
        buf == new_data,
        "Expected {:#04x} bytes, read {:#04x} bytes",
        new_data[0],
        buf[0],
    );
}

/// A metadata table entry write whose future is dropped while it is still in flight must not
/// overwrite data in a cluster that has been freed (by replacing the table) and reallocated in the
/// meantime.
///
/// Here, the table is the L1 table: Dropping a guest write while its L1 entry write is in flight
/// releases the L1 table lock, so growing the image can then replace the L1 table, freeing the old
/// one.
#[tokio::test]
async fn test_dropped_l1_entry_write_vs_l1_growth_and_realloc() {
    let state = Arc::new(MemStorageState::default());
    let mut image = create_image(&state, 64 * 1024).await;

    // L1 table offset from the image header
    let l1_offset = u64::from_be_bytes(state.data.lock().unwrap()[40..48].try_into().unwrap());
    assert_ne!(l1_offset, 0);

    // Issue a write to guest cluster 0, which allocates an L2 table, and drop its future while its
    // L1 entry write is still in flight
    *state.hold_range.lock().unwrap() = Some(l1_offset..(l1_offset + 512));
    state.hold_writes.store(true, Ordering::Relaxed);
    assert!(image.write(&[0x22u8; 512][..], 0).now_or_never().is_none());
    assert_eq!(state.held.lock().unwrap().len(), 1);
    state.hold_writes.store(false, Ordering::Relaxed);
    *state.hold_range.lock().unwrap() = None;

    // Grow the L1 table beyond its first cluster (64 entries of 32 KiB each), freeing the old one
    image
        .resize(4 * 1024 * 1024, PreallocateMode::None)
        .await
        .unwrap();

    // Allocate guest cluster 1 (whose L2 table exists already), which must reuse the old L1
    // table’s cluster for this test to be meaningful
    let new_data = [0x33u8; 512];
    image.write(&new_data[..], 512).await.unwrap();
    assert_eq!(host_offset(&image, 512).await, l1_offset);

    // Now the dropped L1 entry write completes (unless it has already been settled before)
    state.complete_held_writes();

    let mut buf = [0u8; 512];
    image.read(&mut buf[..], 512).await.unwrap();
    assert!(
        buf == new_data,
        "Expected {:#04x} bytes, read {:#04x} bytes",
        new_data[0],
        buf[0],
    );
}

/// Return the cluster-aligned host offset (assuming a cluster size of 512) of the first held
/// write, which must exist.
fn first_held_cluster(state: &MemStorageState) -> u64 {
    state
        .held
        .lock()
        .unwrap()
        .first()
        .expect("Expected a held write")
        .0
        & !511
}

/// If a write allocating a new data cluster is cancelled while copying data into it (COW), the
/// cluster must be freed, not leaked.
#[tokio::test]
async fn test_cancelled_cow_frees_cluster() {
    let state = Arc::new(MemStorageState::default());
    let image = create_image(&state, 64 * 1024).await;

    // Allocate guest cluster 0 (so the L2 table exists)
    image.write(&[0x11u8; 512][..], 0).await.unwrap();

    // Partially write guest cluster 1, so the rest of it must be COW-ed into the new cluster, and
    // drop the future while that write is in flight
    state.hold_writes.store(true, Ordering::Relaxed);
    assert!(image
        .write(&[0x22u8; 256][..], 512)
        .now_or_never()
        .is_none());
    state.hold_writes.store(false, Ordering::Relaxed);
    let cancelled_cluster = first_held_cluster(&state);

    // Guest cluster 1 must not be mapped
    let mapping = image.get_mapping(512, 512).await.unwrap().0;
    assert!(
        !matches!(mapping, Mapping::Raw { .. }),
        "Cancelled write left a mapping: {mapping:?}"
    );

    // Flushing frees the clusters from the cancelled write (after settling its writes)
    image.flush().await.unwrap();

    // So the next allocation can reuse it
    image.write(&[0x33u8; 512][..], 1024).await.unwrap();
    assert_eq!(host_offset(&image, 1024).await, cancelled_cluster);
}

/// If a write allocating a new data cluster fails while copying data into it (COW), the cluster
/// must be freed, not leaked.
#[tokio::test]
async fn test_failed_cow_frees_cluster() {
    let state = Arc::new(MemStorageState::default());
    let image = create_image(&state, 64 * 1024).await;

    // Allocate guest cluster 0 (so the L2 table exists)
    image.write(&[0x11u8; 512][..], 0).await.unwrap();

    // Partially write guest cluster 1, so the rest of it must be COW-ed into the new cluster, and
    // let that write fail
    state.fail_next_write.store(true, Ordering::Relaxed);
    image.write(&[0x22u8; 256][..], 512).await.unwrap_err();
    let failed_cluster = {
        let failed = state.failed.lock().unwrap();
        assert_eq!(failed.len(), 1, "Expected exactly one failed write");
        failed[0] & !511
    };

    // The failed write has freed the cluster, so the next allocation can reuse it
    image.write(&[0x33u8; 512][..], 1024).await.unwrap();
    assert_eq!(host_offset(&image, 1024).await, failed_cluster);
}

/// Open the image on a [`MemStorage`] with the given state.
async fn open_image(state: &Arc<MemStorageState>) -> FormatAccess<MemStorage> {
    let qcow2 = Qcow2::<MemStorage>::builder(MemStorage::new(Arc::clone(state), false))
        .backing(None)
        .write(true)
        .open(DenyImplicitOpenGate::default())
        .await
        .unwrap();

    FormatAccess::new(qcow2)
}

/// Read a big-endian 64-bit value from the storage.
fn read_u64(state: &MemStorageState, offset: u64) -> u64 {
    let data = state.data.lock().unwrap();
    let offset = offset as usize;
    u64::from_be_bytes(data[offset..offset + 8].try_into().unwrap())
}

/// Create a 512-byte-cluster image where the guest cluster at `guest_offset` is a compressed
/// cluster containing `data`. Return the image and the compressed data's host offset.
///
/// imago cannot write compressed clusters, so put the compressed data and the compressed L2 entry
/// directly into the image file.
async fn create_compressed_image(
    state: &Arc<MemStorageState>,
    guest_offset: u64,
    data: &[u8; 512],
) -> (FormatAccess<MemStorage>, u64) {
    let image = create_image(state, 64 * 1024).await;

    // Allocate a host cluster
    image.write(&[0x11u8; 512][..], guest_offset).await.unwrap();
    let compressed_host_offset = host_offset(&image, guest_offset).await;
    image.flush().await.unwrap();
    drop(image);

    // Turn the guest cluster into a compressed cluster: Put compressed data into its host cluster,
    // and replace its L2 entry by a compressed cluster descriptor. With 512-byte clusters, the
    // descriptor has 61 offset bits, followed by the number of additional sectors (0 here).
    let compressed = miniz_oxide::deflate::compress_to_vec(data, 6);
    assert!(compressed.len() <= 512);
    state.write(compressed_host_offset, &compressed);

    let l1_offset = read_u64(state, 40);
    let l2_offset = read_u64(state, l1_offset) & 0x00ff_ffff_ffff_fe00;
    let l2_entry = (1u64 << 62) | compressed_host_offset;
    state.write(l2_offset + 8, &l2_entry.to_be_bytes());

    let image = open_image(state).await;
    let mut buf = [0u8; 512];

    // Check that it’s a compressed cluster
    let mapping = image.get_mapping(guest_offset, 512).await.unwrap();
    assert!(matches!(mapping.0, Mapping::Special { .. }));
    assert_eq!(mapping.1, 512);

    // And verify the content
    image.read(&mut buf[..], guest_offset).await.unwrap();
    assert!(buf == *data, "Compressed cluster not read back correctly");

    (image, compressed_host_offset)
}

/// Reading a compressed cluster while a concurrent write replaces it (freeing the compressed data)
/// must not return data from another cluster that has reused the freed host cluster.
#[tokio::test]
async fn test_compressed_read_vs_cow() {
    // The offset of the cluster where everything happens
    let guest_offset = 512;

    let state = Arc::new(MemStorageState::default());
    let data = [0x44u8; 512];
    let (image, compressed_host_offset) =
        create_compressed_image(&state, guest_offset, &data).await;
    let mut buf = [0u8; 512];

    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);

    // Start reading the compressed cluster, keeping the read of the compressed data pending
    state.hold_reads.store(true, Ordering::Relaxed);
    let mut read_buf = [0u8; 512];
    let mut read = Box::pin(image.read(&mut read_buf[..], guest_offset));
    assert!(read.as_mut().poll(&mut cx).is_pending());
    assert_eq!(*state.held_reads.lock().unwrap(), [compressed_host_offset]);
    state.hold_reads.store(false, Ordering::Relaxed);

    // Concurrently overwrite guest cluster 1, replacing the compressed mapping, which frees the
    // compressed data’s host cluster
    let mut cow_write = Box::pin(image.write(&[0x55u8; 512][..], guest_offset));
    if cow_write.as_mut().poll(&mut cx).is_ready() {
        // The write did not wait for the read, so the freed host cluster can now be reused
        image.write(&[0x66u8; 512][..], 1024).await.unwrap();
        assert_eq!(host_offset(&image, 1024).await, compressed_host_offset);
    }

    // Let the read of the compressed data complete
    state.release_reads.store(true, Ordering::Relaxed);
    let Poll::Ready(result) = read.as_mut().poll(&mut cx) else {
        panic!("Read did not complete");
    };
    result.unwrap();
    drop(read);
    assert!(
        read_buf == data,
        "Expected {:#04x} bytes, read {:#04x} bytes",
        data[0],
        read_buf[0],
    );

    cow_write.await.unwrap();
    image.read(&mut buf[..], guest_offset).await.unwrap();
    assert!(buf == [0x55u8; 512], "COW write not visible");
}

/// If a concurrent write replaces a compressed cluster between `get_mapping()` returning a special
/// mapping and reading it via `readv_special()`, the read must return the new data (and not fail
/// because the range is no longer special).
#[tokio::test]
async fn test_compressed_read_vs_mapping_change() {
    let guest_offset = 512;

    let state = Arc::new(MemStorageState::default());
    let (image, _) = create_compressed_image(&state, guest_offset, &[0x44u8; 512]).await;

    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);

    // Partially overwrite guest cluster 1, so the compressed data must be COW-ed; keep that write
    // pending on a read while it holds the L2 table’s write lock (the first read it does while
    // holding it is for allocating the new cluster, i.e. loading a refblock)
    state.hold_reads.store(true, Ordering::Relaxed);
    let mut cow_write = Box::pin(image.write(&[0x55u8; 256][..], guest_offset));
    assert!(cow_write.as_mut().poll(&mut cx).is_pending());
    assert!(!state.held_reads.lock().unwrap().is_empty());
    state.hold_reads.store(false, Ordering::Relaxed);

    // Start reading guest cluster 1: `get_mapping()` returns a special mapping, then reading it
    // must wait for the write
    let mut read_buf = [0u8; 512];
    let mut read = Box::pin(image.read(&mut read_buf[..], guest_offset));
    assert!(read.as_mut().poll(&mut cx).is_pending());

    // Let the write complete, replacing the compressed mapping
    state.release_reads.store(true, Ordering::Relaxed);
    let Poll::Ready(result) = cow_write.as_mut().poll(&mut cx) else {
        panic!("Write did not complete");
    };
    result.unwrap();

    // The read must see the new mapping and data
    read.await.unwrap();
    assert!(
        read_buf[..256] == [0x55u8; 256] && read_buf[256..] == [0x44u8; 256],
        "Expected new data (0x55, then 0x44), read {:#04x}, then {:#04x}",
        read_buf[0],
        read_buf[256],
    );
}
