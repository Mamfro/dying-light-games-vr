//! DL2's same-frame stereo on Monaka's loop against its own pair code it replaced.

use super::*;
use crate::manifest::MANIFESTS;
use monaka_core::options::Options;

/// The PC checks DL2's own frame generation read from the run's debug flags, each at its use.
fn checks_before(config: &config::Config) -> (bool, bool, bool) {
    (!config.has(debug::NO_HUDLESS), !config.has(debug::NO_HUD_COMPOSE), config.has(debug::SHOW_HUD_FREE))
}

#[test]
fn frame_generation_gets_the_checks_it_read_before() {
    for flags in [0, debug::NO_HUDLESS, debug::NO_HUD_COMPOSE, debug::SHOW_HUD_FREE, debug::NO_HUDLESS | debug::NO_HUD_COMPOSE | debug::SHOW_HUD_FREE, u32::MAX] {
        let config = config::Config::from_options(&Options::parse(&format!("debug_flags={flags}")), &MANIFESTS[0]);
        let checks = framegen_checks(&config);
        assert_eq!((checks.hud_free, checks.compose, checks.show_hud_free), checks_before(&config), "flags {flags}");
        assert!(std::ptr::fn_addr_eq(checks.epoch, dlss::epoch as fn() -> u32), "the per-eye history's epoch");
    }
}

/// The old pause turned stereo off, started each eye's history afresh and gave the next frame to
/// the left eye; the loop's pause does all three, the history by DL2's step at it.
#[test]
fn a_pause_resets_the_history_and_the_alternate_eye() {
    // The right eye's turn next.
    PAIRS.turns().drawn(0);
    assert_eq!(PAIRS.turns().frame(None).0, 1);
    // A left eye's scene takes up a fresh start asked for before.
    dlss::HISTORY.start_over();
    let _ = dlss::HISTORY.begin_scene(1, 1, 1, 0);
    assert!(!dlss::HISTORY.starting_over());
    // The loop's pause; the loop calls DL2's step from it once installed (`PairLoop::paused`).
    PAIRS.pause("a check");
    paused();
    assert!(!PAIRS.running());
    assert!(dlss::HISTORY.starting_over());
    assert_eq!(PAIRS.turns().frame(None).0, 0, "the left eye's turn again");
}

/// The loop pauses with DL2's step: each eye's history started afresh.
#[test]
fn the_loop_pauses_with_dl2s_own_step() {
    use monaka_stereo::same_frame::PresentsPairs;
    let game = <Dl2 as PresentsPairs>::LOOP;
    assert!(game.paused.is_some_and(|step| std::ptr::fn_addr_eq(step, paused as fn())));
}
