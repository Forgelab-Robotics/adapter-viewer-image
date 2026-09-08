//! A bounded, latest-value mailbox with FIFO fairness between pending input IDs.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishResult {
    Queued,
    Replaced,
    Rejected,
    Closed,
}

/// Retains at most one pending value per ID. Replacements keep their FIFO position.
///
/// Limits apply only to pending values, not values already returned by `take`.
/// Byte sizes are supplied by the caller; IDs and container overhead are not counted.
/// Zero limits are supported: a zero byte limit permits only zero-byte values,
/// while a zero input limit rejects all publishes until the mailbox is closed.
pub struct LatestMailbox<T> {
    max_inputs: usize,
    max_bytes: usize,
    state: Mutex<State<T>>,
    ready: Condvar,
}

struct State<T> {
    pending: VecDeque<Entry<T>>,
    bytes: usize,
    closed: bool,
}

struct Entry<T> {
    id: String,
    value: T,
    bytes: usize,
}

impl<T> LatestMailbox<T> {
    pub fn new(max_inputs: usize, max_bytes: usize) -> Self {
        Self {
            max_inputs,
            max_bytes,
            state: Mutex::new(State {
                pending: VecDeque::new(),
                bytes: 0,
                closed: false,
            }),
            ready: Condvar::new(),
        }
    }

    /// Publishes without waiting for capacity. Rejection leaves pending data intact.
    /// A closed mailbox always returns `Closed`, regardless of the supplied size.
    pub fn publish(&self, id: String, value: T, bytes: usize) -> PublishResult {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return PublishResult::Closed;
        }

        let position = state.pending.iter().position(|entry| entry.id == id);
        if position.is_none() && state.pending.len() >= self.max_inputs {
            return PublishResult::Rejected;
        }
        let old_bytes = position.map_or(0, |index| state.pending[index].bytes);
        let Some(total_bytes) = (state.bytes - old_bytes).checked_add(bytes) else {
            return PublishResult::Rejected;
        };
        if total_bytes > self.max_bytes {
            return PublishResult::Rejected;
        }

        let entry = Entry { id, value, bytes };
        let replaced = if let Some(index) = position {
            Some(std::mem::replace(&mut state.pending[index], entry))
        } else {
            state.pending.push_back(entry);
            None
        };
        state.bytes = total_bytes;
        let result = if replaced.is_some() {
            PublishResult::Replaced
        } else {
            PublishResult::Queued
        };
        drop(state);
        self.ready.notify_one();
        // User-defined destructors must not run while the mailbox is locked.
        drop(replaced);
        result
    }

    /// Waits for one pending port, or returns `None` once closed.
    /// Other ports stay pending so publishers can replace them until selected.
    pub fn take(&self) -> Option<(String, T)> {
        let mut state = self.state.lock().unwrap();
        loop {
            if state.closed {
                return None;
            }
            if let Some(entry) = state.pending.pop_front() {
                state.bytes -= entry.bytes;
                return Some((entry.id, entry.value));
            }
            state = self.ready.wait(state).unwrap();
        }
    }

    /// Permanently closes the mailbox, discards pending data, and wakes all takers.
    /// Calling this more than once is harmless.
    pub fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        state.bytes = 0;
        let pending = std::mem::take(&mut state.pending);
        drop(state);
        self.ready.notify_all();
        drop(pending);
    }
}

#[cfg(test)]
mod tests {
    use super::{LatestMailbox, PublishResult::*};
    use std::sync::{Arc, Weak, mpsc};
    use std::thread;
    use std::time::Duration;

    const TIMEOUT: Duration = Duration::from_secs(5);

    #[test]
    fn latest_replaces_before_take() {
        let mailbox = LatestMailbox::new(1, 10);
        assert_eq!(mailbox.publish("input".into(), 1, 10), Queued);
        assert_eq!(mailbox.publish("input".into(), 2, 10), Replaced);
        assert_eq!(mailbox.publish("input".into(), 3, 10), Replaced);
        assert_eq!(mailbox.take(), Some(("input".into(), 3)));
        assert_eq!(mailbox.state.lock().unwrap().bytes, 0);
    }

    #[test]
    fn hot_port_preserves_fifo_and_does_not_starve_other_ports() {
        let mailbox = LatestMailbox::new(3, 3);
        assert_eq!(mailbox.publish("hot".into(), 1, 1), Queued);
        assert_eq!(mailbox.publish("second".into(), 1, 1), Queued);
        assert_eq!(mailbox.publish("third".into(), 1, 1), Queued);
        for sequence in 2..=100 {
            assert_eq!(mailbox.publish("hot".into(), sequence, 1), Replaced);
        }
        assert_eq!(mailbox.take(), Some(("hot".into(), 100)));
        assert_eq!(mailbox.publish("hot".into(), 101, 1), Queued);
        for sequence in 102..=200 {
            assert_eq!(mailbox.publish("hot".into(), sequence, 1), Replaced);
        }
        // These ports must still be pending, not batched out with the first take.
        assert_eq!(mailbox.publish("second".into(), 2, 1), Replaced);
        assert_eq!(mailbox.take(), Some(("second".into(), 2)));
        assert_eq!(mailbox.publish("third".into(), 3, 1), Replaced);
        assert_eq!(mailbox.take(), Some(("third".into(), 3)));
        assert_eq!(mailbox.take(), Some(("hot".into(), 200)));
    }

    #[test]
    fn count_and_byte_caps_account_for_replacements_and_takes() {
        let mailbox = LatestMailbox::new(2, 10);
        assert_eq!(mailbox.publish("a".into(), 1, 6), Queued);
        assert_eq!(mailbox.publish("b".into(), 2, 4), Queued);
        assert_eq!(mailbox.publish("a".into(), 99, 7), Rejected);
        assert_eq!(mailbox.publish("c".into(), 99, 0), Rejected);
        {
            let state = mailbox.state.lock().unwrap();
            assert_eq!(state.bytes, 10);
            assert_eq!(state.pending[0].value, 1);
            assert_eq!(state.pending[0].bytes, 6);
        }
        assert_eq!(mailbox.publish("a".into(), 3, 2), Replaced);
        assert_eq!(mailbox.state.lock().unwrap().bytes, 6);
        assert_eq!(mailbox.publish("b".into(), 4, 8), Replaced);
        assert_eq!(mailbox.publish("b".into(), 99, 9), Rejected);
        assert_eq!(mailbox.take(), Some(("a".into(), 3)));
        assert_eq!(mailbox.state.lock().unwrap().bytes, 8);
        assert_eq!(mailbox.publish("c".into(), 5, 3), Rejected);
        assert_eq!(mailbox.publish("c".into(), 5, 2), Queued);
        assert_eq!(mailbox.take(), Some(("b".into(), 4)));
        assert_eq!(mailbox.take(), Some(("c".into(), 5)));
        assert_eq!(mailbox.publish("a".into(), 6, 11), Rejected);
        assert_eq!(mailbox.publish("a".into(), 6, 10), Queued);
        assert_eq!(mailbox.take(), Some(("a".into(), 6)));
        assert_eq!(mailbox.state.lock().unwrap().bytes, 0);
    }

    #[test]
    fn byte_accounting_cannot_overflow() {
        let mailbox = LatestMailbox::new(2, usize::MAX);
        assert_eq!(mailbox.publish("a".into(), 1, usize::MAX), Queued);
        assert_eq!(mailbox.publish("b".into(), 2, 1), Rejected);
        assert_eq!(mailbox.publish("a".into(), 3, usize::MAX), Replaced);
        assert_eq!(mailbox.publish("a".into(), 4, usize::MAX - 1), Replaced);
        assert_eq!(mailbox.publish("b".into(), 5, 1), Queued);
        assert_eq!(mailbox.publish("a".into(), 6, usize::MAX), Rejected);
        assert_eq!(mailbox.take(), Some(("a".into(), 4)));
        assert_eq!(mailbox.state.lock().unwrap().bytes, 1);
        assert_eq!(mailbox.take(), Some(("b".into(), 5)));
        assert_eq!(mailbox.state.lock().unwrap().bytes, 0);
    }

    #[test]
    fn zero_limits_are_respected() {
        let no_inputs = LatestMailbox::new(0, usize::MAX);
        assert_eq!(no_inputs.publish("a".into(), (), 0), Rejected);
        let no_bytes = LatestMailbox::new(1, 0);
        assert_eq!(no_bytes.publish("a".into(), 1, 1), Rejected);
        assert_eq!(no_bytes.publish("a".into(), 2, 0), Queued);
        assert_eq!(no_bytes.publish("a".into(), 3, 0), Replaced);
        assert_eq!(no_bytes.publish("b".into(), 4, 0), Rejected);
        assert_eq!(no_bytes.take(), Some(("a".into(), 3)));
    }

    struct DropProbe {
        mailbox: Weak<LatestMailbox<DropProbe>>,
        dropped: mpsc::Sender<bool>,
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            let mailbox = self.mailbox.upgrade().unwrap();
            let unlocked = mailbox.state.try_lock().is_ok();
            self.dropped.send(unlocked).unwrap();
        }
    }

    #[test]
    fn discarded_values_drop_unlocked_and_close_clears_retained_data() {
        let mailbox = Arc::new(LatestMailbox::new(2, 10));
        let (dropped, drops) = mpsc::channel();
        let probe = || DropProbe {
            mailbox: Arc::downgrade(&mailbox),
            dropped: dropped.clone(),
        };
        assert_eq!(mailbox.publish("a".into(), probe(), 5), Queued);
        assert_eq!(mailbox.publish("a".into(), probe(), 5), Replaced);
        assert!(drops.recv_timeout(TIMEOUT).unwrap());
        assert_eq!(mailbox.publish("b".into(), probe(), 6), Rejected);
        assert!(drops.recv_timeout(TIMEOUT).unwrap());
        assert_eq!(mailbox.publish("b".into(), probe(), 5), Queued);
        mailbox.close();
        assert!(drops.recv_timeout(TIMEOUT).unwrap());
        assert!(drops.recv_timeout(TIMEOUT).unwrap());
        {
            let state = mailbox.state.lock().unwrap();
            assert!(state.closed);
            assert!(state.pending.is_empty());
            assert_eq!(state.bytes, 0);
        }
        assert!(mailbox.take().is_none());
        assert_eq!(mailbox.publish("a".into(), probe(), usize::MAX), Closed);
        assert!(drops.recv_timeout(TIMEOUT).unwrap());
        mailbox.close();
        assert!(matches!(drops.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }

    #[test]
    fn close_wakes_all_waiters() {
        let mailbox = Arc::new(LatestMailbox::<usize>::new(1, 1));
        let (started, starts) = mpsc::channel();
        let (finished, finishes) = mpsc::channel();
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let mailbox = Arc::clone(&mailbox);
                let started = started.clone();
                let finished = finished.clone();
                thread::spawn(move || {
                    started.send(()).unwrap();
                    finished.send(mailbox.take()).unwrap();
                })
            })
            .collect();
        for _ in 0..workers.len() {
            starts.recv_timeout(TIMEOUT).unwrap();
        }
        assert!(matches!(
            finishes.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        mailbox.close();
        for _ in 0..workers.len() {
            assert_eq!(finishes.recv_timeout(TIMEOUT).unwrap(), None);
        }
        for worker in workers {
            worker.join().unwrap();
        }
    }

    #[test]
    fn slow_decoder_gets_one_in_flight_then_latest() {
        let mailbox = Arc::new(LatestMailbox::new(1, 1));
        let (decoding, decoded) = mpsc::channel();
        let (resume, resumed) = mpsc::channel();
        let worker_mailbox = Arc::clone(&mailbox);
        let worker = thread::spawn(move || {
            let first = worker_mailbox.take().unwrap();
            decoding.send(first).unwrap();
            resumed.recv_timeout(TIMEOUT).unwrap();
            decoding.send(worker_mailbox.take().unwrap()).unwrap();
        });
        assert_eq!(mailbox.publish("input".into(), 1, 1), Queued);
        assert_eq!(decoded.recv_timeout(TIMEOUT).unwrap(), ("input".into(), 1));
        // The worker cannot select again until every replacement has completed.
        assert_eq!(mailbox.publish("input".into(), 2, 1), Queued);
        for sequence in 3..=100 {
            assert_eq!(mailbox.publish("input".into(), sequence, 1), Replaced);
        }
        resume.send(()).unwrap();
        assert_eq!(
            decoded.recv_timeout(TIMEOUT).unwrap(),
            ("input".into(), 100)
        );
        worker.join().unwrap();
        assert_eq!(mailbox.state.lock().unwrap().bytes, 0);
        mailbox.close();
    }
}
