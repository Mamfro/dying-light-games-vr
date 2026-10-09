//! How far the head is horizontally from the character: the view is the game camera plus the
//! tracked head's position, so a step the character does not follow puts the eyes (and the arms,
//! which follow the controllers) away from the body enemies reach for. With room-scale following
//! ([`crate::player::roomscale`]) this is the gap it has not closed yet. Counted per eye view and logged
//! now and then.

use monaka_producer::log;
use std::sync::Mutex;
use std::time::Instant;

/// Bucket upper edges (metres) of the horizontal offset; the last bucket is everything beyond.
const EDGES: [f32; 5] = [0.1, 0.25, 0.5, 0.75, 1.0];
/// How often the counts are logged (seconds).
const EVERY: f32 = 60.0;

struct Drift {
    counts: [u64; EDGES.len() + 1],
    max: f32,
    last_log: Option<Instant>,
}

static DRIFT: Mutex<Drift> = Mutex::new(Drift { counts: [0; EDGES.len() + 1], max: 0.0, last_log: None });

/// One view placed with the head at `head` (tracking space, metres, y up) and the character at
/// `followed` (where room-scale following has walked it to, x and z).
pub fn note_head(head: [f32; 3], followed: [f32; 2]) {
    note([head[0] - followed[0], head[1], head[2] - followed[1]]);
}

/// One view placed with the head `position` from the character (tracking space, metres, y up).
fn note(position: [f32; 3]) {
    let offset = position[0].hypot(position[2]);
    if !offset.is_finite() {
        return;
    }
    let Ok(mut drift) = DRIFT.lock() else { return };
    let bucket = EDGES.iter().position(|&edge| offset < edge).unwrap_or(EDGES.len());
    drift.counts[bucket] += 1;
    drift.max = drift.max.max(offset);
    let now = Instant::now();
    if drift.last_log.is_none_or(|at| now.duration_since(at).as_secs_f32() >= EVERY) {
        let first = drift.last_log.is_none();
        drift.last_log = Some(now);
        if !first {
            log_counts(&drift);
        }
    }
}

fn log_counts(drift: &Drift) {
    let total = drift.counts.iter().sum::<u64>().max(1) as f32;
    let shares: Vec<String> = drift.counts.iter().map(|&c| format!("{:.1}%", 100.0 * c as f32 / total)).collect();
    log!("head drift from the character (horizontal) <0.1/0.25/0.5/0.75/1 m/beyond: {} of views; furthest {:.2} m", shares.join(" "), drift.max);
}

pub fn report() {
    if let Ok(drift) = DRIFT.lock()
        && drift.counts.iter().any(|&c| c > 0)
    {
        log_counts(&drift);
    }
}
