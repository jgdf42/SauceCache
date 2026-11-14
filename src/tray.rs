use crate::github::{Event, TrayCommand};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::Sender,
};
pub struct Tray {
    pub ready: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}
impl Drop for Tray {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
impl Tray {
    pub fn start(events: Sender<Event>) -> Self {
        let ready = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let result = Self {
            ready: ready.clone(),
            stop: stop.clone(),
        };
        #[cfg(windows)]
        std::thread::spawn(move || {
            use tray_icon::{
                MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
                menu::{Menu, MenuEvent, MenuItem},
            };
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
            };
            let menu = Menu::new();
            let mut items = vec![];
            for (label, command) in [
                ("Show SauceCache", TrayCommand::Show),
                ("Check now", TrayCommand::Check),
                ("Start watching", TrayCommand::Start),
                ("Stop", TrayCommand::Stop),
                ("Exit", TrayCommand::Exit),
            ] {
                let item = MenuItem::new(label, true, None);
                let _ = menu.append(&item);
                items.push((item, command));
            }
            let icon = image::load_from_memory(include_bytes!("../assets/saucecache.png"))
                .unwrap()
                .to_rgba8();
            let (width, height) = icon.dimensions();
            let icon = tray_icon::Icon::from_rgba(icon.into_raw(), width, height).unwrap();
            let tray = match TrayIconBuilder::new()
                .with_tooltip("SauceCache")
                .with_icon(icon)
                .with_menu(Box::new(menu))
                .build()
            {
                Ok(tray) => tray,
                Err(error) => {
                    let _ = events.send(Event::Log(format!("Tray unavailable: {error}")));
                    return;
                }
            };
            ready.store(true, Ordering::Relaxed);
            while !stop.load(Ordering::Relaxed) {
                unsafe {
                    let mut msg: MSG = std::mem::zeroed();
                    while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                while let Ok(event) = MenuEvent::receiver().try_recv() {
                    if let Some((_, command)) =
                        items.iter().find(|(item, _)| item.id() == &event.id)
                    {
                        let _ = events.send(Event::Tray(*command));
                    }
                }
                while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                    if matches!(
                        event,
                        TrayIconEvent::DoubleClick {
                            button: MouseButton::Left,
                            ..
                        } | TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        }
                    ) {
                        let _ = events.send(Event::Tray(TrayCommand::Show));
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            drop(tray);
            ready.store(false, Ordering::Relaxed);
        });
        result
    }
}
