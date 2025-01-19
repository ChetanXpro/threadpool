//! A flexible thread pool implementation for executing jobs concurrently.
//!
//! This thread pool manages a configurable number of worker threads and allows
//! submitting jobs for execution. It provides metrics tracking and graceful shutdown.
//!
//! # Configuration
//! The thread pool can be configured with:
//! - Number of worker threads
//! - Timeout duration for shutdown and join operations
//!
//! # Example
//! ```
//! use std::time::Duration;
//! use threadpool::{Threadpool, ThreadPoolConfig};
//!
//! // Create a pool with custom configuration
//! let config = ThreadPoolConfig {
//!     size: 4,
//!     timeout: Duration::from_secs(5),
//! };
//! let mut pool = Threadpool::build(config).unwrap();
//!
//! // Execute tasks
//! pool.execute(|| println!("Task 1")).unwrap();
//! pool.execute(|| println!("Task 2")).unwrap();
//!
//! // Shutdown gracefully
//! pool.shutdown();
//! ```
use crossbeam::channel::{unbounded, Receiver, Sender};
use error::ThreadPoolError;
use log::error;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
mod error;

/// Represents a task that can be executed by the thread pool.
/// Must be Send to allow transfer between threads and 'static to ensure it lives long enough.
type Job = Box<dyn FnOnce() + Send + 'static>;

/// Tracks the basic metrics of the thread pool
pub struct ThreadPoolMetrics {
    /// Total number of jobs that have been submitted to the pool
    total_jobs_submitted: AtomicUsize,
    /// Number of jobs that have completed successfully
    completed_jobs: AtomicUsize,
    /// Highest number of concurrent jobs observed
    peak_concurrent_jobs: AtomicUsize,
    /// Number of jobs that failed due to panics
    failed_jobs: AtomicUsize,
}

/// Inner struct to hold the metrics and synchronization primitives
struct ThreadPoolInner {
    /// Current number of jobs in the pool
    job_count: Mutex<usize>,
    /// Condition variable for waiting until pool is empty
    empty_condvar: Condvar,
    /// Pool performance metrics
    metrics: ThreadPoolMetrics,
}

impl Default for ThreadPoolMetrics {
    fn default() -> Self {
        Self {
            total_jobs_submitted: AtomicUsize::new(0),
            peak_concurrent_jobs: AtomicUsize::new(0),
            completed_jobs: AtomicUsize::new(0),
            failed_jobs: AtomicUsize::new(0),
        }
    }
}
impl Default for ThreadPoolInner {
    fn default() -> Self {
        Self {
            empty_condvar: Condvar::new(),
            job_count: Mutex::new(0),
            metrics: ThreadPoolMetrics::default(),
        }
    }
}

impl ThreadPoolMetrics {
    /// Returns the total number of jobs submitted to the pool
    pub fn total_jobs_submitted(&self) -> usize {
        self.total_jobs_submitted.load(Ordering::Relaxed)
    }

    /// Returns the number of successfully completed jobs
    pub fn completed_jobs(&self) -> usize {
        self.completed_jobs.load(Ordering::Relaxed)
    }

    /// Returns the highest number of concurrent jobs observed
    pub fn peak_concurrent_jobs(&self) -> usize {
        self.peak_concurrent_jobs.load(Ordering::Relaxed)
    }

    /// Returns the number of jobs that failed due to panics
    pub fn failed_jobs(&self) -> usize {
        self.failed_jobs.load(Ordering::Relaxed)
    }
}

impl ThreadPoolInner {
    /// Increments the job count and updates peak concurrent jobs metric
    fn start_job(&self) {
        let mut count = self.job_count.lock().unwrap();

        *count += 1;

        let current = *count;
        self.metrics
            .peak_concurrent_jobs
            .fetch_max(current, Ordering::Release);
    }

    /// Decrements the job count and notifies waiting threads if count reaches zero
    fn finish_job(&self) {
        let mut count = self.job_count.lock().unwrap();

        *count -= 1;

        if *count == 0 {
            self.empty_condvar.notify_one();
        }
    }

    /// Wait for the job count to be zero
    /// Returns true if the job count is zero before the timeout
    /// Need to call this before shutdown to make sure all the jobs are completed
    fn wait_empty(&self, timeout: Duration) -> bool {
        let mut count = self.job_count.lock().unwrap();

        while *count > 0 {
            let (new_count, result) = self.empty_condvar.wait_timeout(count, timeout).unwrap();

            if result.timed_out() {
                return false;
            }
            count = new_count
        }

        true
    }
}

/// Configuration options for creating a thread pool
#[derive(Debug)]
pub struct ThreadPoolConfig {
    /// Number of worker threads to create in the pool
    /// Defaults to number of CPU cores
    pub size: usize,
    /// Timeout duration for shutdown and join operations
    /// Defaults to 5 seconds
    pub timeout: Duration,
}
impl Default for ThreadPoolConfig {
    fn default() -> Self {
        Self {
            size: num_cpus::get(), // Would need to add num_cpus dependency
            timeout: Duration::from_secs(5),
        }
    }
}

/// Threadpool struct to manage the worker threads
/// The worker threads are created when the pool is created
/// The pool will accept the jobs and assign them to the worker threads
pub struct Threadpool {
    workers: Vec<Worker>,
    sender: Option<Sender<Job>>,
    pool_inner: Arc<ThreadPoolInner>,
    timeout: Duration,
}

/// A worker thread that executes jobs from the thread pool
struct Worker {
    /// Unique identifier for the worker thread
    _id: usize,
    /// Handle to the worker's thread, wrapped in Option to allow clean shutdown
    thread: Option<JoinHandle<()>>,
}

/// Drop implementation for the threadpool
/// This will make sure all the jobs are completed before dropping the pool
impl Drop for Threadpool {
    fn drop(&mut self) {
        // Drop the sender to stop accepting new jobs
        drop(self.sender.take());

        self.pool_inner.wait_empty(self.timeout);

        for worker in &mut self.workers {
            let thread = match worker.thread.take() {
                Some(thread) => thread,
                None => continue,
            };

            if let Err(err) = thread.join() {
                error!("Worker thread panicled during shutdown: {:?}", err);
            }
        }
    }
}

/// Implementation of the threadpool
/// The threadpool will create the worker threads and accept the jobs
impl Threadpool {
    /// Creates a new thread pool with default configuration.
    /// Uses number of CPUs for thread count and 5 second timeout.
    ///
    /// # Example
    /// ```
    /// use threadpool::Threadpool;
    ///
    /// let pool = Threadpool::new().unwrap();
    /// ```
    pub fn new() -> Result<Self, ThreadPoolError> {
        Self::build(ThreadPoolConfig::default())
    }

    /// Creates a new thread pool with the specified configuration.
    ///
    /// # Arguments
    /// * `config` - Configuration options for the thread pool
    ///
    /// # Errors
    /// Returns `ThreadPoolError::InvalidSize` if the configured size is 0
    ///
    /// # Example
    /// ```
    /// use std::time::Duration;
    /// use threadpool::{Threadpool, ThreadPoolConfig};
    ///
    /// let config = ThreadPoolConfig {
    ///     size: 4,
    ///     timeout: Duration::from_secs(5),
    /// };
    /// let pool = Threadpool::build(config).unwrap();
    /// ```
    pub fn build(config: ThreadPoolConfig) -> Result<Self, ThreadPoolError> {
        if config.size == 0 {
            return Err(ThreadPoolError::InvalidSize);
        }

        let (tx, rx) = unbounded::<Job>();

        let mut workers = Vec::new();

        let receiver = Arc::new(rx);
        let pool_inner = Arc::new(ThreadPoolInner::default());

        for i in 0..config.size {
            let r = Arc::clone(&receiver);
            let pool_inner = Arc::clone(&pool_inner);

            workers.push(Worker {
                _id: i,
                thread: Some(Self::spawn_worker_thread(r, pool_inner)),
            })
        }

        Ok(Self {
            workers,
            sender: Some(tx),
            pool_inner,
            timeout: config.timeout,
        })
    }

    /// Spawns a worker thread that processes jobs from the channel
    ///
    /// # Arguments
    /// * `r` - The receiver end of the job channel
    /// * `pool_inner` - Shared pool state for metrics and synchronization
    fn spawn_worker_thread(
        r: Arc<Receiver<Job>>,
        pool_inner: Arc<ThreadPoolInner>,
    ) -> JoinHandle<()> {
        thread::spawn(move || loop {
            let job_result = r.recv();

            let job = match job_result {
                Ok(j) => j,
                Err(_) => break,
            };

            pool_inner
                .metrics
                .total_jobs_submitted
                .fetch_add(1, Ordering::Relaxed);
            pool_inner.start_job();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                job();
            }));

            match result {
                Ok(_) => {
                    pool_inner
                        .metrics
                        .completed_jobs
                        .fetch_add(1, Ordering::Relaxed);
                    pool_inner.finish_job();
                }
                Err(_) => {
                    pool_inner
                        .metrics
                        .failed_jobs
                        .fetch_add(1, Ordering::Relaxed);
                    pool_inner.finish_job();
                }
            }
        })
    }

    /// Returns the current number of jobs in the pool
    /// This includes both running and queued jobs
    pub fn get_job_count(&self) -> usize {
        *self.pool_inner.job_count.lock().unwrap()
    }

    /// Returns a reference to the pool's metrics
    pub fn get_metrics(&self) -> &ThreadPoolMetrics {
        &self.pool_inner.metrics
    }

    /// Waits for all jobs to complete with a timeout
    ///
    /// # Arguments
    /// * `timeout` - Maximum time to wait for jobs to complete
    ///
    /// # Returns
    /// `true` if all jobs completed, `false` if timeout occurred
    pub fn join(&self, timeout: Duration) -> bool {
        self.pool_inner.wait_empty(timeout)
    }

    /// Executes a function in the thread pool.
    ///
    /// The function will be run on one of the pool's worker threads.
    /// If all workers are busy, the job will be queued.
    ///
    /// # Arguments
    /// * `f` - The function to execute, must implement `FnOnce() + Send + 'static`
    ///
    /// # Errors
    /// * `ThreadPoolError::ShuttingDown` if the pool is shutting down
    /// * `ThreadPoolError::SendError` if the job couldn't be sent to a worker
    ///
    /// # Example
    /// ```
    /// use std::time::Duration;
    /// use threadpool::{Threadpool, ThreadPoolConfig};
    ///
    /// let config = ThreadPoolConfig {
    ///     size: 4,
    ///     timeout: Duration::from_secs(5),
    /// };
    /// let pool = Threadpool::build(config).unwrap();
    ///
    /// // Execute a simple task
    /// pool.execute(|| {
    ///     println!("Hello from a pool thread!");
    /// }).unwrap();
    ///
    /// // Execute a task that returns a value using a channel
    /// use std::sync::mpsc::channel;
    /// let (tx, rx) = channel();
    /// pool.execute(move || {
    ///     let result = 42; // Some computation
    ///     tx.send(result).unwrap();
    /// }).unwrap();
    /// ```
    pub fn execute<F>(&self, f: F) -> Result<(), ThreadPoolError>
    where
        F: FnOnce() + Send + 'static,
    {
        // Get the sender from the pool if it is not shutdown
        let sender = self.sender.as_ref().ok_or(ThreadPoolError::ShuttingDown)?;

        sender
            .send(Box::new(f))
            .map_err(|_| ThreadPoolError::SendError)
    }

    /// Initiates a graceful shutdown of the thread pool
    ///
    /// Stops accepting new jobs and waits for existing jobs to complete
    ///
    /// # Returns
    /// `true` if all jobs completed successfully, `false` if timeout occurred
    pub fn shutdown(&mut self) -> bool {
        // Drop sender to stop accepting new jobs
        drop(self.sender.take());

        // Wait for existing jobs to complete
        let success = self.pool_inner.wait_empty(self.timeout);

        // Join all worker threads
        for worker in &mut self.workers {
            if let Some(thread) = worker.thread.take() {
                if let Err(err) = thread.join() {
                    error!("Worker thread panicked during shutdown: {:?}", err);
                }
            }
        }

        success
    }
}

#[cfg(test)]
mod test {
    use std::sync::atomic::{AtomicU8, Ordering};

    use super::*;

    fn get_test_config() -> ThreadPoolConfig {
        ThreadPoolConfig {
            size: 10,
            timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn default_config_should_work() {
        let pool = Threadpool::new().unwrap();
        let counter = Arc::new(AtomicU8::new(0));
        let counter_clone = Arc::clone(&counter);

        pool.execute(move || {
            counter_clone.fetch_add(1, Ordering::Release);
        })
        .unwrap();

        // Wait for the job to complete
        assert!(pool.join(Duration::from_secs(5)));

        // Add a small delay to ensure memory ordering
        thread::sleep(Duration::from_millis(10));

        let result = counter.load(Ordering::Acquire);
        assert_eq!(result, 1, "Expected counter to be 1, got {}", result);
    }

    #[test]
    fn multiple_jobs_should_complete_successfully() {
        let count = Arc::new(AtomicU8::new(0));
        let pool = Threadpool::build(get_test_config()).unwrap();
        let counter1 = Arc::clone(&count);
        let counter2 = Arc::clone(&count);
        pool.execute(move || {
            thread::sleep(Duration::from_millis(10));

            counter1.fetch_add(1, Ordering::Release);
        })
        .unwrap();

        pool.execute(move || {
            thread::sleep(Duration::from_millis(10));

            counter2.fetch_add(1, Ordering::Release);
        })
        .unwrap();
        assert!(pool.join(Duration::from_secs(5)), "Pool join timed out");

        thread::sleep(Duration::from_millis(20));

        let last_count = count.load(Ordering::Acquire);
        assert_eq!(last_count, 2, "Expected count to be 2, got {}", last_count);
    }

    #[test]
    fn should_track_running_job_count() {
        let pool = Threadpool::build(get_test_config()).unwrap();

        pool.execute(move || {
            thread::sleep(Duration::from_millis(10));
        })
        .unwrap();

        pool.execute(move || {
            thread::sleep(Duration::from_millis(10));
        })
        .unwrap();

        thread::sleep(Duration::from_millis(5));
        assert_eq!(pool.get_job_count(), 2);

        assert!(pool.join(Duration::from_secs(5)), "Pool join timed out");
    }

    #[test]
    fn timeout_should_work() {
        let pool = Threadpool::build(get_test_config()).unwrap();

        pool.execute(move || {
            thread::sleep(Duration::from_millis(100));
        })
        .unwrap();

        pool.execute(move || {
            thread::sleep(Duration::from_millis(100));
        })
        .unwrap();

        thread::sleep(Duration::from_millis(5));

        assert_eq!(pool.join(Duration::from_millis(10)), false);
    }

    #[test]
    fn shutdown_should_reject_new_jobs() {
        let mut pool = Threadpool::build(get_test_config()).unwrap();

        // Submit initial job
        pool.execute(|| thread::sleep(Duration::from_millis(10)))
            .unwrap();

        // after shutdown
        assert!(pool.shutdown());

        // new job should be rejected
        let result = pool.execute(|| println!("shouldn't run"));
        assert!(matches!(result, Err(ThreadPoolError::ShuttingDown)));
    }

    #[test]
    fn pool_should_handle_panic() {
        let mut pool = Threadpool::build(get_test_config()).unwrap();
        let counter = Arc::new(AtomicU8::new(0));
        let counter_clone = Arc::clone(&counter);

        // Submit a job that panics
        pool.execute(|| panic!("intentional panic")).unwrap();

        // Submit another normal job
        pool.execute(move || {
            counter_clone.fetch_add(1, Ordering::Release);
        })
        .unwrap();

        // Pool should handle the panic and complete the second job
        assert!(pool.shutdown());
        assert_eq!(counter.load(Ordering::Acquire), 1);
    }

    #[test]
    fn pool_should_complete_queued_jobs_during_shutdown() {
        let mut pool = Threadpool::build(get_test_config()).unwrap();
        let counter = Arc::new(AtomicU8::new(0));

        // Queue up several jobs
        for _ in 0..5 {
            let counter_clone = Arc::clone(&counter);
            pool.execute(move || {
                thread::sleep(Duration::from_millis(10));
                counter_clone.fetch_add(1, Ordering::Release);
            })
            .unwrap();
        }

        // Immediate shutdown should still process queued jobs
        assert!(pool.shutdown());
        assert_eq!(counter.load(Ordering::Acquire), 5);
    }

    #[test]
    fn pool_metrics_test() {
        let mut pool = Threadpool::build(get_test_config()).unwrap();
        let counter = Arc::new(AtomicU8::new(0));
        let counter_clone = Arc::clone(&counter);

        // Submit a job that panics
        pool.execute(|| panic!("intentional panic")).unwrap();

        // Submit another normal job
        pool.execute(move || {
            counter_clone.fetch_add(1, Ordering::Release);
        })
        .unwrap();

        // Pool should handle the panic and complete the second job
        assert!(pool.shutdown());

        let metrics = pool.get_metrics();
        assert_eq!(metrics.completed_jobs(), 1);
        assert_eq!(metrics.failed_jobs(), 1);
        assert_eq!(metrics.total_jobs_submitted(), 2);
    }
}
