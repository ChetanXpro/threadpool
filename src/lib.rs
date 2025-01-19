use error::ThreadPoolError;
use log::error;
use std::{
    sync::Arc,
    thread::{self, JoinHandle},
};

use crossbeam::channel::{unbounded, Sender};
mod error;

type Job = Box<dyn FnOnce() + Send + 'static>;

pub struct Threadpool {
    workers: Vec<Worker>,
    sender: Option<Sender<Job>>,
}

struct Worker {
    _id: usize,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Threadpool {
    fn drop(&mut self) {
        drop(self.sender.take());

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

        for i in 0..size {
            let r = Arc::clone(&receiver);
            workers.push(Worker {
                _id: i,
                thread: Some(thread::spawn(move || loop {
                    let job = r.recv();

                    match job {
                        Ok(j) => j(),
                        Err(_) => {
                            // panic while executing job
                            // this thread will be dead to we need some functionality
                            // to re-activate this thread
                            break;
                        }
                    };
                })),
            });
        }

        Ok(Self {
            workers,
            sender: Some(tx),
        })
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
}
