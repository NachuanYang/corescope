use corescope_lib::capabilities::{CapabilityState, EvidenceLevel};
use corescope_lib::inventory::{DeviceKind, RawInventory, normalize_inventory};
use corescope_lib::telemetry::{
    CounterRateError, MetricSample, SampleRing, TelemetryManager, counter_rate,
};
#[cfg(windows)]
use std::thread;
use std::time::Duration;
#[cfg(windows)]
use std::time::Instant;

fn sample(metric_key: &str, value: u64, observed_at: &str) -> MetricSample {
    MetricSample {
        device_id: Some("disk-a".to_owned()),
        metric_key: metric_key.to_owned(),
        state: CapabilityState::Available,
        value: Some(serde_json::json!(value)),
        unit: Some("byte/s".to_owned()),
        source: "fixture-counter".to_owned(),
        evidence_level: Some(EvidenceLevel::Measured),
        observed_at_utc: Some(observed_at.to_owned()),
        reason_code: None,
        native_error_code: None,
        details: serde_json::json!({}),
    }
}

#[test]
fn counter_rates_use_real_elapsed_time_and_reject_invalid_baselines() {
    let rate = counter_rate(1_000, 81_000, Duration::from_secs(2)).unwrap();
    assert_eq!(rate, 40_000.0);
    assert_eq!(
        counter_rate(10, 9, Duration::from_secs(1)),
        Err(CounterRateError::CounterReset)
    );
    assert_eq!(
        counter_rate(10, 20, Duration::ZERO),
        Err(CounterRateError::InvalidInterval)
    );
}

#[test]
fn ring_buffer_keeps_only_the_configured_recent_points_per_metric() {
    let mut ring = SampleRing::new(2);
    ring.push(sample("disk.read.bytes_per_second", 1, "t1"));
    ring.push(sample("disk.write.bytes_per_second", 10, "t1"));
    ring.push(sample("disk.read.bytes_per_second", 2, "t2"));
    ring.push(sample("disk.read.bytes_per_second", 3, "t3"));

    let recent = ring.recent(None);
    assert_eq!(recent.len(), 3);
    let reads: Vec<_> = recent
        .iter()
        .filter(|item| item.metric_key == "disk.read.bytes_per_second")
        .collect();
    assert_eq!(reads.len(), 2);
    assert_eq!(reads[0].value, Some(serde_json::json!(2)));
    assert_eq!(reads[1].value, Some(serde_json::json!(3)));
}

#[test]
fn telemetry_samples_reject_unverified_values_and_accept_zero_as_a_real_reading() {
    let zero = sample("gpu.fan.percent", 0, "2026-10-14T00:00:00Z");
    zero.validate().unwrap();

    let mut missing_evidence = zero.clone();
    missing_evidence.source.clear();
    assert_eq!(
        missing_evidence.validate().unwrap_err(),
        "TELEMETRY_AVAILABLE_EVIDENCE_INCOMPLETE"
    );

    let mut fabricated_value = zero;
    fabricated_value.state = CapabilityState::TempUnavailable;
    assert_eq!(
        fabricated_value.validate().unwrap_err(),
        "TELEMETRY_NON_AVAILABLE_HAS_VALUE"
    );
}

#[test]
fn subscriptions_are_deduplicated_and_foreground_transitions_can_pause_monitoring() {
    let raw: RawInventory = serde_json::from_value(serde_json::json!({
        "cpus": [{"name":"Fixture CPU","physical_cores":8}]
    }))
    .unwrap();
    let snapshot = normalize_inventory(&raw, b"telemetry-test-salt");
    let cpu_id = snapshot
        .devices
        .iter()
        .find(|device| device.kind == DeviceKind::Cpu)
        .unwrap()
        .id
        .clone();
    let manager = TelemetryManager::new(Some(snapshot));

    let started = manager.start(vec![cpu_id.clone(), cpu_id.clone()]).unwrap();
    assert!(started.running);
    assert_eq!(started.subscription_count, 1);
    let background = manager.set_foreground(false).unwrap();
    assert!(background.running);
    assert!(!background.foreground);
    let stopped = manager.stop().unwrap();
    assert!(!stopped.running);
}

#[cfg(windows)]
#[test]
fn paused_monitoring_marks_the_latest_sample_stale_and_removes_its_value() {
    let raw: RawInventory = serde_json::from_value(serde_json::json!({
        "cpus": [{"name":"Fixture CPU","physical_cores":8}]
    }))
    .unwrap();
    let snapshot = normalize_inventory(&raw, b"telemetry-stale-test-salt");
    let cpu_id = snapshot
        .devices
        .iter()
        .find(|device| device.kind == DeviceKind::Cpu)
        .unwrap()
        .id
        .clone();
    let manager = TelemetryManager::new(Some(snapshot));
    manager.start(vec![cpu_id.clone()]).unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut latest = None;
    while Instant::now() < deadline {
        let frame = manager.frame(None).unwrap();
        latest = frame
            .samples
            .into_iter()
            .filter(|sample| {
                sample.device_id.as_deref() == Some(&cpu_id)
                    && sample.metric_key == "cpu.utilization.percent"
            })
            .last();
        if latest
            .as_ref()
            .is_some_and(|sample| sample.state == CapabilityState::Available)
        {
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }
    assert!(latest.is_some_and(|sample| sample.state == CapabilityState::Available));

    manager.set_foreground(false).unwrap();
    let paused = manager.frame(None).unwrap();
    let latest_cpu = paused
        .samples
        .into_iter()
        .filter(|sample| {
            sample.device_id.as_deref() == Some(&cpu_id)
                && sample.metric_key == "cpu.utilization.percent"
        })
        .last()
        .unwrap();
    assert_eq!(latest_cpu.state, CapabilityState::Stale);
    assert_eq!(latest_cpu.value, None);
    manager.stop().unwrap();
}
