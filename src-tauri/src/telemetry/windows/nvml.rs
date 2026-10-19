use std::ffi::c_void;
use std::mem::transmute_copy;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{FARPROC, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};

type NvmlInit = unsafe extern "system" fn() -> i32;
type NvmlShutdown = unsafe extern "system" fn() -> i32;
type NvmlGetCount = unsafe extern "system" fn(*mut u32) -> i32;
type NvmlGetHandleByIndex = unsafe extern "system" fn(u32, *mut NvmlDeviceHandle) -> i32;
type NvmlGetPciInfo = unsafe extern "system" fn(NvmlDeviceHandle, *mut NvmlPciInfo) -> i32;
type NvmlGetUtilization = unsafe extern "system" fn(NvmlDeviceHandle, *mut NvmlUtilization) -> i32;
type NvmlGetMemory = unsafe extern "system" fn(NvmlDeviceHandle, *mut NvmlMemory) -> i32;
type NvmlGetTemperature = unsafe extern "system" fn(NvmlDeviceHandle, u32, *mut u32) -> i32;
type NvmlGetFanSpeed = unsafe extern "system" fn(NvmlDeviceHandle, *mut u32) -> i32;
type NvmlGetPower = unsafe extern "system" fn(NvmlDeviceHandle, *mut u32) -> i32;

#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub(super) struct NvmlDeviceHandle(*mut c_void);

#[derive(Clone, Copy, Debug)]
pub(super) struct NvmlDeviceInfo {
    pub handle: NvmlDeviceHandle,
    pub vendor_id: u16,
    pub product_id: u16,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum NvmlInitError {
    LoadFailed(u32),
    Api(i32),
    MissingFunction,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct NvmlPciInfo {
    bus_id_legacy: [i8; 16],
    domain: u32,
    bus: u32,
    device: u32,
    pci_device_id: u32,
    pci_subsystem_id: u32,
    bus_id: [i8; 32],
}

impl Default for NvmlPciInfo {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
struct NvmlUtilization {
    gpu: u32,
    memory: u32,
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
struct NvmlMemory {
    total: u64,
    free: u64,
    used: u64,
}

pub(super) struct NvmlApi {
    module: HMODULE,
    shutdown: NvmlShutdown,
    get_count: NvmlGetCount,
    get_handle_by_index: NvmlGetHandleByIndex,
    get_pci_info: NvmlGetPciInfo,
    get_utilization: NvmlGetUtilization,
    get_memory: NvmlGetMemory,
    get_temperature: NvmlGetTemperature,
    get_fan_speed: NvmlGetFanSpeed,
    get_power: NvmlGetPower,
}

impl NvmlApi {
    pub fn load() -> Result<Self, NvmlInitError> {
        let library: Vec<u16> = "nvml.dll".encode_utf16().chain(Some(0)).collect();
        let module =
            unsafe { LoadLibraryExW(library.as_ptr(), null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) };
        if module.is_null() {
            return Err(NvmlInitError::LoadFailed(unsafe {
                windows_sys::Win32::Foundation::GetLastError()
            }));
        }

        let (
            init,
            shutdown,
            get_count,
            get_handle_by_index,
            get_pci_info,
            get_utilization,
            get_memory,
            get_temperature,
            get_fan_speed,
            get_power,
        ) = unsafe {
            let init = resolve::<NvmlInit>(module, b"nvmlInit_v2\0")
                .or_else(|| resolve(module, b"nvmlInit\0"));
            let shutdown = resolve::<NvmlShutdown>(module, b"nvmlShutdown\0");
            let get_count = resolve::<NvmlGetCount>(module, b"nvmlDeviceGetCount_v2\0")
                .or_else(|| resolve(module, b"nvmlDeviceGetCount\0"));
            let get_handle_by_index =
                resolve::<NvmlGetHandleByIndex>(module, b"nvmlDeviceGetHandleByIndex_v2\0")
                    .or_else(|| resolve(module, b"nvmlDeviceGetHandleByIndex\0"));
            let get_pci_info = resolve::<NvmlGetPciInfo>(module, b"nvmlDeviceGetPciInfo_v3\0")
                .or_else(|| resolve(module, b"nvmlDeviceGetPciInfo_v2\0"));
            let get_utilization =
                resolve::<NvmlGetUtilization>(module, b"nvmlDeviceGetUtilizationRates\0");
            let get_memory = resolve::<NvmlGetMemory>(module, b"nvmlDeviceGetMemoryInfo\0");
            let get_temperature =
                resolve::<NvmlGetTemperature>(module, b"nvmlDeviceGetTemperature\0");
            let get_fan_speed = resolve::<NvmlGetFanSpeed>(module, b"nvmlDeviceGetFanSpeed\0");
            let get_power = resolve::<NvmlGetPower>(module, b"nvmlDeviceGetPowerUsage\0");
            match (
                init,
                shutdown,
                get_count,
                get_handle_by_index,
                get_pci_info,
                get_utilization,
                get_memory,
                get_temperature,
                get_fan_speed,
                get_power,
            ) {
                (
                    Some(init),
                    Some(shutdown),
                    Some(get_count),
                    Some(get_handle_by_index),
                    Some(get_pci_info),
                    Some(get_utilization),
                    Some(get_memory),
                    Some(get_temperature),
                    Some(get_fan_speed),
                    Some(get_power),
                ) => (
                    init,
                    shutdown,
                    get_count,
                    get_handle_by_index,
                    get_pci_info,
                    get_utilization,
                    get_memory,
                    get_temperature,
                    get_fan_speed,
                    get_power,
                ),
                _ => {
                    windows_sys::Win32::Foundation::FreeLibrary(module);
                    return Err(NvmlInitError::MissingFunction);
                }
            }
        };
        let init_status = unsafe { init() };
        if init_status != 0 {
            unsafe { windows_sys::Win32::Foundation::FreeLibrary(module) };
            return Err(NvmlInitError::Api(init_status));
        }

        Ok(Self {
            module,
            shutdown,
            get_count,
            get_handle_by_index,
            get_pci_info,
            get_utilization,
            get_memory,
            get_temperature,
            get_fan_speed,
            get_power,
        })
    }

    pub fn devices(&self) -> Result<Vec<NvmlDeviceInfo>, i32> {
        let mut count = 0_u32;
        let status = unsafe { (self.get_count)(&mut count) };
        if status != 0 {
            return Err(status);
        }
        if count > 256 {
            return Err(-1);
        }
        let mut devices = Vec::with_capacity(count as usize);
        for index in 0..count {
            let mut handle = NvmlDeviceHandle(null_mut());
            let status = unsafe { (self.get_handle_by_index)(index, &mut handle) };
            if status != 0 || handle.0.is_null() {
                continue;
            }
            let mut pci = NvmlPciInfo::default();
            if unsafe { (self.get_pci_info)(handle, &mut pci) } != 0 {
                continue;
            }
            let (vendor_id, product_id) = pci_device_ids(pci.pci_device_id);
            devices.push(NvmlDeviceInfo {
                handle,
                vendor_id,
                product_id,
            });
        }
        Ok(devices)
    }

    pub fn utilization_gpu(&self, device: NvmlDeviceHandle) -> Result<u32, i32> {
        self.utilization(device).map(|values| values.gpu)
    }

    pub fn utilization_memory(&self, device: NvmlDeviceHandle) -> Result<u32, i32> {
        self.utilization(device).map(|values| values.memory)
    }

    pub fn memory_total(&self, device: NvmlDeviceHandle) -> Result<u64, i32> {
        self.memory(device).map(|values| values.total)
    }

    pub fn memory_used(&self, device: NvmlDeviceHandle) -> Result<u64, i32> {
        self.memory(device).map(|values| values.used)
    }

    pub fn temperature(&self, device: NvmlDeviceHandle) -> Result<u32, i32> {
        let mut value = 0_u32;
        let status = unsafe { (self.get_temperature)(device, 0, &mut value) };
        (status == 0).then_some(value).ok_or(status)
    }

    pub fn fan_speed(&self, device: NvmlDeviceHandle) -> Result<u32, i32> {
        let mut value = 0_u32;
        let status = unsafe { (self.get_fan_speed)(device, &mut value) };
        (status == 0).then_some(value).ok_or(status)
    }

    pub fn power_watts(&self, device: NvmlDeviceHandle) -> Result<f64, i32> {
        let mut milliwatts = 0_u32;
        let status = unsafe { (self.get_power)(device, &mut milliwatts) };
        if status == 0 {
            Ok(milliwatts as f64 / 1000.0)
        } else {
            Err(status)
        }
    }

    fn utilization(&self, device: NvmlDeviceHandle) -> Result<NvmlUtilization, i32> {
        let mut values = NvmlUtilization::default();
        let status = unsafe { (self.get_utilization)(device, &mut values) };
        (status == 0).then_some(values).ok_or(status)
    }

    fn memory(&self, device: NvmlDeviceHandle) -> Result<NvmlMemory, i32> {
        let mut values = NvmlMemory::default();
        let status = unsafe { (self.get_memory)(device, &mut values) };
        (status == 0).then_some(values).ok_or(status)
    }
}

impl Drop for NvmlApi {
    fn drop(&mut self) {
        unsafe {
            (self.shutdown)();
            windows_sys::Win32::Foundation::FreeLibrary(self.module);
        }
    }
}

unsafe fn resolve<T: Copy>(module: HMODULE, name: &'static [u8]) -> Option<T> {
    let function: FARPROC = unsafe { GetProcAddress(module, name.as_ptr()) };
    function.map(|function| unsafe { transmute_copy(&function) })
}

fn pci_device_ids(pci_device_id: u32) -> (u16, u16) {
    (pci_device_id as u16, (pci_device_id >> 16) as u16)
}

#[cfg(test)]
mod tests {
    use super::pci_device_ids;

    #[test]
    fn extracts_vendor_and_product_from_nvml_combined_pci_id() {
        assert_eq!(pci_device_ids(0x20F0_10DE), (0x10DE, 0x20F0));
    }
}
