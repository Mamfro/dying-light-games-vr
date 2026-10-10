//! Frames with no 3D view (a movie, a loading screen) without FSR: to the viewer's flat screen,
//! as the upscaler sends its own (`upscale`), instead of into the eyes. The game draws such a
//! frame through its UI, which keeps a fixed 16:9 size while VR runs (`hud::ui_size`): the square
//! back buffer holds that picture stretched, so it goes out in its own shape.

use monaka_channel::d3d;
use monaka_producer::log;
use monaka_stereo::alternate::{AlternatePublisher, Step};
use monaka_channel::pose::HeadSource;
use monaka_warp::{Scaler, fit};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D};

static SCALER: Mutex<Option<Scaler>> = Mutex::new(None);
static FLAT: AtomicU64 = AtomicU64::new(0);

/// Present `n`'s back buffer `back` to the flat screen, at its own size.
pub fn publish(device: &ID3D11Device, context: &ID3D11DeviceContext, back: &ID3D11Texture2D, n: u64, publisher: &mut AlternatePublisher, head: &HeadSource) -> Step {
    let desc = d3d::texture_desc(back);
    let size = (desc.Width, desc.Height);
    let rect = fit(crate::hud::ui_size::fixed().unwrap_or(size), size);
    let mut scaler = SCALER.lock().unwrap_or_else(|e| e.into_inner());
    if scaler.is_none() {
        match Scaler::new(device) {
            Ok(made) => *scaler = Some(made),
            Err(e) => {
                monaka_producer::log_first!(1, "flat frames unavailable: {e}");
                return Step::Failed;
            }
        }
    }
    let image = match scaler.as_mut().expect("made above").run_into(context, back, size, rect) {
        Ok(image) => image.clone(),
        Err(e) => {
            monaka_producer::log_first!(1, "a flat frame failed: {e}");
            return Step::Failed;
        }
    };
    if FLAT.fetch_add(1, Relaxed) == 0 {
        log!("a frame with no 3D view (a movie or a loading screen) went to the flat screen");
    }
    publisher.publish_flat(device, context, &image, n, head)
}
