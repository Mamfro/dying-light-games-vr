//! Dying Light 2 camera maths. The camera state holds a row-major 3x4 camera-to-world matrix
//! (column 0 right, 1 up, 2 back, 3 position; metres) and a right-handed projection: the canonical
//! form of `monaka_core::camera`, whose functions do the arithmetic. What stays here is the eye
//! description the scene keeps and the checks a pose or camera from the game must pass first.
//! Adapted from farmerarmor/DyingLight2VR CameraMath.h and TrackedCamera.h (MIT).

use monaka_core::camera::{Frustum, Mat34, Mat44, apply_head, is_rigid};
use monaka_core::math::eye_position;
use monaka_core::protocol::HeadPose;

/// One eye in the viewer's LOCAL space: orientation xyzw, position, fov left/right/up/down (radians).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackedEye {
    pub quaternion: [f32; 4],
    pub position: [f32; 3],
    pub fov: [f32; 4],
}

impl Default for TrackedEye {
    fn default() -> Self {
        Self { quaternion: [0.0, 0.0, 0.0, 1.0], position: [0.0; 3], fov: [0.0; 4] }
    }
}

/// The smallest centred field of view (left, right, up, down; radians) that covers `fov`.
pub fn centred_fov(fov: &[f32; 4]) -> [f32; 4] {
    let h = (-fov[0]).max(fov[1]);
    let v = fov[2].max(-fov[3]);
    [-h, h, v, -v]
}

/// Whether a matrix read from the game is a usable camera (finite, orthonormal axes).
pub fn usable(camera: &Mat34) -> bool {
    is_rigid(camera, 0.01)
}

/// The two eyes of one head pose, in the headset's own LOCAL space (its recentre is the only one).
pub fn eyes_from_head(head: &HeadPose) -> Option<[TrackedEye; 2]> {
    if head.valid == 0 || !HeadPose::PLAUSIBLE_IPD.contains(&head.ipd) {
        return None;
    }
    let eye = |e: usize| TrackedEye { quaternion: head.orientation, position: eye_position(head, e), fov: head.fov[e] };
    Some([eye(0), eye(1)])
}

/// The game camera turned and moved by one tracked eye, and the game's projection with that eye's
/// asymmetric field of view (the game's depth mapping kept; only the x/y terms replaced).
pub fn make_tracked_camera(base: &Mat34, projection: &Mat44, eye: &TrackedEye) -> Option<(Mat34, Mat44)> {
    if !usable(base) || !eye.quaternion.iter().all(|v| v.is_finite()) || !eye.position.iter().all(|v| v.is_finite() && v.abs() <= 5.0) {
        return None;
    }
    let norm: f32 = eye.quaternion.iter().map(|v| v * v).sum();
    if (norm - 1.0).abs() > 0.01 {
        return None;
    }
    if !projection.iter().all(|v| v.is_finite()) || (projection[14] + 1.0).abs() > 0.001 || projection[15].abs() > 0.001 {
        return None;
    }
    if !eye.fov.iter().all(|a| a.is_finite() && a.abs() <= 1.55) {
        return None;
    }
    let [left, right, up, down] = eye.fov.map(f32::tan);
    if right - left <= 0.01 || up - down <= 0.01 {
        return None;
    }
    let mut out = *projection;
    Frustum { left, right, up, down }.apply_to_projection(&mut out);
    Some((apply_head(base, eye.quaternion, eye.position), out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: [f32; 12] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0];
    const PROJECTION: [f32; 16] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.05, 0.0, 0.0, -1.0, 0.0];

    #[test]
    fn eyes_straddle_the_head() {
        let head = HeadPose { valid: 1, ipd: 0.064, fov: [[-0.9, 0.7, 0.8, -0.8], [-0.7, 0.9, 0.8, -0.8]], ..HeadPose::default() };
        let eyes = eyes_from_head(&head).unwrap();
        assert!((eyes[0].position[0] + 0.032).abs() < 1e-6 && (eyes[1].position[0] - 0.032).abs() < 1e-6);
        assert_eq!(eyes[1].fov, [-0.7, 0.9, 0.8, -0.8]);
        assert!(eyes_from_head(&HeadPose { ipd: 0.2, ..head }).is_none());
    }

    #[test]
    fn tracked_camera_takes_the_eye_projection() {
        let eye = TrackedEye { position: [0.032, 0.0, 0.0], fov: [-0.9, 0.7, 0.8, -0.8], ..TrackedEye::default() };
        let (inverse, projection) = make_tracked_camera(&IDENTITY, &PROJECTION, &eye).unwrap();
        assert!((inverse[3] - 0.032).abs() < 1e-6);
        let (l, r) = ((-0.9f32).tan(), 0.7f32.tan());
        assert!((projection[0] - 2.0 / (r - l)).abs() < 1e-5 && (projection[2] - (r + l) / (r - l)).abs() < 1e-5);
        assert_eq!(projection[11], 0.05);
        let skewed = [1.0, 0.5, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        assert!(make_tracked_camera(&skewed, &PROJECTION, &eye).is_none(), "not a camera");
    }

    #[test]
    fn centred_fov_covers_the_eye() {
        assert_eq!(centred_fov(&[-0.9, 0.7, 0.8, -0.95]), [-0.9, 0.9, 0.95, -0.95]);
    }
}
