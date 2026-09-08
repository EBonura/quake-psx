//! Exercise the guest reader with a drive that loses unread sectors while
//! the CPU transcodes cached nodes. No hardware MMIO is needed for this test.
extern crate alloc;

#[path = "../game/src/forward_chunk_reader.rs"]
mod forward_chunk_reader;

mod platform {
    use std::cell::RefCell;
    thread_local! {
        pub static DRIVE: RefCell<Drive> = RefCell::new(Drive::default());
    }
    #[derive(Default)]
    pub struct Drive {
        pub active: bool,
        pub opens: Vec<u32>,
        pub lost_data: bool,
    }
    #[derive(Debug)]
    pub enum StorageError {
        OutOfBounds,
        ReadFailed,
    }
    pub fn chunk_size(_: u32) -> Result<u32, StorageError> {
        Ok(4096)
    }
    pub struct ChunkStream;
    impl ChunkStream {
        pub fn open_at(_: u32, offset: u32) -> Result<Self, StorageError> {
            DRIVE.with(|drive| {
                let mut drive = drive.borrow_mut();
                assert!(!drive.active, "only one CD stream can own the drive");
                drive.active = true;
                drive.opens.push(offset);
            });
            Ok(Self)
        }
        pub fn read_exact_at(&mut self, offset: u32, out: &mut [u8]) -> Result<(), StorageError> {
            if DRIVE.with(|drive| drive.borrow().lost_data) {
                return Err(StorageError::ReadFailed);
            }
            for (i, b) in out.iter_mut().enumerate() {
                *b = offset.wrapping_add(i as u32) as u8;
            }
            Ok(())
        }
    }
    impl Drop for ChunkStream {
        fn drop(&mut self) {
            DRIVE.with(|drive| drive.borrow_mut().active = false);
        }
    }
}

use forward_chunk_reader::ForwardChunkReader;
use quake_formats::{LumpRange, ReadAt};

#[test]
fn cached_node_transcode_keeps_cd_paused_until_next_lump() {
    platform::DRIVE.with(|drive| *drive.borrow_mut() = platform::Drive::default());
    let mut cache = Vec::new();
    let mut reader = ForwardChunkReader::open(
        100,
        LumpRange {
            offset: 128,
            len: 96,
        },
        &mut cache,
    )
    .unwrap();
    let mut prefix = [0; 16];
    reader.read_exact_at(0, &mut prefix).unwrap();
    for offset in (128..224).step_by(24) {
        let mut node = [0; 24];
        reader.read_exact_at(offset, &mut node).unwrap();
        assert_eq!(node[0], offset as u8);
        // A node conversion may take longer than the drive's sector queue.
        // An open ReadN here makes subsequent disc data unrecoverable.
        platform::DRIVE.with(|drive| {
            let mut drive = drive.borrow_mut();
            drive.lost_data |= drive.active;
        });
    }
    let mut clip_nodes = [0; 32];
    reader
        .read_exact_at(224, &mut clip_nodes)
        .expect("node conversion must not lose the next lump");
    assert_eq!(clip_nodes[0], 224);
    platform::DRIVE.with(|drive| assert_eq!(drive.borrow().opens, [0, 224]));
    drop(reader);
    platform::DRIVE.with(|drive| assert!(!drive.borrow().active));
}

#[test]
fn empty_read_does_not_start_cd() {
    platform::DRIVE.with(|drive| *drive.borrow_mut() = platform::Drive::default());
    let mut cache = Vec::new();
    let mut reader = ForwardChunkReader::open(
        100,
        LumpRange {
            offset: 128,
            len: 96,
        },
        &mut cache,
    )
    .unwrap();
    assert_eq!(reader.len(), 4096);
    reader.read_exact_at(0, &mut []).unwrap();
    platform::DRIVE.with(|drive| assert!(drive.borrow().opens.is_empty()));
}
