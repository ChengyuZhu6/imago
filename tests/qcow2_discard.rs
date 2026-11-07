//! External QEMU interoperability and physical-space regression for QCOW2 discard.
#![cfg(target_os = "linux")]

use std::env;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use imago::file::File;
use imago::qcow2::Qcow2;
use imago::{DenyImplicitOpenGate, FormatAccess, FormatDriverBuilder, Storage, StorageOpenOptions};

const CLUSTER_SIZE: usize = 64 * 1024;
const DISK_SIZE: usize = 4 * 1024 * 1024;

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn qemu_img(directory: &Path, arguments: &[&str]) -> io::Result<()> {
    let output = Command::new("qemu-img")
        .current_dir(directory)
        .args(arguments)
        .output()?;
    assert!(
        output.status.success(),
        "qemu-img {arguments:?} failed with {}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    Ok(())
}

fn assert_contents(actual: &[u8], expected: &[u8]) {
    assert_eq!(actual.len(), expected.len(), "logical disk length changed");
    assert_eq!(
        actual.iter().zip(expected).position(|(a, b)| a != b),
        None,
        "first differing logical byte",
    );
}

fn check_qemu_contents(directory: &Path, expected: &[u8]) -> io::Result<()> {
    qemu_img(directory, &["check", "-f", "qcow2", "disk.qcow2"])?;
    qemu_img(
        directory,
        &[
            "convert",
            "-f",
            "qcow2",
            "-O",
            "raw",
            "disk.qcow2",
            "converted.raw",
        ],
    )?;
    assert_contents(&fs::read(directory.join("converted.raw"))?, expected);
    Ok(())
}

#[maybe_async::maybe_async]
async fn open_image(path: &Path) -> io::Result<FormatAccess<File>> {
    let storage = File::open(
        StorageOpenOptions::new()
            .filename(path)
            .write(true)
            .direct(false),
    )
    .await?;
    let image = Qcow2::<File>::builder(storage)
        .require_self_contained(true)
        .write(true)
        .open(DenyImplicitOpenGate::default())
        .await?;
    Ok(FormatAccess::new(image))
}

#[maybe_async::test(feature = "sync", async(feature = "async", tokio::test))]
#[ignore = "requires qemu-img and a hole-punch capable filesystem"]
async fn qemu_discard_preserves_contents_snapshots_and_reclaims_space() -> io::Result<()> {
    let directory = env::var_os("TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join(format!(
            "imago-qcow2-discard-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
    fs::create_dir(&directory)?;
    let directory = TempDir(directory);
    let path = directory.0.join("disk.qcow2");

    // Keep half of each cluster pseudorandom and repeat its last 2 KiB in the other half.
    // Repetitions fit QEMU's deflate window, while compressed data still dominates metadata.
    let mut original = vec![0; DISK_SIZE];
    let mut state = 0x1234_5678_u32;
    for cluster in original.chunks_exact_mut(CLUSTER_SIZE) {
        let (first, remaining) = cluster.split_at_mut(CLUSTER_SIZE / 2);
        for byte in first.iter_mut() {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *byte = (state % 255 + 1) as u8;
        }
        let pattern = &first[first.len() - 2048..];
        for repetition in remaining.chunks_exact_mut(pattern.len()) {
            repetition.copy_from_slice(pattern);
        }
    }
    fs::write(directory.0.join("source.raw"), &original)?;
    let zeroes = vec![0; DISK_SIZE];
    let mut actual = vec![0; DISK_SIZE];

    for compat in ["0.10", "1.1"] {
        for (cluster_size, compressed, snapshot) in [
            (CLUSTER_SIZE, false, false),
            (CLUSTER_SIZE, false, true),
            (CLUSTER_SIZE, true, false),
            (CLUSTER_SIZE, true, true),
            (512, false, false),
        ] {
            eprintln!("compat={compat} cluster_size={cluster_size} deflate={compressed} snapshot={snapshot}");
            let mut arguments = vec!["convert", "-f", "raw", "-O", "qcow2", "-o"];
            let mut options = format!("compat={compat},cluster_size={cluster_size}");
            if compressed && compat == "1.1" {
                options.push_str(",compression_type=zlib");
            }
            arguments.push(&options);
            if compressed {
                // QCOW2 v2 only supports deflate, selected by QEMU's -c default.
                arguments.push("-c");
            }
            arguments.extend(["source.raw", "disk.qcow2"]);
            qemu_img(&directory.0, &arguments)?;
            if snapshot {
                qemu_img(&directory.0, &["snapshot", "-c", "original", "disk.qcow2"])?;
            }

            let initial_blocks = fs::metadata(&path)?.blocks();
            assert!(initial_blocks * 512 > DISK_SIZE as u64 / 3);
            if compressed {
                assert!(
                    initial_blocks * 512 < DISK_SIZE as u64 * 3 / 4,
                    "QEMU must actually compress the sample data",
                );
            }
            let mut image = open_image(&path).await?;
            image.read(&mut actual[..], 0).await?;
            assert_contents(&actual, &original);
            let mut expected = original.clone();

            let partial_offset = 512.min(cluster_size / 4);
            let partial_length = 4096.min(cluster_size / 4);
            let boundary = 512.min(cluster_size / 4);
            let tail_length = 4096.min(cluster_size / 2);
            for (offset, length) in [
                (partial_offset, partial_length),
                (cluster_size - boundary, 2 * cluster_size + 2 * boundary),
                (DISK_SIZE - tail_length, tail_length),
                (cluster_size - boundary, 2 * cluster_size + 2 * boundary),
            ] {
                image.discard_to_zero(offset as u64, length as u64).await?;
                expected[offset..offset + length].fill(0);
                image.read(&mut actual[..], 0).await?;
                assert_contents(&actual, &expected);
                image.flush().await?;
                image.sync().await?;
                if snapshot && offset == partial_offset {
                    assert!(
                        fs::metadata(&path)?.blocks() > initial_blocks,
                        "partial discard must COW shared data before full discard",
                    );
                }
            }
            drop(image);
            check_qemu_contents(&directory.0, &expected)?;

            let mut image = open_image(&path).await?;
            image.read(&mut actual[..], 0).await?;
            assert_contents(&actual, &expected);
            let before_full_discard = fs::metadata(&path)?.blocks();
            image.discard_to_zero(0, DISK_SIZE as u64).await?;
            image.flush().await?;
            image.sync().await?;
            image.read(&mut actual[..], 0).await?;
            assert_contents(&actual, &zeroes);
            drop(image);

            let after_full_discard = fs::metadata(&path)?.blocks();
            eprintln!(
                "st_blocks initial={initial_blocks} before_full={before_full_discard} \
                     after_full={after_full_discard}",
            );
            if snapshot {
                // The original snapshot retains its data. Only private COW data can be
                // reclaimed, so compare against both the original and post-COW baselines.
                assert!(
                    after_full_discard >= initial_blocks,
                    "shared snapshot allocation lost"
                );
                assert!(
                    after_full_discard < before_full_discard,
                    "private COW data was not physically reclaimed",
                );
            } else {
                assert!(
                    after_full_discard * 4 <= initial_blocks,
                    "full discard must reclaim at least 75% of initial st_blocks",
                );
            }
            let image = open_image(&path).await?;
            image.read(&mut actual[..], 0).await?;
            assert_contents(&actual, &zeroes);
            image.flush().await?;
            image.sync().await?;
            drop(image);
            check_qemu_contents(&directory.0, &zeroes)?;

            if snapshot {
                qemu_img(&directory.0, &["snapshot", "-a", "original", "disk.qcow2"])?;
                check_qemu_contents(&directory.0, &original)?;
                let image = open_image(&path).await?;
                image.read(&mut actual[..], 0).await?;
                assert_contents(&actual, &original);
                image.flush().await?;
                image.sync().await?;
            }
        }
    }
    Ok(())
}
