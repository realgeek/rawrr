use chrono::{DateTime, Duration, Utc};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tracing::debug;

pub struct RateLimiter {
    /// Queue of poll timestamps
    poll_times: Arc<Mutex<VecDeque<DateTime<Utc>>>>,
    /// Max polls within window
    max_polls: u32,
    /// Time window in seconds
    window_secs: u64,
}

impl RateLimiter {
    pub fn new(max_polls: u32, window_secs: u64) -> Self {
        RateLimiter {
            poll_times: Arc::new(Mutex::new(VecDeque::new())),
            max_polls,
            window_secs,
        }
    }
    
    /// Check if we can poll now without exceeding rate limit
    pub fn can_poll(&self) -> bool {
        let mut times = self.poll_times.lock().unwrap();
        let now = Utc::now();
        let cutoff = now - Duration::seconds(self.window_secs as i64);
        
        // Remove old entries outside the window
        while let Some(&front) = times.front() {
            if front < cutoff {
                times.pop_front();
            } else {
                break;
            }
        }
        
        let current_count = times.len() as u32;
        
        if current_count < self.max_polls {
            debug!(
                "Rate limit OK: {}/{} polls in current window",
                current_count, self.max_polls
            );
            true
        } else {
            debug!(
                "Rate limit exceeded: {}/{} polls in current window",
                current_count, self.max_polls
            );
            false
        }
    }
    
    /// Record that a poll happened
    pub fn record_poll(&self) {
        let mut times = self.poll_times.lock().unwrap();
        times.push_back(Utc::now());
        debug!("Recorded poll, total in window: {}", times.len());
    }
    
    /// Get seconds until next poll is allowed
    pub fn seconds_until_next_poll(&self) -> u64 {
        let times = self.poll_times.lock().unwrap();
        
        if times.is_empty() {
            return 0;
        }
        
        let oldest = times.front().unwrap();
        let cutoff = *oldest + Duration::seconds(self.window_secs as i64);
        let now = Utc::now();
        
        if now >= cutoff {
            0
        } else {
            (cutoff - now).num_seconds() as u64
        }
    }
}

impl Clone for RateLimiter {
    fn clone(&self) -> Self {
        RateLimiter {
            poll_times: Arc::clone(&self.poll_times),
            max_polls: self.max_polls,
            window_secs: self.window_secs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_rate_limiter_allows_under_limit() {
        let limiter = RateLimiter::new(3, 60);
        assert!(limiter.can_poll());
        limiter.record_poll();
        assert!(limiter.can_poll());
        limiter.record_poll();
        assert!(limiter.can_poll());
        limiter.record_poll();
        assert!(!limiter.can_poll());
    }

    #[test]
    fn test_seconds_until_next_poll_empty_queue() {
        let limiter = RateLimiter::new(10, 60);
        assert_eq!(limiter.seconds_until_next_poll(), 0);
    }

    #[test]
    fn test_seconds_until_next_poll_after_record() {
        let limiter = RateLimiter::new(10, 60);
        limiter.record_poll();
        // Should be somewhere between 0 and 60 seconds
        let secs = limiter.seconds_until_next_poll();
        assert!(secs <= 60);
    }

    #[test]
    fn test_clone_shares_state() {
        let limiter = RateLimiter::new(3, 60);
        let clone = limiter.clone();
        limiter.record_poll();
        clone.record_poll();
        clone.record_poll();
        // 3 polls recorded across both handles — limit is reached
        assert!(!limiter.can_poll());
    }
}
