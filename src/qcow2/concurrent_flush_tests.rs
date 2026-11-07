//! Regression for an allocation overtaking a concurrent refcount-cache flush.
//!

use super::*;
use crate::test::mem_storage::{MemStorage, MemStorageState};
use crate::DenyImplicitOpenGate;
use futures::task::noop_waker;
use std::future::Future;
use std::task::{Context, Poll};

fn load_u64(state: &MemStorageState, offset: usize) -> u64 {
    u64::from_be_bytes(state.data.lock()[offset..offset + 8].try_into().unwrap())
}

#[tokio::test]
async fn concurrent_flush_preserves_allocated_cluster_ownership() {
    check_concurrent_flush(1024, 512).await;
}

#[tokio::test]
async fn concurrent_flush_preserves_contiguous_cluster_ownership() {
    // Start in an allocated cluster so the next allocation has a mandatory host address.
    check_concurrent_flush(512, 1024).await;
}

async fn check_concurrent_flush(write_offset: u64, length: usize) {
    let state = Arc::new(MemStorageState::default());
    let qcow2 = Qcow2::<MemStorage>::create_builder(MemStorage::new(Arc::clone(&state), false))
        .cluster_size(512)
        .size(32768)
        .create_open(DenyImplicitOpenGate::default(), |image| {
            Ok(Qcow2::<MemStorage>::builder(image)
                .backing(None)
                .write(true))
        })
        .await
        .unwrap();
    let rb_offset = load_u64(&state, load_u64(&state, 48) as usize);
    let rb = qcow2
        .caches
        .rb_get_or_insert(HostCluster(rb_offset / 512))
        .await
        .unwrap();
    let image = FormatAccess::new(qcow2);
    image.write(&[0x11; 512][..], 0).await.unwrap();
    image.flush().await.unwrap();
    image.sync().await.unwrap();

    // Dirty the existing RB without creating a second L2 table.
    image.write(&[0x22; 512][..], 512).await.unwrap();
    // This lock only controls scheduling: pause the next normal write after its
    // dependency announcement, before its refcount increment.
    let rb_writer = rb.lock_write().await;
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    let payload = vec![0x33; length];
    let mut write = Box::pin(tokio::task::unconstrained(
        image.write(&payload[..], write_offset),
    ));
    assert!(write.as_mut().poll(&mut cx).is_pending());

    *state.hold_range.lock() = Some(rb_offset..rb_offset + 512);
    state.hold_writes.store(true, Ordering::Relaxed);
    let mut flush = Box::pin(tokio::task::unconstrained(image.flush()));
    assert!(flush.as_mut().poll(&mut cx).is_pending());
    assert_eq!(state.held.lock().len(), 1);
    state.hold_writes.store(false, Ordering::Relaxed);

    drop(rb_writer);
    let completed = match write.as_mut().poll(&mut cx) {
        Poll::Ready(result) => {
            result.unwrap();
            true
        }
        Poll::Pending => false,
    };
    state.release_held_writes();
    let Poll::Ready(result) = flush.as_mut().poll(&mut cx) else {
        panic!("flush did not complete after releasing the refblock write");
    };
    result.unwrap();
    drop(flush);
    if !completed {
        write.as_mut().await.unwrap();
    }
    drop(write);
    // No extra flush: it would hide the concurrent flush's missing refcount.
    image.sync().await.unwrap();
    assert!(state.held.lock().is_empty());

    let l1 = load_u64(&state, 40) as usize;
    let l2 = (load_u64(&state, l1) & 0x00ff_ffff_ffff_fe00) as usize;
    let original_host = load_u64(&state, l2 + 2 * 8) & 0x00ff_ffff_ffff_fe00;
    if original_host == 0 {
        // The write completed after flush and its new mapping need not be durable yet.
        assert!(!completed);
        image.flush().await.unwrap();
        image.sync().await.unwrap();
        return;
    }
    let refcount_at = rb_offset as usize + original_host as usize / 512 * 2;
    let stored_refcount = u16::from_be_bytes(
        state.data.lock()[refcount_at..refcount_at + 2]
            .try_into()
            .unwrap(),
    );
    let restart = Arc::new(MemStorageState::default());
    *restart.data.lock() = state.data.lock().clone();
    drop(image);
    drop(rb);

    // Reopen the completed flush's bytes, independently of the old caches.
    let reopened = Qcow2::<MemStorage>::builder(MemStorage::new(restart, false))
        .write(true)
        .require_self_contained(true)
        .open(DenyImplicitOpenGate::default())
        .await
        .unwrap();
    let image = FormatAccess::new(reopened);
    let mut actual = [0; 512];
    image.read(&mut actual[..], 1024).await.unwrap();
    assert_eq!(
        actual, [0x33; 512],
        "data was already lost before reallocation"
    );
    image.write(&[0x44; 512][..], 1536).await.unwrap();
    image.flush().await.unwrap();
    image.sync().await.unwrap();
    let (
        crate::Mapping::Raw {
            offset: next_host, ..
        },
        _,
    ) = image.get_mapping(1536, 512).await.unwrap()
    else {
        panic!("expected an allocated replacement cluster");
    };
    image.read(&mut actual[..], 1024).await.unwrap();
    assert!(
        stored_refcount != 0 && original_host != next_host && actual == [0x33; 512],
        "successful concurrent flush+sync lost ownership: refcount={stored_refcount}, \
         original_host={original_host}, next_host={next_host}, \
         original_guest_byte={:#x}, expected=0x33",
        actual[0]
    );
}
