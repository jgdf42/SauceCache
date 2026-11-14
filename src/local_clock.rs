/// Use the user's regional Windows time pattern, including 12/24-hour mode and AM/PM.
pub fn now() -> String {
    #[cfg(windows)]
    unsafe {
        let mut buffer = [0u16; 128];
        let length = windows_sys::Win32::Globalization::GetTimeFormatEx(
            std::ptr::null(),
            0,
            std::ptr::null(),
            std::ptr::null(),
            buffer.as_mut_ptr(),
            buffer.len() as i32,
        );
        if length > 0 {
            return String::from_utf16_lossy(&buffer[..length as usize - 1]);
        }
    }
    chrono::Local::now().format("%X").to_string()
}
