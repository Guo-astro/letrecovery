//! WinPE side of the optional network runtime ("接通 Wi-Fi PE").
//!
//! Runs entirely on a detached background thread so bringing up networking never blocks the
//! install or maintenance flow. Every step is best-effort with a bounded timeout, uses only
//! Microsoft-supported WinPE tools (wpeutil / sc / net / netsh / drvload), and never logs Wi-Fi
//! keys or profile XML. Gated by the authenticated `PeNetworkEnabled` policy plus the pe-network
//! payload binding carried in the signed handoff config. The host's exported wireless driver
//! package (`X:\LR_PeNetworkDrivers`) is loaded only after every file matches the authenticated
//! manifest inside that payload.

use super::config::AuthenticatedOperationGuard;

/// Entry point invoked from `main` right after the handoff is authenticated. Returns immediately;
/// the actual bring-up happens on a detached background thread.
pub fn start_from_handoff(guard: &AuthenticatedOperationGuard) {
    #[cfg(windows)]
    {
        if !policy_allows(guard) {
            return;
        }
        let payload = load_payload(guard);
        let handle = std::thread::Builder::new()
            .name("lr-pe-network".to_owned())
            .spawn(move || run_bringup(payload));
        if let Err(error) = handle {
            log::info!("[PE NETWORK] unable to start the network thread: {error:#}");
        }
    }
    #[cfg(not(windows))]
    {
        let _ = guard;
    }
}

#[cfg(windows)]
const MAX_DRIVERS: usize = 64;

#[cfg(windows)]
const MAX_INF_BYTES: u64 = 4 * 1024 * 1024;

/// Bounded Wi-Fi connection passes; WlanSvc and a freshly loaded adapter need time to appear.
#[cfg(windows)]
const WIFI_CONNECT_ROUNDS: usize = 4;

#[cfg(windows)]
fn policy_allows(guard: &AuthenticatedOperationGuard) -> bool {
    use lr_core::handoff_auth::HandoffPurpose;
    match guard.purpose() {
        HandoffPurpose::Install => install_policy_enabled(guard),
        HandoffPurpose::Maintenance => maintenance_policy_enabled(guard),
        _ => false,
    }
}

#[cfg(windows)]
fn install_policy_enabled(guard: &AuthenticatedOperationGuard) -> bool {
    match super::config::ConfigFileManager::install_config_from_guard(guard) {
        Ok(config) => config.pe_network_enabled,
        Err(error) => {
            log::info!("[PE NETWORK] cannot read the install policy: {error:#}");
            false
        }
    }
}

#[cfg(windows)]
fn maintenance_policy_enabled(guard: &AuthenticatedOperationGuard) -> bool {
    match std::str::from_utf8(guard.exact_config_bytes()) {
        Ok(text) => lr_core::pe_network::policy_enabled(text),
        Err(_) => false,
    }
}

#[cfg(windows)]
fn load_payload(
    guard: &AuthenticatedOperationGuard,
) -> Option<lr_core::pe_network::PeNetworkPayload> {
    let config_text = std::str::from_utf8(guard.exact_config_bytes()).ok()?;
    let binding = match lr_core::pe_network::PeNetworkPayloadBinding::from_config_text(config_text)
    {
        Ok(Some(binding)) => binding,
        Ok(None) => return None,
        Err(error) => {
            log::info!("[PE NETWORK] network payload binding is invalid: {error:#}");
            return None;
        }
    };
    let path = std::path::Path::new(lr_core::pe_network::PAYLOAD_PE_PATH);
    let bytes = match lr_core::scoped_temp_file::read_bounded_plain_file(
        path,
        lr_core::pe_network::PAYLOAD_MAX_BYTES,
    ) {
        Ok(bytes) => bytes,
        Err(error) => {
            log::info!("[PE NETWORK] cannot read the network payload: {error:#}");
            return None;
        }
    };
    if let Err(error) = binding.verify(&bytes) {
        log::info!("[PE NETWORK] network payload failed its integrity check: {error:#}");
        return None;
    }
    match lr_core::pe_network::PeNetworkPayload::parse(&bytes) {
        Ok(payload) => Some(payload),
        Err(error) => {
            log::info!("[PE NETWORK] cannot parse the network payload: {error:#}");
            None
        }
    }
}

#[cfg(windows)]
fn run_bringup(payload: Option<lr_core::pe_network::PeNetworkPayload>) {
    use std::time::Duration;
    log::info!("[PE NETWORK] starting best-effort network bring-up");
    load_network_drivers();
    if let Some(payload) = payload.as_ref() {
        load_host_driver_package(payload);
    }
    initialize_wired_network();
    if wait_for_connectivity(Duration::from_secs(12)) {
        log::info!("[PE NETWORK] wired connectivity is available");
        return;
    }
    let Some(payload) = payload else {
        log::info!("[PE NETWORK] no Wi-Fi profile available; leaving wired bring-up in place");
        return;
    };
    if payload.entries.is_empty() {
        if !payload.drivers.is_empty() {
            // The host adapter driver is loaded; start WlanSvc so the PE's own Wi-Fi UI can use it.
            start_wlan_service();
        }
        log::info!("[PE NETWORK] payload carries no Wi-Fi profile; wired bring-up only");
        return;
    }
    start_wlan_service();
    // `sc start` returns while WlanSvc is still START_PENDING, and a freshly loaded adapter needs a
    // few seconds to enumerate; until then netsh reports no wireless interface and every profile
    // fails at once. Retry in bounded rounds instead of giving up after the first pass.
    for round in 0..WIFI_CONNECT_ROUNDS {
        if round > 0 {
            std::thread::sleep(Duration::from_secs(5));
            if has_connectivity() {
                log::info!("[PE NETWORK] connectivity appeared while waiting for Wi-Fi");
                return;
            }
        }
        if connect_any_wifi(&payload) {
            log::info!("[PE NETWORK] wireless connectivity is available");
            return;
        }
    }
    log::info!("[PE NETWORK] could not establish wireless connectivity");
}

#[cfg(windows)]
fn initialize_wired_network() {
    use std::time::Duration;
    // wpeutil InitializeNetwork starts the WinPE networking stack and the wired DHCP client.
    run_status(
        "wpeutil.exe",
        &["InitializeNetwork"],
        Duration::from_secs(60),
    );
    for service in ["nsi", "Dhcp", "Dnscache"] {
        ensure_service_running(service);
    }
}

#[cfg(windows)]
fn start_wlan_service() {
    // WlanSvc must be running before netsh can manage Wi-Fi; enable it if the image left it off.
    ensure_service_running("WlanSvc");
    ensure_service_running("Wcmsvc");
}

#[cfg(windows)]
fn ensure_service_running(service: &str) {
    use std::time::Duration;
    let code = run_status("sc.exe", &["start", service], Duration::from_secs(20));
    // 1058 = the service is disabled; enable on-demand start and retry once.
    if code == Some(1058) {
        run_status(
            "sc.exe",
            &["config", service, "start=", "demand"],
            Duration::from_secs(20),
        );
        run_status("sc.exe", &["start", service], Duration::from_secs(20));
    }
}

#[cfg(windows)]
fn connect_any_wifi(payload: &lr_core::pe_network::PeNetworkPayload) -> bool {
    for entry in &payload.entries {
        for candidate in lr_core::pe_network::wifi_profile_candidates(&entry.profile_xml) {
            if try_wifi_profile(&entry.ssid, &candidate) {
                return true;
            }
        }
    }
    false
}

#[cfg(windows)]
fn try_wifi_profile(ssid: &str, profile_xml: &str) -> bool {
    use std::time::Duration;
    let Some(profile_path) = write_profile_file(profile_xml) else {
        return false;
    };
    let path_argument = format!("filename={}", profile_path.display());
    let added = run_status(
        "netsh.exe",
        &["wlan", "add", "profile", &path_argument, "user=all"],
        Duration::from_secs(20),
    );
    let _ = std::fs::remove_file(&profile_path);
    if added != Some(0) {
        return false;
    }
    let profile_name = profile_name_from_xml(profile_xml).unwrap_or_else(|| ssid.to_owned());
    let name_argument = format!("name={profile_name}");
    let ssid_argument = format!("ssid={ssid}");
    run_status(
        "netsh.exe",
        &["wlan", "connect", &name_argument, &ssid_argument],
        Duration::from_secs(20),
    );
    wait_for_connectivity(Duration::from_secs(20))
}

#[cfg(windows)]
fn write_profile_file(profile_xml: &str) -> Option<std::path::PathBuf> {
    use std::io::Write;
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |value| value.as_nanos());
    let path = std::env::temp_dir().join(format!("lr-wifi-{unique}.xml"));
    let mut file = match std::fs::File::create(&path) {
        Ok(file) => file,
        Err(error) => {
            log::info!("[PE NETWORK] cannot stage a Wi-Fi profile: {error:#}");
            return None;
        }
    };
    if file.write_all(profile_xml.as_bytes()).is_err() {
        let _ = std::fs::remove_file(&path);
        return None;
    }
    Some(path)
}

#[cfg(windows)]
fn profile_name_from_xml(xml: &str) -> Option<String> {
    let start = xml.find("<name>")? + "<name>".len();
    let end = xml[start..].find("</name>")? + start;
    let name = xml[start..end].trim();
    if name.is_empty() {
        None
    } else {
        Some(unescape_xml(name))
    }
}

#[cfg(windows)]
fn unescape_xml(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(windows)]
fn load_network_drivers() {
    use std::time::Duration;
    let drivers_root = crate::utils::path::get_exe_dir().join("drivers");
    if !drivers_root.is_dir() {
        return;
    }
    if let Err(error) = lr_core::driver_trust::ensure_pe_driver_signing_trust() {
        log::info!("[PE NETWORK] driver signing trust unavailable: {error:#}");
        return;
    }
    let mut loaded = 0usize;
    for entry in walkdir::WalkDir::new(&drivers_root).max_depth(6) {
        if loaded >= MAX_DRIVERS {
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let is_inf = path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("inf"));
        if !is_inf {
            continue;
        }
        // Storage-controller drivers are handled by the dedicated VMD path; skip them here.
        if path_contains_component(path, "storage_controller") {
            continue;
        }
        let within_limit = entry
            .metadata()
            .is_ok_and(|metadata| metadata.len() <= MAX_INF_BYTES);
        if !within_limit {
            continue;
        }
        // Many vendor INFs are UTF-16LE or ANSI; `read_to_string` would silently skip them.
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let text = lr_core::pe_network::decode_inf_text(&bytes);
        if !lr_core::pe_network::inf_text_declares_net_class(&text) {
            continue;
        }
        let Some(path_str) = path.to_str() else {
            continue;
        };
        run_status("drvload.exe", &[path_str], Duration::from_secs(60));
        loaded += 1;
    }
}

/// Load the host's exported wireless driver package, but only when the injected tree matches the
/// authenticated manifest exactly (paths, lengths, SHA-256, and no extra files or links).
#[cfg(windows)]
fn load_host_driver_package(payload: &lr_core::pe_network::PeNetworkPayload) {
    use std::time::Duration;
    if payload.drivers.is_empty() {
        return;
    }
    let root = std::path::Path::new(lr_core::pe_network::DRIVER_TREE_PE_PATH);
    if let Err(error) = lr_core::pe_network::verify_driver_tree(root, &payload.drivers) {
        log::info!("[PE NETWORK] host driver package failed verification; not loading: {error:#}");
        return;
    }
    // The exported package is signed by its vendor; the PE trust store only adds optional roots.
    if let Err(error) = lr_core::driver_trust::ensure_pe_driver_signing_trust() {
        log::info!("[PE NETWORK] driver signing trust unavailable: {error:#}");
    }
    let mut loaded = 0usize;
    for file in &payload.drivers {
        if !file.relative_path.to_ascii_lowercase().ends_with(".inf") {
            continue;
        }
        let path = lr_core::pe_network::driver_tree_path(root, &file.relative_path);
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let text = lr_core::pe_network::decode_inf_text(&bytes);
        if !lr_core::pe_network::inf_text_declares_net_class(&text) {
            continue;
        }
        let Some(path_str) = path.to_str() else {
            continue;
        };
        let status = run_status("drvload.exe", &[path_str], Duration::from_secs(120));
        if status == Some(0) {
            loaded += 1;
        } else {
            log::info!("[PE NETWORK] drvload of a host network driver returned {status:?}");
        }
    }
    log::info!("[PE NETWORK] loaded {loaded} host network driver INF(s)");
}

#[cfg(windows)]
fn path_contains_component(path: &std::path::Path, needle: &str) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|value| value.eq_ignore_ascii_case(needle))
    })
}

#[cfg(windows)]
fn wait_for_connectivity(timeout: std::time::Duration) -> bool {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + timeout;
    loop {
        if has_connectivity() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(750));
    }
}

#[cfg(windows)]
fn has_connectivity() -> bool {
    let output = read_console("ipconfig.exe", &[], std::time::Duration::from_secs(10));
    lr_core::pe_network::text_has_routable_ipv4(&output)
}

#[cfg(windows)]
fn run_status(program: &str, args: &[&str], timeout: std::time::Duration) -> Option<i32> {
    use std::process::Stdio;
    use std::time::Instant;
    let mut command = lr_core::command::new_command(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            log::info!("[PE NETWORK] cannot start {program}: {error:#}");
            return None;
        }
    };
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    log::info!("[PE NETWORK] {program} timed out");
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
            Err(error) => {
                log::info!("[PE NETWORK] waiting on {program} failed: {error:#}");
                return None;
            }
        }
    }
}

#[cfg(windows)]
fn read_console(program: &str, args: &[&str], timeout: std::time::Duration) -> String {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::Instant;
    let mut command = lr_core::command::new_command(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => return String::new(),
    };
    // Drain stdout while the child runs. Waiting for exit first deadlocks as soon as the output
    // fills the pipe buffer, which turns every connectivity probe into a silent timeout.
    let reader = child.stdout.take().map(|mut stdout| {
        std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = stdout.read_to_end(&mut buffer);
            buffer
        })
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return String::new();
                }
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return String::new();
            }
        }
    }
    let buffer = reader
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    lr_core::encoding::decode_windows_console_output(&buffer)
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::*;

    #[cfg(windows)]
    #[test]
    fn extracts_and_unescapes_profile_name() {
        let xml = "<WLANProfile><name>Home &amp; Away</name><SSIDConfig/></WLANProfile>";
        assert_eq!(profile_name_from_xml(xml).unwrap(), "Home & Away");
        assert!(profile_name_from_xml("<WLANProfile/>").is_none());
    }
}
