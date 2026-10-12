//! The run's counters every two seconds (presents, stereo, DLSS's inputs, frame generation) and the
//! probes' scans and reports, on a thread of its own.

use crate::{Beast, dlss, research, stereo};
use monaka_core::sync::Locked;
use monaka_producer::game::{Component, on, place};
use monaka_producer::log;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const REPORT: Component<Beast> = Component { name: "report", arm: start_reporter, quiet: stop_reporter, ..Component::NONE };

/// The components' places, for the report to ask whether each is on.
const PRESENT_AT: usize = place::<Beast>(&stereo::PRESENT_REQUEST);
const EYES_AT: usize = place::<Beast>(&stereo::EYES);
const STREAMLINE_AT: usize = place::<Beast>(&dlss::STREAMLINE);

static REPORTER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
static STOPPING: AtomicBool = AtomicBool::new(false);
const REPORT_EVERY: Duration = Duration::from_secs(2);

fn start_reporter() {
    STOPPING.store(false, Ordering::Release);
    let reporter = std::thread::Builder::new().name("monaka_beast-report".into()).spawn(|| {
        let started = Instant::now();
        let mut last = started;
        while !STOPPING.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(50));
            research::poll(started.elapsed());
            if last.elapsed() < REPORT_EVERY {
                continue;
            }
            let seconds = last.elapsed().as_secs_f64();
            if on::<Beast>(PRESENT_AT) {
                stereo::present12::report(seconds);
            }
            if on::<Beast>(EYES_AT) {
                stereo::eyes::report_rates(seconds);
            }
            if on::<Beast>(STREAMLINE_AT) {
                dlss::streamline::report(seconds);
            }
            if stereo::framegen::enabled() {
                log!("{}", stereo::framegen::report());
            }
            research::periodic(seconds);
            last = Instant::now();
        }
    });
    match reporter {
        Ok(handle) => *REPORTER.locked() = Some(handle),
        Err(e) => log!("no reporter thread ({e})"),
    }
}

fn stop_reporter() {
    STOPPING.store(true, Ordering::Release);
    if let Some(reporter) = REPORTER.locked().take() {
        let _ = reporter.join();
    }
}
