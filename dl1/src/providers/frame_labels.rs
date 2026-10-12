//! Provider: frame labels (`monaka_stereo::providers::FrameLabels`).
//!
//! What Dying Light 1 provides once for the ready-made features (`monaka_stereo::providers`): its
//! frames labelled from its present schedule. One eye per frame, the eyes taking turns: the frame
//! being drawn is the one the next present shows ([`eye_site::drawing_present`]), and its eye is
//! the one the schedule gives that present. The pose is the head pose the present loop chose last
//! ([`eye_site::pending_pose`]): it carries the headset's fields of view and eye distance every
//! frame is drawn with, but its orientation can be a pair newer than the frame's own (the view
//! setup records each frame's own cameras for the upscaler and AFW).

use crate::Dl1;
use crate::providers::eye_site;
use monaka_core::alternate::Schedule;
use monaka_core::camera::Frustum;
use monaka_core::protocol::HeadPose;
use monaka_stereo::driver::Pending;
use monaka_stereo::providers::FrameLabels;

impl FrameLabels for Dl1 {
    /// The frame being drawn: the eye the schedule gives the present that shows it.
    fn labelled() -> Option<Pending> {
        let eye = drawn_eye(eye_site::schedule()?, eye_site::drawing_present()?);
        Some(Pending { eye, pose: eye_site::pending_pose() })
    }

    /// The frame the present request shows (the present counted last), once the first eye camera
    /// written has reached the screen.
    fn presenting() -> Option<Pending> {
        let eye = shown_eye(eye_site::schedule()?, eye_site::drawing_present()?)?;
        Some(Pending { eye, pose: eye_site::pending_pose() })
    }

    fn running() -> bool {
        eye_site::capturing()
    }

    fn frustum(pose: &HeadPose, eye: usize) -> Option<Frustum> {
        eye_site::rendered_frustum(pose, eye)
    }

    fn separation(pose: &HeadPose) -> Option<f32> {
        eye_site::separation(pose)
    }
}

/// The eye of the frame being drawn while present `drawing` is the next to come.
fn drawn_eye(schedule: Schedule, drawing: u64) -> usize {
    schedule.eye_at(drawing)
}

/// The eye shown by the present made last while present `drawing` is the next to come: in its
/// present request the loop has counted it, so the frame being drawn is the one after it.
fn shown_eye(schedule: Schedule, drawing: u64) -> Option<usize> {
    schedule.shown_eye(drawing.checked_sub(1)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The present loop's numbering: present `n`'s request runs once the loop has counted it, so
    /// the frame being drawn then (and between it and the next present) is `n + 1`; a HUD draw
    /// comes before its frame's present, while `n` itself is being drawn.
    #[test]
    fn the_labels_give_the_eyes_the_schedule_gave_before() {
        for latency in [1, 2, 3] {
            let schedule = Schedule::new(latency).unwrap();
            for n in 0..40 {
                // The HUD placement drew present `n`'s frame with the schedule's eye for `n`.
                assert_eq!(drawn_eye(schedule, n), schedule.eye_at(n));
                // The upscaler's publish at present `n` took the eye the schedule shows there.
                assert_eq!(shown_eye(schedule, n + 1), schedule.shown_eye(n));
            }
        }
    }
}
