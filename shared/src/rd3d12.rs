//! rd3d12, the Chrome Engine's D3D12 renderer (Dying Light 2, The Beast): the present request
//! object a producer hooks to see each frame before it is presented, and the game's direct queue
//! (a Streamline proxy when Streamline is loaded), which the producers submit their own work on.
//! The function and the fields are the same in both games; each build moves the offsets, so the
//! game crate supplies them in a [`Layout`].
//!
//! Everything is read through [`monaka_hook::mem`] and checked by interface queries, so a moved
//! field reads as nothing rather than faulting.

use monaka_hook::mem;
use monaka_hook::module::plausible_object;
use std::ffi::c_void;
use windows::Win32::Graphics::Direct3D12::ID3D12CommandQueue;
use windows::core::{IUnknown, Interface};

/// The present request: `(object)`, presenting the object's swapchain when its flag is set.
pub type PresentRequestFn = unsafe extern "system" fn(object: usize) -> usize;

/// Where one build keeps what the producers read.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    /// In the present request object: non-zero when this request presents.
    pub request_active: usize,
    /// In the present request object: the swapchain it presents.
    pub request_swapchain: usize,
    /// rd3d12's pointer to the holder of the game's direct queue (an RVA).
    pub queue_holder: usize,
}

impl Layout {
    /// Whether the present request `object` presents.
    pub fn active(&self, object: usize) -> bool {
        mem::read::<usize>(object + self.request_active).unwrap_or(0) != 0
    }

    /// The address of the present request's swapchain (0 when unreadable).
    pub fn swapchain(&self, object: usize) -> usize {
        mem::read::<usize>(object + self.request_swapchain).unwrap_or(0)
    }

    /// The game's direct queue, from rd3d12 loaded at `base`.
    pub fn game_queue(&self, base: usize) -> Option<ID3D12CommandQueue> {
        let holder = mem::read::<usize>(base + self.queue_holder).filter(|&h| h != 0)?;
        query(mem::read::<usize>(holder)?)
    }
}

/// A live COM object at `address` (read from game memory) as `T`, by interface query; `None` if it
/// is not one.
pub fn query<T: Interface>(address: usize) -> Option<T> {
    if !plausible_object(address, 3) {
        return None;
    }
    let raw = address as *mut c_void;
    // SAFETY: a pointer whose vtable lies in loaded code; only QueryInterface is called on it.
    let unknown = unsafe { IUnknown::from_raw_borrowed(&raw) }?;
    unknown.cast().ok()
}
