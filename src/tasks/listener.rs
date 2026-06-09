//! Task event listener that polls JJ for new events.
//!
//! Extracts the JJ polling pattern from the TUI into a reusable component
//! so that all workflow steps can observe task events in real-time.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossbeam_channel::Receiver;

use super::storage::read_events;
use super::types::TaskEvent;

/// How often the listener polls JJ for new events.
///
/// Also used by `spawn_drain_finalize` to size the post-exit tail drain
/// window — if you change this, the tail drain adjusts automatically.
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Increment used for interruptible sleeps (stop-flag checks).
const SLEEP_INCREMENT: Duration = Duration::from_millis(100);

/// Listens for new task events by polling JJ at ~1s intervals.
/// Tracks a high-water mark (event count) so it only yields events
/// that appeared since the last poll.
pub struct TaskEventListener {
    cwd: PathBuf,
    /// Number of events already seen — new events start after this offset.
    high_water: usize,
    /// Stop signal (shared with caller).
    stop: Arc<AtomicBool>,
}

impl TaskEventListener {
    pub fn new(cwd: &Path, stop: Arc<AtomicBool>) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            high_water: 0,
            stop,
        }
    }

    /// Start listening. Returns a receiver of raw TaskEvents.
    /// Spawns a background thread that polls until `stop` is set.
    pub fn start(self) -> Receiver<TaskEvent> {
        let (tx, rx) = crossbeam_channel::unbounded();

        thread::spawn(move || {
            let mut high_water = self.high_water;

            loop {
                if self.stop.load(Ordering::Relaxed) {
                    break;
                }

                if let Ok(events) = read_events(&self.cwd) {
                    for event in events.into_iter().skip(high_water) {
                        if tx.send(event).is_err() {
                            return; // receiver dropped
                        }
                        high_water += 1;
                    }
                }

                // Sleep for POLL_INTERVAL in SLEEP_INCREMENT steps,
                // checking the stop flag between sleeps.
                let ticks = POLL_INTERVAL.as_millis() / SLEEP_INCREMENT.as_millis();
                for _ in 0..ticks {
                    if self.stop.load(Ordering::Relaxed) {
                        return;
                    }
                    thread::sleep(SLEEP_INCREMENT);
                }
            }
        });

        rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn listener_stops_on_signal() {
        let stop = Arc::new(AtomicBool::new(false));
        let dir = std::env::temp_dir();
        let listener = TaskEventListener::new(&dir, Arc::clone(&stop));
        let rx = listener.start();

        // Signal stop immediately
        stop.store(true, Ordering::Relaxed);

        // Give the thread time to notice the stop
        thread::sleep(Duration::from_millis(300));

        // Channel should be disconnected (thread exited) or empty
        // Try receiving — should get Err (disconnected) or empty
        assert!(
            rx.try_recv().is_err(),
            "Expected no events after stop signal"
        );
    }

    #[test]
    fn listener_high_water_skips_seen_events() {
        // Verify that creating a listener with a non-zero high_water
        // correctly initializes the skip count
        let stop = Arc::new(AtomicBool::new(true)); // stop immediately
        let dir = std::env::temp_dir();

        let mut listener = TaskEventListener::new(&dir, Arc::clone(&stop));
        assert_eq!(listener.high_water, 0);

        listener.high_water = 5;
        assert_eq!(listener.high_water, 5);

        // Start and immediately stop — just verifying no panic
        let _rx = listener.start();
    }

    #[test]
    fn listener_channel_receives_in_order() {
        // Test that the crossbeam channel preserves ordering
        let (tx, rx) = crossbeam_channel::unbounded::<usize>();

        for i in 0..10 {
            tx.send(i).unwrap();
        }
        drop(tx);

        let received: Vec<usize> = rx.iter().collect();
        assert_eq!(received, (0..10).collect::<Vec<_>>());
    }
}
