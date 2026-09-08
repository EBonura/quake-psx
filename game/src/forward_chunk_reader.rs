//! Forward CD payload reads with a paused, RAM-backed node transcode.

use crate::platform::{self, StorageError};
use alloc::vec::Vec;
use quake_formats::{LumpRange, ReadAt};

/// Payload reader used after one PSB index has already been validated. Its
/// first payload access opens a single forward-only CD session; the shared
/// resident loader then consumes ModelData through Entities in disc order.
pub(crate) struct ForwardChunkReader<'a> {
    chunk_id: u32,
    len: u32,
    stream: Option<platform::ChunkStream>,
    node_range: LumpRange,
    node_cache: &'a mut Vec<u8>,
}

impl<'a> ForwardChunkReader<'a> {
    #[cfg_attr(not(test), optimize(size))]
    pub(crate) fn open(
        chunk_id: u32,
        node_range: LumpRange,
        node_cache: &'a mut Vec<u8>,
    ) -> Result<Self, StorageError> {
        Ok(Self {
            chunk_id,
            len: platform::chunk_size(chunk_id)?,
            stream: None,
            node_range,
            node_cache,
        })
    }
}

impl ReadAt for ForwardChunkReader<'_> {
    type Error = StorageError;

    #[cfg_attr(not(test), optimize(size))]
    fn len(&self) -> u32 {
        self.len
    }

    #[cfg_attr(not(test), optimize(size))]
    fn read_exact_at(&mut self, offset: u32, output: &mut [u8]) -> Result<(), Self::Error> {
        if output.is_empty() {
            return Ok(());
        }
        let output_end = offset
            .checked_add(u32::try_from(output.len()).map_err(|_| StorageError::OutOfBounds)?)
            .ok_or(StorageError::OutOfBounds)?;
        if offset >= self.node_range.offset && output_end <= self.node_range.end() {
            if self.node_cache.is_empty() {
                if self.stream.is_none() {
                    self.stream = Some(platform::ChunkStream::open_at(self.chunk_id, offset)?);
                }
                self.node_cache.resize(self.node_range.len as usize, 0);
                self.stream
                    .as_mut()
                    .ok_or(StorageError::ReadFailed)?
                    .read_exact_at(self.node_range.offset, self.node_cache)?;
                // PSB5 nodes are expanded one record at a time by the shared
                // loader. Pause ReadN while that CPU pass consumes the cache;
                // otherwise the drive advances past the following clip-node
                // sector before the next payload read.
                self.stream = None;
            }
            let start = (offset - self.node_range.offset) as usize;
            let end = start + output.len();
            output.copy_from_slice(
                self.node_cache
                    .get(start..end)
                    .ok_or(StorageError::OutOfBounds)?,
            );
            return Ok(());
        }
        // Cached node reads must leave the drive paused for the entire CPU
        // transcode. Reopen only when a later, uncached lump needs the disc.
        if self.stream.is_none() {
            self.stream = Some(platform::ChunkStream::open_at(self.chunk_id, offset)?);
        }
        self.stream
            .as_mut()
            .ok_or(StorageError::ReadFailed)?
            .read_exact_at(offset, output)
    }
}
