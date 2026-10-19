use crate::capabilities::{CapabilityState, EvidenceLevel};
use crate::inventory::{DeviceKind, InventorySnapshot};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(windows)]
mod windows;

const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);
const RING_CAPACITY: usize = 120;
const MAX_BACKOFF: Duration = Duration::from_secs(30);

pub type SampleKey = (Option<String>, String);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricSample {
    pub device_id: Option<String>,
    pub metric_key: String,
    pub state: CapabilityState,
    pub value: Option<Value>,
    pub unit: Option<String>,
    pub source: String,
    pub evidence_level: Option<EvidenceLevel>,
    pub observed_at_utc: Option<String>,
    pub reason_code: Option<String>,
    pub native_error_code: Option<String>,
    pub details: Value,
}

impl MetricSample {
    pub fn validate(&self) -> Result<(), String> {
        if self.state == CapabilityState::Available
            && (self.value.is_none()
                || self.source.trim().is_empty()
                || self.observed_at_utc.is_none())
        {
            return Err("TELEMETRY_AVAILABLE_EVIDENCE_INCOMPLETE".to_owned());
        }
        if self.state != CapabilityState::Available && self.value.is_some() {
            return Err("TELEMETRY_NON_AVAILABLE_HAS_VALUE".to_owned());
        }
        Ok(())
    }

    fn key(&self) -> SampleKey {
        (self.device_id.clone(), self.metric_key.clone())
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryFrame {
    pub monitoring: MonitoringStatus,
    pub samples: Vec<MetricSample>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitoringStatus {
    pub running: bool,
    pub foreground: bool,
    pub subscription_count: usize,
    pub sample_interval_ms: u64,
    pub latest_observed_at_utc: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CounterRateError {
    CounterReset,
    InvalidInterval,
}

pub fn counter_rate(
    previous: u64,
    current: u64,
    elapsed: Duration,
) -> Result<f64, CounterRateError> {
    if elapsed.is_zero() {
        return Err(CounterRateError::InvalidInterval);
    }
    let delta = current
        .checked_sub(previous)
        .ok_or(CounterRateError::CounterReset)?;
    let seconds = elapsed.as_secs_f64();
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(CounterRateError::InvalidInterval);
    }
    Ok(delta as f64 / seconds)
}

#[derive(Default)]
pub struct SampleRing {
    capacity: usize,
    entries: HashMap<SampleKey, VecDeque<MetricSample>>,
}

impl SampleRing {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: HashMap::new(),
        }
    }

    pub fn push(&mut self, sample: MetricSample) {
        let points = self.entries.entry(sample.key()).or_default();
        points.push_back(sample);
        while points.len() > self.capacity {
            points.pop_front();
        }
    }

    pub fn recent(&self, device_ids: Option<&HashSet<String>>) -> Vec<MetricSample> {
        let mut samples = self
            .entries
            .iter()
            .filter(|((device_id, _), _)| {
                device_ids.is_none_or(|ids| device_id.as_ref().is_some_and(|id| ids.contains(id)))
            })
            .flat_map(|(_, samples)| samples.iter().cloned())
            .collect::<Vec<_>>();
        samples.sort_by(|left, right| {
            left.observed_at_utc
                .cmp(&right.observed_at_utc)
                .then_with(|| left.metric_key.cmp(&right.metric_key))
        });
        samples
    }

    fn latest_observed_at(&self) -> Option<String> {
        self.entries
            .values()
            .filter_map(|samples| samples.back()?.observed_at_utc.clone())
            .max()
    }
}

#[derive(Clone)]
pub struct TelemetryManager {
    inner: Arc<ManagerInner>,
}

struct ManagerInner {
    state: Mutex<ManagerState>,
    wake: Condvar,
    worker: Mutex<Option<JoinHandle<()>>>,
}

struct ManagerState {
    inventory: Option<InventorySnapshot>,
    subscriptions: HashSet<String>,
    running: bool,
    foreground: bool,
    generation: u64,
    ring: SampleRing,
}

#[derive(Clone, Copy, Default)]
struct FailureBackoff {
    failures: u32,
    retry_at: Option<Instant>,
    permanent: bool,
}

impl TelemetryManager {
    pub fn new(inventory: Option<InventorySnapshot>) -> Self {
        Self {
            inner: Arc::new(ManagerInner {
                state: Mutex::new(ManagerState {
                    inventory,
                    subscriptions: HashSet::new(),
                    running: false,
                    foreground: true,
                    generation: 0,
                    ring: SampleRing::new(RING_CAPACITY),
                }),
                wake: Condvar::new(),
                worker: Mutex::new(None),
            }),
        }
    }

    pub fn configure_inventory(&self, inventory: InventorySnapshot) -> Result<(), String> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| "TELEMETRY_STATE_LOCK_POISONED".to_owned())?;
        state.inventory = Some(inventory);
        let valid_ids = state
            .inventory
            .as_ref()
            .map(|inventory| {
                inventory
                    .devices
                    .iter()
                    .filter(|device| {
                        supports_telemetry(device.kind)
                            && device.is_present == Some(true)
                            && device.is_virtual != Some(true)
                    })
                    .map(|device| device.id.clone())
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        state
            .subscriptions
            .retain(|device_id| valid_ids.contains(device_id));
        state.generation = state.generation.wrapping_add(1);
        self.inner.wake.notify_all();
        Ok(())
    }

    pub fn start(&self, device_ids: Vec<String>) -> Result<MonitoringStatus, String> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| "TELEMETRY_STATE_LOCK_POISONED".to_owned())?;
        let inventory = state
            .inventory
            .as_ref()
            .ok_or_else(|| "TELEMETRY_INVENTORY_NOT_AVAILABLE".to_owned())?;
        let valid_ids: HashSet<&str> = inventory
            .devices
            .iter()
            .filter(|device| {
                supports_telemetry(device.kind)
                    && device.is_present == Some(true)
                    && device.is_virtual != Some(true)
            })
            .map(|device| device.id.as_str())
            .collect();
        let subscriptions = device_ids
            .into_iter()
            .filter(|device_id| valid_ids.contains(device_id.as_str()))
            .collect::<HashSet<_>>();
        if subscriptions.is_empty() {
            return Err("TELEMETRY_NO_SUPPORTED_PRESENT_DEVICE".to_owned());
        }
        state.subscriptions = subscriptions;
        state.running = true;
        state.generation = state.generation.wrapping_add(1);
        let status = status_from(&state);
        self.inner.wake.notify_all();
        drop(state);

        let mut worker = self
            .inner
            .worker
            .lock()
            .map_err(|_| "TELEMETRY_WORKER_LOCK_POISONED".to_owned())?;
        if worker.is_none() {
            let inner = Arc::clone(&self.inner);
            *worker = Some(
                thread::Builder::new()
                    .name("corescope-telemetry".to_owned())
                    .spawn(move || worker_loop(inner))
                    .map_err(|error| format!("TELEMETRY_WORKER_START_FAILED: {error}"))?,
            );
        }
        Ok(status)
    }

    pub fn stop(&self) -> Result<MonitoringStatus, String> {
        let status = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| "TELEMETRY_STATE_LOCK_POISONED".to_owned())?;
            state.running = false;
            state.generation = state.generation.wrapping_add(1);
            let status = status_from(&state);
            self.inner.wake.notify_all();
            status
        };
        self.join_worker()?;
        Ok(status)
    }

    pub fn set_foreground(&self, foreground: bool) -> Result<MonitoringStatus, String> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| "TELEMETRY_STATE_LOCK_POISONED".to_owned())?;
        if state.foreground != foreground {
            state.foreground = foreground;
            state.generation = state.generation.wrapping_add(1);
            self.inner.wake.notify_all();
        }
        Ok(status_from(&state))
    }

    pub fn frame(&self, device_ids: Option<Vec<String>>) -> Result<TelemetryFrame, String> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| "TELEMETRY_STATE_LOCK_POISONED".to_owned())?;
        let device_ids = device_ids.map(|ids| ids.into_iter().collect::<HashSet<_>>());
        let monitoring = status_from(&state);
        let mut samples = state.ring.recent(device_ids.as_ref());
        let mut latest_by_key = HashMap::<SampleKey, usize>::new();
        for (index, sample) in samples.iter().enumerate() {
            latest_by_key.insert(sample.key(), index);
        }
        let now = Utc::now();
        for index in latest_by_key.values().copied() {
            let sample = &mut samples[index];
            if sample.state != CapabilityState::Available {
                continue;
            }
            let too_old = sample
                .observed_at_utc
                .as_deref()
                .and_then(|time| DateTime::parse_from_rfc3339(time).ok())
                .is_some_and(|time| {
                    now.signed_duration_since(time.with_timezone(&Utc))
                        .to_std()
                        .is_ok_and(|age| age > SAMPLE_INTERVAL * 3)
                });
            if !monitoring.running || !monitoring.foreground || too_old {
                sample.state = CapabilityState::Stale;
                sample.value = None;
                sample.reason_code = Some(if !monitoring.running || !monitoring.foreground {
                    "MONITORING_PAUSED".to_owned()
                } else {
                    "SAMPLE_OLDER_THAN_FRESHNESS_WINDOW".to_owned()
                });
                sample.native_error_code = None;
            }
        }
        Ok(TelemetryFrame {
            monitoring,
            samples,
        })
    }

    fn join_worker(&self) -> Result<(), String> {
        let worker = self
            .inner
            .worker
            .lock()
            .map_err(|_| "TELEMETRY_WORKER_LOCK_POISONED".to_owned())?
            .take();
        if let Some(worker) = worker {
            worker
                .join()
                .map_err(|_| "TELEMETRY_WORKER_PANICKED".to_owned())?;
        }
        Ok(())
    }
}

fn status_from(state: &ManagerState) -> MonitoringStatus {
    MonitoringStatus {
        running: state.running,
        foreground: state.foreground,
        subscription_count: state.subscriptions.len(),
        sample_interval_ms: SAMPLE_INTERVAL.as_millis() as u64,
        latest_observed_at_utc: state.ring.latest_observed_at(),
    }
}

fn supports_telemetry(kind: DeviceKind) -> bool {
    matches!(
        kind,
        DeviceKind::Cpu
            | DeviceKind::GpuAdapter
            | DeviceKind::MemoryModule
            | DeviceKind::PhysicalDisk
            | DeviceKind::NetworkAdapter
    )
}

trait Collector {
    fn collect(
        &mut self,
        inventory: &InventorySnapshot,
        subscriptions: &HashSet<String>,
        suppressed: &HashSet<SampleKey>,
    ) -> Vec<MetricSample>;

    fn reset_baselines(&mut self);
}

#[cfg(windows)]
fn create_collector() -> Box<dyn Collector> {
    Box::new(windows::WindowsCollector::new())
}

#[cfg(not(windows))]
fn create_collector() -> Box<dyn Collector> {
    Box::new(UnsupportedCollector)
}

#[cfg(not(windows))]
struct UnsupportedCollector;

#[cfg(not(windows))]
impl Collector for UnsupportedCollector {
    fn collect(
        &mut self,
        _inventory: &InventorySnapshot,
        _subscriptions: &HashSet<String>,
        _suppressed: &HashSet<SampleKey>,
    ) -> Vec<MetricSample> {
        Vec::new()
    }

    fn reset_baselines(&mut self) {}
}

fn worker_loop(inner: Arc<ManagerInner>) {
    let mut collector = create_collector();
    let mut last_generation = None;
    let mut backoffs = HashMap::<SampleKey, FailureBackoff>::new();
    loop {
        let (inventory, subscriptions, generation) = {
            let mut state = match inner.state.lock() {
                Ok(state) => state,
                Err(_) => return,
            };
            while state.running && (!state.foreground || state.inventory.is_none()) {
                state = match inner.wake.wait(state) {
                    Ok(state) => state,
                    Err(_) => return,
                };
            }
            if !state.running {
                return;
            }
            let Some(inventory) = state.inventory.clone() else {
                continue;
            };
            (inventory, state.subscriptions.clone(), state.generation)
        };
        if last_generation != Some(generation) {
            collector.reset_baselines();
            backoffs.clear();
            last_generation = Some(generation);
        }
        let now = Instant::now();
        let suppressed = backoffs
            .iter()
            .filter_map(|(key, backoff)| {
                (backoff.permanent || backoff.retry_at.is_some_and(|retry_at| now < retry_at))
                    .then_some(key.clone())
            })
            .collect::<HashSet<_>>();
        let samples = collector.collect(&inventory, &subscriptions, &suppressed);
        let mut state = match inner.state.lock() {
            Ok(state) => state,
            Err(_) => return,
        };
        if !state.running || state.generation != generation {
            continue;
        }
        let sample_time = Instant::now();
        for sample in samples {
            if let Err(error) = sample.validate() {
                tracing::error!(metric = %sample.metric_key, error_code = %error, "Rejected invalid telemetry sample");
                continue;
            }
            if sample.state != CapabilityState::Available
                && sample.state != CapabilityState::Probing
            {
                tracing::warn!(
                    metric = %sample.metric_key,
                    source = %sample.source,
                    state = sample.state.as_str(),
                    reason_code = sample.reason_code.as_deref().unwrap_or("none"),
                    native_error_code = sample.native_error_code.as_deref().unwrap_or("none"),
                    "Telemetry source did not provide a current value"
                );
            }
            let key = sample.key();
            if sample.state == CapabilityState::Available {
                backoffs.remove(&key);
            } else if matches!(
                sample.state,
                CapabilityState::PermissionRequired
                    | CapabilityState::Unsupported
                    | CapabilityState::NotTested
                    | CapabilityState::NotApplicable
                    | CapabilityState::Unknown
            ) {
                backoffs.insert(
                    key,
                    FailureBackoff {
                        failures: 1,
                        retry_at: None,
                        permanent: true,
                    },
                );
            } else if sample.state == CapabilityState::TempUnavailable {
                let backoff = backoffs.entry(key).or_default();
                backoff.failures = backoff.failures.saturating_add(1).min(5);
                let seconds = 1_u64 << backoff.failures.min(5);
                backoff.retry_at =
                    Some(sample_time + Duration::from_secs(seconds).min(MAX_BACKOFF));
            }
            state.ring.push(sample);
        }
        let (state, _) = match inner.wake.wait_timeout(state, SAMPLE_INTERVAL) {
            Ok(result) => result,
            Err(_) => return,
        };
        if !state.running {
            return;
        }
    }
}
