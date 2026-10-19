#![cfg(windows)]

use corescope_lib::inventory::{DeviceKind, normalize_inventory, scan_local};
use corescope_lib::telemetry::TelemetryManager;
use std::collections::BTreeMap;
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_ACCEPTANCE_DURATION: Duration = Duration::from_secs(300);

#[test]
#[ignore = "five-minute standard-user Windows telemetry acceptance run"]
fn samples_live_cpu_memory_disk_network_and_nvidia_metrics_for_five_minutes() {
    let raw = scan_local().expect("standard-user inventory scan should succeed");
    let snapshot = normalize_inventory(&raw, b"live-telemetry-test-install-salt");
    let device_ids = snapshot
        .devices
        .iter()
        .filter(|device| {
            device.is_present == Some(true)
                && matches!(
                    device.kind,
                    DeviceKind::Cpu
                        | DeviceKind::GpuAdapter
                        | DeviceKind::MemoryModule
                        | DeviceKind::PhysicalDisk
                        | DeviceKind::NetworkAdapter
                )
        })
        .map(|device| device.id.clone())
        .collect::<Vec<_>>();
    let nvidia_devices = snapshot
        .devices
        .iter()
        .filter(|device| {
            device.kind == DeviceKind::GpuAdapter
                && (device
                    .manufacturer
                    .as_deref()
                    .is_some_and(|name| name.to_ascii_lowercase().contains("nvidia"))
                    || device.display_name.to_ascii_lowercase().contains("nvidia"))
        })
        .collect::<Vec<_>>();
    let has_nvidia = !nvidia_devices.is_empty();
    for device in &nvidia_devices {
        println!(
            "M4 NVIDIA adapter identity: vendor={:?} product={:?}",
            device.attributes.get("pciVendorId"),
            device.attributes.get("pciDeviceId")
        );
    }
    let manager = TelemetryManager::new(Some(snapshot));
    manager.start(device_ids).unwrap();

    let started = Instant::now();
    let mut seen = BTreeMap::<String, BTreeMap<String, usize>>::new();
    let mut last_seen = BTreeMap::<(Option<String>, String), String>::new();
    let mut latest = BTreeMap::<
        String,
        (
            String,
            Option<serde_json::Value>,
            Option<String>,
            Option<String>,
        ),
    >::new();
    let sample_seconds = std::env::var("CORESCOPE_M4_SAMPLE_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_ACCEPTANCE_DURATION);
    while started.elapsed() < sample_seconds {
        let frame = manager.frame(None).unwrap();
        for sample in frame.samples {
            let Some(observed_at) = sample.observed_at_utc.clone() else {
                continue;
            };
            let key = (sample.device_id.clone(), sample.metric_key.clone());
            if last_seen
                .get(&key)
                .is_some_and(|previous| previous >= &observed_at)
            {
                continue;
            }
            last_seen.insert(key, observed_at.clone());
            *seen
                .entry(sample.metric_key.clone())
                .or_default()
                .entry(sample.state.as_str().to_owned())
                .or_default() += 1;
            latest.insert(
                sample.metric_key,
                (
                    sample.state.as_str().to_owned(),
                    sample.value,
                    sample.reason_code,
                    sample.native_error_code,
                ),
            );
        }
        thread::sleep(Duration::from_secs(1));
    }
    manager.stop().unwrap();

    for metric in [
        "cpu.utilization.percent",
        "memory.used.bytes",
        "memory.available.bytes",
        "disk.read.bytes_per_second",
        "disk.write.bytes_per_second",
        "network.receive.bytes_per_second",
        "network.send.bytes_per_second",
    ] {
        let counts = seen.get(metric).cloned().unwrap_or_default();
        println!("M4 live metric: {metric} states={counts:?}");
        assert!(
            counts.get("AVAILABLE").copied().unwrap_or_default() > 0,
            "expected at least one measured value for {metric}"
        );
    }
    if has_nvidia {
        for metric in [
            "gpu.utilization.percent",
            "gpu.memory_controller.utilization.percent",
            "gpu.memory.total.bytes",
            "gpu.memory.used.bytes",
            "gpu.temperature.celsius",
            "gpu.fan.percent",
        ] {
            let counts = seen.get(metric).cloned().unwrap_or_default();
            println!("M4 live metric: {metric} states={counts:?}");
            assert!(
                counts.get("AVAILABLE").copied().unwrap_or_default() > 0,
                "NVML should provide live readings for {metric}"
            );
        }
        let power = seen.get("gpu.power.watts").cloned().unwrap_or_default();
        assert!(
            !power.is_empty(),
            "NVML must report a numeric power value or a documented non-available state"
        );
        let (power_state, power_value, _, _) = latest.get("gpu.power.watts").unwrap();
        assert!(
            power_state == "AVAILABLE" || power_value.is_none(),
            "unavailable GPU power must not carry a fabricated numeric value"
        );
    }
    for (metric, (state, value, reason, native)) in &latest {
        println!(
            "M4 live latest: {metric} state={state} value={value:?} reason={reason:?} native={native:?}"
        );
    }
}
