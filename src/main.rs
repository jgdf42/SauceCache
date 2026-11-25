#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod activity;
mod app;
mod backup;
mod config;
mod diagnostics;
mod github;
mod local_clock;
mod mqtt;
mod repo_menu;
mod repo_split;
mod retention;
mod source;
mod text_menu;
mod tray;

fn main() -> iced::Result {
    if let Err(error) = std::fs::create_dir_all(config::directory()) {
        rfd::MessageDialog::new()
            .set_title("SauceCache")
            .set_description(format!("Cannot create settings folder: {error}"))
            .show();
        return Ok(());
    }
    let lock = match std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(config::directory().join("instance.lock"))
    {
        Ok(lock) => lock,
        Err(error) => {
            rfd::MessageDialog::new()
                .set_title("SauceCache")
                .set_description(format!("Cannot open instance lock: {error}"))
                .show();
            return Ok(());
        }
    };
    if fs2::FileExt::try_lock_exclusive(&lock).is_err() {
        rfd::MessageDialog::new()
            .set_title("SauceCache")
            .set_description("SauceCache is already running. Check its window or tray icon.")
            .show();
        return Ok(());
    }
    let session = diagnostics::start(&config::directory());
    let icon = image::load_from_memory(include_bytes!("../assets/saucecache.png"))
        .unwrap()
        .resize(64, 64, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    let (width, height) = icon.dimensions();
    let result = iced::application(app::App::new, app::App::update, app::App::view)
        .title(concat!("SauceCache v", env!("CARGO_PKG_VERSION")))
        .theme(app::App::theme)
        .subscription(app::App::subscription)
        .default_font(iced::Font::with_name("Segoe UI"))
        .window(iced::window::Settings {
            size: iced::Size::new(660.0, 560.0),
            min_size: Some(iced::Size::new(620.0, 440.0)),
            exit_on_close_request: false,
            icon: iced::window::icon::from_rgba(icon.into_raw(), width, height).ok(),
            ..Default::default()
        })
        .run();
    match &result {
        Ok(()) => session.finish(),
        Err(error) => diagnostics::record("fatal", &format!("UI runtime returned: {error}")),
    }
    drop(lock);
    result
}
