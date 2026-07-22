use std::sync::{Arc, Mutex, MutexGuard};

struct LimiterState {
    max: usize,
    active: usize,
}

#[derive(Clone)]
pub struct ConcurrencyLimiter {
    state: Arc<Mutex<LimiterState>>,
}

pub struct ConcurrencyPermit {
    state: Option<Arc<Mutex<LimiterState>>>,
}

impl ConcurrencyLimiter {
    pub fn new(max: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(LimiterState { max, active: 0 })),
        }
    }

    pub fn try_acquire(&self) -> Option<ConcurrencyPermit> {
        let mut state = lock_state(&self.state);
        if state.max != 0 && state.active >= state.max {
            return None;
        }

        state.active += 1;
        Some(ConcurrencyPermit {
            state: Some(Arc::clone(&self.state)),
        })
    }

    pub fn set_limit(&self, max: usize) {
        lock_state(&self.state).max = max;
    }

    pub fn limit(&self) -> usize {
        lock_state(&self.state).max
    }
}

impl Drop for ConcurrencyPermit {
    fn drop(&mut self) {
        let Some(state) = self.state.take() else {
            return;
        };
        let mut state = lock_state(&state);
        if let Some(active) = state.active.checked_sub(1) {
            state.active = active;
        }
    }
}

fn lock_state(state: &Mutex<LimiterState>) -> MutexGuard<'_, LimiterState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::ConcurrencyLimiter;

    #[test]
    fn limit_two_rejects_third_permit() {
        let limiter = ConcurrencyLimiter::new(2);

        let _first = limiter.try_acquire().expect("first permit should succeed");
        let _second = limiter.try_acquire().expect("second permit should succeed");

        assert!(limiter.try_acquire().is_none());
    }

    #[test]
    fn dropping_permit_allows_next_acquisition() {
        let limiter = ConcurrencyLimiter::new(1);
        let permit = limiter.try_acquire().expect("permit should succeed");
        assert!(limiter.try_acquire().is_none());

        drop(permit);

        assert!(limiter.try_acquire().is_some());
    }

    #[test]
    fn zero_limit_allows_multiple_active_permits() {
        let limiter = ConcurrencyLimiter::new(0);
        let mut permits = Vec::new();

        for _ in 0..100 {
            permits.push(
                limiter
                    .try_acquire()
                    .expect("unlimited limiter should always grant permits"),
            );
        }

        assert_eq!(permits.len(), 100);
    }

    #[test]
    fn lowering_limit_keeps_existing_permits_and_rejects_new_ones() {
        let limiter = ConcurrencyLimiter::new(3);
        let first = limiter.try_acquire().expect("first permit should succeed");
        let second = limiter.try_acquire().expect("second permit should succeed");
        let third = limiter.try_acquire().expect("third permit should succeed");

        limiter.set_limit(1);

        assert_eq!(limiter.limit(), 1);
        assert!(limiter.try_acquire().is_none());
        drop(first);
        drop(second);
        assert!(limiter.try_acquire().is_none());
        drop(third);
        assert!(limiter.try_acquire().is_some());
    }

    #[test]
    fn raising_limit_allows_new_permits_immediately() {
        let limiter = ConcurrencyLimiter::new(1);
        let _first = limiter.try_acquire().expect("first permit should succeed");
        assert!(limiter.try_acquire().is_none());

        limiter.set_limit(3);

        assert_eq!(limiter.limit(), 3);
        let _second = limiter.try_acquire().expect("second permit should succeed");
        let _third = limiter.try_acquire().expect("third permit should succeed");
        assert!(limiter.try_acquire().is_none());
    }

    #[test]
    fn poisoned_mutex_is_recovered_for_drop_and_future_acquisitions() {
        let limiter = ConcurrencyLimiter::new(1);
        let permit = limiter.try_acquire().expect("permit should succeed");
        let poison_target = limiter.clone();

        let panic_result = std::thread::spawn(move || {
            let _guard = poison_target.state.lock().unwrap();
            panic!("poison limiter mutex");
        })
        .join();
        assert!(panic_result.is_err());

        drop(permit);

        assert!(limiter.try_acquire().is_some());
    }
}
