use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

pub(crate) const MAX_PENDING_DISMISSALS: usize = 512;

#[derive(Default)]
struct Pending {
    deadlines: BTreeMap<(Instant, u32), u64>,
    by_id: HashMap<u32, Instant>,
    stopped: bool,
}

/// One deadline queue per adapter. A newer token for an ID supersedes its
/// previous timer; distinct IDs apply backpressure when the queue is full.
pub(crate) struct TimerQueue {
    pending: Mutex<Pending>,
    deadline_changed: Condvar,
    space_available: Condvar,
    capacity: usize,
}

impl TimerQueue {
    pub(crate) fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            pending: Mutex::new(Pending::default()),
            deadline_changed: Condvar::new(),
            space_available: Condvar::new(),
            capacity,
        }
    }

    pub(crate) fn schedule(&self, id: u32, token: u64, delay: Duration) -> io::Result<()> {
        let deadline = Instant::now() + delay;
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| io::Error::other("dismissal timer lock poisoned"))?;
        loop {
            if pending.stopped {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "timer worker stopped",
                ));
            }
            if let Some(old_deadline) = pending.by_id.remove(&id) {
                pending.deadlines.remove(&(old_deadline, id));
            }
            if pending.deadlines.len() < self.capacity {
                break;
            }
            pending = self
                .space_available
                .wait(pending)
                .map_err(|_| io::Error::other("dismissal timer lock poisoned"))?;
        }
        pending.deadlines.insert((deadline, id), token);
        pending.by_id.insert(id, deadline);
        self.deadline_changed.notify_one();
        Ok(())
    }

    pub(crate) fn run(&self, mut on_due: impl FnMut(u32, u64)) -> io::Result<()> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| io::Error::other("dismissal timer lock poisoned"))?;
        loop {
            if pending.stopped {
                return Ok(());
            }
            let Some((&(deadline, id), &token)) = pending.deadlines.first_key_value() else {
                pending = self
                    .deadline_changed
                    .wait(pending)
                    .map_err(|_| io::Error::other("dismissal timer lock poisoned"))?;
                continue;
            };
            let now = Instant::now();
            if deadline > now {
                (pending, _) = self
                    .deadline_changed
                    .wait_timeout(pending, deadline.duration_since(now))
                    .map_err(|_| io::Error::other("dismissal timer lock poisoned"))?;
                continue;
            }
            pending.deadlines.pop_first();
            pending.by_id.remove(&id);
            self.space_available.notify_all();
            drop(pending);
            on_due(id, token);
            pending = self
                .pending
                .lock()
                .map_err(|_| io::Error::other("dismissal timer lock poisoned"))?;
        }
    }

    pub(crate) fn stop(&self) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.stopped = true;
        pending.deadlines.clear();
        pending.by_id.clear();
        self.deadline_changed.notify_all();
        self.space_available.notify_all();
    }

    #[cfg(test)]
    fn pending_count(&self) -> usize {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .deadlines
            .len()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "bounded queue tests"
)]
mod tests {
    use std::sync::{Arc, mpsc};
    use std::thread;

    use super::*;

    #[test]
    fn burst_of_replacements_uses_one_queue_entry() {
        let queue = TimerQueue::new(MAX_PENDING_DISMISSALS);
        for token in 0..10_000 {
            queue.schedule(42, token, Duration::from_secs(30)).unwrap();
        }
        assert_eq!(queue.pending_count(), 1);
        assert_eq!(
            *queue
                .pending
                .lock()
                .unwrap()
                .deadlines
                .values()
                .next()
                .unwrap(),
            9_999
        );
    }

    #[test]
    fn full_queue_blocks_until_worker_frees_a_slot() {
        let queue = Arc::new(TimerQueue::new(4));
        for id in 0..4 {
            queue
                .schedule(id, id.into(), Duration::from_millis(80))
                .unwrap();
        }
        let (scheduled, result) = mpsc::channel();
        let sender_queue = Arc::clone(&queue);
        let sender = thread::spawn(move || {
            scheduled
                .send(sender_queue.schedule(4, 4, Duration::from_secs(1)))
                .unwrap();
        });
        assert!(result.recv_timeout(Duration::from_millis(20)).is_err());
        assert_eq!(queue.pending_count(), 4);

        let worker_queue = Arc::clone(&queue);
        let worker = thread::spawn(move || worker_queue.run(|_, _| {}));
        result
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert!(queue.pending_count() <= 4);
        sender.join().unwrap();
        queue.stop();
        worker.join().unwrap().unwrap();
    }
}
