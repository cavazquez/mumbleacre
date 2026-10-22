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
static ACRE_ACTIVE: AtomicBool = AtomicBool::new(false);
pub fn init() {
    CLOCK.get_or_init(Instant::now);
    ACRE_ACTIVE.store(false, Ordering::Release);
    FORCED.store(false, Ordering::Release);
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
pub fn set_acre_active(active: bool) {
    if ACRE_ACTIVE.swap(active, Ordering::AcqRel) != active {
        // A new session must earn a fresh capture lease.
        close();
    }
}
pub fn input(samples: &mut [i16], speech: bool) -> bool {
    if !FORCED.load(Ordering::Acquire) {
        NATIVE.store(speech, Ordering::Release);
    }
    if ACRE_ACTIVE.load(Ordering::Acquire) && elapsed() >= DEADLINE.load(Ordering::Acquire) {
        samples.fill(0);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_is_transparent_outside_acre_and_requires_a_new_lease_on_reconnect() {
        let _guard = crate::audio::TEST_LOCK.lock().unwrap();
        init();
        let original = [1000, -1000, 500, -500];
        let mut pcm = original;
        assert!(!input(&mut pcm, true));
        assert_eq!(pcm, original);
        assert!(native());

        set_acre_active(true);
        assert!(input(&mut pcm, true));
        assert_eq!(pcm, [0; 4]);
        renew();
        pcm = original;
        assert!(!input(&mut pcm, true));
        assert_eq!(pcm, original);

        set_acre_active(false);
        close();
        assert!(!input(&mut pcm, false));
        assert_eq!(pcm, original);
        assert!(!native());
        set_acre_active(true);
        assert!(input(&mut pcm, true));
        assert_eq!(pcm, [0; 4]);
        set_acre_active(false);
    }
}
