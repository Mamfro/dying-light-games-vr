//! The pair: at the left eye's present request, its present dispatched, then the right eye's scene
//! call on the same simulation frame and its present (one eye per frame in alternate-eye, one centre
//! frame in depth stereo), each eye's frame handed to the loop, the camera given back after.

use super::*;

/// Runs the engine's queued GPU work now (D3D11), tagging any present it dispatches with `eye`.
pub(super) fn drain(eye: u32) {
    let phase = QUEUE_PHASE_ORIGINAL.get();
    local(|l| l.dispatch_eye.set(eye));
    // SAFETY: the engine's queue phase function, as the engine calls it.
    unsafe {
        phase(2, 0);
        local(|l| l.dispatch_eye.set(0));
        phase(0, 0);
        phase(0, 0);
    }
}

monaka_hook::caller_shim!(present_request_stub => present_request);

pub(super) unsafe extern "system" fn present_request(renderer: usize, a: usize, b: usize, caller: usize) -> usize {
    let _guard = InFlight::enter();
    let game = game::get();
    // SAFETY (every call of `original` below): the original, with the game's arguments.
    let original = PRESENT_REQUEST_ORIGINAL.get();
    if caller != game.engine_at(engine::NORMAL_PRESENT_CALLER) || local(|l| l.stereo_repeat.get()) {
        return unsafe { original(renderer, a, b) };
    }
    let input = take_left();
    if !input.normal_left {
        return unsafe { original(renderer, a, b) };
    }
    // The left eye is drawn. A frame that cannot become a pair is presented with the camera restored.
    let abort = |why: &str, fatal: bool| {
        restore_camera(&input);
        RENDER_EYE.store(0, Ordering::Release);
        if fatal { PAIRS.stop_for_run(why) } else { PAIRS.pause(why) }
        unsafe { original(renderer, a, b) }
    };
    if !PAIRS.running() || !PAIRS.publishing() {
        return abort("stereo switched off", false);
    }
    let mut bank = 0;
    if let Some(r11) = game.renderer11 {
        let driver = mem::read::<usize>(game.engine_at(engine::DRIVER_GLOBAL)).unwrap_or(0);
        if !present::drain_available(r11) || driver == 0 || mem::read::<usize>(driver) != Some(r11 + engine::DRIVER_VTABLE_11) {
            return abort("renderer backend identity mismatch", true);
        }
        bank = mem::read::<u32>(r11 + engine::QUEUE_BANK_11).unwrap_or(u32::MAX);
        if bank > 1 {
            return abort("invalid queue bank", true);
        }
    }
    let Some((game_object, counter)) = game.counter() else { return abort("game unavailable", true) };
    if input.game != game_object || input.counter != counter || input.a != a || input.b != b {
        return abort("scene changed before present", false);
    }
    // The direct render path runs in engine thread context 2.
    let manager = mem::read::<usize>(game.engine_at(engine::THREAD_CONTEXT_GLOBAL)).and_then(mem::read::<usize>).unwrap_or(0);
    let context = if manager == 0 {
        None
    } else {
        // SAFETY: reads this thread's value of the engine's TLS index.
        mem::read::<u32>(manager + 8).map(|index| unsafe { windows::Win32::System::Threading::TlsGetValue(index) } as usize)
    };
    if context != Some(2) {
        return abort("unexpected engine thread context", true);
    }
    monaka_producer::log_first!(1, "stereo pair thread {}", monaka_hook::thread_id());

    if input.depth_stereo {
        let pair = PAIRS.start_pair();
        local(|l| {
            l.dispatch_pair.set(pair);
            l.eye_copies.set(0);
            l.pair_head.set(input.head);
            l.pair_fov.set(input.center_fov);
            l.pair_aim.set(monaka_core::camera::aim_in_view(&input.inverse, &input.rendered_view, input.rendered_scale));
            l.dispatch_eye.set(3);
        });
        let started = Instant::now();
        let result = unsafe { original(renderer, a, b) };
        local(|l| l.dispatch_eye.set(0));
        PAIRS.time(Stage::LeftDrain, started.elapsed());
        restore_camera(&input);
        RENDER_EYE.store(0, Ordering::Release);
        if local(|l| l.eye_copies.get()) != 1 {
            PAIRS.pause("depth stereo pair unavailable");
        } else {
            PAIRS.pair_done("depth stereo", input.counter);
        }
        return result;
    }
    if input.alternate_eye != 0 {
        // This frame is one eye. The left frame starts a pair; the right frame publishes it.
        let eye = input.alternate_eye;
        if eye == 1 {
            let pair = PAIRS.start_pair();
            local(|l| l.dispatch_pair.set(pair));
        }
        local(|l| {
            l.eye_copies.set(0);
            l.pair_head.set(input.head);
            l.dispatch_eye.set(eye);
        });
        let started = Instant::now();
        let result = unsafe { original(renderer, a, b) };
        local(|l| l.dispatch_eye.set(0));
        PAIRS.time(if eye == 1 { Stage::LeftDrain } else { Stage::RightDrain }, started.elapsed());
        restore_camera(&input);
        RENDER_EYE.store(0, Ordering::Release);
        if local(|l| l.eye_copies.get()) != 1 {
            PAIRS.pause("eye image unavailable");
            return result;
        }
        PAIRS.turns().drawn(eye as usize - 1);
        if eye == 2 {
            PAIRS.pair_done("alternate-eye", input.counter);
        }
        return result;
    }

    let pair = PAIRS.start_pair();
    local(|l| {
        l.dispatch_pair.set(pair);
        l.eye_copies.set(0);
        l.pair_head.set(input.head);
    });
    // Left eye: on D3D11 the request queues the present and the drain dispatches it now; on D3D12
    // the request presents directly. Either way the eye is taken before its present runs.
    let d3d11 = game.renderer11.is_some();
    let started = Instant::now();
    let result = if d3d11 {
        let result = unsafe { original(renderer, a, b) };
        drain(1);
        result
    } else {
        local(|l| l.dispatch_eye.set(1));
        let result = unsafe { original(renderer, a, b) };
        local(|l| l.dispatch_eye.set(0));
        result
    };
    PAIRS.time(Stage::LeftDrain, started.elapsed());
    let level = input.camera - engine::LEVEL_CAMERA;
    // SAFETY: the engine functions the pair calls, on the live renderer, camera and level.
    unsafe { (game.renderer_enter)(renderer, std::ptr::null()) };
    local(|l| {
        l.stereo_repeat.set(true);
        l.tracked_visibility_camera.set(input.camera);
    });
    apply_eye_frustum(input.camera, &input.fovs[1]);
    // SAFETY: as above.
    unsafe {
        (game.reset_level)(level);
        set_camera_matrix(input.camera, &input.right_inverse);
    }
    local(|l| l.right_eye_scene.set(true));
    // SAFETY: the scene detour itself, with this frame's scene arguments.
    unsafe { scene(input.game, input.token, input.a, input.b) };
    local(|l| l.right_eye_scene.set(false));
    let second = snapshot(input.game, input.token, input.a, input.b, input.camera);
    let valid = second.valid && second.camera == input.camera && second.counter == input.counter && camera_matches(&second, &input.right_inverse, &input.right_projection);
    // Right eye; a rejected one is still presented but never published.
    let started = Instant::now();
    if d3d11 {
        unsafe { original(renderer, a, b) };
        drain(if valid { 2 } else { 0 });
    } else {
        local(|l| l.dispatch_eye.set(if valid { 2 } else { 0 }));
        unsafe { original(renderer, a, b) };
        local(|l| l.dispatch_eye.set(0));
    }
    PAIRS.time(Stage::RightDrain, started.elapsed());
    restore_camera(&input);
    // SAFETY: as above.
    unsafe {
        (game.reset_level)(level);
        (game.renderer_leave)(renderer);
    }
    local(|l| {
        l.stereo_repeat.set(false);
        l.tracked_visibility_camera.set(0);
        l.prepared_camera.set(input.camera);
    });
    RENDER_EYE.store(0, Ordering::Release);
    let restored = snapshot(input.game, input.token, input.a, input.b, input.camera);
    let restored_ok = restored.valid
        && restored.camera == input.camera
        && restored.counter == input.counter
        && restored.inverse == input.inverse
        && restored.projection == input.projection
        && game.renderer11.is_none_or(|r11| mem::read::<u32>(r11 + engine::QUEUE_BANK_11) == Some(bank));
    if !restored_ok {
        PAIRS.stop_for_run("camera, counter or queue not restored after the pair");
    } else if !valid {
        PAIRS.pause("right-eye camera changed during the scene");
    } else if local(|l| l.eye_copies.get()) != 2 {
        PAIRS.pause("eye images unavailable");
    } else {
        PAIRS.pair_done("stereo", input.counter);
    }
    result
}
