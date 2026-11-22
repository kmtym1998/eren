//! 一度に一つだけ実行し、実行待ちは最新の要求で置き換える。
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc,
};

#[derive(Clone)]
pub struct Cancellation {
    generation: Arc<AtomicU64>,
    requested: u64,
}
impl Cancellation {
    pub fn is_cancelled(&self) -> bool {
        self.generation.load(Ordering::Relaxed) != self.requested
    }
}

type Work<T> = Box<dyn FnOnce(Cancellation) -> T + Send>;

pub struct BackgroundTask<T> {
    generation: Arc<AtomicU64>,
    running: Option<(u64, mpsc::Receiver<T>)>,
    pending: Option<Work<T>>,
}
impl<T> Default for BackgroundTask<T> {
    fn default() -> Self {
        Self {
            generation: Arc::new(AtomicU64::new(0)),
            running: None,
            pending: None,
        }
    }
}
impl<T: Send + 'static> BackgroundTask<T> {
    pub fn submit(&mut self, work: impl FnOnce(Cancellation) -> T + Send + 'static) {
        self.invalidate();
        self.pending = Some(Box::new(work));
        self.start_pending();
    }
    pub fn invalidate(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.pending = None;
    }
    pub fn is_running(&self) -> bool {
        self.running.is_some() || self.pending.is_some()
    }
    fn start_pending(&mut self) {
        if self.running.is_some() {
            return;
        }
        let Some(work) = self.pending.take() else {
            return;
        };
        let requested = self.generation.load(Ordering::Relaxed);
        let cancellation = Cancellation {
            generation: self.generation.clone(),
            requested,
        };
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(work(cancellation));
        });
        self.running = Some((requested, receiver));
    }
    pub fn poll(&mut self) -> Option<Result<T, &'static str>> {
        let (requested, receiver) = self.running.as_ref()?;
        let result = match receiver.try_recv() {
            Ok(value) => Ok(value),
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Err("バックグラウンド処理が終了した"),
        };
        let current = *requested == self.generation.load(Ordering::Relaxed);
        self.running = None;
        self.start_pending();
        current.then_some(result)
    }
}
impl<T> Drop for BackgroundTask<T> {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    #[test]
    fn slow_work_does_not_block_poll_and_only_latest_pending_request_runs() {
        let mut task = BackgroundTask::default();
        let (release, gate) = mpsc::channel();
        let (started, ready) = mpsc::channel();
        task.submit(move |cancel| {
            started.send(()).unwrap();
            gate.recv().unwrap();
            assert!(cancel.is_cancelled());
            1
        });
        ready.recv_timeout(Duration::from_secs(3)).unwrap();
        task.submit(|_| panic!("中間要求は実行しない"));
        task.submit(|_| 3);
        assert!(task.poll().is_none()); // gate を閉じたまま UI が操作できる。
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let value = loop {
            if let Some(value) = task.poll() {
                break value.unwrap();
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        };
        assert_eq!(value, 3);
        assert!(!task.is_running());
    }
    #[test]
    fn project_invalidation_discards_late_result_without_starting_replacement() {
        let mut task = BackgroundTask::default();
        let (release, gate) = mpsc::channel();
        let (started, ready) = mpsc::channel();
        task.submit(move |cancel| {
            started.send(()).unwrap();
            gate.recv().unwrap();
            assert!(cancel.is_cancelled());
            42
        });
        ready.recv_timeout(Duration::from_secs(3)).unwrap();
        task.invalidate();
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while task.is_running() {
            assert!(task.poll().is_none());
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }

    #[test]
    fn invalidated_and_failed_workers_do_not_leave_task_busy() {
        let mut task = BackgroundTask::<()>::default();
        task.submit(|_| panic!("simulated worker failure"));
        let deadline = Instant::now() + Duration::from_secs(3);
        while task.poll().is_none() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(!task.is_running());
    }
}
