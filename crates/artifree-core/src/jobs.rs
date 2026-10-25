//! Planned: transactional event outbox, job lifecycle, retry, and reconnect.
//! A UI cancellation request must not imply that an external side effect was
//! rolled back. No task executor or async runtime is selected yet.
