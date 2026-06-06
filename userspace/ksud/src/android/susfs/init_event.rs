use std::{
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

use log::{info, warn};

use crate::android::susfs::api;
use crate::android::susfs::config;
use crate::android::susfs::config::data::Data;

const USER_0_CE_AVAILABLE_PROP: &str = "sys.user.0.ce_available";
const CE_AVAILABLE_WAIT_TIMEOUT_SECS: u64 = 10 * 60;
const CE_AVAILABLE_POLL_INTERVAL_SECS: u64 = 1;
const USER_0_CE_PATH_PREFIXES: &[&str] = &[
    "/sdcard",
    "/storage/emulated/0",
    "/storage/self/primary",
    "/mnt/user/0/primary",
    "/mnt/runtime/default/emulated/0",
    "/mnt/runtime/read/emulated/0",
    "/mnt/runtime/write/emulated/0",
    "/mnt/runtime/full/emulated/0",
    "/mnt/pass_through/0/emulated/0",
    "/mnt/installer/0/emulated/0",
    "/mnt/androidwritable/0/emulated/0",
    "/mnt/media_rw/emulated/0",
    "/data/media/0",
    "/data/user/0",
    "/data/data",
    "/data/misc_ce/0",
    "/data/system_ce/0",
    "/data/vendor_ce/0",
    "/data_mirror/data_ce/null/0",
    "/data_mirror/data_ce/0",
];

enum CeAvailability {
    Available,
    Locked,
    Unknown,
}

pub fn on_boot_completed() {
    let config = config::read_config();

    if has_ce_sensitive_config_entries(&config) {
        if try_apply_after_ce_available(&config, "user-0-ce-available-at-boot-completed") {
            return;
        }
        wait_for_user_0_ce_available();
    } else {
        info!("{USER_0_CE_AVAILABLE_PROP} is unavailable or not required");
        apply_config_after_ce_available(&config, "boot-completed-without-ce-property");
    }
}

pub fn on_services() {
    // let config = config::read_config();

    // apply_sus_paths(&config);
    // apply_sus_maps(&config);
}

fn apply_sus_paths(config: &Data) -> bool {
    let mut success = true;

    for sus_path in &config.sus_path.sus_path {
        if sus_path.trim().is_empty() {
            continue;
        }
        if !apply_sus_path_entry(&api::SusPathType::Normal, "sus_path", sus_path) {
            success = false;
        }
    }
    for sus_path_loop in &config.sus_path.sus_path_loop {
        if sus_path_loop.trim().is_empty() {
            continue;
        }
        if !apply_sus_path_entry(&api::SusPathType::Loop, "sus_path_loop", sus_path_loop) {
            success = false;
        }
    }

    success
}

fn apply_sus_path_entry(path_type: &api::SusPathType, label: &str, path: &str) -> bool {
    match api::add_sus_path(path_type, &path) {
        Ok(()) => true,
        Err(e) if is_already_applied_error(&e) => {
            info!("{label} '{path}' is already applied");
            true
        }
        Err(e) => {
            warn!("failed to add {label} '{path}': {e}");
            false
        }
    }
}

fn apply_sus_maps(config: &Data) -> bool {
    let mut success = true;

    for sus_map in &config.sus_map {
        if sus_map.trim().is_empty() {
            continue;
        }
        match api::add_sus_map(sus_map.as_str()) {
            Ok(()) => {}
            Err(e) if is_already_applied_error(&e) => {
                info!("sus_map '{sus_map}' is already applied");
            }
            Err(e) => {
                warn!("failed to add sus_map '{sus_map}': {e}");
                success = false;
            }
        }
    }

    success
}

pub fn on_post_fs_data() {
    let config = config::read_config();

    if let Err(e) = api::set_uname(&config.common.spoof_version, &config.common.spoof_release) {
        warn!("failed to set uname: {e}");
    }

    if let Err(e) = api::enable_avc_log_spoofing(config.common.avc_spoofing.into()) {
        warn!("failed to enable avc log spoofing: {e}");
    }

    if let Err(e) = api::enable_log(config.common.enable_susfs_log.into()) {
        warn!("failed to enable susfs log: {e}");
    }

    if let Err(e) =
        api::hide_sus_mnts_for_non_su_procs(config.common.hide_sus_mnts_for_non_su_procs.into())
    {
        warn!("failed to hide sus mnts for non su procs: {e}");
    }

    // apply_sus_paths(&config);

    apply_sus_kstat_additions(&config);
}

pub fn on_post_mount() {
    let config = config::read_config();

    // apply_sus_paths(&config);
    // apply_sus_maps(&config);

    apply_kstat_updates(&config);
}

fn apply_config_after_ce_available(config: &Data, reason: &str) -> bool {
    info!("applying susfs CE-sensitive entries for {reason}");

    let mut success = true;
    if !apply_sus_paths(config) {
        success = false;
    }
    if !apply_sus_maps(config) {
        success = false;
    }
    if !apply_sus_kstat_additions(config) {
        success = false;
    }
    if !apply_kstat_updates(config) {
        success = false;
    }

    if success {
        info!("applied susfs CE-sensitive entries for {reason}");
    }
    success
}

fn try_apply_after_ce_available(config: &Data, reason: &str) -> bool {
    if !is_user_0_ce_ready(config) {
        return false;
    }

    if !are_configured_paths_available(config) {
        return false;
    }

    apply_config_after_ce_available(config, reason)
}

fn apply_sus_kstat_additions(config: &Data) -> bool {
    let mut success = true;

    for sus_kstat in &config.kstat.sus_kstat {
        if sus_kstat.trim().is_empty() {
            continue;
        }
        match api::add_sus_kstat(sus_kstat.as_str()) {
            Ok(()) => {}
            Err(e) if is_already_applied_error(&e) => {
                info!("sus_kstat '{sus_kstat}' is already applied");
            }
            Err(e) => {
                warn!("failed to add sus_kstat '{sus_kstat}': {e}");
                success = false;
            }
        }
    }
    for statically in &config.kstat.statically {
        if statically.path.trim().is_empty() {
            continue;
        }
        match api::add_sus_kstat_statically(
            &statically.path,
            &statically.ino,
            &statically.dev,
            &statically.nlink,
            &statically.size,
            &statically.atime,
            &statically.atime_nsec,
            &statically.mtime,
            &statically.mtime_nsec,
            &statically.ctime,
            &statically.ctime_nsec,
            &statically.blocks,
            &statically.blksize,
        ) {
            Ok(()) => {}
            Err(e) if is_already_applied_error(&e) => {
                info!("sus_kstat_statically '{}' is already applied", statically.path);
            }
            Err(e) => {
                warn!(
                    "failed to add sus_kstat_statically '{}': {}",
                    statically.path, e
                );
                success = false;
            }
        }
    }

    success
}

fn apply_kstat_updates(config: &Data) -> bool {
    let mut success = true;

    for update_kstat in &config.kstat.update_kstat {
        if update_kstat.trim().is_empty() {
            continue;
        }
        match api::update_sus_kstat(update_kstat.as_str()) {
            Ok(()) => {}
            Err(e) if is_already_applied_error(&e) => {
                info!("sus_kstat '{update_kstat}' is already updated");
            }
            Err(e) => {
                warn!("failed to update sus_kstat '{update_kstat}': {e}");
                success = false;
            }
        }
    }
    for full_clone in &config.kstat.full_clone {
        if full_clone.trim().is_empty() {
            continue;
        }
        match api::update_sus_kstat_full_clone(full_clone.as_str()) {
            Ok(()) => {}
            Err(e) if is_already_applied_error(&e) => {
                info!("sus_kstat_full_clone '{full_clone}' is already updated");
            }
            Err(e) => {
                warn!("failed to update sus_kstat_full_clone '{full_clone}': {e}");
                success = false;
            }
        }
    }

    success
}

fn is_already_applied_error(e: &anyhow::Error) -> bool {
    let message = e.to_string();
    message.contains("SuSFS error: -17")
        || message.contains("SuSFS error: 17")
        || message.contains("File exists")
        || message.contains("os error 17")
}

fn user_0_ce_availability() -> CeAvailability {
    match crate::android::utils::getprop(USER_0_CE_AVAILABLE_PROP)
        .as_deref()
        .map(str::trim)
    {
        Some(value) if is_true_property_value(value) => CeAvailability::Available,
        Some(value) if is_false_property_value(value) => CeAvailability::Locked,
        _ => CeAvailability::Unknown,
    }
}

fn is_true_property_value(value: &str) -> bool {
    value == "1" || value.eq_ignore_ascii_case("true")
}

fn is_false_property_value(value: &str) -> bool {
    value == "0" || value.eq_ignore_ascii_case("false")
}

fn has_ce_sensitive_config_entries(config: &Data) -> bool {
    any_config_path(config, is_user_ce_path)
}

fn is_configured_ce_path_available(config: &Data) -> bool {
    any_config_path(config, |path| is_user_ce_path(path) && Path::new(path).exists())
}

fn are_configured_paths_available(config: &Data) -> bool {
    all_config_path(config, |path| Path::new(path).exists())
}

fn is_user_0_ce_ready(config: &Data) -> bool {
    matches!(user_0_ce_availability(), CeAvailability::Available)
        || is_configured_ce_path_available(config)
        || is_user_0_unlocked_by_cmd()
}

fn is_user_0_unlocked_by_cmd() -> bool {
    let Ok(output) = Command::new("cmd")
        .args(["user", "is-user-unlocked", "0"])
        .output()
    else {
        return false;
    };

    if !output.status.success() {
        return false;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .split_whitespace()
        .any(|token| token.eq_ignore_ascii_case("true"))
}

fn any_config_path<F>(config: &Data, mut predicate: F) -> bool
where
    F: FnMut(&str) -> bool,
{
    config
        .sus_path
        .sus_path
        .iter()
        .chain(config.sus_path.sus_path_loop.iter())
        .chain(config.sus_map.iter())
        .chain(config.kstat.sus_kstat.iter())
        .chain(config.kstat.update_kstat.iter())
        .chain(config.kstat.full_clone.iter())
        .any(|path| predicate(path.trim()))
        || config
            .kstat
            .statically
            .iter()
            .any(|entry| predicate(entry.path.trim()))
}

fn all_config_path<F>(config: &Data, mut predicate: F) -> bool
where
    F: FnMut(&str) -> bool,
{
    config
        .sus_path
        .sus_path
        .iter()
        .chain(config.sus_path.sus_path_loop.iter())
        .chain(config.sus_map.iter())
        .chain(config.kstat.sus_kstat.iter())
        .chain(config.kstat.update_kstat.iter())
        .chain(config.kstat.full_clone.iter())
        .filter_map(|path| non_empty_path(path))
        .all(|path| predicate(path))
        && config
            .kstat
            .statically
            .iter()
            .filter_map(|entry| non_empty_path(&entry.path))
            .all(|path| predicate(path))
}

fn non_empty_path(path: &str) -> Option<&str> {
    let path = path.trim();
    if path.is_empty() { None } else { Some(path) }
}

fn is_user_ce_path(path: &str) -> bool {
    let path = path.trim_end_matches('/');
    USER_0_CE_PATH_PREFIXES
        .iter()
        .any(|prefix| is_path_or_child(path, prefix))
}

fn is_path_or_child(path: &str, prefix: &str) -> bool {
    if path == prefix {
        return true;
    }

    matches!(path.strip_prefix(prefix), Some(rest) if rest.starts_with('/'))
}

fn wait_for_user_0_ce_available() {
    match crate::android::utils::create_daemon(false) {
        Ok(true) => {}
        Ok(false) => return,
        Err(e) => {
            warn!("failed to daemonize susfs CE availability watcher: {e}");
            return;
        }
    }

    let _ = wait_for_user_0_ce_available_inner();
    unsafe {
        libc::_exit(0);
    }
}

fn wait_for_user_0_ce_available_inner() -> bool {
    let started_at = Instant::now();

    info!("waiting for {USER_0_CE_AVAILABLE_PROP}, user unlock state, or configured CE paths");
    loop {
        let config = config::read_config();
        if try_apply_after_ce_available(&config, "user-0-ce-available") {
            return true;
        }

        let elapsed = started_at.elapsed();
        if elapsed >= Duration::from_secs(CE_AVAILABLE_WAIT_TIMEOUT_SECS) {
            warn!("timed out waiting for user 0 CE availability");
            return false;
        }

        let remaining = Duration::from_secs(CE_AVAILABLE_WAIT_TIMEOUT_SECS).saturating_sub(elapsed);
        thread::sleep(remaining.min(Duration::from_secs(CE_AVAILABLE_POLL_INTERVAL_SECS)));
    }
}
