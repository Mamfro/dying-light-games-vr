//! Providers: frame labels (`FrameLabels`), frame inputs (`FrameInputs`, collected by
//! `crate::INPUTS`) and the HUD layer (`HudLayer`, `HudLayer12`).
//!
//! What The Beast provides once for the ready-made features (`monaka_stereo::providers`): its frames
//! labelled by their camera as DLSS or FSR gets it (the present loop's labels, `dlss`, `fsr`), each
//! eye's inputs from whichever upscaler the game runs (DLSS: its Streamline calls, copied at DLSS's
//! evaluate on the game's own command list; FSR: its dispatch, copied there), and its HUD layer
//! (the game's HUD draws kept out of the frame, `eng_chr::providers::hudlayer`) with where it goes
//! over each eye (`eng_chr::providers::hud_over`).

use crate::Beast;
use crate::providers::eye_site;
use monaka_core::camera::Frustum;
use monaka_core::protocol::HeadPose;
use monaka_framegen::inputs::Place;
use monaka_stereo::driver::Pending;
use monaka_stereo::feature::DRIVER;
use monaka_stereo::foveate12;
use monaka_stereo::hudover12::Params;
use eng_chr::providers::hudlayer;
use monaka_stereo::providers::{CopyAt, FrameInputs, FrameLabels, HudLayer, HudLayer12, Sources};
use windows::Win32::Graphics::Direct3D12::{ID3D12CommandQueue, ID3D12Resource};

impl FrameLabels for Beast {
    /// The label the frame's camera gave it as DLSS's constants or FSR's dispatch came
    /// (`Driver::label_frame`); the present takes it.
    fn labelled() -> Option<Pending> {
        DRIVER.peek_label().map(|label| Pending { eye: label.eye, pose: label.pose })
    }

    /// Before the label comes, the eye after the last labelled frame's.
    fn rendering() -> Option<Pending> {
        foveate12::labelled_frame()
    }

    fn presenting() -> Option<Pending> {
        Self::labelled()
    }

    fn running() -> bool {
        DRIVER.capturing()
    }

    fn frustum(pose: &HeadPose, eye: usize) -> Option<Frustum> {
        eye_site::eye_frustum(pose, eye)
    }

    fn own_frustum() -> Option<Frustum> {
        eye_site::game_frustum()
    }

    fn separation(pose: &HeadPose) -> Option<f32> {
        eye_site::separation(pose)
    }
}

/// Depth reversed and infinite, its near plane in each frame's projection; the game's units are
/// metres (the eyes are set the eye distance in metres apart: `eyes::eye_camera`).
impl FrameInputs for Beast {
    const SOURCES: Sources = Sources { streamline: Some(CopyAt::Evaluate), fsr: true };

    /// The frame's camera from the renderer's frame data, as DLSS's constants carry it.
    fn upscaled_place() -> Option<Place> {
        crate::providers::fsr::frame_place()
    }
}

impl HudLayer for Beast {
    fn over(size: (u32, u32), eye: Option<usize>) -> Option<Params> {
        eng_chr::providers::hud_over::over::<Self>(size, eye)
    }
}

/// The layer of the game's own HUD draws (`eng_chr::providers::hudlayer`), for the lay-over at the
/// present request.
impl HudLayer12 for Beast {
    fn take_layer(queue: &ID3D12CommandQueue, size: (u32, u32)) -> Option<ID3D12Resource> {
        hudlayer::take_layer(queue, size)
    }

    fn return_layer(queue: &ID3D12CommandQueue) {
        hudlayer::return_layer(queue);
    }

    fn kept_out() -> bool {
        hudlayer::redirecting()
    }

    fn let_through(through: bool) {
        hudlayer::let_through(through);
    }

    fn frame_end() {
        hudlayer::frame_end();
    }
}
