//! Host copies of textures the title fills, placed in card heaps of our own
//! instead of each being a committed resource of vkd3d-proton's.
//!
//! vkd3d-proton puts committed textures under 8 MiB in chunks of its own
//! and gives a chunk back only when nothing is left in it: copies let go
//! leave holes the system's count of memory in use cannot show
//! (`crate::resource::holes`), every new one is zeroed by a clear on the
//! card first, and the allocator walks its chunks under one lock. Here a
//! heap of [`HEAP`] bytes is cut into slots of one size (the copy's bytes
//! rounded up to a power of two, at least 64 KiB); a copy is a placed
//! resource in a slot, and its slot is free again once the card has run
//! everything submitted before the copy went (`crate::host::retire`). The
//! heaps are not zeroed: a copy's levels are all written by its upload
//! (the title's or zeros, `crate::texture`) before a list samples it.
//! Heaps with every slot free go back while more than [`spare_most`]
//! bytes of slots are free. Off unless `VICTORY_CARD_SLABS=1`: run of
//! 2026-11-27 lost 37 % to slots rounded up to a power of two (4.3 GB of
//! copies in 5.9 GB of slots), and 4.4 GB of free slots of other sizes sat
//! unused while 11,833 textures waited for a copy. The packing wants a
//! finer allocator, compared against vkd3d-proton's first (`apps/vkd3d-bench`).

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use windows::Win32::Graphics::Direct3D12::*;

/// A heap's bytes, and the largest slot.
const HEAP: u64 = 64 << 20;
const SLOT_MOST: u64 = 8 << 20;

/// A copy's place: its slot size, the heap, and the offset in it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Slot {
    size: u64,
    heap: u32,
    offset: u64,
}

struct Heap {
    heap: ID3D12Heap,
    size: u64,
    taken: u32,
}

// SAFETY: a D3D12 heap (free-threaded); its slots are handed out under the lock.
unsafe impl Send for Heap {}

struct Heaps {
    heaps: HashMap<u32, Heap>,
    /// Free slots by size: (heap, offset).
    free: BTreeMap<u64, Vec<(u32, u64)>>,
    next: u32,
}

static HEAPS: Mutex<Option<Heaps>> = Mutex::new(None);

/// Now: the heaps' bytes, the bytes of free slots in them, the copies
/// placed and their bytes (their own size, not the slot's).
pub(crate) static BYTES: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// Whether copies are placed in heaps of ours (`VICTORY_CARD_SLABS=1`).
pub(crate) fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_CARD_SLABS").is_some_and(|v| v == "1"))
}

/// The most bytes of free slots kept in heaps nothing else uses
/// (`VICTORY_CARD_SLABS_SPARE_MB`, 512 unless said).
fn spare_most() -> u64 {
    static MOST: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *MOST.get_or_init(|| {
        std::env::var("VICTORY_CARD_SLABS_SPARE_MB").ok().and_then(|v| v.trim().parse::<u64>().ok()).map_or(512 << 20, |mb| mb << 20)
    })
}

/// A copy of `desc` placed in a slot, in `state`, and its slot; `None`
/// when it does not fit a slot (too large, more than one sample) or the
/// card has no room for a heap: the caller makes it committed instead.
pub(crate) fn place(device: &ID3D12Device, desc: &D3D12_RESOURCE_DESC, state: D3D12_RESOURCE_STATES) -> Option<(ID3D12Resource, Slot)> {
    if desc.SampleDesc.Count > 1 || desc.Alignment > 64 << 10 {
        return None;
    }
    // SAFETY: a plain query on a description.
    let info = unsafe { device.GetResourceAllocationInfo(0, &[*desc]) };
    if info.SizeInBytes == u64::MAX || info.SizeInBytes > SLOT_MOST || info.Alignment > 64 << 10 {
        return None;
    }
    let size = info.SizeInBytes.max(64 << 10).next_power_of_two();
    let slot = take(device, size)?;
    let heap = {
        let guard = HEAPS.lock().unwrap_or_else(|e| e.into_inner());
        guard.as_ref()?.heaps.get(&slot.heap)?.heap.clone()
    };
    let mut made: Option<ID3D12Resource> = None;
    // SAFETY: a heap of ours, an offset inside it with room for the
    // resource (its slot), and an out pointer of ours.
    let placed = unsafe { device.CreatePlacedResource(&heap, slot.offset, desc, state, None, &mut made) };
    match (placed, made) {
        (Ok(()), Some(made)) => {
            BYTES[2].fetch_add(1, Relaxed);
            BYTES[3].fetch_add(info.SizeInBytes, Relaxed);
            Some((made, slot))
        }
        _ => {
            give_back(slot);
            None
        }
    }
}

/// A free slot of `size` bytes, from a new heap if none is free.
fn take(device: &ID3D12Device, size: u64) -> Option<Slot> {
    let mut guard = HEAPS.lock().unwrap_or_else(|e| e.into_inner());
    let all = guard.get_or_insert_with(|| Heaps { heaps: HashMap::new(), free: BTreeMap::new(), next: 0 });
    if all.free.get(&size).is_none_or(Vec::is_empty) {
        let desc = D3D12_HEAP_DESC {
            SizeInBytes: HEAP,
            Properties: D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() },
            Alignment: 64 << 10,
            Flags: D3D12_HEAP_FLAG_ALLOW_ONLY_NON_RT_DS_TEXTURES | D3D12_HEAP_FLAG_CREATE_NOT_ZEROED,
        };
        let mut heap: Option<ID3D12Heap> = None;
        // SAFETY: a plain description and an out pointer of ours.
        unsafe { device.CreateHeap(&desc, &mut heap) }.ok()?;
        let heap = heap?;
        let number = all.next;
        all.next = all.next.wrapping_add(1);
        all.heaps.insert(number, Heap { heap, size, taken: 0 });
        // (Highest offset first: slots are taken from the end, lowest first.)
        all.free.entry(size).or_default().extend((0..HEAP / size).rev().map(|i| (number, i * size)));
        BYTES[0].fetch_add(HEAP, Relaxed);
        BYTES[1].fetch_add(HEAP, Relaxed);
    }
    let (heap, offset) = all.free.get_mut(&size)?.pop()?;
    all.heaps.get_mut(&heap)?.taken += 1;
    BYTES[1].fetch_sub(size, Relaxed);
    Some(Slot { size, heap, offset })
}

/// Gives `slot` back; a heap with every slot free goes back to the host
/// while more than [`spare_most`] bytes of slots are free.
fn give_back(slot: Slot) {
    let mut guard = HEAPS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(all) = guard.as_mut() else { return };
    let Some(heap) = all.heaps.get_mut(&slot.heap) else { return };
    heap.taken -= 1;
    all.free.entry(slot.size).or_default().push((slot.heap, slot.offset));
    let spare = BYTES[1].fetch_add(slot.size, Relaxed) + slot.size;
    if heap.taken == 0 && spare > spare_most() {
        let size = heap.size;
        if let Some(free) = all.free.get_mut(&size) {
            free.retain(|&(n, _)| n != slot.heap);
        }
        BYTES[0].fetch_sub(HEAP, Relaxed);
        BYTES[1].fetch_sub(HEAP, Relaxed);
        let gone = all.heaps.remove(&slot.heap);
        drop(guard);
        drop(gone);
    }
}

/// A placed copy's slot, given back when this is dropped: retired with the
/// copy (`crate::host::retire`), so once the card has run what was
/// submitted while the copy lived.
pub(crate) struct Taken(pub(crate) Slot, pub(crate) u64);

impl Drop for Taken {
    fn drop(&mut self) {
        BYTES[2].fetch_sub(1, Relaxed);
        BYTES[3].fetch_sub(self.1, Relaxed);
        give_back(self.0);
    }
}
