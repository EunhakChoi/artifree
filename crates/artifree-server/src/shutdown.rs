//! Shared shutdown state for persistent servers and one-shot sessions.

use std::sync::Arc;

use tokio::sync::watch;

/// A retained request to stop. Once set, current and later waiters complete.
#[derive(Debug, Clone)]
pub struct StopLatch {
    flag: Arc<watch::Sender<bool>>,
}

impl Default for StopLatch {
    fn default() -> Self {
        Self::new()
    }
}

impl StopLatch {
    /// A latch that is not set.
    #[must_use]
    pub fn new() -> Self {
        Self {
            flag: Arc::new(watch::channel(false).0),
        }
    }

    /// Requests a stop.
    pub fn set(&self) {
        self.flag.send_replace(true);
    }

    /// Whether a stop was requested.
    #[must_use]
    pub fn is_set(&self) -> bool {
        *self.flag.borrow()
    }

    /// Completes once a stop was requested.
    pub async fn wait(&self) {
        let mut receiver = self.flag.subscribe();
        // The sender lives in this latch, so the channel cannot close.
        drop(receiver.wait_for(|set| *set).await);
    }
}

#[cfg(test)]
mod tests {
    use super::StopLatch;
    use std::time::Duration;

    #[tokio::test]
    async fn stop_completes_waiters_created_after_the_request()
    -> Result<(), tokio::time::error::Elapsed> {
        let stop = StopLatch::new();
        let earlier_listener = stop.clone();
        stop.set();
        let later_listener = stop.clone();

        tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(earlier_listener.wait(), later_listener.wait());
        })
        .await
    }
}
