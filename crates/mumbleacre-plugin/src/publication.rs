//! Keep old publications alive until control can reclaim them off the audio thread.
use arc_swap::ArcSwap;
use std::sync::{Arc, Mutex};
pub struct Publication<T> {
    current: ArcSwap<T>,
    retired: Mutex<Vec<Arc<T>>>,
}
impl<T> Publication<T> {
    pub fn new(value: T) -> Self {
        Self {
            current: ArcSwap::from_pointee(value),
            retired: Mutex::new(Vec::new()),
        }
    }
    pub fn load_full(&self) -> Arc<T> {
        self.current.load_full()
    }
    pub fn store(&self, value: Arc<T>) {
        let mut retired = self
            .retired
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Readers only acquire the current publication. Once retired and
        // uniquely held here, no reader can acquire this value again.
        retired.retain(|old| Arc::strong_count(old) > 1);
        retired.push(self.current.swap(value));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_snapshot_is_reclaimed_on_publisher() {
        let publication = Publication::new(1);
        let reader = publication.load_full();
        publication.store(Arc::new(2));
        assert_eq!(*reader, 1);
        assert_eq!(Arc::strong_count(&reader), 2);
        drop(reader);
        publication.store(Arc::new(3));
        assert_eq!(publication.retired.lock().unwrap().len(), 1);
    }
}
