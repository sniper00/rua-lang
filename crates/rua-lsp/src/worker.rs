//! Small bounded worker pool for protocol-neutral analysis jobs.

use std::thread::{self, JoinHandle};

use crossbeam_channel::{Sender, TrySendError, bounded};

type Job = Box<dyn FnOnce() + Send + 'static>;

pub(crate) struct WorkerPool {
    sender: Option<Sender<Job>>,
    workers: Vec<JoinHandle<()>>,
}

impl WorkerPool {
    pub(crate) fn new(worker_count: usize, queue_capacity: usize) -> Self {
        assert!(worker_count > 0);
        assert!(queue_capacity > 0);
        let (sender, receiver) = bounded::<Job>(queue_capacity);
        let workers = (0..worker_count)
            .map(|index| {
                let receiver = receiver.clone();
                thread::Builder::new()
                    .name(format!("rua-analysis-{index}"))
                    .spawn(move || {
                        while let Ok(job) = receiver.recv() {
                            job();
                        }
                    })
                    .expect("spawn Rua analysis worker")
            })
            .collect();
        Self {
            sender: Some(sender),
            workers,
        }
    }

    pub(crate) fn try_execute(
        &self,
        job: impl FnOnce() + Send + 'static,
    ) -> Result<(), WorkerQueueFull> {
        let sender = self.sender.as_ref().expect("worker pool is running");
        sender.try_send(Box::new(job)).map_err(|error| match error {
            TrySendError::Full(_) => WorkerQueueFull,
            TrySendError::Disconnected(_) => WorkerQueueFull,
        })
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.sender.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WorkerQueueFull;

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn executes_jobs_and_joins_on_drop() {
        let values = Arc::new(Mutex::new(Vec::new()));
        {
            let pool = WorkerPool::new(2, 4);
            for value in 0..4 {
                let values = Arc::clone(&values);
                pool.try_execute(move || values.lock().unwrap().push(value))
                    .unwrap();
            }
        }
        let mut values = values.lock().unwrap().clone();
        values.sort();
        assert_eq!(values, vec![0, 1, 2, 3]);
    }
}
