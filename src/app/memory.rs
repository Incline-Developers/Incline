//! Central limits for allocations that can approach the WASM32 address space.

#[cfg(target_arch = "wasm32")]
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[cfg(target_arch = "wasm32")]
pub(crate) const WORKING_SET_LIMIT: usize = 3 * 1024 * 1024 * 1024;
#[cfg(target_arch = "wasm32")]
pub(crate) const INPUT_FILE_LIMIT: usize = 2 * 1024 * 1024 * 1024;
// Ceilings on the up-front cost of building one block-model volume, not
// hardware limits: the cell backing is mmap-backed and streamed to a bounded
// GPU pool, and the GPU-side storage-buffer checks (which now see the
// adapter's real binding size, see `graphics::init`) are the true backstop,
// with a graceful cube-render fallback when they trip. Kept generous on native
// so large translucent models still reach the volume raycaster instead of the
// far slower instanced-cube path; they only bound worst-case build time and
// the temp-file/mmap footprint. `VOLUME_CELL_LIMIT` caps packed 16-bit cell
// bytes (~4M occupied bricks native); `VOLUME_METADATA_LIMIT` caps dense
// per-brick tables at ~64 B/brick (~32M total grid bricks native). On wasm32
// `usize` is 32-bit and the whole working set is 3 GiB, so both stay well
// under that (and under 4 GiB) and are further gated by `reserve`.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const VOLUME_CELL_LIMIT: usize = 4 * 1024 * 1024 * 1024;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const VOLUME_METADATA_LIMIT: usize = 2 * 1024 * 1024 * 1024;
#[cfg(target_arch = "wasm32")]
pub(crate) const VOLUME_CELL_LIMIT: usize = 1024 * 1024 * 1024;
#[cfg(target_arch = "wasm32")]
pub(crate) const VOLUME_METADATA_LIMIT: usize = 512 * 1024 * 1024;
#[cfg(target_arch = "wasm32")]
pub(crate) const FILE_CHUNK_BYTES: usize = 64 * 1024 * 1024;

#[cfg(target_arch = "wasm32")]
static RESERVED: AtomicUsize = AtomicUsize::new(0);

#[cfg(target_arch = "wasm32")]
#[derive(Clone, Debug)]
pub(crate) struct MemoryReservation {
    _reservation: Arc<Reservation>,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub(crate) struct MemoryReservation;

#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
struct Reservation {
    bytes: usize,
}

#[cfg(target_arch = "wasm32")]
impl Drop for Reservation {
    fn drop(&mut self) {
        if self.bytes != 0 {
            RESERVED.fetch_sub(self.bytes, Ordering::AcqRel);
        }
    }
}

impl MemoryReservation {
    pub(crate) fn untracked() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            Self
        }
        #[cfg(target_arch = "wasm32")]
        {
            Self {
                _reservation: Arc::new(Reservation { bytes: 0 }),
            }
        }
    }
}

pub(crate) fn reserve(bytes: usize, description: &str) -> Result<MemoryReservation, String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (bytes, description);
        Ok(MemoryReservation::untracked())
    }

    #[cfg(target_arch = "wasm32")]
    loop {
        let used = RESERVED.load(Ordering::Acquire);
        let requested = used.checked_add(bytes).ok_or_else(|| format!("{description} memory estimate overflows"))?;
        if requested > WORKING_SET_LIMIT {
            return Err(format!(
                "{description} needs {bytes} bytes, but only {} bytes remain in Incline Design's 3 GiB browser working-set budget",
                WORKING_SET_LIMIT.saturating_sub(used)
            ));
        }
        if RESERVED.compare_exchange_weak(used, requested, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            return Ok(MemoryReservation {
                _reservation: Arc::new(Reservation { bytes }),
            });
        }
    }
}
