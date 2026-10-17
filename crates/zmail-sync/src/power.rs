//! Power and network conditions that background work defers to.
//!
//! Background body indexing pauses on battery power and on metered
//! connections unless the user allows it ([`IndexingPreferences`]). What
//! the platform can report sits behind [`PowerMonitor`], so the app (or a
//! test) can install a better source without the indexer changing.
//!
//! [`SystemPowerMonitor`] reads Linux's `/sys/class/power_supply`, which
//! needs no extra dependency. macOS (IOKit power sources), Windows
//! (`GetSystemPowerStatus`) and metered-network detection everywhere
//! (NetworkManager, `NWPath.isExpensive`, `NetworkCostType`) need platform
//! bindings the workspace does not carry yet, so they report "unknown",
//! and unknown never pauses anything.
//!
//! [`IndexingPreferences`]: zmail_core::preferences::IndexingPreferences

use std::{fs, path::Path};

/// What the platform knows about the current power and network state.
/// `None` means the platform cannot tell.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PowerConditions {
    /// Whether the machine runs on battery power.
    pub on_battery: Option<bool>,
    /// Whether the active network connection is metered.
    pub metered: Option<bool>,
}

/// A source of [`PowerConditions`]; polled before each indexing batch, so
/// implementations should be cheap.
pub trait PowerMonitor: Send + Sync {
    /// The current conditions.
    fn conditions(&self) -> PowerConditions;
}

/// The built-in monitor: battery state on Linux, unknown elsewhere.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemPowerMonitor;

impl PowerMonitor for SystemPowerMonitor {
    fn conditions(&self) -> PowerConditions {
        PowerConditions {
            on_battery: if cfg!(target_os = "linux") {
                linux_on_battery(Path::new("/sys/class/power_supply"))
            } else {
                None
            },
            metered: None,
        }
    }
}

/// Reads the battery state from a Linux `power_supply` class directory.
///
/// Any online mains or USB supply means external power. Otherwise a system
/// battery (not a peripheral's, which reports `scope` `Device`) means the
/// machine runs on it. Returns `None` when the directory cannot be read.
fn linux_on_battery(root: &Path) -> Option<bool> {
    let read = |path: &Path, name: &str| {
        fs::read_to_string(path.join(name))
            .map(|value| value.trim().to_owned())
            .unwrap_or_default()
    };
    let mut system_battery = false;
    for entry in fs::read_dir(root).ok()?.flatten() {
        let supply = entry.path();
        match read(&supply, "type").as_str() {
            "Mains" | "USB" if read(&supply, "online") == "1" => return Some(false),
            "Battery" if read(&supply, "scope") != "Device" => system_battery = true,
            _ => {}
        }
    }
    Some(system_battery)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "zmail-power-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
            fs::create_dir_all(&root).expect("fixture directory");
            Self(root)
        }

        fn supply(&self, name: &str, files: &[(&str, &str)]) -> &Self {
            let dir = self.0.join(name);
            fs::create_dir_all(&dir).expect("supply directory");
            for (file, value) in files {
                fs::write(dir.join(file), format!("{value}\n")).expect("supply file");
            }
            self
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn laptops_on_and_off_mains() {
        let on_mains = Fixture::new("mains");
        on_mains
            .supply("AC", &[("type", "Mains"), ("online", "1")])
            .supply("BAT0", &[("type", "Battery"), ("status", "Charging")]);
        assert_eq!(linux_on_battery(&on_mains.0), Some(false));

        let unplugged = Fixture::new("unplugged");
        unplugged
            .supply("AC", &[("type", "Mains"), ("online", "0")])
            .supply("BAT0", &[("type", "Battery"), ("status", "Discharging")]);
        assert_eq!(linux_on_battery(&unplugged.0), Some(true));
    }

    #[test]
    fn peripheral_batteries_and_desktops_are_not_battery_power() {
        let desktop = Fixture::new("desktop");
        desktop.supply(
            "hidpp_battery_0",
            &[("type", "Battery"), ("scope", "Device")],
        );
        assert_eq!(linux_on_battery(&desktop.0), Some(false));
        assert_eq!(linux_on_battery(&Fixture::new("empty").0), Some(false));
        assert_eq!(linux_on_battery(Path::new("/nonexistent/zmail")), None);
    }
}
