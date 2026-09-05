//! Audio-input state is atomic: the input callback never enters the control queue.
use std::sync::{
    OnceLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};
static CLOCK: OnceLock<Instant> = OnceLock::new();
static FORCED: AtomicBool = AtomicBool::new(false);
static NATIVE: AtomicBool = AtomicBool::new(false);
static DEADLINE: AtomicU64 = AtomicU64::new(0);
pub fn init() {
    CLOCK.get_or_init(Instant::now);
    close();
    NATIVE.store(false, Ordering::Release);
}
fn elapsed() -> u64 {
    CLOCK.get().map_or(u64::MAX, |c| {
        c.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
    })
}
pub fn force(active: bool) -> bool {
    FORCED.swap(active, Ordering::AcqRel)
}
pub fn clear_native() {
    NATIVE.store(false, Ordering::Release);
}
pub fn native() -> bool {
    NATIVE.load(Ordering::Acquire)
}
pub fn close() {
    DEADLINE.store(0, Ordering::Release);
}
pub fn renew() {
    DEADLINE.store(
        elapsed().saturating_add(Duration::from_millis(250).as_millis() as u64),
        Ordering::Release,
    );
}
pub fn input(samples: &mut [i16], speech: bool) -> bool {
    if !FORCED.load(Ordering::Acquire) {
        NATIVE.store(speech, Ordering::Release);
    }
    if elapsed() >= DEADLINE.load(Ordering::Acquire) {
        samples.fill(0);
        true
    } else {
        false
    }
}
