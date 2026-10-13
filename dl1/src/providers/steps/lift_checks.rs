//! The walk body's reasons for following to wait against DL1's own rules before them.

use super::*;

/// DL1's walk body flags before, in their order (the mod drawing and no cutscene).
fn old_gate(full: usize) -> Option<&'static str> {
    if mem::read::<u8>(full + SCRIPTED_MOVE).unwrap_or(1) != 0 {
        return Some("scripted move");
    }
    if mem::read::<u8>(full + LOCK_POSITION).unwrap_or(1) != 0 {
        return Some("position locked");
    }
    if mem::read::<u8>(full + PHYSICS_OFF).unwrap_or(1) != 0 {
        return Some("physics off");
    }
    None
}

fn label(wait: Option<Wait>) -> Option<&'static str> {
    wait.map(|wait| match wait {
        Wait::ScriptedMove => "scripted move",
        Wait::PositionLocked => "position locked",
        Wait::PhysicsOff => "physics off",
        other => panic!("not a walk body flag: {other:?}"),
    })
}

#[test]
fn every_set_of_flags_waits_as_before() {
    let mut body = vec![0u8; STATE + 0x10];
    let full = body.as_mut_ptr() as usize;
    for set in 0..8u8 {
        for (k, flag) in [SCRIPTED_MOVE, LOCK_POSITION, PHYSICS_OFF].into_iter().enumerate() {
            body[flag] = (set >> k) & 1;
        }
        assert_eq!(label(flags_waiting(full)), old_gate(full), "flags {set:03b}");
    }
}
