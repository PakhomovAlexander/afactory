//! What the host reports about its own load, for evidence beside the processes it ran: a check
//! that failed while the machine was saturated says so.

use std::time::Duration;

/// The host's 1-minute load average, or `None` when it reports none: Linux's `/proc/loadavg`,
/// macOS's `vm.loadavg` through `/usr/sbin/sysctl`, else through `/usr/bin/uptime`, nothing
/// elsewhere.
///
/// A sandboxed macOS Worker may not look `vm.loadavg` up by name (`sysctl` answers `Operation
/// not permitted`), while `getloadavg(3)`, which reads it by number, is allowed. The workspace
/// forbids `unsafe`, so the call is not made here directly: `uptime` makes it.
pub fn load_average_1m() -> Option<f64> {
    if cfg!(target_os = "linux") {
        first_load(&std::fs::read_to_string("/proc/loadavg").ok()?)
    } else if cfg!(target_os = "macos") {
        host_output("/usr/sbin/sysctl", &["-n", "vm.loadavg"])
            .and_then(|text| first_load(&text))
            .or_else(|| uptime_load(&host_output("/usr/bin/uptime", &[])?))
    } else {
        None
    }
}

/// What `program` prints with `args`, in a cleared environment, when it exits 0 within 5 s.
fn host_output(program: &str, args: &[&str]) -> Option<String> {
    let mut command = std::process::Command::new(program);
    command.args(args).env_clear();
    let output = crate::run_supervised(&mut command, None, Duration::from_secs(5)).ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// The 1-minute load `uptime` prints after `load averages:` (macOS) or `load average:`
/// (`… load averages: 1.52 1.58 1.59`).
fn uptime_load(text: &str) -> Option<f64> {
    let (_, loads) = text.rsplit_once("load average")?;
    let loads = loads.trim_start_matches('s').strip_prefix(':')?;
    let load: f64 = loads
        .split([' ', ','])
        .find(|field| !field.is_empty())?
        .parse()
        .ok()?;
    (load.is_finite() && load >= 0.0).then_some(load)
}

/// This host's name, as `hostname` prints it: Linux's `/proc/sys/kernel/hostname`, `/bin/hostname`
/// elsewhere; `None` when neither answers. What a record of a process names so a reader on
/// another machine knows the process is not theirs to signal.
pub fn host_name() -> Option<String> {
    let text = if cfg!(target_os = "linux") {
        std::fs::read_to_string("/proc/sys/kernel/hostname").ok()?
    } else {
        let mut command = std::process::Command::new("/bin/hostname");
        command.env_clear();
        let output = crate::run_supervised(&mut command, None, Duration::from_secs(5)).ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout).ok()?
    };
    let name = text.trim();
    (!name.is_empty() && name.len() <= 255 && !name.chars().any(char::is_control))
        .then(|| name.to_owned())
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

    /// `uptime`'s spellings on macOS and Linux, and what is not a load.
    #[test]
    fn uptime_names_the_one_minute_load() {
        assert_eq!(
            uptime_load(" 4:05  up 14 days,  7:30, 4 users, load averages: 31.69 22.52 20.89\n"),
            Some(31.69)
        );
        assert_eq!(
            uptime_load(" 10:01:02 up 3 days,  1 user,  load average: 0.52, 0.58, 0.59\n"),
            Some(0.52)
        );
        assert_eq!(uptime_load("load averages: -1 0 0"), None);
        assert_eq!(uptime_load("up 3 days"), None);
    }

    #[test]
    fn the_host_has_a_name() {
        let name = host_name().expect("macOS and Linux name the host");
        assert!(!name.is_empty() && !name.contains('\n'), "{name:?}");
        assert_eq!(host_name(), Some(name), "the same name twice");
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
