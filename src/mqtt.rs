use crate::{config::MqttSettings, github::Event};
use rumqttc::{
    Client, Connection, Event as MqttEvent, Incoming, LastWill, MqttOptions, QoS, TlsConfiguration,
    Transport,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Sender},
    },
    time::{Duration, Instant},
};

const SERVICE: &str = "SauceCache Rust MQTT";
pub fn password(settings: &MqttSettings) -> String {
    read_password(settings).ok().flatten().unwrap_or_default()
}
pub fn read_password(settings: &MqttSettings) -> Result<Option<String>, String> {
    if settings.host.is_empty() {
        return Ok(None);
    }
    let entry = keyring::Entry::new(SERVICE, &settings.credential()).map_err(|e| e.to_string())?;
    match entry.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(format!("Could not read the saved MQTT password: {error}")),
    }
}
pub fn save_password(settings: &MqttSettings, value: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(SERVICE, &settings.credential()).map_err(|e| e.to_string())?;
    if value.is_empty() {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    } else {
        entry.set_password(value).map_err(|e| e.to_string())
    }
}
pub fn save_configuration(config: &crate::config::Config, password: &str) -> Result<bool, String> {
    save_configuration_with(config, password, crate::config::Config::save)
}
pub(crate) fn save_configuration_with(
    config: &crate::config::Config,
    password: &str,
    save: impl FnOnce(&crate::config::Config) -> Result<(), String>,
) -> Result<bool, String> {
    let previous = read_password(&config.mqtt)?;
    let changed =
        !config.mqtt.host.is_empty() && previous.as_deref().unwrap_or_default() != password;
    if changed {
        save_password(&config.mqtt, password)?;
    }
    if let Err(error) = save(config) {
        if changed && save_password(&config.mqtt, previous.as_deref().unwrap_or_default()).is_err()
        {
            return Err(format!(
                "Could not save config: {error}. The MQTT credential could not be rolled back."
            ));
        }
        return Err(format!("Could not save config: {error}"));
    }
    Ok(changed)
}
fn connection(
    settings: &MqttSettings,
    secret: &str,
    will: bool,
) -> Result<(Client, Connection), String> {
    settings.validate()?;
    if settings.host.is_empty() {
        return Err("Enter an MQTT broker".into());
    }
    let id = format!(
        "saucecache-rust-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_millis()
    );
    let mut options = MqttOptions::new(id, &settings.host, settings.port);
    options.set_keep_alive(Duration::from_secs(30));
    if !settings.username.is_empty() {
        options.set_credentials(&settings.username, secret);
    }
    if settings.tls {
        options.set_transport(Transport::tls_with_config(TlsConfiguration::Native));
    }
    if will {
        options.set_last_will(LastWill::new(
            format!("{}/availability", settings.topic),
            "offline",
            QoS::AtLeastOnce,
            true,
        ));
    }
    Ok(Client::new(options, 100))
}
enum Command {
    Publish(String, String, bool),
    Stop,
}
pub struct Mqtt {
    commands: Option<Sender<Command>>,
    connected: Arc<AtomicBool>,
}
impl Default for Mqtt {
    fn default() -> Self {
        Self {
            commands: None,
            connected: Arc::new(AtomicBool::new(false)),
        }
    }
}
impl Drop for Mqtt {
    fn drop(&mut self) {
        self.stop();
    }
}
impl Mqtt {
    pub fn stop(&mut self) {
        if let Some(sender) = self.commands.take() {
            let _ = sender.send(Command::Stop);
        }
        self.connected.store(false, Ordering::Relaxed);
    }
    pub fn configure(&mut self, settings: MqttSettings, events: Sender<Event>) {
        self.stop();
        self.connected = Arc::new(AtomicBool::new(false));
        if !settings.enabled {
            let _ = events.send(Event::Mqtt(false, "Disabled".into()));
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.commands = Some(tx);
        let connected = self.connected.clone();
        std::thread::spawn(move || {
            let _ = events.send(Event::Mqtt(false, "Connecting…".into()));
            let (client, mut connection) = match connection(&settings, &password(&settings), true) {
                Ok(pair) => pair,
                Err(error) => {
                    let _ = events.send(Event::Mqtt(false, error));
                    return;
                }
            };
            let topic = |name: &str| format!("{}/{name}", settings.topic.trim_matches('/'));
            let mut was_connected = false;
            loop {
                while let Ok(command) = rx.try_recv() {
                    match command {
                        Command::Publish(name, payload, retain) => {
                            if connected.load(Ordering::Relaxed) {
                                let _ = client.try_publish(
                                    topic(&name),
                                    QoS::AtLeastOnce,
                                    retain,
                                    payload,
                                );
                            }
                        }
                        Command::Stop => {
                            if connected.load(Ordering::Relaxed) {
                                let _ = client.try_publish(
                                    topic("availability"),
                                    QoS::AtLeastOnce,
                                    true,
                                    "offline",
                                );
                                let _ = client.try_disconnect();
                                let until = Instant::now() + Duration::from_secs(1);
                                while Instant::now() < until {
                                    if matches!(
                                        connection.recv_timeout(Duration::from_millis(50)),
                                        Ok(Ok(MqttEvent::Outgoing(rumqttc::Outgoing::Disconnect)))
                                    ) {
                                        break;
                                    }
                                }
                            }
                            connected.store(false, Ordering::Relaxed);
                            return;
                        }
                    }
                }
                match connection.recv_timeout(Duration::from_millis(100)) {
                    Ok(Ok(MqttEvent::Incoming(Incoming::ConnAck(_)))) => {
                        connected.store(true, Ordering::Relaxed);
                        was_connected = true;
                        let _ = client.try_publish(
                            topic("availability"),
                            QoS::AtLeastOnce,
                            true,
                            "online",
                        );
                        for (name, enabled) in [
                            ("status", settings.publish_status),
                            ("repositories", settings.publish_repositories),
                        ] {
                            if !enabled {
                                let _ = client.try_publish(topic(name), QoS::AtLeastOnce, true, "");
                            }
                        }
                        let _ = events.send(Event::Mqtt(true, "Connected".into()));
                    }
                    Ok(Err(_)) => {
                        connected.store(false, Ordering::Relaxed);
                        if was_connected {
                            let _ =
                                events.send(Event::Mqtt(false, "Disconnected; retrying".into()));
                            was_connected = false;
                        } else {
                            let _ = events
                                .send(Event::Mqtt(false, "Connection failed; retrying".into()));
                        }
                        std::thread::sleep(Duration::from_millis(500));
                    }
                    _ => {}
                }
            }
        });
    }
    pub fn publish(&self, settings: &MqttSettings, category: &str, payload: serde_json::Value) {
        let enabled = match category {
            "status" => settings.publish_status,
            "repositories" => settings.publish_repositories,
            "downloads" => settings.publish_downloads,
            _ => false,
        };
        if enabled
            && self.connected.load(Ordering::Relaxed)
            && let Some(sender) = &self.commands
        {
            let _ = sender.send(Command::Publish(
                category.into(),
                payload.to_string(),
                category != "downloads",
            ));
        }
    }
}
pub fn test(settings: MqttSettings, secret: String, events: Sender<Event>) {
    std::thread::spawn(move || {
        let result = (|| {
            let (client, mut connection) = connection(&settings, &secret, false)?;
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                match connection.recv_timeout(Duration::from_millis(250)) {
                    Ok(Ok(MqttEvent::Incoming(Incoming::ConnAck(_)))) => {
                        let _ = client.try_disconnect();
                        let _ = connection.recv_timeout(Duration::from_millis(100));
                        return Ok("Connection successful".to_string());
                    }
                    Ok(Err(_)) => return Err(
                        "Connection failed; check host, TLS, credentials and broker permissions"
                            .into(),
                    ),
                    _ => {}
                }
            }
            Err("Connection timed out".into())
        })();
        let _ = events.send(Event::MqttTest(result.unwrap_or_else(|error| error)));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn connection_test_only_handshakes() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut prefix = [0];
            stream.read_exact(&mut prefix).unwrap();
            assert_eq!(prefix[0], 0x10);
            let mut length = 0;
            let mut multiplier = 1;
            loop {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                length += (byte[0] & 127) as usize * multiplier;
                if byte[0] & 128 == 0 {
                    break;
                }
                multiplier *= 128;
            }
            let mut connect = vec![0; length];
            stream.read_exact(&mut connect).unwrap();
            stream.write_all(&[0x20, 0x02, 0, 0]).unwrap();
            let mut next = [0; 2];
            let received = stream.read(&mut next).unwrap_or(0);
            if received > 0 {
                assert_eq!(next[0], 0xe0, "test must disconnect without publishing");
            }
        });
        let (tx, rx) = mpsc::channel();
        let mut settings = MqttSettings::default();
        settings.host = "127.0.0.1".into();
        settings.port = port;
        test(settings, String::new(), tx);
        assert!(
            matches!(rx.recv_timeout(Duration::from_secs(5)).unwrap(),Event::MqttTest(value) if value=="Connection successful")
        );
        server.join().unwrap();
    }
}
