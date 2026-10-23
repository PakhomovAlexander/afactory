//! What the host reports about its own load, for evidence beside the processes it ran: a check
//! that failed while the machine was saturated says so.

use std::time::Duration;

/// The host's 1-minute load average, or `None` when it reports none: Linux's `/proc/loadavg`,
/// macOS's `vm.loadavg` through `/usr/sbin/sysctl`, nothing elsewhere.
pub fn load_average_1m() -> Option<f64> {
    let text = if cfg!(target_os = "linux") {
        std::fs::read_to_string("/proc/loadavg").ok()?
    } else if cfg!(target_os = "macos") {
        let mut command = std::process::Command::new("/usr/sbin/sysctl");
        command.args(["-n", "vm.loadavg"]).env_clear();
        let output = crate::run_supervised(&mut command, None, Duration::from_secs(5)).ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout).ok()?
    } else {
        return None;
    };
    first_load(&text)
}

/// The first number of `/proc/loadavg` (`0.52 0.58 0.59 1/389 12345`) or of `vm.loadavg`
/// (`{ 0.52 0.58 0.59 }`).
fn first_load(text: &str) -> Option<f64> {
    let load: f64 = text
        .split_whitespace()
        .find(|field| *field != "{")?
        .parse()
        .ok()?;
    (load.is_finite() && load >= 0.0).then_some(load)
}

/// The host's name, as `uname -n` would print it: Linux's `/proc/sys/kernel/hostname`, macOS's
/// `kern.hostname` through `/usr/sbin/sysctl`; `None` elsewhere or when it reports none.
pub fn hostname() -> Option<String> {
    let text = if cfg!(target_os = "linux") {
        std::fs::read_to_string("/proc/sys/kernel/hostname").ok()?
    } else if cfg!(target_os = "macos") {
        let mut command = std::process::Command::new("/usr/sbin/sysctl");
        command.args(["-n", "kern.hostname"]).env_clear();
        let output = crate::run_supervised(&mut command, None, Duration::from_secs(5)).ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout).ok()?
    } else {
        return None;
    };
    let name = text.trim();
    (!name.is_empty() && name.len() <= 255 && !name.chars().any(char::is_control))
        .then(|| name.to_string())
}

/// The CPUs available to this process: its affinity and quota where the host has them, at
/// least one.
pub fn available_cpus() -> u32 {
    std::thread::available_parallelism()
        .map_or(1, |cpus| u32::try_from(cpus.get()).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_reports_a_load_average_and_at_least_one_cpu() {
        assert!(available_cpus() >= 1);
        let load = load_average_1m().expect("macOS and Linux report a load average");
        assert!(load >= 0.0 && load.is_finite());
    }

    #[test]
    fn the_host_names_itself() {
        let name = hostname().expect("macOS and Linux report a host name");
        assert!(!name.is_empty() && !name.contains('\n'));
    }

    #[test]
    fn both_load_formats_read_their_first_figure() {
        assert_eq!(first_load("0.52 0.58 0.59 1/389 12345\n"), Some(0.52));
        assert_eq!(first_load("{ 22.44 9.79 10.44 }\n"), Some(22.44));
        assert_eq!(first_load(""), None);
        assert_eq!(first_load("{ nan 1 2 }"), None);
        assert_eq!(first_load("{ -1.0 1 2 }"), None);
    }
}
