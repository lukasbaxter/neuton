//! Pacing chunk delivery.
//!
//! The server sends chunks in batches and waits to be told how the last one
//! went before sending the next. A client that never answers gets ten batches
//! and then nothing at all, which looks exactly like a world that stops
//! generating a few chunks out.
//!
//! The answer is a rate: how many chunks a tick this client would like. The
//! game arrives at it by timing its own batches and aiming to spend about seven
//! milliseconds a tick on them, and so does this.

use std::time::Instant;

/// How long a batch should take, in nanoseconds. Seven milliseconds of a fifty
/// millisecond tick.
const TARGET_NANOS: f64 = 7_000_000.0;
/// A first guess at how long a chunk takes, before any have been timed.
const INITIAL_NANOS_PER_CHUNK: f64 = 2_000_000.0;
/// How far one batch is allowed to pull the running average, so a single
/// stalled batch does not collapse the rate.
const CLAMP: f64 = 3.0;
/// The running average settles once it has this many samples behind it.
const MAX_OLD_SAMPLES: u32 = 49;

#[derive(Debug)]
pub struct BatchRate {
    nanos_per_chunk: f64,
    samples: u32,
    started: Instant,
}

impl Default for BatchRate {
    fn default() -> Self {
        Self::new()
    }
}

impl BatchRate {
    pub fn new() -> Self {
        Self {
            nanos_per_chunk: INITIAL_NANOS_PER_CHUNK,
            samples: 1,
            started: Instant::now(),
        }
    }

    pub fn start(&mut self) {
        self.started = Instant::now();
    }

    /// Folds one finished batch into the running average.
    pub fn finish(&mut self, chunks: i32) {
        if chunks <= 0 {
            return;
        }
        let elapsed = self.started.elapsed().as_nanos() as f64;
        let per_chunk = elapsed / f64::from(chunks);
        let bounded = per_chunk.clamp(self.nanos_per_chunk / CLAMP, self.nanos_per_chunk * CLAMP);
        let weight = f64::from(self.samples);
        self.nanos_per_chunk = (self.nanos_per_chunk * weight + bounded) / (weight + 1.0);
        self.samples = (self.samples + 1).min(MAX_OLD_SAMPLES);
    }

    /// What to ask the server for.
    pub fn desired_per_tick(&self) -> f32 {
        (TARGET_NANOS / self.nanos_per_chunk) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_rate_asks_for_something_sensible() {
        let rate = BatchRate::new();
        let asked = rate.desired_per_tick();
        assert!((asked - 3.5).abs() < 0.01, "asked for {asked} chunks a tick");
    }

    #[test]
    fn a_fast_client_asks_for_more_than_a_slow_one() {
        // The clamp means one batch cannot move the average far, so this walks
        // several batches to show the direction of travel.
        let mut fast = BatchRate::new();
        let mut slow = BatchRate::new();
        for _ in 0..10 {
            fast.start();
            fast.finish(64);
            slow.start();
            std::thread::sleep(std::time::Duration::from_millis(2));
            slow.finish(1);
        }
        assert!(
            fast.desired_per_tick() > slow.desired_per_tick(),
            "fast asked {} against slow {}",
            fast.desired_per_tick(),
            slow.desired_per_tick()
        );
    }

    #[test]
    fn an_empty_batch_changes_nothing() {
        let mut rate = BatchRate::new();
        let before = rate.desired_per_tick();
        rate.finish(0);
        assert_eq!(rate.desired_per_tick(), before);
    }
}
