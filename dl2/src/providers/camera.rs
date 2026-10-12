//! Provider, to be: part of the eye site (Dying Light 2's camera form, for writing an eye into the
//! game camera); the recipe it uses (`monaka_core::camera`) is shared already.
//!
//! Dying Light 2 camera maths. The camera state holds a row-major 3x4 camera-to-world matrix
//! (column 0 right, 1 up, 2 back, 3 position; metres) and a right-handed projection: the canonical
//! form of `monaka_core::camera`, whose recipe makes the view centre and the eyes. What stays here
//! is the pair's cameras from one head pose, each eye's projection, and the checks a pose or camera
//! from the game must pass first.
//! Adapted from farmerarmor/DyingLight2VR CameraMath.h and TrackedCamera.h (MIT).

use monaka_core::camera::{Base, Frustum, HeadPosition, Mat34, Mat44, eye_camera, is_rigid, view_centre};
use monaka_core::protocol::HeadPose;

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

/// Whether a head pose is one to draw a pair from: valid, a plausible eye distance, a unit
/// orientation and a position within 5 m of the headset's origin.
pub fn usable_head(head: &HeadPose) -> bool {
    let norm: f32 = head.orientation.iter().map(|v| v * v).sum();
    head.valid != 0
        && HeadPose::PLAUSIBLE_IPD.contains(&head.ipd)
        && head.orientation.iter().all(|v| v.is_finite())
        && (norm - 1.0).abs() <= 0.01
        && head.position.iter().all(|v| v.is_finite() && v.abs() <= 5.0)
}

/// The view centre and both eyes' cameras (left, right) for `head` on the game camera `game` taken
/// as `base` ([`view_centre`], the head placed by `head_position`), the eyes `separation` metres
/// apart ([`eye_camera`]).
pub fn head_cameras(game: &Mat34, base: Base, head: &HeadPose, head_position: HeadPosition, separation: f32) -> Option<(Mat34, [Mat34; 2])> {
    if !usable(game) || !usable_head(head) {
        return None;
    }
    let centre = view_centre(game, base, head.orientation, head_position.position(head))?;
    Some((centre, [0, 1].map(|eye| eye_camera(&centre, eye, separation))))
}

/// The game's projection with the field of view `fov` (left, right, up, down; radians), possibly
/// asymmetric: the game's depth mapping kept, only the x/y terms replaced.
pub fn eye_projection(projection: &Mat44, fov: &[f32; 4]) -> Option<Mat44> {
    if !projection.iter().all(|v| v.is_finite()) || (projection[14] + 1.0).abs() > 0.001 || projection[15].abs() > 0.001 {
        return None;
    }
    if !fov.iter().all(|a| a.is_finite() && a.abs() <= 1.55) {
        return None;
    }
    let [left, right, up, down] = fov.map(f32::tan);
    if right - left <= 0.01 || up - down <= 0.01 {
        return None;
    }
    let mut out = *projection;
    Frustum { left, right, up, down }.apply_to_projection(&mut out);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use monaka_core::camera::{apply_head, distance, from_back_up, level, turn};
    use monaka_core::math::{NECK_TO_EYES, eye_position, rotate};

    const PROJECTION: [f32; 16] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.05, 0.0, 0.0, -1.0, 0.0];

    fn head() -> HeadPose {
        let (s, c) = (0.35f32).sin_cos();
        HeadPose { valid: 1, ipd: 0.064, orientation: [0.1 * c, s, 0.0, c * 0.995], position: [0.12, 1.65, -0.3], ..HeadPose::default() }
    }

    /// A pitched game camera looking 30 degrees left of -Z, at (5, 1, 2).
    fn game() -> Mat34 {
        let yaw = 30f32.to_radians();
        from_back_up([yaw.sin() * 0.9, 0.43, yaw.cos() * 0.9], [0.0, 1.0, 0.0], [5.0, 1.0, 2.0]).unwrap()
    }

    #[test]
    fn the_full_head_gives_the_cameras_the_pair_had() {
        // As each eye was made before: the game camera levelled and turned back by the baked yaw,
        // then turned and moved by the head with the eye half the IPD along the head's right axis.
        let mut head = head();
        let norm = head.orientation.iter().map(|v| v * v).sum::<f32>().sqrt();
        head.orientation = head.orientation.map(|v| v / norm);
        let view = turn(&level(&game()).unwrap(), -0.2);
        let (centre, eyes) = head_cameras(&game(), Base::Level { baked_yaw: 0.2 }, &head, HeadPosition::Full, head.ipd).unwrap();
        assert!(distance(&centre, &apply_head(&view, head.orientation, head.position)) < 1e-5, "the depth-stereo centre");
        for (e, eye) in eyes.iter().enumerate() {
            let before = apply_head(&view, head.orientation, eye_position(&head, e));
            assert!(distance(eye, &before) < 1e-5, "eye {e}: {eye:?} {before:?}");
        }
    }

    #[test]
    fn leaning_the_default_head_turns_about_the_neck_without_moving_the_eyes() {
        // The head turned 40 degrees left about the neck: tracked, the eyes swing left and back.
        let (s, c) = (20f32.to_radians()).sin_cos();
        let orientation = [0.0, s, 0.0, c];
        let swing = rotate(orientation, NECK_TO_EYES);
        let position = [0, 1, 2].map(|k| swing[k] - NECK_TO_EYES[k]);
        let head = HeadPose { valid: 1, ipd: 0.064, orientation, position, ..HeadPose::default() };
        let game = game();
        let base = Base::Level { baked_yaw: 0.0 };
        let (lean, _) = head_cameras(&game, base, &head, HeadPosition::Lean, 0.064).unwrap();
        let (full, _) = head_cameras(&game, base, &head, HeadPosition::Full, 0.064).unwrap();
        let at = |m: &Mat34| [m[3], m[7], m[11]];
        assert!(at(&lean).iter().zip(at(&game)).all(|(a, b)| (a - b).abs() < 1e-5), "lean: the view turns where it stands: {:?}", at(&lean));
        assert!(monaka_core::math::distance(at(&full), at(&game)) > 0.02, "full: the eyes swing with the neck");
        // Leaning itself still moves the view.
        let leaned = HeadPose { position: [position[0] + 0.1, position[1], position[2]], ..head };
        let (moved, _) = head_cameras(&game, base, &leaned, HeadPosition::Lean, 0.064).unwrap();
        assert!((monaka_core::math::distance(at(&moved), at(&lean)) - 0.1).abs() < 1e-4);
    }

    #[test]
    fn an_unusable_head_or_camera_draws_nothing() {
        let base = Base::Level { baked_yaw: 0.0 };
        assert!(head_cameras(&game(), base, &HeadPose { ipd: 0.2, ..head() }, HeadPosition::Lean, 0.064).is_none(), "eye distance");
        assert!(head_cameras(&game(), base, &HeadPose { position: [0.0, 6.0, 0.0], ..head() }, HeadPosition::Lean, 0.064).is_none(), "far off");
        assert!(head_cameras(&game(), base, &HeadPose { orientation: [0.0, 0.0, 0.0, 2.0], ..head() }, HeadPosition::Lean, 0.064).is_none(), "not a rotation");
        let skewed = [1.0, 0.5, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        assert!(head_cameras(&skewed, base, &head(), HeadPosition::Lean, 0.064).is_none(), "not a camera");
    }

    #[test]
    fn an_eye_projection_takes_its_field_of_view() {
        let projection = eye_projection(&PROJECTION, &[-0.9, 0.7, 0.8, -0.8]).unwrap();
        let (l, r) = ((-0.9f32).tan(), 0.7f32.tan());
        assert!((projection[0] - 2.0 / (r - l)).abs() < 1e-5 && (projection[2] - (r + l) / (r - l)).abs() < 1e-5);
        assert_eq!(projection[11], 0.05);
        assert!(eye_projection(&PROJECTION, &[-1.6, 0.7, 0.8, -0.8]).is_none());
    }

    #[test]
    fn centred_fov_covers_the_eye() {
        assert_eq!(centred_fov(&[-0.9, 0.7, 0.8, -0.95]), [-0.9, 0.9, 0.95, -0.95]);
    }
}
