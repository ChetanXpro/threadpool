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

type Job = Box<dyn FnOnce() + Send + 'static>;

pub struct ThreadPoolMetrics {
    total_jobs_submitted: AtomicUsize,
    completed_jobs: AtomicUsize,
    peak_concurrent_jobs: AtomicUsize,
    failed_jobs: AtomicUsize,
}
struct ThreadPoolInner {
    job_count: Mutex<usize>,
    empty_condvar: Condvar,
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
    pub fn total_jobs_submitted(&self) -> usize {
        self.total_jobs_submitted.load(Ordering::Relaxed)
    }

    pub fn completed_jobs(&self) -> usize {
        self.completed_jobs.load(Ordering::Relaxed)
    }

    pub fn peak_concurrent_jobs(&self) -> usize {
        self.peak_concurrent_jobs.load(Ordering::Relaxed)
    }

    pub fn failed_jobs(&self) -> usize {
        self.failed_jobs.load(Ordering::Relaxed)
    }
}

impl ThreadPoolInner {
    fn start_job(&self) {
        let mut count = self.job_count.lock().unwrap();

        *count += 1;

        let current = *count;
        self.metrics
            .peak_concurrent_jobs
            .fetch_max(current, Ordering::Release);
    }

    fn finish_job(&self) {
        let mut count = self.job_count.lock().unwrap();

        *count -= 1;

        if *count == 0 {
            self.empty_condvar.notify_one();
        }
    }

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

pub struct Threadpool {
    workers: Vec<Worker>,
    sender: Option<Sender<Job>>,
    pool_inner: Arc<ThreadPoolInner>,
}

struct Worker {
    _id: usize,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Threadpool {
    fn drop(&mut self) {
        drop(self.sender.take());

        self.pool_inner.wait_empty(Duration::from_secs(5));

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

impl Threadpool {
    pub fn build(size: usize) -> Result<Self, ThreadPoolError> {
        if size <= 0 {
            return Err(ThreadPoolError::InvalidSize);
        }

        let (tx, rx) = unbounded::<Job>();

        let mut workers = Vec::new();

        let receiver = Arc::new(rx);
        let pool_inner = Arc::new(ThreadPoolInner::default());

        for i in 0..size {
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
        })
    }

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

    pub fn get_job_count(&self) -> usize {
        *self.pool_inner.job_count.lock().unwrap()
    }

    pub fn get_metrics(&self) -> &ThreadPoolMetrics {
        &self.pool_inner.metrics
    }
    pub fn join(&self, timeout: Duration) -> bool {
        self.pool_inner.wait_empty(timeout)
    }

    pub fn execute<F>(&self, f: F) -> Result<(), ThreadPoolError>
    where
        F: FnOnce() + Send + 'static,
    {
        let sender = self.sender.as_ref().ok_or(ThreadPoolError::ShuttingDown)?;

        sender
            .send(Box::new(f))
            .map_err(|_| ThreadPoolError::SendError)
    }

    pub fn shutdown(&mut self) -> bool {
        // Drop sender to stop accepting new jobs
        drop(self.sender.take());

        // Wait for existing jobs to complete
        let success = self.pool_inner.wait_empty(Duration::from_secs(5));

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

    #[test]
    fn pool_should_increment_count() {
        let count = Arc::new(AtomicU8::new(0));
        let pool = Threadpool::build(10).unwrap();
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
    fn get_correct_job_count() {
        let pool = Threadpool::build(10).unwrap();

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
        let pool = Threadpool::build(10).unwrap();

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
        let mut pool = Threadpool::build(2).unwrap();

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
        let mut pool = Threadpool::build(2).unwrap();
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
        let mut pool = Threadpool::build(2).unwrap();
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
        let mut pool = Threadpool::build(2).unwrap();
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
