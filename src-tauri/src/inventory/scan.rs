use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::watch, time::Instant};

pub const DEFAULT_SCAN_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanAbort {
    Cancelled,
    TimedOut,
}

impl ScanAbort {
    pub fn code(self) -> &'static str {
        match self {
            Self::Cancelled => "SCAN_CANCELLED",
            Self::TimedOut => "SCAN_TIMEOUT",
        }
    }
}

#[derive(Clone, Default)]
pub struct ScanCoordinator {
    inner: Arc<Mutex<CoordinatorState>>,
}

#[derive(Default)]
struct CoordinatorState {
    next_id: u64,
    active: Option<ActiveScan>,
}

struct ActiveScan {
    id: u64,
    cancel: watch::Sender<bool>,
}

#[derive(Clone)]
pub struct ScanContext {
    cancelled: watch::Receiver<bool>,
    deadline: Instant,
}

#[derive(Clone)]
pub struct ScanCompletion {
    _lease: Arc<CompletionLease>,
}

struct CompletionLease {
    inner: Arc<Mutex<CoordinatorState>>,
    id: u64,
}

impl ScanCoordinator {
    pub fn begin(&self) -> Result<(ScanContext, ScanCompletion), String> {
        self.begin_with_timeout(DEFAULT_SCAN_TIMEOUT)
    }

    pub fn begin_with_timeout(
        &self,
        timeout: Duration,
    ) -> Result<(ScanContext, ScanCompletion), String> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| "SCAN_COORDINATOR_UNAVAILABLE".to_owned())?;
        if state.active.is_some() {
            return Err("SCAN_ALREADY_RUNNING".to_owned());
        }

        state.next_id = state.next_id.wrapping_add(1).max(1);
        let id = state.next_id;
        let (cancel, cancelled) = watch::channel(false);
        state.active = Some(ActiveScan { id, cancel });

        Ok((
            ScanContext {
                cancelled,
                deadline: Instant::now() + timeout,
            },
            ScanCompletion {
                _lease: Arc::new(CompletionLease {
                    inner: self.inner.clone(),
                    id,
                }),
            },
        ))
    }

    pub fn cancel(&self) -> Result<(), String> {
        let state = self
            .inner
            .lock()
            .map_err(|_| "SCAN_COORDINATOR_UNAVAILABLE".to_owned())?;
        let active = state
            .active
            .as_ref()
            .ok_or_else(|| "SCAN_NOT_RUNNING".to_owned())?;
        active
            .cancel
            .send(true)
            .map_err(|_| "SCAN_NOT_RUNNING".to_owned())
    }
}

impl ScanContext {
    pub fn check(&self) -> Result<(), ScanAbort> {
        if *self.cancelled.borrow() {
            return Err(ScanAbort::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(ScanAbort::TimedOut);
        }
        Ok(())
    }

    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    pub async fn race<F: Future>(&self, operation: F) -> Result<F::Output, ScanAbort> {
        self.check()?;
        let cancelled = self.cancelled.clone();
        tokio::select! {
            biased;
            _ = wait_until_cancelled(cancelled) => Err(ScanAbort::Cancelled),
            _ = tokio::time::sleep_until(self.deadline) => Err(ScanAbort::TimedOut),
            output = operation => {
                self.check()?;
                Ok(output)
            },
        }
    }
}

async fn wait_until_cancelled(mut cancelled: watch::Receiver<bool>) {
    loop {
        if *cancelled.borrow_and_update() {
            return;
        }
        if cancelled.changed().await.is_err() {
            return;
        }
    }
}

impl Drop for CompletionLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.inner.lock()
            && state
                .active
                .as_ref()
                .is_some_and(|active| active.id == self.id)
        {
            state.active = None;
        }
    }
}
