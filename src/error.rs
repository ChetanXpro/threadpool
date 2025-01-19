use std::error::Error;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ThreadPoolError {
    #[error("Threadpool size should be bigger then 0")]
    InvalidSize,
    #[error("Mutex was poisoned")]
    PoisonedLock,

    #[error("Thread pool is shutting down")]
    ShuttingDown,

    #[error("Failed to send task: receiver disconnected")]
    SendError,

    #[error("Thread already exited")]
    ThreadAlreadyExited,

    #[error("Error joining thread")]
    ErrorJoiningThread,
}
