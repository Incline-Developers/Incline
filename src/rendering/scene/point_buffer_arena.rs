//! Suballocator for point-cloud vertex data.
//!
//! Every full spatial chunk of a cloud shares an identical LOD prefix layout -
//! `level_counts` depends only on the chunk's point count, not on which points
//! the density-aware ordering picked - so a given capacity level always requests
//! the same number of bytes. That makes fixed-size slot pools a perfect fit:
//! residency changes hand slots back and forth with no fragmentation, and new
//! GPU buffers are created once per *block* (hundreds to thousands of slots)
//! rather than once per chunk. The per-frame chunk-count cap that used to bound
//! `create_buffer` churn is therefore no longer needed.

use std::collections::HashMap;
#[cfg(target_arch = "wasm32")]
use std::rc::Rc as SharedBuffer;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc as SharedBuffer;

/// Vertex buffer offsets and attribute reads must be 4-byte aligned; rounding
/// slot strides to 16 keeps every slot start aligned for both position-only
/// (12-byte) and coloured (16-byte) records with negligible padding.
const SLOT_ALIGNMENT: u64 = 16;
/// Target bytes per backing block. Slots pack into blocks of roughly this size,
/// so one `create_buffer` amortises across many chunks; large slots still get at
/// least a few per block, tiny bootstrap slots get thousands.
const TARGET_BLOCK_BYTES: u64 = 16 * 1024 * 1024;
/// Cap slots per block so a rare one-off size (a cloud's partial tail chunk)
/// cannot reserve an enormous block for a single occupant.
const MAX_SLOTS_PER_BLOCK: u32 = 4_096;

/// A suballocated region of a shared block buffer. Holds shared ownership of its block
/// so a chunk can draw and be uploaded to without consulting the arena, and
/// carries the location needed to return the slot on eviction.
pub(crate) struct PointSlot {
    buffer: SharedBuffer<wgpu::Buffer>,
    offset: u64,
    /// Padded stride this slot occupies; also the free-list pool key.
    stride: u64,
    block: u32,
    index: u32,
}

impl PointSlot {
    /// The block buffer this slot lives in. Draw and upload address it with
    /// `offset()`.
    pub(crate) fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    /// Byte offset of this slot within `buffer()`.
    pub(crate) fn offset(&self) -> u64 {
        self.offset
    }

    /// Byte range of this slot within `buffer()`, spanning its full padded
    /// capacity. Drawing binds this range; the draw call's instance count bounds
    /// how much of it the pipeline actually reads.
    pub(crate) fn vertex_range(&self) -> std::ops::Range<u64> {
        self.offset..self.offset + self.stride
    }
}

#[derive(Clone, Copy)]
struct FreeSlot {
    block: u32,
    index: u32,
}

struct SlotPool {
    slots_per_block: u32,
    blocks: Vec<SharedBuffer<wgpu::Buffer>>,
    free: Vec<FreeSlot>,
}

/// Fixed-size slot pools keyed by padded slot stride.
#[derive(Default)]
pub(crate) struct PointBufferArena {
    pools: HashMap<u64, SlotPool>,
}

fn align_up(value: u64, alignment: u64) -> u64 {
    value.div_ceil(alignment) * alignment
}

impl PointBufferArena {
    /// Reserve a slot at least `bytes` large. `bytes` must not exceed the
    /// adapter's per-buffer limit; callers clamp their capacity level so a
    /// single slot always fits one block.
    pub(crate) fn alloc(&mut self, device: &wgpu::Device, bytes: u64) -> PointSlot {
        let stride = align_up(bytes.max(SLOT_ALIGNMENT), SLOT_ALIGNMENT);
        let pool = self.pools.entry(stride).or_insert_with(|| SlotPool {
            slots_per_block: slots_per_block(stride),
            blocks: Vec::new(),
            free: Vec::new(),
        });
        let free = pool.free.pop().unwrap_or_else(|| {
            let block = pool.blocks.len() as u32;
            let block_bytes = stride * u64::from(pool.slots_per_block);
            pool.blocks.push(SharedBuffer::new(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Point Cloud Slot Block"),
                size: block_bytes,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })));
            // Expose every slot but the first; the first is returned below.
            pool.free.extend((1..pool.slots_per_block).map(|index| FreeSlot { block, index }));
            FreeSlot { block, index: 0 }
        });
        PointSlot {
            buffer: SharedBuffer::clone(&pool.blocks[free.block as usize]),
            offset: u64::from(free.index) * stride,
            stride,
            block: free.block,
            index: free.index,
        }
    }

    /// Return a slot to its pool for reuse. Blocks themselves are retained.
    pub(crate) fn free(&mut self, slot: PointSlot) {
        if let Some(pool) = self.pools.get_mut(&slot.stride) {
            pool.free.push(FreeSlot {
                block: slot.block,
                index: slot.index,
            });
        }
    }
}

fn slots_per_block(stride: u64) -> u32 {
    let by_target = (TARGET_BLOCK_BYTES / stride).max(1);
    u32::try_from(by_target).unwrap_or(MAX_SLOTS_PER_BLOCK).min(MAX_SLOTS_PER_BLOCK)
}
