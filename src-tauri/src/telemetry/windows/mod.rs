mod nvml;

use super::{Collector, CounterRateError, MetricSample, SampleKey, counter_rate};
use crate::capabilities::{CapabilityState, EvidenceLevel};
use crate::inventory::{DeviceKind, DeviceRecord, InventorySnapshot};
use chrono::{SecondsFormat, Utc};
use nvml::{NvmlApi, NvmlInitError};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::{null, null_mut};
use std::time::Instant;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED, FILETIME,
    GetLastError, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIfTable2, MIB_IF_ROW2, MIB_IF_TABLE2,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{DISK_PERFORMANCE, IOCTL_DISK_PERFORMANCE};
use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows_sys::Win32::System::Threading::GetSystemTimes;

const NVIDIA_VENDOR_ID: u16 = 0x10de;
const NVML_NOT_SUPPORTED: i32 = 3;
const NVML_NO_PERMISSION: i32 = 4;

#[derive(Clone, Copy)]
struct CpuCounters {
    idle: u64,
    kernel: u64,
    user: u64,
    observed_at: Instant,
}

#[derive(Clone, Copy)]
struct DiskCounters {
    read_bytes: u64,
    written_bytes: u64,
    observed_at: Instant,
}

#[derive(Clone, Copy)]
struct NetworkCounters {
    received_bytes: u64,
    sent_bytes: u64,
    observed_at: Instant,
}

pub(super) struct WindowsCollector {
    previous_cpu: Option<CpuCounters>,
    previous_disks: HashMap<String, DiskCounters>,
    previous_network: HashMap<String, NetworkCounters>,
    nvml: Result<NvmlApi, NvmlInitError>,
}

impl WindowsCollector {
    pub(super) fn new() -> Self {
        Self {
            previous_cpu: None,
            previous_disks: HashMap::new(),
            previous_network: HashMap::new(),
            nvml: NvmlApi::load(),
        }
    }

    fn sample_cpu(
        &mut self,
        devices: &[&DeviceRecord],
        suppressed: &HashSet<SampleKey>,
        now: Instant,
        observed_at: &str,
        output: &mut Vec<MetricSample>,
    ) {
        if devices.is_empty() {
            return;
        }
        let (device_id, metric_key) = if devices.len() == 1 {
            (Some(devices[0].id.clone()), "cpu.utilization.percent")
        } else {
            (None, "system.cpu.utilization.percent")
        };
        let key = (device_id.clone(), metric_key.to_owned());
        if suppressed.contains(&key) {
            return;
        }
        let mut idle = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let ok = unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) };
        if ok == 0 {
            output.push(win32_failure(
                device_id,
                metric_key,
                "percent",
                "GetSystemTimes",
                unsafe { GetLastError() },
                observed_at,
            ));
            return;
        }
        let current = CpuCounters {
            idle: filetime_value(idle),
            kernel: filetime_value(kernel),
            user: filetime_value(user),
            observed_at: now,
        };
        let Some(previous) = self.previous_cpu.replace(current) else {
            output.push(probing_sample(
                device_id,
                metric_key,
                "percent",
                "GetSystemTimes",
                "WARMUP_REQUIRED",
                observed_at,
            ));
            return;
        };
        let kernel_delta = current.kernel.checked_sub(previous.kernel);
        let user_delta = current.user.checked_sub(previous.user);
        let idle_delta = current.idle.checked_sub(previous.idle);
        let elapsed = current
            .observed_at
            .saturating_duration_since(previous.observed_at);
        let Some((kernel_delta, user_delta, idle_delta)) = kernel_delta
            .zip(user_delta)
            .zip(idle_delta)
            .map(|((k, u), i)| (k, u, i))
        else {
            output.push(unavailable_sample(
                device_id,
                metric_key,
                "percent",
                "GetSystemTimes",
                CapabilityState::TempUnavailable,
                "COUNTER_RESET",
                "WIN32_COUNTER_RESET",
                observed_at,
            ));
            return;
        };
        let total = kernel_delta.saturating_add(user_delta);
        if elapsed.is_zero() || total == 0 || idle_delta > total {
            output.push(unavailable_sample(
                device_id,
                metric_key,
                "percent",
                "GetSystemTimes",
                CapabilityState::TempUnavailable,
                "INVALID_COUNTER_INTERVAL",
                "WIN32_COUNTER_INTERVAL_INVALID",
                observed_at,
            ));
            return;
        }
        let utilization = ((total - idle_delta) as f64 / total as f64 * 100.0).clamp(0.0, 100.0);
        output.push(available_sample(
            device_id,
            metric_key,
            "percent",
            "GetSystemTimes",
            EvidenceLevel::Measured,
            json!(round_to(utilization, 2)),
            observed_at,
            json!({"intervalMs": elapsed.as_millis()}),
        ));
    }

    fn sample_memory(
        &self,
        enabled: bool,
        suppressed: &HashSet<SampleKey>,
        observed_at: &str,
        output: &mut Vec<MetricSample>,
    ) {
        if !enabled {
            return;
        }
        let metrics = [
            "memory.total.bytes",
            "memory.available.bytes",
            "memory.used.bytes",
            "memory.load.percent",
        ];
        let metrics_to_read: Vec<_> = metrics
            .into_iter()
            .filter(|metric| !suppressed.contains(&(None, (*metric).to_owned())))
            .collect();
        if metrics_to_read.is_empty() {
            return;
        }
        let mut status = MEMORYSTATUSEX {
            dwLength: size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
            let error = unsafe { GetLastError() };
            for metric in metrics_to_read {
                output.push(win32_failure(
                    None,
                    metric,
                    metric_unit(metric),
                    "GlobalMemoryStatusEx",
                    error,
                    observed_at,
                ));
            }
            return;
        }
        if status.ullTotalPhys == 0 || status.ullAvailPhys > status.ullTotalPhys {
            for metric in metrics_to_read {
                output.push(unavailable_sample(
                    None,
                    metric,
                    metric_unit(metric),
                    "GlobalMemoryStatusEx",
                    CapabilityState::TempUnavailable,
                    "INVALID_MEMORY_STATUS",
                    "WIN32_MEMORY_STATUS_INVALID",
                    observed_at,
                ));
            }
            return;
        }
        let used = status.ullTotalPhys - status.ullAvailPhys;
        let values = [
            ("memory.total.bytes", json!(status.ullTotalPhys)),
            ("memory.available.bytes", json!(status.ullAvailPhys)),
            ("memory.used.bytes", json!(used)),
            (
                "memory.load.percent",
                json!(status.dwMemoryLoad.clamp(0, 100)),
            ),
        ];
        for (metric, value) in values {
            if !suppressed.contains(&(None, metric.to_owned())) {
                output.push(available_sample(
                    None,
                    metric,
                    metric_unit(metric),
                    "GlobalMemoryStatusEx",
                    EvidenceLevel::OsReported,
                    value,
                    observed_at,
                    json!({}),
                ));
            }
        }
    }

    fn sample_disks(
        &mut self,
        devices: &[&DeviceRecord],
        suppressed: &HashSet<SampleKey>,
        now: Instant,
        observed_at: &str,
        output: &mut Vec<MetricSample>,
    ) {
        for device in devices {
            let read_key = (
                Some(device.id.clone()),
                "disk.read.bytes_per_second".to_owned(),
            );
            let write_key = (
                Some(device.id.clone()),
                "disk.write.bytes_per_second".to_owned(),
            );
            let read_enabled = !suppressed.contains(&read_key);
            let write_enabled = !suppressed.contains(&write_key);
            if !read_enabled && !write_enabled {
                continue;
            }
            let Some(index) = device.attributes.get("index").and_then(Value::as_u64) else {
                for (enabled, key) in [
                    (read_enabled, "disk.read.bytes_per_second"),
                    (write_enabled, "disk.write.bytes_per_second"),
                ] {
                    if enabled {
                        output.push(unavailable_sample(
                            Some(device.id.clone()),
                            key,
                            "byte/s",
                            "IOCTL_DISK_PERFORMANCE",
                            CapabilityState::NotTested,
                            "DISK_INDEX_NOT_COLLECTED",
                            "DISK_INDEX_MISSING",
                            observed_at,
                        ));
                    }
                }
                continue;
            };
            match query_disk_performance(index as u32) {
                Ok(performance) => {
                    let current = DiskCounters {
                        read_bytes: performance.BytesRead.max(0) as u64,
                        written_bytes: performance.BytesWritten.max(0) as u64,
                        observed_at: now,
                    };
                    let previous = self.previous_disks.insert(device.id.clone(), current);
                    let Some(previous) = previous else {
                        for (enabled, key) in [
                            (read_enabled, "disk.read.bytes_per_second"),
                            (write_enabled, "disk.write.bytes_per_second"),
                        ] {
                            if enabled {
                                output.push(probing_sample(
                                    Some(device.id.clone()),
                                    key,
                                    "byte/s",
                                    "IOCTL_DISK_PERFORMANCE",
                                    "WARMUP_REQUIRED",
                                    observed_at,
                                ));
                            }
                        }
                        continue;
                    };
                    let elapsed = current
                        .observed_at
                        .saturating_duration_since(previous.observed_at);
                    for (enabled, key, before, after) in [
                        (
                            read_enabled,
                            "disk.read.bytes_per_second",
                            previous.read_bytes,
                            current.read_bytes,
                        ),
                        (
                            write_enabled,
                            "disk.write.bytes_per_second",
                            previous.written_bytes,
                            current.written_bytes,
                        ),
                    ] {
                        if !enabled {
                            continue;
                        }
                        match counter_rate(before, after, elapsed) {
                            Ok(rate) if rate.is_finite() => output.push(available_sample(
                                Some(device.id.clone()),
                                key,
                                "byte/s",
                                "IOCTL_DISK_PERFORMANCE",
                                EvidenceLevel::Measured,
                                json!(round_to(rate, 2)),
                                observed_at,
                                json!({"intervalMs": elapsed.as_millis()}),
                            )),
                            Err(CounterRateError::CounterReset) => output.push(unavailable_sample(
                                Some(device.id.clone()),
                                key,
                                "byte/s",
                                "IOCTL_DISK_PERFORMANCE",
                                CapabilityState::TempUnavailable,
                                "COUNTER_RESET",
                                "DISK_COUNTER_RESET",
                                observed_at,
                            )),
                            Err(CounterRateError::InvalidInterval) | Ok(_) => {
                                output.push(unavailable_sample(
                                    Some(device.id.clone()),
                                    key,
                                    "byte/s",
                                    "IOCTL_DISK_PERFORMANCE",
                                    CapabilityState::TempUnavailable,
                                    "INVALID_COUNTER_INTERVAL",
                                    "DISK_COUNTER_INTERVAL_INVALID",
                                    observed_at,
                                ))
                            }
                        }
                    }
                }
                Err(error) => {
                    for (enabled, key) in [
                        (read_enabled, "disk.read.bytes_per_second"),
                        (write_enabled, "disk.write.bytes_per_second"),
                    ] {
                        if enabled {
                            output.push(win32_failure(
                                Some(device.id.clone()),
                                key,
                                "byte/s",
                                "IOCTL_DISK_PERFORMANCE",
                                error,
                                observed_at,
                            ));
                        }
                    }
                }
            }
        }
    }

    fn sample_network(
        &mut self,
        devices: &[&DeviceRecord],
        suppressed: &HashSet<SampleKey>,
        now: Instant,
        observed_at: &str,
        output: &mut Vec<MetricSample>,
    ) {
        let active = devices
            .iter()
            .filter(|device| {
                [
                    "network.receive.bytes_per_second",
                    "network.send.bytes_per_second",
                ]
                .iter()
                .any(|key| !suppressed.contains(&(Some(device.id.clone()), (*key).to_owned())))
            })
            .copied()
            .collect::<Vec<_>>();
        if active.is_empty() {
            return;
        }
        let table = match query_network_counters() {
            Ok(table) => table,
            Err(error) => {
                for device in active {
                    for key in [
                        "network.receive.bytes_per_second",
                        "network.send.bytes_per_second",
                    ] {
                        if !suppressed.contains(&(Some(device.id.clone()), key.to_owned())) {
                            output.push(win32_failure(
                                Some(device.id.clone()),
                                key,
                                "byte/s",
                                "GetIfTable2",
                                error,
                                observed_at,
                            ));
                        }
                    }
                }
                return;
            }
        };
        for device in active {
            let metric_keys = [
                "network.receive.bytes_per_second",
                "network.send.bytes_per_second",
            ];
            let enabled = metric_keys
                .map(|key| !suppressed.contains(&(Some(device.id.clone()), key.to_owned())));
            let Some(interface_index) = device
                .attributes
                .get("interfaceIndex")
                .and_then(Value::as_u64)
                .map(|value| value as u32)
            else {
                for (is_enabled, key) in enabled.into_iter().zip(metric_keys) {
                    if is_enabled {
                        output.push(unavailable_sample(
                            Some(device.id.clone()),
                            key,
                            "byte/s",
                            "GetIfTable2",
                            CapabilityState::NotTested,
                            "INTERFACE_INDEX_NOT_COLLECTED",
                            "NETWORK_INTERFACE_INDEX_MISSING",
                            observed_at,
                        ));
                    }
                }
                continue;
            };
            let matches = table
                .iter()
                .filter(|(index, _, _)| *index == interface_index)
                .collect::<Vec<_>>();
            if matches.len() != 1 {
                let (state, reason, code) = if matches.is_empty() {
                    (
                        CapabilityState::TempUnavailable,
                        "INTERFACE_COUNTER_NOT_FOUND",
                        "NETWORK_INTERFACE_COUNTER_NOT_FOUND",
                    )
                } else {
                    (
                        CapabilityState::Unknown,
                        "INTERFACE_INDEX_AMBIGUOUS",
                        "NETWORK_INTERFACE_INDEX_AMBIGUOUS",
                    )
                };
                for (is_enabled, key) in enabled.into_iter().zip(metric_keys) {
                    if is_enabled {
                        output.push(unavailable_sample(
                            Some(device.id.clone()),
                            key,
                            "byte/s",
                            "GetIfTable2",
                            state,
                            reason,
                            code,
                            observed_at,
                        ));
                    }
                }
                continue;
            }
            let (_, received, sent) = *matches[0];
            let current = NetworkCounters {
                received_bytes: received,
                sent_bytes: sent,
                observed_at: now,
            };
            let previous = self.previous_network.insert(device.id.clone(), current);
            let Some(previous) = previous else {
                for (is_enabled, key) in enabled.into_iter().zip(metric_keys) {
                    if is_enabled {
                        output.push(probing_sample(
                            Some(device.id.clone()),
                            key,
                            "byte/s",
                            "GetIfTable2",
                            "WARMUP_REQUIRED",
                            observed_at,
                        ));
                    }
                }
                continue;
            };
            let elapsed = current
                .observed_at
                .saturating_duration_since(previous.observed_at);
            for (is_enabled, key, before, after) in [
                (
                    enabled[0],
                    metric_keys[0],
                    previous.received_bytes,
                    current.received_bytes,
                ),
                (
                    enabled[1],
                    metric_keys[1],
                    previous.sent_bytes,
                    current.sent_bytes,
                ),
            ] {
                if !is_enabled {
                    continue;
                }
                match counter_rate(before, after, elapsed) {
                    Ok(rate) if rate.is_finite() => output.push(available_sample(
                        Some(device.id.clone()),
                        key,
                        "byte/s",
                        "GetIfTable2",
                        EvidenceLevel::Measured,
                        json!(round_to(rate, 2)),
                        observed_at,
                        json!({"intervalMs": elapsed.as_millis()}),
                    )),
                    Err(CounterRateError::CounterReset) => output.push(unavailable_sample(
                        Some(device.id.clone()),
                        key,
                        "byte/s",
                        "GetIfTable2",
                        CapabilityState::TempUnavailable,
                        "COUNTER_RESET",
                        "NETWORK_COUNTER_RESET",
                        observed_at,
                    )),
                    Err(CounterRateError::InvalidInterval) | Ok(_) => {
                        output.push(unavailable_sample(
                            Some(device.id.clone()),
                            key,
                            "byte/s",
                            "GetIfTable2",
                            CapabilityState::TempUnavailable,
                            "INVALID_COUNTER_INTERVAL",
                            "NETWORK_COUNTER_INTERVAL_INVALID",
                            observed_at,
                        ))
                    }
                }
            }
        }
    }

    fn sample_gpus(
        &self,
        devices: &[&DeviceRecord],
        suppressed: &HashSet<SampleKey>,
        observed_at: &str,
        output: &mut Vec<MetricSample>,
    ) {
        let nvidia_devices = devices
            .iter()
            .filter(|device| {
                device
                    .manufacturer
                    .as_deref()
                    .is_some_and(|name| name.to_ascii_lowercase().contains("nvidia"))
                    || device.display_name.to_ascii_lowercase().contains("nvidia")
            })
            .copied()
            .collect::<Vec<_>>();
        if nvidia_devices.is_empty() {
            return;
        }
        let runtime = match &self.nvml {
            Ok(runtime) => runtime,
            Err(error) => {
                for device in nvidia_devices {
                    for (key, unit) in gpu_metric_units() {
                        if !suppressed.contains(&(Some(device.id.clone()), key.to_owned())) {
                            output.push(nvml_init_failure(device, key, unit, error, observed_at));
                        }
                    }
                }
                return;
            }
        };
        let nvml_devices = match runtime.devices() {
            Ok(devices) => devices,
            Err(error) => {
                for device in nvidia_devices {
                    for (key, unit) in gpu_metric_units() {
                        if !suppressed.contains(&(Some(device.id.clone()), key.to_owned())) {
                            output.push(nvml_failure(device, key, unit, error, observed_at));
                        }
                    }
                }
                return;
            }
        };
        for device in nvidia_devices {
            let vendor_id = device
                .attributes
                .get("pciVendorId")
                .and_then(Value::as_u64)
                .map(|v| v as u16);
            let product_id = device
                .attributes
                .get("pciDeviceId")
                .and_then(Value::as_u64)
                .map(|v| v as u16);
            let matches = match (vendor_id, product_id) {
                (Some(vendor), Some(product)) => nvml_devices
                    .iter()
                    .filter(|nvml| nvml.vendor_id == vendor && nvml.product_id == product)
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            };
            if matches.len() != 1 {
                let (state, reason, code) =
                    if vendor_id == Some(NVIDIA_VENDOR_ID) && matches.len() > 1 {
                        (
                            CapabilityState::Unknown,
                            "GPU_PCI_MATCH_AMBIGUOUS",
                            "NVML_GPU_MATCH_AMBIGUOUS",
                        )
                    } else if vendor_id.is_none() || product_id.is_none() {
                        (
                            CapabilityState::Unknown,
                            "GPU_PCI_ID_NOT_RECORDED",
                            "NVML_GPU_PCI_ID_MISSING",
                        )
                    } else {
                        (
                            CapabilityState::TempUnavailable,
                            "GPU_NOT_FOUND_IN_NVML",
                            "NVML_GPU_NOT_FOUND",
                        )
                    };
                for (key, unit) in gpu_metric_units() {
                    if !suppressed.contains(&(Some(device.id.clone()), key.to_owned())) {
                        output.push(unavailable_sample(
                            Some(device.id.clone()),
                            key,
                            unit,
                            "NVML",
                            state,
                            reason,
                            code,
                            observed_at,
                        ));
                    }
                }
                continue;
            }
            let handle = matches[0].handle;
            self.append_nvml_value(
                device,
                "gpu.utilization.percent",
                "percent",
                || runtime.utilization_gpu(handle).map(|v| json!(v)),
                suppressed,
                observed_at,
                output,
            );
            self.append_nvml_value(
                device,
                "gpu.memory_controller.utilization.percent",
                "percent",
                || runtime.utilization_memory(handle).map(|v| json!(v)),
                suppressed,
                observed_at,
                output,
            );
            self.append_nvml_value(
                device,
                "gpu.memory.total.bytes",
                "byte",
                || runtime.memory_total(handle).map(|v| json!(v)),
                suppressed,
                observed_at,
                output,
            );
            self.append_nvml_value(
                device,
                "gpu.memory.used.bytes",
                "byte",
                || runtime.memory_used(handle).map(|v| json!(v)),
                suppressed,
                observed_at,
                output,
            );
            self.append_nvml_value(
                device,
                "gpu.temperature.celsius",
                "°C",
                || runtime.temperature(handle).map(|v| json!(v)),
                suppressed,
                observed_at,
                output,
            );
            self.append_nvml_value(
                device,
                "gpu.fan.percent",
                "percent",
                || runtime.fan_speed(handle).map(|v| json!(v)),
                suppressed,
                observed_at,
                output,
            );
            self.append_nvml_value(
                device,
                "gpu.power.watts",
                "W",
                || runtime.power_watts(handle).map(|v| json!(round_to(v, 2))),
                suppressed,
                observed_at,
                output,
            );
        }
    }

    fn append_nvml_value(
        &self,
        device: &DeviceRecord,
        metric_key: &str,
        unit: &str,
        read: impl FnOnce() -> Result<Value, i32>,
        suppressed: &HashSet<SampleKey>,
        observed_at: &str,
        output: &mut Vec<MetricSample>,
    ) {
        if suppressed.contains(&(Some(device.id.clone()), metric_key.to_owned())) {
            return;
        }
        let result = read();
        match result {
            Ok(value) => output.push(available_sample(
                Some(device.id.clone()),
                metric_key,
                unit,
                "NVML",
                EvidenceLevel::SourceReported,
                value,
                observed_at,
                json!({"adapter": "NVIDIA Management Library"}),
            )),
            Err(code) => output.push(nvml_failure(device, metric_key, unit, code, observed_at)),
        }
    }
}

impl Collector for WindowsCollector {
    fn collect(
        &mut self,
        inventory: &InventorySnapshot,
        subscriptions: &HashSet<String>,
        suppressed: &HashSet<SampleKey>,
    ) -> Vec<MetricSample> {
        let now = Instant::now();
        let observed_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let subscribed = inventory
            .devices
            .iter()
            .filter(|device| subscriptions.contains(&device.id) && device.is_present == Some(true))
            .collect::<Vec<_>>();
        let cpus = subscribed
            .iter()
            .copied()
            .filter(|device| device.kind == DeviceKind::Cpu)
            .collect::<Vec<_>>();
        let memories = subscribed
            .iter()
            .any(|device| device.kind == DeviceKind::MemoryModule);
        let disks = subscribed
            .iter()
            .copied()
            .filter(|device| device.kind == DeviceKind::PhysicalDisk)
            .collect::<Vec<_>>();
        let networks = subscribed
            .iter()
            .copied()
            .filter(|device| device.kind == DeviceKind::NetworkAdapter)
            .collect::<Vec<_>>();
        let gpus = subscribed
            .iter()
            .copied()
            .filter(|device| device.kind == DeviceKind::GpuAdapter)
            .collect::<Vec<_>>();

        let mut samples = Vec::new();
        self.sample_cpu(&cpus, suppressed, now, &observed_at, &mut samples);
        self.sample_memory(memories, suppressed, &observed_at, &mut samples);
        self.sample_disks(&disks, suppressed, now, &observed_at, &mut samples);
        self.sample_network(&networks, suppressed, now, &observed_at, &mut samples);
        self.sample_gpus(&gpus, suppressed, &observed_at, &mut samples);
        samples
    }

    fn reset_baselines(&mut self) {
        self.previous_cpu = None;
        self.previous_disks.clear();
        self.previous_network.clear();
    }
}

fn query_disk_performance(index: u32) -> Result<DISK_PERFORMANCE, u32> {
    let path: Vec<u16> = format!(r"\\.\PhysicalDrive{index}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(unsafe { GetLastError() });
    }
    let mut performance = DISK_PERFORMANCE::default();
    let mut bytes_returned = 0_u32;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_DISK_PERFORMANCE,
            null(),
            0,
            (&mut performance as *mut DISK_PERFORMANCE).cast::<c_void>(),
            size_of::<DISK_PERFORMANCE>() as u32,
            &mut bytes_returned,
            null_mut(),
        )
    };
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        Err(unsafe { GetLastError() })
    } else if bytes_returned < size_of::<DISK_PERFORMANCE>() as u32 {
        Err(ERROR_INVALID_FUNCTION)
    } else {
        Ok(performance)
    }
}

fn query_network_counters() -> Result<Vec<(u32, u64, u64)>, u32> {
    let mut table: *mut MIB_IF_TABLE2 = null_mut();
    let result = unsafe { GetIfTable2(&mut table) };
    if result != 0 {
        return Err(result);
    }
    if table.is_null() {
        return Err(ERROR_NOT_SUPPORTED);
    }
    struct TableGuard(*mut MIB_IF_TABLE2);
    impl Drop for TableGuard {
        fn drop(&mut self) {
            unsafe { FreeMibTable(self.0.cast::<c_void>()) };
        }
    }
    let _guard = TableGuard(table);
    let table_ref = unsafe { &*table };
    if table_ref.NumEntries > 4096 {
        return Err(ERROR_NOT_SUPPORTED);
    }
    let rows = unsafe {
        std::slice::from_raw_parts(table_ref.Table.as_ptr(), table_ref.NumEntries as usize)
    };
    Ok(rows
        .iter()
        .map(|row: &MIB_IF_ROW2| (row.InterfaceIndex, row.InOctets, row.OutOctets))
        .collect())
}

fn filetime_value(value: FILETIME) -> u64 {
    (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
}

fn win32_failure(
    device_id: Option<String>,
    metric_key: &str,
    unit: &str,
    source: &str,
    error: u32,
    observed_at: &str,
) -> MetricSample {
    let state = if error == ERROR_ACCESS_DENIED {
        CapabilityState::PermissionRequired
    } else if error == ERROR_INVALID_FUNCTION || error == ERROR_NOT_SUPPORTED {
        CapabilityState::Unsupported
    } else {
        CapabilityState::TempUnavailable
    };
    let reason = match state {
        CapabilityState::PermissionRequired => "STANDARD_USER_ACCESS_DENIED",
        CapabilityState::Unsupported => "SOURCE_REPORTED_NOT_SUPPORTED",
        _ => "SOURCE_QUERY_FAILED",
    };
    unavailable_sample(
        device_id,
        metric_key,
        unit,
        source,
        state,
        reason,
        &format!("WIN32_ERROR_{error}"),
        observed_at,
    )
}

fn nvml_init_failure(
    device: &DeviceRecord,
    metric_key: &str,
    unit: &str,
    error: &NvmlInitError,
    observed_at: &str,
) -> MetricSample {
    let (state, reason, code) = match error {
        NvmlInitError::LoadFailed(code) => (
            CapabilityState::TempUnavailable,
            "NVML_RUNTIME_UNAVAILABLE",
            format!("WIN32_ERROR_{code}"),
        ),
        NvmlInitError::Api(code) if *code == NVML_NO_PERMISSION => (
            CapabilityState::PermissionRequired,
            "NVML_PERMISSION_REQUIRED",
            format!("NVML_ERROR_{code}"),
        ),
        NvmlInitError::Api(code) => (
            CapabilityState::TempUnavailable,
            "NVML_INITIALIZATION_FAILED",
            format!("NVML_ERROR_{code}"),
        ),
        NvmlInitError::MissingFunction => (
            CapabilityState::TempUnavailable,
            "NVML_API_INCOMPLETE",
            "NVML_FUNCTION_MISSING".to_owned(),
        ),
    };
    unavailable_sample(
        Some(device.id.clone()),
        metric_key,
        unit,
        "NVML",
        state,
        reason,
        &code,
        observed_at,
    )
}

fn nvml_failure(
    device: &DeviceRecord,
    metric_key: &str,
    unit: &str,
    error: i32,
    observed_at: &str,
) -> MetricSample {
    let (state, reason) = if error == NVML_NOT_SUPPORTED {
        (
            CapabilityState::Unsupported,
            "SOURCE_REPORTED_NOT_SUPPORTED",
        )
    } else if error == NVML_NO_PERMISSION {
        (
            CapabilityState::PermissionRequired,
            "NVML_PERMISSION_REQUIRED",
        )
    } else {
        (CapabilityState::TempUnavailable, "NVML_QUERY_FAILED")
    };
    unavailable_sample(
        Some(device.id.clone()),
        metric_key,
        unit,
        "NVML",
        state,
        reason,
        &format!("NVML_ERROR_{error}"),
        observed_at,
    )
}

fn probing_sample(
    device_id: Option<String>,
    metric_key: &str,
    unit: &str,
    source: &str,
    reason: &str,
    observed_at: &str,
) -> MetricSample {
    unavailable_sample(
        device_id,
        metric_key,
        unit,
        source,
        CapabilityState::Probing,
        reason,
        "WARMUP_IN_PROGRESS",
        observed_at,
    )
}

fn unavailable_sample(
    device_id: Option<String>,
    metric_key: &str,
    unit: &str,
    source: &str,
    state: CapabilityState,
    reason: &str,
    native_error: &str,
    observed_at: &str,
) -> MetricSample {
    MetricSample {
        device_id,
        metric_key: metric_key.to_owned(),
        state,
        value: None,
        unit: Some(unit.to_owned()),
        source: source.to_owned(),
        evidence_level: Some(EvidenceLevel::SourceReported),
        observed_at_utc: Some(observed_at.to_owned()),
        reason_code: Some(reason.to_owned()),
        native_error_code: Some(native_error.to_owned()),
        details: json!({}),
    }
}

fn available_sample(
    device_id: Option<String>,
    metric_key: &str,
    unit: &str,
    source: &str,
    evidence_level: EvidenceLevel,
    value: Value,
    observed_at: &str,
    details: Value,
) -> MetricSample {
    MetricSample {
        device_id,
        metric_key: metric_key.to_owned(),
        state: CapabilityState::Available,
        value: Some(value),
        unit: Some(unit.to_owned()),
        source: source.to_owned(),
        evidence_level: Some(evidence_level),
        observed_at_utc: Some(observed_at.to_owned()),
        reason_code: None,
        native_error_code: None,
        details,
    }
}

fn gpu_metric_units() -> [(&'static str, &'static str); 7] {
    [
        ("gpu.utilization.percent", "percent"),
        ("gpu.memory_controller.utilization.percent", "percent"),
        ("gpu.memory.total.bytes", "byte"),
        ("gpu.memory.used.bytes", "byte"),
        ("gpu.temperature.celsius", "°C"),
        ("gpu.fan.percent", "percent"),
        ("gpu.power.watts", "W"),
    ]
}

fn metric_unit(metric_key: &str) -> &'static str {
    match metric_key {
        "memory.load.percent" => "percent",
        _ => "byte",
    }
}

fn round_to(value: f64, digits: u32) -> f64 {
    let multiplier = 10_f64.powi(digits as i32);
    (value * multiplier).round() / multiplier
}
