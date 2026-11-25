#[derive(Clone, Copy)]
pub enum Command {
    Options,
    KeepLatest,
}

#[cfg(windows)]
pub fn popup(keep_latest: bool, enabled: bool) -> Option<Command> {
    use windows_sys::Win32::{Foundation::POINT, UI::WindowsAndMessaging::*};
    unsafe {
        let owner = GetForegroundWindow();
        let mut point = POINT { x: 0, y: 0 };
        if owner.is_null() || GetCursorPos(&mut point) == 0 {
            return None;
        }
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return None;
        }
        for (id, label, checked) in [
            (1, "Download overrides...", false),
            (2, "Keep only newest version", keep_latest),
        ] {
            let label: Vec<u16> = label.encode_utf16().chain(Some(0)).collect();
            AppendMenuW(
                menu,
                MF_STRING
                    | if checked { MF_CHECKED } else { MF_UNCHECKED }
                    | if enabled { MF_ENABLED } else { MF_GRAYED },
                id,
                label.as_ptr(),
            );
        }
        let result = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            0,
            owner,
            std::ptr::null(),
        );
        DestroyMenu(menu);
        match result {
            1 => Some(Command::Options),
            2 => Some(Command::KeepLatest),
            _ => None,
        }
    }
}

#[cfg(not(windows))]
pub fn popup(_: bool, _: bool) -> Option<Command> {
    None
}
