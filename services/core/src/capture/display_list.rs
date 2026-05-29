use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub handle: isize,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
    pub refresh_rate: u32,
    pub index: u32,
}

#[cfg(windows)]
pub fn list_monitors() -> Result<Vec<MonitorInfo>> {
    use windows::Win32::Foundation::{BOOL, LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW, DEVMODEW,
        ENUM_CURRENT_SETTINGS, HDC, HMONITOR, MONITORINFOEXW,
    };

    let mut monitors = Vec::new();

    unsafe {
        unsafe extern "system" fn enum_monitors_callback(
            hmonitor: HMONITOR,
            _hdc: HDC,
            _rect: *mut RECT,
            lparam: LPARAM,
        ) -> BOOL {
            let list = &mut *(lparam.0 as *mut Vec<MonitorInfo>);
            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            if GetMonitorInfoW(hmonitor, &mut info.monitorInfo as *mut _ as *mut _).as_bool() {
                let name = String::from_utf16_lossy(&info.szDevice)
                    .trim_matches('\0')
                    .to_string();
                let width = (info.monitorInfo.rcMonitor.right - info.monitorInfo.rcMonitor.left)
                    .abs() as u32;
                let height = (info.monitorInfo.rcMonitor.bottom - info.monitorInfo.rcMonitor.top)
                    .abs() as u32;
                let is_primary = (info.monitorInfo.dwFlags & 1) != 0; // MONITORINFOF_PRIMARY = 1

                let mut refresh_rate = 60;
                let mut devmode = DEVMODEW::default();
                devmode.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
                let sz_device_pcwstr = windows::core::PCWSTR::from_raw(info.szDevice.as_ptr());
                if EnumDisplaySettingsW(sz_device_pcwstr, ENUM_CURRENT_SETTINGS, &mut devmode)
                    .as_bool()
                {
                    refresh_rate = devmode.dmDisplayFrequency;
                }

                let index = name
                    .chars()
                    .filter(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse::<u32>()
                    .unwrap_or(0);

                list.push(MonitorInfo {
                    handle: hmonitor.0 as isize,
                    name,
                    width,
                    height,
                    is_primary,
                    refresh_rate,
                    index,
                });
            }
            BOOL(1)
        }

        if !EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(enum_monitors_callback),
            LPARAM(&mut monitors as *mut _ as isize),
        )
        .as_bool()
        {
            return Err(anyhow::anyhow!("EnumDisplayMonitors failed"));
        }
    }

    Ok(monitors)
}

#[cfg(not(windows))]
pub fn list_monitors() -> Result<Vec<MonitorInfo>> {
    Ok(vec![])
}
