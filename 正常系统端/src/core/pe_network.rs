//! Normal-endpoint side of the optional WinPE network runtime ("接通 Wi-Fi PE").
//!
//! When `pe_network_enabled` is set in config.json and the target PE advertises the
//! `pe-network-v1` capability, this captures the currently connected Wi-Fi profile, exports the
//! host wireless adapter's OEM driver package, and packages both into an authenticated payload
//! that WinPE re-reads to bring up networking. The design assumes the user has swapped in a
//! Wi-Fi-capable PE WIM (WLAN stack present, lr_pe and its startup item built in); the exported
//! driver covers adapters that PE does not ship a driver for. Every step is best-effort: a failure
//! here only means PE falls back to whatever networking it can start on its own, never that the
//! install or maintenance boot is blocked.

use std::path::{Path, PathBuf};
use std::time::Duration;

use zeroize::Zeroizing;

use lr_core::pe_network::{
    DriverFileEntry, PeNetworkPayload, PeNetworkPayloadBinding, WifiProfileEntry,
};
use lr_core::registry::OfflineRegistry;
use lr_core::scoped_temp_file::ScopedTempDir;

use super::app_config::AppConfig;

/// Network adapter setup-class key; each numbered subkey maps `NetCfgInstanceId` to `InfPath`.
const NET_CLASS_KEY: &str =
    "HKLM\\SYSTEM\\CurrentControlSet\\Control\\Class\\{4d36e972-e325-11ce-bfc1-08002be10318}";

/// `pnputil /export-driver` normally finishes in seconds; never let it stall the handoff.
const DRIVER_EXPORT_TIMEOUT: Duration = Duration::from_secs(180);

/// The exported host driver package, kept in an administrators-only staging directory until the
/// boot-image writer injects it. The staging directory is removed when this is dropped.
pub(crate) struct StagedDriverTree {
    _directory: ScopedTempDir,
    root: PathBuf,
    files: Vec<DriverFileEntry>,
}

impl StagedDriverTree {
    /// Directory whose contents become `\LR_PeNetworkDrivers` in the boot image.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Authenticated manifest of every staged file.
    pub(crate) fn files(&self) -> &[DriverFileEntry] {
        &self.files
    }
}

/// What the boot-image writer injects: the authenticated payload bytes plus the optional driver
/// package they describe.
pub(crate) struct PeNetworkBootPayload {
    pub(crate) bytes: Zeroizing<Vec<u8>>,
    pub(crate) drivers: Option<StagedDriverTree>,
}

/// A prepared, self-consistent payload plus the binding to mirror into the signed config.
pub(crate) struct PreparedPeNetworkPayload {
    pub(crate) bytes: Zeroizing<Vec<u8>>,
    pub(crate) binding: PeNetworkPayloadBinding,
    pub(crate) drivers: Option<StagedDriverTree>,
}

/// Everything the install/maintenance writers need to attach the PE network payload.
pub(crate) struct PeNetworkHandoff {
    enabled: bool,
    payload: Option<PreparedPeNetworkPayload>,
}

impl PeNetworkHandoff {
    /// A handoff that carries nothing (feature off or unsupported PE).
    pub(crate) fn disabled() -> Self {
        Self {
            enabled: false,
            payload: None,
        }
    }

    /// Resolve the policy from config.json and, when enabled and supported, capture the payload.
    /// `pe_path` is the PE WIM the handoff will target; its capability marker gates the feature.
    pub(crate) fn prepare(pe_path: &Path) -> Self {
        if !AppConfig::load().pe_network_enabled() {
            return Self::disabled();
        }
        Self {
            enabled: true,
            payload: prepare_for_pe(pe_path),
        }
    }

    /// The authenticated config lines for the maintenance handoff (policy switch + binding).
    pub(crate) fn config_lines(&self) -> String {
        if !self.enabled {
            return String::new();
        }
        match &self.payload {
            Some(prepared) => format!(
                "PeNetworkEnabled=true\r\n{}",
                prepared.binding.to_config_lines()
            ),
            None => "PeNetworkEnabled=true\r\n".to_owned(),
        }
    }

    /// Length recorded in the install config (`0` when there is no payload).
    pub(crate) fn payload_length(&self) -> u64 {
        match &self.payload {
            Some(prepared) => prepared.binding.length_bytes,
            None => 0,
        }
    }

    /// SHA-256 recorded in the install config (empty when there is no payload).
    pub(crate) fn payload_sha256(&self) -> String {
        match &self.payload {
            Some(prepared) => prepared.binding.sha256.clone(),
            None => String::new(),
        }
    }

    /// Consume the prepared payload (and staged driver package) for the boot-image writer.
    pub(crate) fn into_boot_payload(self) -> Option<PeNetworkBootPayload> {
        self.payload.map(|prepared| PeNetworkBootPayload {
            bytes: prepared.bytes,
            drivers: prepared.drivers,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(bytes: Vec<u8>) -> Self {
        let bytes = Zeroizing::new(bytes);
        let binding = PeNetworkPayloadBinding::from_bytes(&bytes)
            .expect("test payload must be within the supported size range");
        Self {
            enabled: true,
            payload: Some(PreparedPeNetworkPayload {
                bytes,
                binding,
                drivers: None,
            }),
        }
    }
}

/// Capture the current Wi-Fi profile, export the wireless driver package and build the
/// authenticated payload for `pe_path`. Returns `None` (no payload) when the PE does not support
/// the feature or when a fatal step fails.
fn prepare_for_pe(pe_path: &Path) -> Option<PreparedPeNetworkPayload> {
    if !super::pe::supports_pe_network_payload(pe_path) {
        log::info!("[PE NETWORK] target PE lacks the pe-network capability; skipping payload");
        return None;
    }
    let nonce = match lr_core::handoff_auth::generate_locator_token() {
        Ok(token) => token.as_str().to_owned(),
        Err(error) => {
            log::warn!("[PE NETWORK] cannot generate payload nonce: {error:#}");
            return None;
        }
    };
    let mut entries = Vec::new();
    match super::native_wifi::capture_connected_wifi() {
        Ok(profile) => {
            log::info!("[PE NETWORK] captured the connected Wi-Fi profile");
            entries.push(WifiProfileEntry::new(profile.ssid, profile.xml));
        }
        Err(error) => {
            log::info!("[PE NETWORK] no portable Wi-Fi captured ({error:#}); wired only");
        }
    }
    let drivers = export_wifi_driver_package();
    build_prepared_payload(&nonce, entries, drivers)
}

/// Assemble the payload with the driver manifest; if the package does not fit, keep the Wi-Fi
/// profiles and drop only the drivers.
fn build_prepared_payload(
    nonce: &str,
    entries: Vec<WifiProfileEntry>,
    drivers: Option<StagedDriverTree>,
) -> Option<PreparedPeNetworkPayload> {
    if let Some(tree) = drivers {
        match assemble_payload(nonce, entries.clone(), tree.files.clone()) {
            Ok((bytes, binding)) => {
                return Some(PreparedPeNetworkPayload {
                    bytes,
                    binding,
                    drivers: Some(tree),
                });
            }
            Err(error) => {
                log::warn!("[PE NETWORK] driver package does not fit the payload: {error:#}");
            }
        }
    }
    match assemble_payload(nonce, entries, Vec::new()) {
        Ok((bytes, binding)) => Some(PreparedPeNetworkPayload {
            bytes,
            binding,
            drivers: None,
        }),
        Err(error) => {
            log::warn!("[PE NETWORK] cannot assemble network payload: {error:#}");
            None
        }
    }
}

fn assemble_payload(
    nonce: &str,
    entries: Vec<WifiProfileEntry>,
    drivers: Vec<DriverFileEntry>,
) -> anyhow::Result<(Zeroizing<Vec<u8>>, PeNetworkPayloadBinding)> {
    let payload = PeNetworkPayload::new(nonce, entries)?.with_drivers(drivers)?;
    let bytes = payload.to_bytes();
    let binding = PeNetworkPayloadBinding::from_bytes(&bytes)?;
    Ok((bytes, binding))
}

/// Export the OEM driver package of every WLAN adapter into a fresh administrators-only staging
/// directory and build its manifest. Inbox drivers are skipped (they are not exportable driver
/// store packages), and `pnputil /export-driver` needs Windows 10 1607 or later; on anything
/// older, or on any failure, PE simply relies on its own drivers.
fn export_wifi_driver_package() -> Option<StagedDriverTree> {
    let guids = match super::native_wifi::wifi_interface_guids() {
        Ok(guids) if !guids.is_empty() => guids,
        Ok(_) => {
            log::info!("[PE NETWORK] no WLAN adapter found; no driver package to export");
            return None;
        }
        Err(error) => {
            log::info!("[PE NETWORK] WLAN adapters unavailable ({error:#}); no driver export");
            return None;
        }
    };
    let inf_names = wifi_driver_inf_names(&guids);
    if inf_names.is_empty() {
        log::info!("[PE NETWORK] WLAN adapter uses no exportable OEM driver package");
        return None;
    }
    let pnputil = match lr_core::windows_compat::system_directory() {
        Ok(directory) => directory.join("pnputil.exe"),
        Err(error) => {
            log::warn!("[PE NETWORK] cannot locate the system directory: {error:#}");
            return None;
        }
    };
    let parent = std::env::temp_dir();
    let staging = ScopedTempDir::create_system_administrators_in(&parent, "lr-pe-drivers");
    let directory = match staging {
        Ok(directory) => directory,
        Err(error) => {
            log::warn!("[PE NETWORK] cannot create the driver staging directory: {error:#}");
            return None;
        }
    };
    let root = directory
        .path()
        .join(lr_core::pe_network::DRIVER_TREE_DIR_NAME);
    if let Err(error) = std::fs::create_dir(&root) {
        log::warn!("[PE NETWORK] cannot create the driver package root: {error:#}");
        return None;
    }
    let mut exported = 0_usize;
    for (index, inf_name) in inf_names.iter().enumerate() {
        let target = root.join(format!("package{index}"));
        if std::fs::create_dir(&target).is_err() {
            continue;
        }
        if export_driver_package(&pnputil, inf_name, &target) {
            exported += 1;
        } else {
            log::info!("[PE NETWORK] pnputil could not export {inf_name}; skipping it");
            let _ = std::fs::remove_dir_all(&target);
        }
    }
    if exported == 0 {
        return None;
    }
    match lr_core::pe_network::driver_tree_manifest(&root) {
        Ok(files) if !files.is_empty() => {
            log::info!(
                "[PE NETWORK] staged {exported} wireless driver package(s), {} file(s)",
                files.len()
            );
            Some(StagedDriverTree {
                _directory: directory,
                root,
                files,
            })
        }
        Ok(_) => None,
        Err(error) => {
            log::warn!("[PE NETWORK] exported driver package is not usable: {error:#}");
            None
        }
    }
}

/// Published OEM INF names (`oemNN.inf`) of the drivers bound to the given WLAN interfaces.
fn wifi_driver_inf_names(interface_guids: &[String]) -> Vec<String> {
    let subkeys = match OfflineRegistry::enumerate_subkeys(NET_CLASS_KEY) {
        Ok(subkeys) => subkeys,
        Err(error) => {
            log::info!("[PE NETWORK] cannot enumerate network adapters: {error:#}");
            return Vec::new();
        }
    };
    let mut names = Vec::new();
    for subkey in subkeys {
        // Adapter instances are the four-digit subkeys; skip `Properties` and similar.
        if subkey.len() != 4 || !subkey.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let key = format!("{NET_CLASS_KEY}\\{subkey}");
        let Some(instance) = registry_string(&key, "NetCfgInstanceId") else {
            continue;
        };
        let bound = interface_guids
            .iter()
            .any(|guid| guid.eq_ignore_ascii_case(instance.trim()));
        if !bound {
            continue;
        }
        let Some(inf_path) = registry_string(&key, "InfPath") else {
            continue;
        };
        let inf_name = inf_path.trim().to_ascii_lowercase();
        if !is_published_oem_inf(&inf_name) {
            log::info!("[PE NETWORK] WLAN adapter uses inbox driver {inf_name}; not exported");
            continue;
        }
        if !names.contains(&inf_name) {
            names.push(inf_name);
        }
    }
    names
}

fn registry_string(key: &str, value_name: &str) -> Option<String> {
    OfflineRegistry::query_string_optional(key, value_name)
        .ok()
        .flatten()
}

/// Whether `inf_name` is a published driver-store name such as `oem42.inf`.
fn is_published_oem_inf(inf_name: &str) -> bool {
    inf_name
        .strip_prefix("oem")
        .and_then(|rest| rest.strip_suffix(".inf"))
        .is_some_and(|digits| {
            !digits.is_empty() && digits.len() <= 6 && digits.bytes().all(|b| b.is_ascii_digit())
        })
}

/// Run `pnputil /export-driver <oemNN.inf> <target>` with a bounded wait.
fn export_driver_package(pnputil: &Path, inf_name: &str, target: &Path) -> bool {
    use std::process::Stdio;
    use std::time::Instant;
    let mut command = lr_core::command::new_command(pnputil);
    command
        .arg("/export-driver")
        .arg(inf_name)
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            log::info!("[PE NETWORK] cannot start pnputil: {error}");
            return false;
        }
    };
    let deadline = Instant::now() + DRIVER_EXPORT_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_handoff_writes_no_config_lines() {
        let handoff = PeNetworkHandoff::disabled();
        assert!(handoff.config_lines().is_empty());
        assert_eq!(handoff.payload_length(), 0);
        assert!(handoff.payload_sha256().is_empty());
        assert!(handoff.into_boot_payload().is_none());
    }

    #[test]
    fn prepared_handoff_exposes_binding_and_bytes() {
        let raw = b"[PeNetwork]\r\nVersion=1\r\nNonce=abc\r\nWifiProfileCount=0\r\n".to_vec();
        let expected = lr_core::hash::sha256_bytes(&raw);
        let handoff = PeNetworkHandoff::for_test(raw.clone());
        assert_eq!(handoff.payload_length(), raw.len() as u64);
        assert_eq!(handoff.payload_sha256(), expected);
        let lines = handoff.config_lines();
        assert!(lines.contains("PeNetworkEnabled=true\r\n"));
        assert!(lines.contains(&format!("PeNetworkPayloadLength={}\r\n", raw.len())));
        let boot = PeNetworkHandoff::for_test(raw.clone())
            .into_boot_payload()
            .unwrap();
        assert_eq!(boot.bytes.as_slice(), raw.as_slice());
        assert!(boot.drivers.is_none());
    }

    #[test]
    fn only_published_oem_driver_packages_are_exported() {
        assert!(is_published_oem_inf("oem7.inf"));
        assert!(is_published_oem_inf("oem123.inf"));
        assert!(!is_published_oem_inf("netwtw08.inf"));
        assert!(!is_published_oem_inf("oem.inf"));
        assert!(!is_published_oem_inf("oem12.inf.bak"));
        assert!(!is_published_oem_inf("oemx1.inf"));
    }

    #[test]
    fn payload_keeps_wifi_when_the_driver_manifest_is_absent() {
        let entries = vec![WifiProfileEntry::new("Home", "<WLANProfile />")];
        let prepared = build_prepared_payload("nonce", entries, None).unwrap();
        assert!(prepared.drivers.is_none());
        let parsed = PeNetworkPayload::parse(&prepared.bytes).unwrap();
        assert_eq!(parsed.entries.len(), 1);
        assert!(parsed.drivers.is_empty());
        prepared.binding.verify(&prepared.bytes).unwrap();
    }
}
