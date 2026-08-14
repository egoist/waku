//! Daemon-owned, user-editable configuration.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use uuid::Uuid;
pub use waku_protocol::settings::DaemonSettings;

pub struct DaemonSettingsStore {
    path: PathBuf,
    settings: Mutex<DaemonSettings>,
}

impl DaemonSettingsStore {
    pub fn open(path: PathBuf) -> io::Result<Self> {
        Self::open_with_legacy(path, std::iter::empty())
    }

    /// Open the daemon document, importing the first old combined settings
    /// file only when the new path does not exist. The source is left intact
    /// so the desktop can independently migrate its app-owned fields.
    pub fn open_with_legacy(
        path: PathBuf,
        legacy_paths: impl IntoIterator<Item = PathBuf>,
    ) -> io::Result<Self> {
        let (mut settings, write_current) = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(settings) => (settings, false),
                Err(error) => {
                    let backup = quarantine_corrupt_settings(&path)?;
                    eprintln!(
                        "Waku daemon moved invalid settings to {}: {error}",
                        backup.display()
                    );
                    (DaemonSettings::default(), true)
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut restored: Option<DaemonSettings> = None;
                for legacy_path in legacy_paths {
                    if legacy_path == path {
                        continue;
                    }
                    match fs::read(legacy_path) {
                        Ok(bytes) => {
                            if let Ok(settings) = serde_json::from_slice(&bytes) {
                                restored = Some(settings);
                                break;
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error),
                    }
                }
                let migrated = restored.is_some();
                (restored.unwrap_or_default(), migrated)
            }
            Err(error) => return Err(error),
        };
        settings.discard_legacy_app_keys();
        if write_current {
            write_atomic(&path, &settings)?;
        }
        Ok(Self {
            path,
            settings: Mutex::new(settings),
        })
    }

    pub fn get(&self) -> DaemonSettings {
        self.settings.lock().clone()
    }

    pub fn replace(&self, mut settings: DaemonSettings) -> io::Result<()> {
        settings.discard_legacy_app_keys();
        let mut current = self.settings.lock();
        write_atomic(&self.path, &settings)?;
        *current = settings;
        Ok(())
    }
}

fn quarantine_corrupt_settings(path: &Path) -> io::Result<PathBuf> {
    let extension = format!("json.corrupt-{}", Uuid::new_v4().simple());
    let backup = path.with_extension(extension);
    fs::rename(path, &backup)?;
    Ok(backup)
}

fn write_atomic(path: &Path, settings: &DaemonSettings) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_vec_pretty(settings).map_err(to_io_error)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, data)?;
    fs::rename(temporary, path)
}

fn to_io_error(error: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use uuid::Uuid;
    use waku_protocol::model::ProviderKind;

    #[test]
    fn legacy_combined_settings_keep_only_daemon_fields() {
        let path = std::env::temp_dir().join(format!("waku-settings-{}.json", Uuid::new_v4()));
        fs::write(
            &path,
            r#"{"theme":"dark","analytics_enabled":false,"computer_use_enabled":true,"future":42}"#,
        )
        .unwrap();

        let store = DaemonSettingsStore::open(path.clone()).unwrap();
        let settings = store.get();
        assert!(settings.computer_use_enabled);
        assert_eq!(settings.extra.get("future"), Some(&Value::from(42)));
        assert!(!settings.extra.contains_key("theme"));
        store.replace(settings).unwrap();

        let value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(value.get("theme").is_none());
        assert!(value.get("analytics_enabled").is_none());
        assert_eq!(value["future"], 42);
        fs::remove_file(path).ok();
    }

    #[test]
    fn imports_daemon_fields_from_a_debug_combined_settings_file() {
        let directory = std::env::temp_dir().join(format!("waku-settings-{}", Uuid::new_v4()));
        let path = directory.join("home/settings.json");
        let legacy = directory.join("checkout/temp/settings.json");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(
            &legacy,
            r#"{"theme":"dark","disabled_providers":["claude"]}"#,
        )
        .unwrap();

        let store = DaemonSettingsStore::open_with_legacy(path.clone(), [legacy.clone()]).unwrap();

        assert_eq!(store.get().disabled_providers, vec![ProviderKind::Claude]);
        let migrated: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(migrated["disabled_providers"][0], "claude");
        assert!(migrated.get("theme").is_none());
        assert!(legacy.exists());
        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn corrupt_current_settings_are_quarantined_and_replaced() {
        let directory = std::env::temp_dir().join(format!("waku-settings-{}", Uuid::new_v4()));
        let path = directory.join("settings.json");
        fs::create_dir_all(&directory).unwrap();
        fs::write(&path, b"{ definitely not json").unwrap();

        let store = DaemonSettingsStore::open(path.clone()).unwrap();

        assert_eq!(store.get(), DaemonSettings::default());
        assert!(serde_json::from_slice::<DaemonSettings>(&fs::read(&path).unwrap()).is_ok());
        assert!(fs::read_dir(&directory).unwrap().flatten().any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("settings.json.corrupt-")
        }));
        fs::remove_dir_all(directory).ok();
    }
}
