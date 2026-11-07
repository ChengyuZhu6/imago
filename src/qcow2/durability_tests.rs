//! Model volatile host writes independently from writes made durable by sync.

use super::*;
use crate::test::mem_storage::{MemStorage, MemStorageState, StorageEvent};

fn load_u64(data: &[u8], offset: usize) -> u64 {
    data.get(offset..offset + 8)
        .map(|bytes| u64::from_be_bytes(bytes.try_into().unwrap()))
        .unwrap_or(0)
}

fn assert_owned_clusters(data: &[u8], event_index: usize) {
    let cluster_bits = u32::from_be_bytes(data[20..24].try_into().unwrap());
    let cluster_size = 1usize << cluster_bits;
    let l1 = load_u64(data, 40) as usize;
    let rt = load_u64(data, 48) as usize;
    let entries = u32::from_be_bytes(data[36..40].try_into().unwrap()) as usize;
    let refcount = |offset: usize| {
        let cluster = offset / cluster_size;
        let rb_entries = cluster_size / 2; // These fixtures use 16-bit refcounts.
        let rb = load_u64(data, rt + (cluster / rb_entries) * 8) as usize;
        if rb == 0 {
            return 0;
        }
        let offset = rb + (cluster % rb_entries) * 2;
        data.get(offset..offset + 2)
            .map(|bytes| u16::from_be_bytes(bytes.try_into().unwrap()))
            .unwrap_or(0)
    };
    let owned = |offset| {
        assert_ne!(
            refcount(offset),
            0,
            "unowned cluster at {offset:#x} after storage event {event_index}"
        );
    };
    owned(0);
    let rt_clusters = u32::from_be_bytes(data[56..60].try_into().unwrap()) as usize;
    for offset in (rt..rt + rt_clusters * cluster_size).step_by(cluster_size) {
        owned(offset);
    }
    for offset in (l1..l1 + entries * 8).step_by(cluster_size) {
        owned(offset);
    }
    for index in 0..rt_clusters * cluster_size / 8 {
        let rb = load_u64(data, rt + index * 8) as usize;
        if rb != 0 {
            owned(rb);
        }
    }
    for index in 0..entries {
        let l2 = (load_u64(data, l1 + index * 8) & 0x00ff_ffff_ffff_fe00) as usize;
        if l2 == 0 {
            continue;
        }
        owned(l2);
        for index in 0..cluster_size / 8 {
            let entry = load_u64(data, l2 + index * 8);
            assert_eq!(entry & (1 << 62), 0, "fixture must use ordinary clusters");
            let offset = (entry & 0x00ff_ffff_ffff_fe00) as usize;
            if offset != 0 {
                owned(offset);
            }
        }
    }
}

fn apply_write(data: &mut Vec<u8>, offset: u64, payload: &[u8]) {
    let start = offset as usize;
    data.resize(data.len().max(start + payload.len()), 0);
    data[start..start + payload.len()].copy_from_slice(payload);
}

fn check_power_cuts(mut durable: Vec<u8>, events: Vec<StorageEvent>) {
    let mut pending = Vec::new();
    for (index, event) in events.into_iter().enumerate() {
        match event {
            StorageEvent::Write(offset, payload) => {
                // ponytail: Model one newly destaged write at each cut, not torn writes.
                // Exhaustive subsets and sector tears belong in a block fault-injection suite.
                let mut crash = durable.clone();
                apply_write(&mut crash, offset, &payload);
                assert_owned_clusters(&crash, index);
                pending.push((offset, payload));
            }
            StorageEvent::Sync => {
                for (offset, payload) in pending.drain(..) {
                    apply_write(&mut durable, offset, &payload);
                }
                assert_owned_clusters(&durable, index);
            }
        }
    }
}

#[maybe_async::test(feature = "sync", async(feature = "async", tokio::test))]
async fn metadata_dependencies_survive_reordered_host_writeback() {
    check_metadata_dependencies(false).await;
    check_metadata_dependencies(true).await;
}

#[maybe_async]
async fn check_metadata_dependencies(buffered: bool) {
    let state = Arc::new(MemStorageState::default());
    let mut image = Qcow2::<MemStorage>::create_builder(MemStorage::new(Arc::clone(&state), false))
        .size(32768)
        .cluster_size(512)
        .create_open(crate::DenyImplicitOpenGate::default(), |storage| {
            Ok(Qcow2::<MemStorage>::builder(storage).write(true))
        })
        .await
        .unwrap();
    image.flush().await.unwrap();
    image.sync().await.unwrap();
    // Force both caches to evict dirty tables during the large allocation below.
    image.caches = Arc::new(MetadataCaches::new(&image.metadata, &image.header, 2, 2));
    image.allocator = Some(Mutex::new(
        Allocator::new(
            Arc::clone(&image.metadata),
            Arc::clone(&image.header),
            Arc::clone(&image.caches),
            Arc::clone(&image.clusters_to_free),
        )
        .await
        .unwrap(),
    ));
    let baseline = state.data.lock().clone();
    *state.events.lock() = Some(Vec::new());
    state.buffer_events.store(buffered, Ordering::Relaxed);

    let mut access = FormatAccess::new(image);
    access.write(&[0x5a; 512][..], 0).await.unwrap();
    access.flush().await.unwrap();
    access.sync().await.unwrap();
    // Grow across L1 and refcount table boundaries, free clusters, and reuse them.
    let size = 9 * 1024 * 1024;
    access
        .resize_grow(size, PreallocateMode::None)
        .await
        .unwrap();
    access
        .write(vec![0x6b; size as usize].as_slice(), 0)
        .await
        .unwrap();
    access.flush().await.unwrap();
    access.sync().await.unwrap();
    access.discard_to_zero(0, size).await.unwrap();
    access.resize_shrink(32768).await.unwrap();
    access.write(&[0x7c; 512][..], 0).await.unwrap();
    access.flush().await.unwrap();
    access.sync().await.unwrap();
    let events = state.events.lock().take().unwrap();
    check_power_cuts(baseline, events);
}

#[maybe_async::test(feature = "sync", async(feature = "async", tokio::test))]
async fn failed_dependency_sync_does_not_publish_l2_pointer() {
    let state = Arc::new(MemStorageState::default());
    let image = Qcow2::<MemStorage>::create_builder(MemStorage::new(Arc::clone(&state), false))
        .size(32768)
        .cluster_size(512)
        .create_open(crate::DenyImplicitOpenGate::default(), |storage| {
            Ok(Qcow2::<MemStorage>::builder(storage).write(true))
        })
        .await
        .unwrap();
    image.flush().await.unwrap();
    image.sync().await.unwrap();
    let access = FormatAccess::new(image);
    state.fail_next_sync.store(true, Ordering::Relaxed);
    let result = access.write(&[0x5a; 512][..], 0).await;
    assert!(result.is_err());
    {
        let data = state.data.lock();
        let l1 = load_u64(&data, 40) as usize;
        assert_eq!(load_u64(&data, l1), 0);
        assert_owned_clusters(&data, 0);
    }
    access.write(&[0x6b; 512][..], 0).await.unwrap();
    access.flush().await.unwrap();
    access.sync().await.unwrap();
    let mut buf = [0; 512];
    access.read(&mut buf[..], 0).await.unwrap();
    assert_eq!(buf, [0x6b; 512]);
    assert_owned_clusters(&state.data.lock(), 1);
}
