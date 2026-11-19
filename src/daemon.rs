//! Desktop ownership of the Waku daemon process.

use std::path::PathBuf;

use anyhow::{Context as _, anyhow, bail};

/// Where the desktop talks to its daemon.
enum DaemonTarget {
    /// Named by the environment: the user asked for this daemon, so failing
    /// to reach it is fatal rather than a silent switch.
    Required { address: String, token: String },
    /// A saved profile: an unreachable one falls back to the local daemon so
    /// the app can still open.
    Profile {
        address: String,
        token: String,
        name: String,
    },
    /// The daemon this app starts and supervises.
    Local,
}

/// Decides where the desktop connects.
///
/// The environment variables come first — a shell that exports them is
/// explicit — then the active profile, which is how a GUI bundle names its
/// daemon, and finally the local daemon the app owns.
fn daemon_target(
    env_address: Option<String>,
    env_token: Option<String>,
    settings: &waku_client::persistence::AppSettings,
) -> anyhow::Result<DaemonTarget> {
    match (env_address, env_token) {
        (Some(address), Some(token)) => {
            return Ok(DaemonTarget::Required {
                address: address.trim().to_owned(),
                token,
            });
        }
        (Some(_), None) => bail!(
            "{} is set but {} is missing",
            waku_client::DAEMON_ADDRESS_ENV,
            waku_client::DAEMON_TOKEN_ENV
        ),
        (None, Some(_)) => bail!(
            "{} is set but {} is missing",
            waku_client::DAEMON_TOKEN_ENV,
            waku_client::DAEMON_ADDRESS_ENV
        ),
        (None, None) => {}
    }
    if let Some(profile) = settings.active_daemon_profile.and_then(|id| {
        settings
            .daemon_profiles
            .iter()
            .find(|profile| profile.id == id)
    }) {
        return Ok(DaemonTarget::Profile {
            address: profile.normalized_address(),
            token: profile.token.trim().to_owned(),
            name: profile.name.clone(),
        });
    }
    Ok(DaemonTarget::Local)
}

pub fn start_process() -> anyhow::Result<waku_client::DaemonSupervisor> {
    let env_address = std::env::var(waku_client::DAEMON_ADDRESS_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty());
    let env_token = std::env::var(waku_client::DAEMON_TOKEN_ENV)
        .ok()
        .filter(|value| !value.is_empty());
    let app_settings = waku_client::persistence::load_or_create_app_settings()
        .context("could not load desktop daemon settings")?;
    match daemon_target(env_address, env_token, &app_settings)? {
        DaemonTarget::Required { address, token } => {
            waku_client::DaemonSupervisor::connect(&address, token)
        }
        DaemonTarget::Profile {
            address,
            token,
            name,
        } => match waku_client::DaemonSupervisor::connect(&address, token) {
            Ok(supervisor) => Ok(supervisor),
            Err(error) => {
                eprintln!("waku: could not connect to daemon profile {name}: {error:#}");
                Ok(waku_client::DaemonSupervisor::spawn_configured(
                    &daemon_executable_path()?,
                    cfg!(debug_assertions),
                    app_settings.daemon_exposure,
                )?)
            }
        },
        DaemonTarget::Local => Ok(waku_client::DaemonSupervisor::spawn_configured(
            &daemon_executable_path()?,
            cfg!(debug_assertions),
            app_settings.daemon_exposure,
        )?),
    }
}

/// Resolve the local host name once during app construction. Settings can
/// then show a useful LAN URL without touching the OS from a render frame.
pub fn local_hostname() -> Option<String> {
    #[cfg(unix)]
    {
        let mut buffer = [0_u8; 256];
        let result = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
        if result == 0 {
            let length = buffer
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(buffer.len());
            let hostname = String::from_utf8_lossy(&buffer[..length]).trim().to_owned();
            if !hostname.is_empty() {
                return Some(hostname);
            }
        }
    }
    // `COMPUTERNAME` is the Windows equivalent and is always set; `HOSTNAME`
    // covers the shells that export it.
    ["COMPUTERNAME", "HOSTNAME"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|hostname| hostname.trim().to_owned())
        .find(|hostname| !hostname.is_empty())
}

fn daemon_executable_path() -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("WAKU_DAEMON_PATH").filter(|path| !path.is_empty()) {
        return Ok(path.into());
    }
    let executable = format!("waku-daemon{}", std::env::consts::EXE_SUFFIX);
    let current = std::env::current_exe().context("could not locate the Waku executable")?;

    // Development keeps the daemon beside Cargo's debug artifacts rather than
    // inside Waku Debug.app. The supervisor watches this file and swaps only
    // the daemon when the development watcher relinks it.
    #[cfg(debug_assertions)]
    if let Some(debug_directory) = current
        .ancestors()
        .find(|candidate| candidate.file_name().is_some_and(|name| name == "debug"))
    {
        let external = debug_directory.join(&executable);
        if external.is_file() {
            return Ok(external);
        }
    }

    let sibling = current
        .parent()
        .map(|directory| directory.join(&executable))
        .ok_or_else(|| anyhow!("Waku executable has no parent directory"))?;
    if sibling.is_file() {
        return Ok(sibling);
    }
    #[cfg(debug_assertions)]
    bail!(
        "Waku daemon was not found in Cargo's debug directory or next to the app executable: {}",
        sibling.display(),
    );
    #[cfg(not(debug_assertions))]
    bail!(
        "Waku daemon is missing next to the app executable: {}",
        sibling.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str, address: &str) -> waku_client::DaemonProfile {
        waku_client::DaemonProfile {
            id: uuid::Uuid::new_v4(),
            name: name.to_owned(),
            address: address.to_owned(),
            token: "token".to_owned(),
        }
    }

    #[test]
    fn the_environment_wins_over_a_named_profile() {
        let mut settings = waku_client::persistence::AppSettings::default();
        let named = profile("server", "10.0.0.5:34123");
        settings.active_daemon_profile = Some(named.id);
        settings.daemon_profiles = vec![named];

        let target = daemon_target(
            Some("127.0.0.1:9999".into()),
            Some("env-token".into()),
            &settings,
        )
        .unwrap();
        match target {
            DaemonTarget::Required { address, token } => {
                assert_eq!(address, "127.0.0.1:9999");
                assert_eq!(token, "env-token");
            }
            _ => panic!("the environment must win"),
        }
    }

    #[test]
    fn a_half_configured_environment_is_an_error() {
        let settings = waku_client::persistence::AppSettings::default();
        assert!(
            daemon_target(Some("127.0.0.1:9999".into()), None, &settings).is_err(),
            "an address without a token must not silently fall back"
        );
        assert!(
            daemon_target(None, Some("token".into()), &settings).is_err(),
            "a token without an address must not silently fall back"
        );
    }

    #[test]
    fn the_active_profile_is_used_when_the_environment_is_silent() {
        let mut settings = waku_client::persistence::AppSettings::default();
        let named = profile("server", "10.0.0.5:34123");
        settings.active_daemon_profile = Some(named.id);
        settings.daemon_profiles = vec![named];

        let target = daemon_target(None, None, &settings).unwrap();
        match target {
            DaemonTarget::Profile {
                address,
                token,
                name,
            } => {
                assert_eq!(address, "ws://10.0.0.5:34123");
                assert_eq!(token, "token");
                assert_eq!(name, "server");
            }
            _ => panic!("the active profile must be used"),
        }
    }

    #[test]
    fn without_a_target_the_desktop_starts_its_own_daemon() {
        let settings = waku_client::persistence::AppSettings::default();
        assert!(matches!(
            daemon_target(None, None, &settings).unwrap(),
            DaemonTarget::Local
        ));

        // A profile that is not the active one does not steer the desktop.
        let mut settings = waku_client::persistence::AppSettings::default();
        settings.daemon_profiles = vec![profile("server", "10.0.0.5:34123")];
        assert!(matches!(
            daemon_target(None, None, &settings).unwrap(),
            DaemonTarget::Local
        ));
    }

    #[test]
    fn a_profile_address_gains_a_scheme_and_needs_its_parts() {
        let mut named = profile("server", "10.0.0.5:34123");
        assert_eq!(named.normalized_address(), "ws://10.0.0.5:34123");
        named.address = "wss://box.example".into();
        assert_eq!(named.normalized_address(), "wss://box.example");
        named.address = "ws://box.example".into();
        assert_eq!(named.normalized_address(), "ws://box.example");

        assert!(named.validate().is_ok());
        named.name = "  ".into();
        assert!(named.validate().is_err());
        named.name = "server".into();
        named.address = String::new();
        assert!(named.validate().is_err());
        named.address = "10.0.0.5:34123".into();
        named.token = String::new();
        assert!(named.validate().is_err());
    }
}
