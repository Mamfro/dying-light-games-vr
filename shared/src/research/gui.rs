//! The gui probe (`probe_gui`): each document of the engine's `gui::` tree met is logged once with
//! its widgets (the root's children: name, class, visibility, opacity, place and size) and the
//! shown tree under each, and its visible widgets again whenever they change, a snapshot at most
//! every [`SNAPSHOT_EVERY_MS`]. It rides on the tree's reads ([`crate::providers::gui`]).

use crate::providers::gui::{Calls, Node, called, children, read};
use monaka_hook::probe::class_name;
use monaka_producer::log;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

/// Whether the probe logs.
static PROBING: AtomicBool = AtomicBool::new(false);
static SNAPSHOT_AT: AtomicU64 = AtomicU64::new(0);
/// Snapshots are taken at most this often (ms) while probing.
const SNAPSHOT_EVERY_MS: u64 = 2000;

/// Documents logged in full already (root address: its name), and each one's visible widgets as
/// last logged.
static DOCUMENTS: Mutex<BTreeMap<usize, (String, Vec<String>)>> = Mutex::new(BTreeMap::new());

/// The probe is on for this run (or off).
pub(crate) fn configure(probe: bool) {
    PROBING.store(probe, Relaxed);
}

/// Whether the probe logs.
pub(crate) fn probing() -> bool {
    PROBING.load(Relaxed)
}

/// The engine worked on an element of document `root` (from its implementation `from`) at `now`
/// (ms): a document not met before is read at once, the others again every so often.
pub(crate) fn document(calls: &Calls, root: usize, from: usize, now: u64) {
    let known = DOCUMENTS.lock().is_ok_and(|d| d.contains_key(&root));
    if probing() && (!known || now.saturating_sub(SNAPSHOT_AT.load(Relaxed)) >= SNAPSHOT_EVERY_MS) {
        if known {
            SNAPSHOT_AT.store(now, Relaxed);
        }
        let started = std::time::Instant::now();
        snapshot(calls, root, from);
        note_cost(started.elapsed());
    }
}

/// One element's line in the probe's log.
fn line(node: &Node) -> String {
    format!(
        "{:#x} '{}' {} vis {}/{} opacity {:.2}/{:.2} at [{:.1} {:.1} {:.1}] size [{:.1} {:.1}] world [{:.1} {:.1} {:.1}] children {}",
        node.element,
        node.name,
        node.class,
        node.visible,
        node.actually_visible,
        node.opacity,
        node.actual_opacity,
        node.at[0],
        node.at[1],
        node.at[2],
        node.size[0],
        node.size[1],
        node.world[0],
        node.world[1],
        node.world[2],
        node.children
    )
}

fn snapshot(calls: &Calls, root: usize, from: usize) {
    let top = read(calls, root);
    let Ok(mut documents) = DOCUMENTS.lock() else { return };
    let widgets: Vec<Node> = children(calls, root, top.children).into_iter().map(|c| read(calls, c)).collect();
    let visible: Vec<String> = widgets.iter().filter(|w| w.actually_visible && w.actual_opacity > 0.0).map(|w| w.name.clone()).collect();
    match documents.get_mut(&root) {
        Some((_, shown)) if *shown != visible => {
            log!("gui document '{}': visible widgets now: {}", top.name, if visible.is_empty() { "none".to_owned() } else { visible.join(", ") });
            *shown = visible;
        }
        Some(_) => {}
        None => {
            log!("gui document '{}' (from the implementation {from:#x} {}): root {}", top.name, class_name(from).unwrap_or_default(), line(&top));
            for widget in &widgets {
                log!("  {}", line(widget));
                if widget.actually_visible && widget.actual_opacity > 0.0 {
                    let mut lines = Vec::new();
                    tree(calls, widget.element, widget.children, 2, &mut lines);
                    for line in lines {
                        log!("{line}");
                    }
                }
            }
            log!("gui document '{}': visible widgets: {}", top.name, if visible.is_empty() { "none".to_owned() } else { visible.join(", ") });
            documents.insert(root, (top.name, visible));
        }
    }
}

/// The shown elements under `element` (visible, with opacity), `depth` levels down at most, as
/// indented lines: name, class, box in the layout (pixels) and on screen (the world matrix's place
/// and scale), for finding the pieces the dynamic HUD takes.
fn tree(calls: &Calls, element: usize, count: i32, depth: usize, lines: &mut Vec<String>) {
    if depth > 8 || lines.len() > 400 {
        return;
    }
    for child in children(calls, element, count) {
        let node = read(calls, child);
        if !node.actually_visible || node.actual_opacity <= 0.0 {
            continue;
        }
        lines.push(format!(
            "{:indent$}'{}' {} layout [{:.0} {:.0}] {:.0}x{:.0} screen [{:.0} {:.0}] scale {:.2}{}",
            "",
            node.name,
            node.class.trim_start_matches(".?AV").trim_end_matches("@@"),
            node.at[0],
            node.at[1],
            node.size[0],
            node.size[1],
            node.world[0],
            node.world[1],
            node.scale,
            if node.children > 0 { format!(" ({} children)", node.children) } else { String::new() },
            indent = depth * 2
        ));
        if node.children > 0 {
            tree(calls, child, node.children, depth + 1, lines);
        }
    }
}

/// What snapshots cost the game thread: logged after 1, 10 and every 100 of them.
fn note_cost(took: std::time::Duration) {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    static TOTAL_US: AtomicU64 = AtomicU64::new(0);
    let us = took.as_micros() as u64;
    let n = COUNT.fetch_add(1, Relaxed) + 1;
    let total = TOTAL_US.fetch_add(us, Relaxed) + us;
    if n == 1 || n == 10 || n.is_multiple_of(100) {
        log!("gui probe: {n} snapshots, {} us each on average; getter calls: {}", total / n, called());
    }
}
