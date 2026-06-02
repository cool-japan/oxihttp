//! Retry logic with exponential backoff for the HTTP client.

use std::collections::HashSet;
use std::time::Duration;

/// Configuration for automatic request retries.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts.
    pub max_retries: u32,
    /// Base delay for exponential backoff (e.g. 100ms).
    pub backoff_base: Duration,
    /// Maximum delay between retries.
    pub backoff_max: Duration,
    /// HTTP status codes that trigger a retry.
    pub retryable_status_codes: HashSet<u16>,
    /// Whether to retry on connection errors.
    pub retry_on_connection_error: bool,
    /// Whether to retry on timeout errors.
    pub retry_on_timeout: bool,
}

impl RetryPolicy {
    /// Create a new retry policy with the given maximum retry count.
    pub fn new(max_retries: u32) -> Self {
        let mut retryable = HashSet::new();
        // Common retryable status codes
        retryable.insert(429); // Too Many Requests
        retryable.insert(500); // Internal Server Error
        retryable.insert(502); // Bad Gateway
        retryable.insert(503); // Service Unavailable
        retryable.insert(504); // Gateway Timeout

        Self {
            max_retries,
            backoff_base: Duration::from_millis(100),
            backoff_max: Duration::from_secs(30),
            retryable_status_codes: retryable,
            retry_on_connection_error: true,
            retry_on_timeout: true,
        }
    }

    /// Set the base delay for exponential backoff.
    pub fn with_backoff_base(mut self, base: Duration) -> Self {
        self.backoff_base = base;
        self
    }

    /// Set the maximum delay between retries.
    pub fn with_backoff_max(mut self, max: Duration) -> Self {
        self.backoff_max = max;
        self
    }

    /// Set the retryable status codes.
    pub fn with_retryable_status_codes(mut self, codes: HashSet<u16>) -> Self {
        self.retryable_status_codes = codes;
        self
    }

    /// Add a retryable status code.
    pub fn add_retryable_status(mut self, code: u16) -> Self {
        self.retryable_status_codes.insert(code);
        self
    }

    /// Set whether to retry on connection errors.
    pub fn with_retry_on_connection_error(mut self, retry: bool) -> Self {
        self.retry_on_connection_error = retry;
        self
    }

    /// Set whether to retry on timeout errors.
    pub fn with_retry_on_timeout(mut self, retry: bool) -> Self {
        self.retry_on_timeout = retry;
        self
    }

    /// Returns `true` if the given status code should trigger a retry.
    pub fn should_retry_status(&self, status_code: u16) -> bool {
        self.retryable_status_codes.contains(&status_code)
    }

    /// Calculate the backoff delay for the given attempt number (0-indexed).
    pub fn backoff_delay(&self, attempt: u32) -> Duration {
        let delay = self
            .backoff_base
            .saturating_mul(2u32.saturating_pow(attempt));
        std::cmp::min(delay, self.backoff_max)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::new(3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_retry_policy() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_retries, 3);
        assert!(policy.should_retry_status(503));
        assert!(policy.should_retry_status(429));
        assert!(!policy.should_retry_status(404));
    }

    #[test]
    fn test_backoff_delay() {
        let policy = RetryPolicy::new(5).with_backoff_base(Duration::from_millis(100));
        assert_eq!(policy.backoff_delay(0), Duration::from_millis(100));
        assert_eq!(policy.backoff_delay(1), Duration::from_millis(200));
        assert_eq!(policy.backoff_delay(2), Duration::from_millis(400));
        assert_eq!(policy.backoff_delay(3), Duration::from_millis(800));
    }

    #[test]
    fn test_backoff_delay_capped() {
        let policy = RetryPolicy::new(5)
            .with_backoff_base(Duration::from_secs(1))
            .with_backoff_max(Duration::from_secs(5));
        assert_eq!(policy.backoff_delay(0), Duration::from_secs(1));
        assert_eq!(policy.backoff_delay(1), Duration::from_secs(2));
        assert_eq!(policy.backoff_delay(2), Duration::from_secs(4));
        assert_eq!(policy.backoff_delay(3), Duration::from_secs(5)); // capped
    }

    #[test]
    fn test_custom_retryable_status() {
        let policy = RetryPolicy::new(1).add_retryable_status(418);
        assert!(policy.should_retry_status(418));
        assert!(policy.should_retry_status(503)); // still has defaults
    }
}
