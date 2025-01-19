//! Error types for the thread pool implementation.
//!
//! This module defines all possible errors that can occur during thread pool operations,
//! from creation to job execution and shutdown.

use thiserror::Error;

/// Errors that can occur during thread pool operations
#[derive(Error, Debug)]
pub enum ThreadPoolError {
    /// Attempted to create a thread pool with size 0 or less
    #[error("Threadpool size should be bigger than 0")]
    InvalidSize,

    /// A mutex was poisoned, typically due to a thread panic while holding the lock
    #[error("Mutex was poisoned")]
    PoisonedLock,

    /// The thread pool is shutting down and cannot accept new jobs
    #[error("Thread pool is shutting down")]
    ShuttingDown,

    /// Failed to send a job to a worker thread because the receiver was disconnected
    #[error("Failed to send task: receiver disconnected")]
    SendError,

    /// Attempted to access a thread that has already exited
    #[error("Thread already exited")]
    ThreadAlreadyExited,

    /// Failed to join a worker thread, typically due to a panic
    #[error("Error joining thread")]
    ErrorJoiningThread,
}
