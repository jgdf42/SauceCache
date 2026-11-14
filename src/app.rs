use crate::{
    config::{self, Config},
    github::{Event, TrayCommand, Watcher},
    mqtt,
    source::{self, Provider},
    text_menu::TextMenu,
    tray::Tray,
};
use iced::{
    Element,
    Length::Fill,
    Subscription, Task, Theme,
    widget::{Space, column, container, image, mouse_area, progress_bar, row, text, text_editor},
    window,
};
use sk_iced::{
    components::{
        Appearance, ButtonTone, Dropdown, FocusScope, KeyboardControl, Popup, RedrawBoundary,
        ScrollArea, Scrollbar, StatusBadge, UiContext,
    },
    motion::Policy,
};
use std::{
    collections::BTreeSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const INTERVALS: [Interval; 6] = [
    Interval(1),
    Interval(5),
    Interval(30),
    Interval(60),
    Interval(360),
    Interval(1440),
];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interval(pub u64);
impl std::fmt::Display for Interval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0 >= 60 && self.0.is_multiple_of(60) {
            write!(
                f,
                "{} {}",
                self.0 / 60,
                if self.0 == 60 { "hour" } else { "hours" }
            )
        } else {
            write!(f, "{} min", self.0)
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub enum Field {
    Host,
    Port,
    Username,
    Password,
    Topic,
    Suffixes,
}
#[derive(Clone, Copy, Debug)]
pub enum Toggle {
    Tray,
    Mqtt,
    Tls,
    Status,
    Repositories,
    Downloads,
}
#[derive(Clone, Debug)]
pub enum Message {
    Tick,
    Start,
    Check,
    Stop,
    Theme,
    Settings,
    Add,
    Remove,
    Dismiss,
    CopyRepos,
    ConfirmAdd,
    ConfirmRemove,
    Edit(text_editor::Action),
    Select(usize),
    OpenRepo(usize),
    RemoveSelect(usize, bool),
    Output(String),
    Browse,
    Folder(Option<String>),
    Interval(Interval),
    Field(Field, String),
    Toggle(Toggle, bool),
    Tab(usize),
    TestMqtt,
    Provider(Provider),
    Credits,
    ResizeRepos(f32),
    FocusTextBox(iced::Rectangle),
    BackupConfig,
    RestoreConfig,
    BackupFinished(Result<bool, String>),
    RestoreFinished(Result<Option<crate::backup::Backup>, String>),
    Window(window::Id, window::Event),
    WindowId(Option<window::Id>),
    Minimized(Option<bool>),
    Exit,
}
enum Dialog {
    Add {
        content: text_editor::Content,
        provider: Provider,
    },
    Remove(BTreeSet<usize>),
    Settings {
        draft: Box<Config>,
        password: String,
        suffixes: String,
        port: String,
        tab: usize,
    },
}
pub struct App {
    config: Config,
    dialog: Option<Dialog>,
    error: String,
    mqtt_feedback: String,
    selected: Option<usize>,
    logs: crate::activity::History,
    repo_ratio: f32,
    transfer_pending: bool,
    transfer_status: String,
    mqtt_dirty: bool,
    mqtt_save_at: Option<Instant>,
    tx: mpsc::Sender<Event>,
    rx: mpsc::Receiver<Event>,
    watcher: Arc<Mutex<Option<Watcher>>>,
    cancel: Arc<AtomicBool>,
    busy: bool,
    watching: bool,
    next_check: Option<Instant>,
    not_before: Option<Instant>,
    progress: Option<(String, u64, u64)>,
    mqtt: mqtt::Mqtt,
    mqtt_connected: bool,
    mqtt_status: String,
    tray: Tray,
    window: Option<window::Id>,
    hidden: bool,
    exiting: bool,
    smoke_end: Option<Instant>,
}
impl App {
    pub fn new() -> (Self, Task<Message>) {
        let loaded = Config::load();
        let error = loaded.as_ref().err().cloned().unwrap_or_default();
        let config = loaded.unwrap_or_default();
        let preview_tab = std::env::args().find_map(|arg| {
            if arg == "--preview-about" {
                Some(2)
            } else {
                arg.strip_prefix("--preview-settings=")
                    .and_then(|tab| tab.parse::<usize>().ok())
                    .filter(|tab| *tab < 3)
            }
        });
        let preview_dialog = preview_tab
            .map(|tab| Dialog::Settings {
                draft: Box::new(config.clone()),
                password: String::new(),
                suffixes: config.suffixes.join(", "),
                port: config.mqtt.port.to_string(),
                tab,
            })
            .or_else(|| {
                std::env::args()
                    .any(|arg| arg == "--preview-add")
                    .then(|| Dialog::Add {
                        content: text_editor::Content::new(),
                        provider: Provider::Auto,
                    })
            });
        let (tx, rx) = mpsc::channel();
        let tray = Tray::start(tx.clone());
        let mut mqtt = mqtt::Mqtt::default();
        mqtt.configure(config.mqtt.clone(), tx.clone());
        (
            Self {
                config,
                dialog: preview_dialog,
                error,
                mqtt_feedback: String::new(),
                selected: None,
                logs: crate::activity::History::default(),
                repo_ratio: 0.6,
                transfer_pending: false,
                transfer_status: String::new(),
                mqtt_dirty: false,
                mqtt_save_at: None,
                tx,
                rx,
                watcher: Arc::new(Mutex::new(None)),
                cancel: Arc::new(AtomicBool::new(false)),
                busy: false,
                watching: false,
                next_check: None,
                not_before: None,
                progress: None,
                mqtt,
                mqtt_connected: false,
                mqtt_status: "Disabled".into(),
                tray,
                window: None,
                hidden: false,
                exiting: false,
                smoke_end: std::env::args()
                    .any(|arg| arg == "--smoke-test")
                    .then(|| Instant::now() + Duration::from_secs(8)),
            },
            Task::batch([
                window::oldest().map(Message::WindowId),
                if std::env::args().any(|arg| arg == "--preview-mqtt-test") {
                    Task::done(Message::TestMqtt)
                } else {
                    Task::none()
                },
            ]),
        )
    }
    pub fn theme(&self) -> Theme {
        sk_iced::palette::palette(self.config.dark, false).theme()
    }
    fn ui(&self) -> UiContext {
        UiContext::new(
            Appearance::fixed(
                iced::Font::DEFAULT,
                sk_iced::palette::palette(self.config.dark, false),
            ),
            Policy::detect(),
        )
    }
    fn log(&mut self, value: impl Into<String>) {
        self.logs
            .push(format!("{}  {}", crate::local_clock::now(), value.into()));
    }
    fn persist(&mut self) {
        if let Err(error) = self.config.save() {
            self.error = format!("Could not save settings: {error}");
        }
    }
    fn save_mqtt_edits(&mut self) -> bool {
        self.mqtt_save_at = None;
        if !self.mqtt_dirty {
            return true;
        }
        let Some(Dialog::Settings {
            draft,
            password,
            port,
            ..
        }) = &self.dialog
        else {
            return true;
        };
        let settings = match draft.mqtt.validated_with_port(port) {
            Ok(settings) => settings,
            Err(error) => {
                self.mqtt_feedback = error;
                return false;
            }
        };
        let mut updated = self.config.clone();
        updated.mqtt = settings;
        let password_changed = match mqtt::save_configuration(&updated, password) {
            Ok(changed) => changed,
            Err(error) => {
                self.mqtt_feedback = error;
                return false;
            }
        };
        let reconnect = updated.mqtt != self.config.mqtt || password_changed;
        self.config = updated;
        self.mqtt_dirty = false;
        self.mqtt_feedback.clear();
        if reconnect {
            self.mqtt
                .configure(self.config.mqtt.clone(), self.tx.clone());
            self.snapshot();
        }
        true
    }
    fn snapshot(&self) {
        self.mqtt.publish(&self.config.mqtt,"status",serde_json::json!({"state":if self.watching{"watching"}else if self.busy{"checking"}else{"idle"}}));
        let down = self
            .config
            .repositories
            .iter()
            .filter(|r| r.unavailable)
            .count();
        self.mqtt.publish(&self.config.mqtt,"repositories",serde_json::json!({"active_count":self.config.repositories.len()-down,"unavailable_count":down}));
    }
    fn begin_check(&mut self) {
        if self.busy {
            return;
        }
        if let Some(until) = self.not_before
            && until > Instant::now()
        {
            self.error = format!(
                "Host rate limit: wait {} seconds",
                until.saturating_duration_since(Instant::now()).as_secs()
            );
            return;
        }
        if self.config.repositories.is_empty() {
            self.error = "Add at least one repo".into();
            return;
        }
        if self.config.output_dir.trim().is_empty() {
            self.error = "Choose a download folder".into();
            return;
        }
        self.persist();
        self.error.clear();
        self.busy = true;
        self.next_check = None;
        self.progress = None;
        self.cancel = Arc::new(AtomicBool::new(false));
        self.snapshot();
        let config = self.config.clone();
        let cancel = self.cancel.clone();
        let tx = self.tx.clone();
        let watcher = self.watcher.clone();
        std::thread::spawn(move || {
            let Ok(mut holder) = watcher.lock() else {
                let _ = tx.send(Event::Log("Watcher thread could not start".into()));
                let _ = tx.send(Event::Finished);
                return;
            };
            if holder.is_none() {
                match Watcher::new() {
                    Ok(w) => *holder = Some(w),
                    Err(error) => {
                        let _ = tx.send(Event::Log(error));
                        let _ = tx.send(Event::Finished);
                        return;
                    }
                }
            }
            holder.as_mut().unwrap().check(config, cancel, tx);
        });
    }
    fn show(&mut self) -> Task<Message> {
        self.hidden = false;
        if let Some(id) = self.window {
            Task::batch([
                window::set_mode(id, window::Mode::Windowed),
                window::minimize(id, false),
                window::gain_focus(id),
            ])
        } else {
            Task::none()
        }
    }
    fn hide(&mut self) -> Task<Message> {
        if !self.tray.ready.load(Ordering::Relaxed) {
            self.log("Tray unavailable; window stays visible");
            return Task::none();
        }
        self.hidden = true;
        self.persist();
        if let Some(id) = self.window {
            window::set_mode(id, window::Mode::Hidden)
        } else {
            Task::none()
        }
    }
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::time::every(Duration::from_millis(250)).map(|_| Message::Tick),
            iced::event::listen_with(|event, _, id| match event {
                iced::Event::Window(event) => Some(Message::Window(id, event)),
                _ => None,
            }),
        ])
    }
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::ResizeRepos(ratio) => self.repo_ratio = ratio,
            Message::FocusTextBox(bounds) => {
                return iced::advanced::widget::operate(crate::text_menu::focus_only(bounds))
                    .discard();
            }
            Message::BackupConfig if !self.transfer_pending => {
                if !self.save_mqtt_edits() {
                    if let Some(Dialog::Settings { tab, .. }) = &mut self.dialog {
                        *tab = 1;
                    }
                    return Task::none();
                }
                self.transfer_pending = true;
                self.transfer_status = "Choose where to save the backup…".into();
                self.error.clear();
                return Task::perform(
                    crate::backup::choose_backup(self.config.clone()),
                    Message::BackupFinished,
                );
            }
            Message::RestoreConfig if !self.transfer_pending && !self.busy && !self.watching => {
                self.transfer_pending = true;
                self.transfer_status = "Choose a backup to restore…".into();
                self.error.clear();
                return Task::perform(crate::backup::choose_restore(), Message::RestoreFinished);
            }
            Message::BackupConfig | Message::RestoreConfig => {}
            Message::BackupFinished(result) => {
                self.transfer_pending = false;
                self.transfer_status.clear();
                match result {
                    Ok(true) => {
                        self.transfer_status = "Configuration backup saved.".into();
                        self.log("Configuration backup saved");
                    }
                    Ok(false) => {}
                    Err(error) => self.error = error,
                }
            }
            Message::RestoreFinished(result) => {
                self.transfer_pending = false;
                self.transfer_status.clear();
                match result {
                    Ok(Some(backup)) => {
                        if self.busy || self.watching {
                            self.error = "Stop watching before restoring configuration".into();
                        } else if let Err(error) = backup.apply() {
                            self.error = error;
                        } else {
                            self.config = backup.config;
                            self.selected = None;
                            self.not_before = None;
                            self.next_check = None;
                            self.mqtt
                                .configure(self.config.mqtt.clone(), self.tx.clone());
                            self.snapshot();
                            self.log("Configuration restored from backup");
                            let task = self.update(Message::Settings);
                            self.transfer_status = "Configuration restored.".into();
                            return task;
                        }
                    }
                    Ok(None) => {}
                    Err(error) => self.error = error,
                }
            }
            Message::Tick => {
                if self.mqtt_save_at.is_some_and(|at| Instant::now() >= at) {
                    self.save_mqtt_edits();
                }
                if self.smoke_end.is_some_and(|until| Instant::now() >= until) {
                    return self.update(Message::Exit);
                }
                let mut tasks = vec![];
                while let Ok(event) = self.rx.try_recv() {
                    match event{
                    Event::Log(line)=>self.log(line),
                    Event::Availability(name,down)=>{if let Some(repo)=self.config.repositories.iter_mut().find(|r|r.name==name){repo.unavailable=down;}self.persist();self.snapshot();},
                    Event::SourceResolved { previous, repository } => {
                        if let Some(repo) = self.config.repositories.iter_mut().find(|r| r.name == previous) {
                            repo.name = repository.name;
                            repo.provider = repository.provider;
                            self.persist();
                        }
                    },
                    Event::Progress(name,done,total)=>self.progress=Some((name,done,total)),
                    Event::Download{repository,tag,asset,size}=>self.mqtt.publish(&self.config.mqtt,"downloads",serde_json::json!({"repository":repository,"tag":tag,"asset":asset,"size":size})),
                    Event::Finished => {
                        self.busy = false;
                        self.progress = None;
                        if self.exiting {
                            self.mqtt.stop();
                            self.persist();
                            return iced::exit();
                        }
                        if self.watching {
                            let next = Instant::now() + Duration::from_secs(self.config.interval_minutes * 60);
                            self.next_check = Some(self.not_before.map_or(next, |until| until.max(next)));
                        }
                        self.snapshot();
                    },
                    Event::Backoff(seconds)=>self.not_before=Some(Instant::now()+Duration::from_secs(seconds)),
                    Event::Mqtt(connected,status)=>{let changed=self.mqtt_connected!=connected || self.mqtt_status!=status;self.mqtt_connected=connected;self.mqtt_status=status;if connected && changed{self.snapshot();}},
                    Event::MqttTest(result)=>self.mqtt_feedback=result,
                    Event::Tray(command)=>tasks.push(self.update(match command{TrayCommand::Show=>return self.show(),TrayCommand::Check=>Message::Check,TrayCommand::Start=>Message::Start,TrayCommand::Stop=>Message::Stop,TrayCommand::Exit=>Message::Exit})),
                }
                }
                if self.watching
                    && !self.busy
                    && self.next_check.is_some_and(|at| at <= Instant::now())
                {
                    self.begin_check();
                }
                if !self.hidden
                    && let Some(id) = self.window
                {
                    tasks.push(window::is_minimized(id).map(Message::Minimized));
                }
                return Task::batch(tasks);
            }
            Message::WindowId(id) => self.window = id,
            Message::Window(id, event) => {
                self.window = Some(id);
                match event {
                    window::Event::CloseRequested => {
                        return if self.config.close_to_tray
                            && self.tray.ready.load(Ordering::Relaxed)
                        {
                            self.hide()
                        } else {
                            self.update(Message::Exit)
                        };
                    }
                    window::Event::Focused => {}
                    _ => {}
                }
            }
            Message::Minimized(Some(true)) => return self.hide(),
            Message::Minimized(_) => {}
            Message::Start => {
                self.watching = true;
                self.begin_check();
                if !self.busy {
                    self.watching = false;
                } else {
                    self.log("Started watching");
                }
            }
            Message::Check => {
                let was_busy = self.busy;
                self.begin_check();
                if !was_busy && self.busy {
                    self.log("Started manual check");
                }
            }
            Message::Stop => {
                self.watching = false;
                self.next_check = None;
                self.cancel.store(true, Ordering::Relaxed);
                self.log("Stopped watching; cancelling any active check");
                self.snapshot();
            }
            Message::Exit => {
                self.save_mqtt_edits();
                self.watching = false;
                self.cancel.store(true, Ordering::Relaxed);
                self.persist();
                if self.busy {
                    self.exiting = true;
                    self.log("Stopping downloads before exit…");
                } else {
                    self.mqtt.stop();
                    return iced::exit();
                }
            }
            Message::Theme => {
                self.config.dark = !self.config.dark;
                self.persist();
            }
            Message::Settings => {
                self.error.clear();
                self.mqtt_feedback.clear();
                self.transfer_status.clear();
                self.mqtt_dirty = false;
                self.mqtt_save_at = None;
                self.dialog = Some(Dialog::Settings {
                    draft: Box::new(self.config.clone()),
                    password: mqtt::password(&self.config.mqtt),
                    suffixes: self.config.suffixes.join(", "),
                    port: self.config.mqtt.port.to_string(),
                    tab: 0,
                });
            }
            Message::Add if !self.busy && !self.watching => {
                self.error.clear();
                self.dialog = Some(Dialog::Add {
                    content: text_editor::Content::new(),
                    provider: Provider::Auto,
                });
            }
            Message::Remove if !self.busy && !self.watching => {
                self.error.clear();
                self.dialog = Some(Dialog::Remove(self.selected.into_iter().collect()));
            }
            Message::Add | Message::Remove => {}
            Message::Dismiss => {
                if !self.save_mqtt_edits() {
                    if let Some(Dialog::Settings { tab, .. }) = &mut self.dialog {
                        *tab = 1;
                    }
                    return Task::none();
                }
                self.dialog = None;
                self.error.clear();
            }
            Message::Edit(action) => {
                if let Some(Dialog::Add { content, .. }) = &mut self.dialog {
                    content.perform(action);
                }
            }
            Message::Select(index) => {
                self.selected = if self.selected == Some(index) {
                    None
                } else {
                    Some(index)
                }
            }
            Message::OpenRepo(index) => {
                if let Some(repo) = self.config.repositories.get(index)
                    && let Err(error) = webbrowser::open(&source::browser_url(repo))
                {
                    self.error = error.to_string();
                }
            }
            Message::RemoveSelect(index, checked) => {
                if let Some(Dialog::Remove(selection)) = &mut self.dialog {
                    if checked {
                        selection.insert(index);
                    } else {
                        selection.remove(&index);
                    }
                }
            }
            Message::ConfirmAdd => {
                if let Some(Dialog::Add { content, provider }) = &self.dialog {
                    let text = content.text();
                    let mut additions = vec![];
                    for line in text.lines().flat_map(|line| line.split(',')) {
                        if !line.trim().is_empty() {
                            match source::parse(line, *provider) {
                                Ok(repo) => additions.push(repo),
                                Err(error) => {
                                    self.error = error;
                                    return Task::none();
                                }
                            }
                        }
                    }
                    if additions.is_empty() {
                        self.error = "Enter at least one repo".into();
                        return Task::none();
                    }
                    for repo in additions {
                        if !self.config.repositories.iter().any(|r| {
                            r.name.eq_ignore_ascii_case(&repo.name) && r.provider == repo.provider
                        }) {
                            self.config.repositories.push(repo);
                        }
                    }
                    self.persist();
                    self.dialog = None;
                    self.snapshot();
                }
            }
            Message::ConfirmRemove => {
                if let Some(Dialog::Remove(selection)) = &self.dialog {
                    if selection.is_empty() {
                        self.error = "Select at least one repo".into();
                        return Task::none();
                    }
                    self.config.repositories = self
                        .config
                        .repositories
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| !selection.contains(index))
                        .map(|(_, repo)| repo.clone())
                        .collect();
                    self.selected = None;
                    self.persist();
                    self.dialog = None;
                    self.snapshot();
                }
            }
            Message::Output(value) => {
                self.config.output_dir = value;
                self.persist();
            }
            Message::CopyRepos => {
                let names: Vec<_> = if let Some(index) = self.selected {
                    self.config
                        .repositories
                        .get(index)
                        .map(|repo| repo.name.clone())
                        .into_iter()
                        .collect()
                } else {
                    self.config
                        .repositories
                        .iter()
                        .map(|repo| repo.name.clone())
                        .collect()
                };
                if !names.is_empty() {
                    self.log(format!("Copied {} repos to clipboard", names.len()));
                    return iced::clipboard::write(names.join("\n"));
                }
            }
            Message::Browse => {
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("SauceCache download folder")
                            .pick_folder()
                            .await
                            .map(|file| file.path().to_string_lossy().into_owned())
                    },
                    Message::Folder,
                );
            }
            Message::Folder(Some(path)) => {
                self.config.output_dir = path;
                self.persist();
            }
            Message::Folder(None) => {}
            Message::Interval(interval) => {
                self.config.interval_minutes = interval.0;
                self.persist();
                if self.watching && !self.busy {
                    self.next_check = Some(Instant::now() + Duration::from_secs(interval.0 * 60));
                }
            }
            Message::Tab(tab) => {
                self.save_mqtt_edits();
                if let Some(Dialog::Settings { tab: current, .. }) = &mut self.dialog {
                    *current = tab;
                }
            }
            Message::Field(field, value) => {
                if let Some(Dialog::Settings {
                    draft,
                    password,
                    suffixes,
                    port,
                    ..
                }) = &mut self.dialog
                {
                    match field {
                        Field::Host => draft.mqtt.host = value,
                        Field::Port => *port = value,
                        Field::Username => draft.mqtt.username = value,
                        Field::Password => *password = value,
                        Field::Topic => draft.mqtt.topic = value,
                        Field::Suffixes => *suffixes = value,
                    }
                    if matches!(field, Field::Suffixes) {
                        self.config.suffixes = config::suffixes(suffixes);
                        self.persist();
                    } else {
                        self.mqtt_dirty = true;
                        self.mqtt_save_at = Some(Instant::now() + Duration::from_millis(650));
                    }
                }
            }
            Message::Toggle(toggle, value) => {
                if let Some(Dialog::Settings { draft, .. }) = &mut self.dialog {
                    match toggle {
                        Toggle::Tray => draft.close_to_tray = value,
                        Toggle::Mqtt => draft.mqtt.enabled = value,
                        Toggle::Tls => draft.mqtt.tls = value,
                        Toggle::Status => draft.mqtt.publish_status = value,
                        Toggle::Repositories => draft.mqtt.publish_repositories = value,
                        Toggle::Downloads => draft.mqtt.publish_downloads = value,
                    }
                    if matches!(toggle, Toggle::Tray) {
                        self.config.close_to_tray = value;
                        self.persist();
                    } else {
                        self.mqtt_dirty = true;
                        self.save_mqtt_edits();
                    }
                }
            }
            Message::TestMqtt => {
                if !self.save_mqtt_edits() {
                    return Task::none();
                }
                if let Some(Dialog::Settings {
                    draft,
                    password,
                    port,
                    ..
                }) = &self.dialog
                {
                    let mut settings = match draft.mqtt.validated_with_port(port) {
                        Ok(settings) => settings,
                        Err(error) => {
                            self.mqtt_feedback = error;
                            return Task::none();
                        }
                    };
                    settings.enabled = true;
                    if let Err(error) = settings.validate() {
                        self.mqtt_feedback = error;
                    } else {
                        self.mqtt_feedback = "Testing connection…".into();
                        mqtt::test(settings, password.clone(), self.tx.clone());
                    }
                }
            }
            Message::Provider(value) => {
                if let Some(Dialog::Add { provider, .. }) = &mut self.dialog {
                    *provider = value;
                }
            }
            Message::Credits => {
                if let Err(error) = webbrowser::open("https://github.com/SkylerWallace/sk-iced") {
                    self.error = error.to_string();
                }
            }
        }
        Task::none()
    }

    pub fn view(&self) -> Element<'_, Message> {
        let ui = self.ui();
        let editable = !self.busy && !self.watching;
        let mut choices = INTERVALS.to_vec();
        let selected = Interval(self.config.interval_minutes);
        if !choices.contains(&selected) {
            choices.push(selected);
        }
        let interval = row![
            text("Check every").size(13),
            Dropdown::new(ui.appearance, choices, Some(selected), Message::Interval).width(110)
        ]
        .spacing(6)
        .align_y(iced::Center);
        let actions = row![
            image(image::Handle::from_bytes(
                include_bytes!("../assets/saucecache.png").as_slice()
            ))
            .width(24)
            .height(24),
            ui.button("Start watching")
                .text_size(13)
                .padding([7, 8])
                .tone(ButtonTone::Important)
                .style(iced::widget::button::success)
                .enabled(editable)
                .on_press(Message::Start),
            ui.button("Check now")
                .text_size(13)
                .padding([7, 8])
                .enabled(!self.busy && !self.watching)
                .on_press(Message::Check),
            ui.button("Stop")
                .text_size(13)
                .padding([7, 8])
                .tone(ButtonTone::Danger)
                .enabled(self.busy || self.watching)
                .on_press(Message::Stop),
        ]
        .spacing(6)
        .align_y(iced::Center);
        let toolbar = row![
            actions,
            Space::new().width(8),
            interval,
            row![
                Space::new().width(Fill),
                ui.button("💡").width(36).on_press(Message::Theme),
                ui.button("⚙").width(36).on_press(Message::Settings),
            ]
            .spacing(6)
            .width(Fill),
        ]
        .spacing(8)
        .align_y(iced::Center);
        let down = self
            .config
            .repositories
            .iter()
            .filter(|r| r.unavailable)
            .count();
        let summary = row![
            text("Repos").size(16),
            Space::new().width(Fill),
            column![
                text(format!("Unavailable: {down}")),
                text(format!("MQTT: {}", self.mqtt_status)).size(13).color(
                    if self.mqtt_connected {
                        iced::Color::from_rgb8(40, 175, 100)
                    } else {
                        iced::Color::from_rgb8(225, 80, 80)
                    }
                )
            ]
            .align_x(iced::Right)
        ]
        .align_y(iced::Center);
        let mut repositories = column![].spacing(2).width(Fill);
        if self.config.repositories.is_empty() {
            repositories = repositories
                .push(text("Your release pantry is empty. Add a repo to get started.").size(13));
        }
        for (index, repo) in self.config.repositories.iter().enumerate() {
            let badge = StatusBadge::new(
                ui.appearance,
                if repo.unavailable {
                    "Unavailable"
                } else {
                    "Active"
                },
                if repo.unavailable { 0xEF6666 } else { 0x23B56E },
            )
            .width(88)
            .height(20)
            .center_y(20);
            let item = row![badge, text(&repo.name).size(13).width(Fill)]
                .spacing(8)
                .align_y(iced::Center);
            let selected = self.selected == Some(index);
            let row = container(item)
                .padding([3, 8])
                .width(Fill)
                .style(move |theme: &Theme| container::Style {
                    background: Some(row_color(theme, index, selected).into()),
                    border: iced::Border {
                        radius: 6.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                });
            repositories = repositories.push(
                mouse_area(row)
                    .on_press(Message::Select(index))
                    .on_double_click(Message::OpenRepo(index)),
            );
        }
        let repository_list = container(
            ScrollArea::new(ui.appearance, repositories)
                .width(Fill)
                .height(Fill)
                .vertical(Scrollbar::embedded()),
        )
        .height(Fill)
        .width(Fill)
        .padding(6)
        .style(container::bordered_box);
        let repository_list = KeyboardControl::new(ui.appearance, repository_list)
            .activation(false)
            .enabled(!self.config.repositories.is_empty())
            .on_key(move |key| {
                use iced::keyboard::key::Named;
                let count = self.config.repositories.len();
                if count == 0 {
                    return None;
                }
                match key {
                    Named::ArrowDown | Named::ArrowUp if count == 1 && self.selected == Some(0) => {
                        None
                    }
                    Named::ArrowDown => Some(Message::Select(
                        self.selected.map_or(0, |i| (i + 1) % count),
                    )),
                    Named::ArrowUp => Some(Message::Select(
                        self.selected.map_or(count - 1, |i| (i + count - 1) % count),
                    )),
                    Named::Enter => self.selected.map(Message::OpenRepo),
                    Named::Space if self.selected.is_none() => Some(Message::Select(0)),
                    _ => None,
                }
            });
        let folder = row![
            text("Download folder").size(13).width(110),
            TextMenu::input(
                ui.text_input("Choose a folder…", &self.config.output_dir)
                    .on_input(Message::Output)
                    .padding([7, 10])
                    .width(Fill),
                &self.config.output_dir
            )
            .on_focus(Message::FocusTextBox),
            ui.button("Browse…").height(36).on_press(Message::Browse)
        ]
        .spacing(8)
        .align_y(iced::Center);
        let status = text(if self.busy {
            "Checking releases…".into()
        } else if let Some(at) = self.next_check {
            format!(
                "Next check in {}:{:02}",
                at.saturating_duration_since(Instant::now()).as_secs() / 60,
                at.saturating_duration_since(Instant::now()).as_secs() % 60
            )
        } else if self.watching {
            "Watching".into()
        } else {
            "Idle".into()
        })
        .size(12);
        let mut controls = column![
            row![
                ui.button("Add repos…")
                    .text_size(13)
                    .enabled(editable)
                    .on_press(Message::Add),
                ui.button("Remove repos…")
                    .text_size(13)
                    .enabled(editable)
                    .on_press(Message::Remove),
                Space::new().width(8),
                ui.button("Copy repos to clipboard")
                    .text_size(13)
                    .enabled(!self.config.repositories.is_empty())
                    .on_press(Message::CopyRepos)
            ]
            .spacing(8),
            folder,
            status
        ]
        .spacing(8);
        if let Some((name, done, total)) = &self.progress {
            controls = controls
                .push(text(format!("{name} — {done} / {total} bytes")).size(13))
                .push(progress_bar(
                    0.0..=1.0,
                    if *total > 0 {
                        *done as f32 / *total as f32
                    } else {
                        0.0
                    },
                ));
        }
        let log = column(
            self.logs
                .iter()
                .rev()
                .map(|line| text(line).size(12).width(Fill).into()),
        )
        .spacing(4)
        .width(Fill);
        let activity = column![
            text("Activity").size(18),
            TextMenu::copy_area(
                container(
                    ScrollArea::new(ui.appearance, log)
                        .width(Fill)
                        .height(Fill)
                        .vertical(Scrollbar::embedded()),
                )
                .height(Fill)
                .width(Fill)
                .padding(6)
                .style(container::bordered_box),
                !self.logs.is_empty(),
                || self.logs.copy_text()
            )
            .on_focus(Message::FocusTextBox),
        ]
        .spacing(8)
        .height(Fill);
        if !self.error.is_empty() && self.dialog.is_none() {
            controls = controls.push(text(&self.error).color(iced::Color::from_rgb8(225, 100, 80)));
        }
        let body = column![
            summary,
            crate::repo_split::RepoSplit::new(
                repository_list,
                controls,
                activity,
                self.repo_ratio,
                Message::ResizeRepos
            )
        ]
        .spacing(8)
        .height(Fill);
        let base = container(column![toolbar, body].spacing(10))
            .padding(12)
            .width(Fill)
            .height(Fill);
        let base = FocusScope::new(base).active(self.dialog.is_none());
        let dialog = self.dialog_view();
        RedrawBoundary::new(
            Popup::new(base, dialog)
                .open(self.dialog.is_some())
                .centered(true)
                .fit_viewport(true)
                .max_width(720.0)
                .scrim(iced::Color::from_rgba(0.0, 0.0, 0.0, 0.2))
                .on_dismiss(Message::Dismiss)
                .policy(ui.motion),
        )
        .full_frame(true)
        .into()
    }
    fn dialog_view(&self) -> Element<'_, Message> {
        let ui = self.ui();
        let Some(dialog) = &self.dialog else {
            return text("").into();
        };
        let mut content = column![].spacing(14).width(Fill);
        match dialog {
            Dialog::Add {
                content: input,
                provider,
            } => {
                content = content
                    .push(text("Add repos").size(22))
                    .push(Dropdown::new(
                        ui.appearance,
                        source::PROVIDERS.to_vec(),
                        Some(*provider),
                        Message::Provider,
                    ))
                    .push(
                        text("Full HTTPS repo URLs, one per line. owner/repo defaults to GitHub.")
                            .size(13),
                    )
                    .push(
                        text("Auto-detect identifies public self-hosted Gitea/Forgejo and GitLab APIs when checked.").size(12),
                    )
                    .push(
                        TextMenu::editor(
                            text_editor(input).height(Fill).on_action(Message::Edit),
                            input,
                        )
                        .on_focus(Message::FocusTextBox),
                    )
                    .push(
                        row![
                            ui.button("Cancel").on_press(Message::Dismiss),
                            ui.button("Add")
                                .tone(ButtonTone::Important)
                                .on_press(Message::ConfirmAdd)
                        ]
                        .spacing(10),
                    );
            }
            Dialog::Remove(selected) => {
                content = content
                    .push(text("Remove repos").size(22))
                    .push(text("Select repos to forget. Downloaded files are kept."));
                let mut choices = column![].spacing(10);
                for (index, repo) in self.config.repositories.iter().enumerate() {
                    choices = choices.push(
                        container(
                            row![
                                ui.checkbox(selected.contains(&index))
                                    .label(repo.name.clone())
                                    .on_toggle(move |checked| Message::RemoveSelect(
                                        index, checked
                                    )),
                                Space::new().width(Fill),
                                StatusBadge::new(
                                    ui.appearance,
                                    if repo.unavailable {
                                        "Unavailable"
                                    } else {
                                        "Active"
                                    },
                                    if repo.unavailable { 0xEF6666 } else { 0x23B56E }
                                )
                                .width(100)
                            ]
                            .spacing(10)
                            .align_y(iced::Center),
                        )
                        .padding(8)
                        .style(move |theme: &Theme| container::Style {
                            background: Some(row_color(theme, index, false).into()),
                            ..Default::default()
                        }),
                    );
                }
                content = content
                    .push(ScrollArea::new(ui.appearance, choices).height(220))
                    .push(
                        row![
                            ui.button("Cancel").on_press(Message::Dismiss),
                            ui.button("Remove selected")
                                .tone(ButtonTone::Danger)
                                .on_press(Message::ConfirmRemove)
                        ]
                        .spacing(10),
                    );
            }
            Dialog::Settings {
                draft,
                password,
                suffixes,
                port,
                tab,
            } => {
                let mut tabs = row![].spacing(4).width(Fill);
                for (index, label) in ["General", "MQTT", "About"].iter().enumerate() {
                    tabs = tabs.push(
                        ui.button(*label)
                            .width(Fill)
                            .height(28)
                            .tone(if *tab == index {
                                ButtonTone::Important
                            } else {
                                ButtonTone::Default
                            })
                            .id(format!("settings-tab-{index}"))
                            .on_press(Message::Tab(index)),
                    );
                }
                content = content.push(tabs);
                match *tab {
                    0 => {
                        content = content
                            .push(text("Limit Extensions to Download"))
                            .push(TextMenu::input(ui.text_input(".zip, .msi, .AppImage", suffixes).on_input(|s| Message::Field(Field::Suffixes, s)), suffixes).on_focus(Message::FocusTextBox))
                            .push(text("Separate extensions with commas. Leave blank to download all files. Applies to the next check.").size(12))
                            .push(iced::widget::rule::horizontal(1))
                            .push(
                                ui.checkbox(draft.close_to_tray)
                                    .label("Minimize to tray when closed")
                                    .on_toggle(|v| Message::Toggle(Toggle::Tray, v)),
                            )
                            .push(
                                text("Enabled by default. Minimizing always goes to the tray.")
                                    .size(12),
                            )
                            .push(iced::widget::rule::horizontal(1))
                            .push(text("Configuration Backup & Restore"))
                            .push(row![
                                ui.button("Back up to ZIP…").enabled(!self.transfer_pending).on_press(Message::BackupConfig),
                                ui.button("Restore from ZIP…").enabled(!self.transfer_pending && !self.busy && !self.watching).on_press(Message::RestoreConfig)
                            ].spacing(8))
                            .push(text("Back up settings and repos. Changes save automatically. Stop watching before restoring.").size(12));
                        if !self.transfer_status.is_empty() {
                            content = content.push(text(&self.transfer_status).size(12));
                        }
                    }
                    1 => {
                        content = content.spacing(10).push(
                            ui.checkbox(draft.mqtt.enabled)
                                .label("Enable MQTT")
                                .on_toggle(|v| Message::Toggle(Toggle::Mqtt, v)),
                        );
                        for (label, value, field) in [
                            ("Broker hostname / IP", &draft.mqtt.host, Field::Host),
                            ("Port", port, Field::Port),
                            ("Username", &draft.mqtt.username, Field::Username),
                            ("Password", password, Field::Password),
                            ("Topic prefix", &draft.mqtt.topic, Field::Topic),
                        ] {
                            let input = ui
                                .text_input(label, value)
                                .secure(matches!(field, Field::Password))
                                .width(if matches!(field, Field::Port) {
                                    80
                                } else {
                                    240
                                })
                                .on_input(move |s| Message::Field(field, s));
                            let editor: Element<'_, Message> = if matches!(field, Field::Port) {
                                row![
                                    TextMenu::input(input, value).on_focus(Message::FocusTextBox),
                                    ui.checkbox(draft.mqtt.tls)
                                        .label("TLS (8883)")
                                        .on_toggle(|v| Message::Toggle(Toggle::Tls, v))
                                ]
                                .spacing(10)
                                .align_y(iced::Center)
                                .into()
                            } else {
                                TextMenu::input(input, value)
                                    .on_focus(Message::FocusTextBox)
                                    .into()
                            };
                            content = content.push(
                                row![text(label).size(13).width(140), editor]
                                    .spacing(8)
                                    .width(Fill)
                                    .align_y(iced::Center),
                            );
                        }
                        for (label, value, toggle) in [
                            (
                                "Publish application status",
                                draft.mqtt.publish_status,
                                Toggle::Status,
                            ),
                            (
                                "Publish unavailable-repo counts",
                                draft.mqtt.publish_repositories,
                                Toggle::Repositories,
                            ),
                            (
                                "Publish download notifications",
                                draft.mqtt.publish_downloads,
                                Toggle::Downloads,
                            ),
                        ] {
                            content = content.push(
                                ui.checkbox(value)
                                    .label(label)
                                    .on_toggle(move |v| Message::Toggle(toggle, v)),
                            );
                        }
                        content = content.push(text("Passwords use Windows Credential Manager. No remote MQTT commands.").size(12));
                    }
                    _ => {
                        content = content
                            .push(text("SauceCache").size(26))
                            .push(text(concat!("Version ", env!("CARGO_PKG_VERSION"))))
                            .push(text("A release pantry for your favorite repos."))
                            .push(Space::new().height(12))
                            .push(text("Shoutout to SkylerWallace for the UI toolkit and design assistance;"))
                            .push(ui.button("SkylerWallace / sk-iced").on_press(Message::Credits));
                    }
                }
            }
        }
        let mqtt_tab = matches!(dialog, Dialog::Settings { tab: 1, .. });
        if !self.error.is_empty() {
            content = content.push(text(&self.error).color(iced::Color::from_rgb8(230, 100, 80)));
        }
        let height = match dialog {
            Dialog::Settings { .. } => 500,
            Dialog::Remove(_) => 350,
            Dialog::Add { .. } => 380,
        };
        let body: Element<'_, Message> = if matches!(dialog, Dialog::Add { .. }) {
            content.height(height).into()
        } else {
            ScrollArea::new(ui.appearance, content)
                .width(Fill)
                .vertical(Scrollbar::embedded())
                .height(if matches!(dialog, Dialog::Settings { .. }) {
                    Fill
                } else {
                    height.into()
                })
                .into()
        };
        let panel: Element<'_, Message> = if matches!(dialog, Dialog::Settings { .. }) {
            let mut panel = column![
                row![
                    column![
                        text("Settings").size(18),
                        text("Changes save automatically.").size(11)
                    ],
                    Space::new().width(Fill),
                    ui.button("×")
                        .id("settings-close")
                        .text_size(14)
                        .padding(0)
                        .width(24)
                        .height(24)
                        .on_press(Message::Dismiss)
                ]
                .align_y(iced::Center),
                body
            ]
            .spacing(10);
            if mqtt_tab {
                let result = text(&self.mqtt_feedback).size(12).width(Fill).color(
                    if self.mqtt_feedback == "Connection successful" {
                        iced::Color::from_rgb8(40, 175, 100)
                    } else if self.mqtt_feedback.is_empty()
                        || self.mqtt_feedback == "Testing connection…"
                    {
                        self.theme().palette().text
                    } else {
                        iced::Color::from_rgb8(230, 100, 80)
                    },
                );
                panel = panel.push(iced::widget::rule::horizontal(1)).push(
                    row![
                        ui.button("Test connection")
                            .text_size(13)
                            .on_press(Message::TestMqtt),
                        ScrollArea::new(ui.appearance, result)
                            .width(Fill)
                            .height(52)
                            .vertical(Scrollbar::embedded())
                    ]
                    .spacing(10)
                    .align_y(iced::Center),
                );
            }
            panel.height(height).into()
        } else {
            body
        };
        let dialog = container(panel)
            .padding(18)
            .width(if matches!(dialog, Dialog::Settings { .. }) {
                480
            } else {
                600
            })
            .style(|theme: &Theme| container::Style {
                border: iced::Border {
                    width: 2.0,
                    color: theme.extended_palette().primary.strong.color,
                    radius: 10.0.into(),
                },
                ..container::rounded_box(theme)
            });
        let scope = FocusScope::new(dialog);
        if matches!(self.dialog, Some(Dialog::Settings { .. })) {
            scope.autofocus("settings-close").into()
        } else {
            scope.into()
        }
    }
}

fn row_color(theme: &Theme, index: usize, selected: bool) -> iced::Color {
    if selected {
        return theme.extended_palette().primary.weak.color;
    }
    let dark = theme.palette().background.r < 0.5;
    if dark {
        if index.is_multiple_of(2) {
            iced::Color::from_rgb8(30, 35, 44)
        } else {
            iced::Color::from_rgb8(41, 47, 59)
        }
    } else if index.is_multiple_of(2) {
        iced::Color::from_rgb8(255, 255, 255)
    } else {
        iced::Color::from_rgb8(237, 242, 248)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn check_settings_tab_navigation(app: &mut App, tab: usize, expected: usize) {
        use iced::advanced::{
            Layout, Shell, layout, mouse,
            widget::{Id, Operation, Tree, operation::Focusable},
        };
        #[derive(Default)]
        struct Snapshot {
            stops: Vec<(Option<Id>, bool)>,
            scrolls: Vec<f32>,
        }
        impl Operation for Snapshot {
            fn traverse(&mut self, f: &mut dyn FnMut(&mut dyn Operation)) {
                f(self);
            }
            fn focusable(
                &mut self,
                id: Option<&Id>,
                _: iced::Rectangle,
                state: &mut dyn Focusable,
            ) {
                self.stops.push((id.cloned(), state.is_focused()));
            }
            fn scrollable(
                &mut self,
                _: Option<&Id>,
                _: iced::Rectangle,
                _: iced::Rectangle,
                translation: iced::Vector,
                _: &mut dyn iced::advanced::widget::operation::Scrollable,
            ) {
                self.scrolls.push(translation.y);
            }
        }
        let _ = app.update(Message::Settings);
        let _ = app.update(Message::Tab(tab));
        let renderer = iced::Renderer::new(iced::Font::DEFAULT, iced::Pixels(16.0));
        // This mimics the disabled background plus the modal dialog at a small window size.
        let outside =
            FocusScope::new(app.ui().button("Outside").on_press(Message::Check)).active(false);
        let mut root: Element<'_, Message> =
            iced::widget::column![outside, app.dialog_view()].into();
        let mut tree = Tree::new(&root);
        let limits = layout::Limits::new(iced::Size::ZERO, iced::Size::new(480.0, 400.0));
        let mut node = root.as_widget_mut().layout(&mut tree, &renderer, &limits);
        let mut initial = Snapshot::default();
        root.as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut initial);
        assert_eq!(
            initial.stops.len(),
            expected,
            "Disabled background controls must be excluded"
        );
        assert_eq!(initial.stops[0].0, Some(Id::new("settings-close")));
        for index in 0..3 {
            assert_eq!(
                initial.stops[index + 1].0,
                Some(Id::from(format!("settings-tab-{index}")))
            );
        }
        assert!(initial.stops[0].1);
        let mut clipboard = iced::advanced::clipboard::Null;
        let mut saw_scroll = false;
        for reverse in [false, true] {
            for step in 1..=expected {
                let event = iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Tab),
                    modified_key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Tab),
                    physical_key: iced::keyboard::key::Physical::Code(
                        iced::keyboard::key::Code::Tab,
                    ),
                    location: iced::keyboard::Location::Standard,
                    modifiers: if reverse {
                        iced::keyboard::Modifiers::SHIFT
                    } else {
                        iced::keyboard::Modifiers::default()
                    },
                    text: None,
                    repeat: false,
                });
                let mut messages = vec![];
                root.as_widget_mut().update(
                    &mut tree,
                    &event,
                    Layout::new(&node),
                    mouse::Cursor::Unavailable,
                    &renderer,
                    &mut clipboard,
                    &mut Shell::new(&mut messages),
                    &Layout::new(&node).bounds(),
                );
                assert!(
                    messages.is_empty(),
                    "Tab must focus controls without activating them"
                );
                node = root.as_widget_mut().layout(&mut tree, &renderer, &limits);
                let mut snapshot = Snapshot::default();
                root.as_widget_mut().operate(
                    &mut tree,
                    Layout::new(&node),
                    &renderer,
                    &mut snapshot,
                );
                let focused: Vec<_> = snapshot
                    .stops
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| item.1)
                    .map(|(index, _)| index)
                    .collect();
                assert_eq!(
                    focused,
                    vec![if reverse {
                        (expected - step % expected) % expected
                    } else {
                        step % expected
                    }]
                );
                saw_scroll |= snapshot.scrolls.iter().any(|offset| *offset > 0.0);
            }
        }
        if tab == 1 {
            assert!(saw_scroll, "Tab should reveal offscreen MQTT controls");
        }
    }
    #[test]
    #[ignore = "Uses isolated APPDATA and a temporary tray; run alone explicitly"]
    fn settings_autosave_survives_reopening_and_invalid_mqtt_edits() {
        struct Fixture {
            appdata: Option<std::ffi::OsString>,
            directory: std::path::PathBuf,
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                // This test is run alone. It never opens or changes the user's settings.
                unsafe {
                    if let Some(value) = &self.appdata {
                        std::env::set_var("APPDATA", value);
                    } else {
                        std::env::remove_var("APPDATA");
                    }
                }
                let _ = std::fs::remove_dir_all(&self.directory);
            }
        }
        let directory = std::env::temp_dir().join(format!(
            "saucecache-autosave-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let _fixture = Fixture {
            appdata: std::env::var_os("APPDATA"),
            directory: directory.clone(),
        };
        unsafe {
            std::env::set_var("APPDATA", &directory);
        }
        let (mut app, _) = App::new();
        assert!(app.mqtt_feedback.is_empty());
        let original =
            source::parse("https://self-hosted.example/team/project", Provider::GitLab).unwrap();
        app.config.repositories.push(original.clone());
        let mut detected = original.clone();
        detected.provider = Provider::Gitea;
        app.tx
            .send(Event::SourceResolved {
                previous: original.name,
                repository: detected,
            })
            .unwrap();
        let _ = app.update(Message::Tick);
        assert_eq!(
            Config::load().unwrap().repositories[0].provider,
            Provider::Gitea
        );
        let _ = app.update(Message::Settings);
        let _ = app.update(Message::Toggle(Toggle::Tray, false));
        let _ = app.update(Message::Field(Field::Suffixes, ".zip, .msi".into()));
        let _ = app.update(Message::Field(Field::Port, "8884".into()));
        app.mqtt_save_at = Some(Instant::now() - Duration::from_secs(1));
        let _ = app.update(Message::Tick);
        assert!(app.dialog.is_some());
        assert_eq!(Config::load().unwrap().mqtt.port, 8884);
        let _ = app.update(Message::Toggle(Toggle::Status, false));
        assert!(!Config::load().unwrap().mqtt.publish_status);
        let _ = app.update(Message::Field(Field::Port, "8883".into()));
        let _ = app.update(Message::Dismiss); // Closing immediately flushes pending edits.
        assert!(app.dialog.is_none());
        let saved = Config::load().unwrap();
        assert!(!saved.close_to_tray);
        assert_eq!(saved.suffixes, vec![".zip", ".msi"]);
        assert_eq!(saved.mqtt.port, 8883);
        drop(app);
        let (mut reopened, _) = App::new();
        assert_eq!(reopened.config.mqtt.port, 8883);
        let _ = reopened.update(Message::Settings);
        let _ = reopened.update(Message::Field(Field::Port, String::new()));
        let _ = reopened.update(Message::Toggle(Toggle::Tray, true));
        let _ = reopened.update(Message::Dismiss);
        assert!(reopened.dialog.is_some());
        let saved = Config::load().unwrap();
        assert_eq!(saved.mqtt.port, 8883);
        assert!(saved.close_to_tray); // General changes still save during a partial MQTT edit.
        let _ = reopened.update(Message::Field(Field::Port, "1883".into()));
        let _ = reopened.update(Message::Dismiss);
        assert!(reopened.dialog.is_none());
        assert_eq!(Config::load().unwrap().mqtt.port, 1883);
        let _ = reopened.update(Message::Settings);
        let _ = reopened.update(Message::Tab(1));
        let _ = reopened.update(Message::TestMqtt);
        assert_eq!(reopened.mqtt_feedback, "Enter a broker hostname or IP");
        assert!(reopened.error.is_empty());
        check_settings_tab_navigation(&mut reopened, 0, 8);
        check_settings_tab_navigation(&mut reopened, 1, 15);
        check_settings_tab_navigation(&mut reopened, 2, 5);
        let _ = reopened.update(Message::Tab(0));
        reopened
            .tx
            .send(Event::MqttTest("Connection successful".into()))
            .unwrap();
        let _ = reopened.update(Message::Tick);
        assert_eq!(reopened.mqtt_feedback, "Connection successful");
        assert!(reopened.error.is_empty());
    }
}
