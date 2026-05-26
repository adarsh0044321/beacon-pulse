//! Windows registry access helpers.

#[cfg(windows)]
use std::ffi::OsStr;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

#[cfg(windows)]
const HKEY_CURRENT_USER: isize = -2147483647;
#[cfg(windows)]
const KEY_READ: u32 = 0x20019;
#[cfg(windows)]
const KEY_WRITE: u32 = 0x20006;
#[cfg(windows)]
const REG_SZ: u32 = 1;
#[cfg(windows)]
const REG_DWORD: u32 = 4;
#[cfg(windows)]
const REG_OPTION_NON_VOLATILE: u32 = 0;
#[cfg(windows)]
const ERROR_SUCCESS: i32 = 0;

#[cfg(windows)]
#[link(name = "advapi32")]
extern "system" {
    fn RegOpenKeyExW(
        hkey: isize,
        lpsubkey: *const u16,
        uloptions: u32,
        samdesired: u32,
        phkresult: *mut isize,
    ) -> i32;

    fn RegCreateKeyExW(
        hkey: isize,
        lpsubkey: *const u16,
        reserved: u32,
        lpclass: *const u16,
        dwoptions: u32,
        samdesired: u32,
        lpsecurityattributes: *const std::ffi::c_void,
        phkresult: *mut isize,
        lpdwdisposition: *mut u32,
    ) -> i32;

    fn RegSetValueExW(
        hkey: isize,
        lpvaluename: *const u16,
        reserved: u32,
        dwtype: u32,
        lpdata: *const u8,
        cbdata: u32,
    ) -> i32;

    fn RegQueryValueExW(
        hkey: isize,
        lpvaluename: *const u16,
        reserved: *mut u32,
        lptype: *mut u32,
        lpdata: *mut u8,
        lpcbdata: *mut u32,
    ) -> i32;

    fn RegCloseKey(hkey: isize) -> i32;
}

#[cfg(windows)]
fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

#[cfg(windows)]
const SUBKEY: &str = "Software\\Beacon";

#[cfg(windows)]
pub fn read_dword(name: &str) -> Option<u32> {
    unsafe {
        let mut hkey: isize = 0;
        let subkey_w = to_wide(SUBKEY);
        if RegOpenKeyExW(HKEY_CURRENT_USER, subkey_w.as_ptr(), 0, KEY_READ, &mut hkey)
            != ERROR_SUCCESS
        {
            return None;
        }
        let name_w = to_wide(name);
        let mut value_type: u32 = 0;
        let mut data: u32 = 0;
        let mut data_size = std::mem::size_of::<u32>() as u32;
        let res = RegQueryValueExW(
            hkey,
            name_w.as_ptr(),
            std::ptr::null_mut(),
            &mut value_type,
            &mut data as *mut _ as *mut u8,
            &mut data_size,
        );
        RegCloseKey(hkey);
        if res == ERROR_SUCCESS && value_type == REG_DWORD {
            Some(data)
        } else {
            None
        }
    }
}

#[cfg(windows)]
pub fn write_dword(name: &str, val: u32) -> bool {
    unsafe {
        let mut hkey: isize = 0;
        let subkey_w = to_wide(SUBKEY);
        let mut disp: u32 = 0;
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey_w.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &mut hkey,
            &mut disp,
        ) != ERROR_SUCCESS
        {
            return false;
        }
        let name_w = to_wide(name);
        let res = RegSetValueExW(
            hkey,
            name_w.as_ptr(),
            0,
            REG_DWORD,
            &val as *const _ as *const u8,
            std::mem::size_of::<u32>() as u32,
        );
        RegCloseKey(hkey);
        res == ERROR_SUCCESS
    }
}

#[cfg(windows)]
pub fn read_string(name: &str) -> Option<String> {
    unsafe {
        let mut hkey: isize = 0;
        let subkey_w = to_wide(SUBKEY);
        if RegOpenKeyExW(HKEY_CURRENT_USER, subkey_w.as_ptr(), 0, KEY_READ, &mut hkey)
            != ERROR_SUCCESS
        {
            return None;
        }
        let name_w = to_wide(name);
        let mut value_type: u32 = 0;
        let mut data_size: u32 = 0;
        let mut res = RegQueryValueExW(
            hkey,
            name_w.as_ptr(),
            std::ptr::null_mut(),
            &mut value_type,
            std::ptr::null_mut(),
            &mut data_size,
        );
        if res != ERROR_SUCCESS || value_type != REG_SZ {
            RegCloseKey(hkey);
            return None;
        }
        let mut buf = vec![0u16; (data_size as usize / 2) + 1];
        res = RegQueryValueExW(
            hkey,
            name_w.as_ptr(),
            std::ptr::null_mut(),
            &mut value_type,
            buf.as_mut_ptr() as *mut u8,
            &mut data_size,
        );
        RegCloseKey(hkey);
        if res == ERROR_SUCCESS {
            let len = (data_size as usize / 2).saturating_sub(1);
            Some(String::from_utf16_lossy(&buf[..len]))
        } else {
            None
        }
    }
}

#[cfg(windows)]
pub fn write_string(name: &str, val: &str) -> bool {
    unsafe {
        let mut hkey: isize = 0;
        let subkey_w = to_wide(SUBKEY);
        let mut disp: u32 = 0;
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey_w.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &mut hkey,
            &mut disp,
        ) != ERROR_SUCCESS
        {
            return false;
        }
        let name_w = to_wide(name);
        let val_w = to_wide(val);
        let res = RegSetValueExW(
            hkey,
            name_w.as_ptr(),
            0,
            REG_SZ,
            val_w.as_ptr() as *const u8,
            (val_w.len() * 2) as u32,
        );
        RegCloseKey(hkey);
        res == ERROR_SUCCESS
    }
}

#[cfg(windows)]
pub fn write_startup(exe_path: &str, args: &str) -> bool {
    unsafe {
        let mut hkey: isize = 0;
        let startup_subkey_w = to_wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            startup_subkey_w.as_ptr(),
            0,
            KEY_WRITE,
            &mut hkey,
        ) != ERROR_SUCCESS
        {
            return false;
        }
        let name_w = to_wide("BeaconHost");
        let val = format!("\"{}\" {}", exe_path, args);
        let val_w = to_wide(&val);
        let res = RegSetValueExW(
            hkey,
            name_w.as_ptr(),
            0,
            REG_SZ,
            val_w.as_ptr() as *const u8,
            (val_w.len() * 2) as u32,
        );
        RegCloseKey(hkey);
        res == ERROR_SUCCESS
    }
}

#[cfg(windows)]
pub fn delete_startup() -> bool {
    unsafe {
        let mut hkey: isize = 0;
        let startup_subkey_w = to_wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            startup_subkey_w.as_ptr(),
            0,
            KEY_WRITE,
            &mut hkey,
        ) != ERROR_SUCCESS
        {
            return false;
        }
        let name_w = to_wide("BeaconHost");
        #[link(name = "advapi32")]
        extern "system" {
            fn RegDeleteValueW(hkey: isize, lpvaluename: *const u16) -> i32;
        }
        let res = RegDeleteValueW(hkey, name_w.as_ptr());
        RegCloseKey(hkey);
        res == ERROR_SUCCESS
    }
}

// Fallbacks for non-Windows targets
#[cfg(not(windows))]
pub fn read_dword(_name: &str) -> Option<u32> {
    None
}
#[cfg(not(windows))]
pub fn write_dword(_name: &str, _val: u32) -> bool {
    false
}
#[cfg(not(windows))]
pub fn read_string(_name: &str) -> Option<String> {
    None
}
#[cfg(not(windows))]
pub fn write_string(_name: &str, _val: &str) -> bool {
    false
}
#[cfg(not(windows))]
pub fn write_startup(_exe_path: &str, _args: &str) -> bool {
    false
}
#[cfg(not(windows))]
pub fn delete_startup() -> bool {
    false
}
