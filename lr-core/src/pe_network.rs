//! Shared definitions for the optional WinPE network runtime ("接通 Wi-Fi PE").
//!
//! The normal endpoint captures the currently connected Wi-Fi profile, packages it into an
//! authenticated payload, and binds its length and SHA-256 into the signed install/maintenance
//! configuration. WinPE re-reads the payload, verifies the binding, and brings up wired and
//! wireless networking with Microsoft-supported tools. Everything here is pure shared logic; no
//! Windows API is touched.
//!
//! The payload can also list the host's exported wireless driver package (path, length and
//! SHA-256 of every file). The package itself is injected next to the payload; WinPE verifies the
//! whole tree against that authenticated manifest before `drvload` ever sees it.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use zeroize::Zeroizing;

use crate::hash::sha256_bytes;
use crate::install_handoff::{decode_hex_array, encode_hex};

/// Capability string a Wi-Fi-capable PE advertises in its handoff-capabilities marker.
pub const PE_NETWORK_CAPABILITY: &str = "pe-network-v1";

/// Fixed in-WIM path (and, after boot, fixed X: path) of the authenticated network payload.
pub const PAYLOAD_WIM_PATH: &str = "\\LR_PeNetwork.ini";
pub const PAYLOAD_PE_PATH: &str = "X:\\LR_PeNetwork.ini";
pub const PAYLOAD_FILE_NAME: &str = "LR_PeNetwork.ini";

/// The payload is a small text file; refuse anything larger before allocation.
pub const PAYLOAD_MAX_BYTES: u64 = 512 * 1024;

/// Upper bound on stored Wi-Fi profiles, keeping the payload small and bounded.
pub const MAX_WIFI_PROFILES: usize = 8;

/// `Net` device setup class GUID. Used to keep optional third-party PE drivers to network cards.
pub const NET_CLASS_GUID: &str = "{4d36e972-e325-11ce-bfc1-08002be10318}";

/// Fixed in-WIM directory (and, after boot, fixed X: directory) holding the exported host
/// network driver package. Every file in it is listed in the authenticated payload.
pub const DRIVER_TREE_WIM_PATH: &str = "\\LR_PeNetworkDrivers";
pub const DRIVER_TREE_PE_PATH: &str = "X:\\LR_PeNetworkDrivers";
pub const DRIVER_TREE_DIR_NAME: &str = "LR_PeNetworkDrivers";

/// Bounds for the exported host driver package: file count, total bytes and UTF-8 path length.
/// They keep the manifest well inside `PAYLOAD_MAX_BYTES` and the boot image reasonably small.
pub const MAX_DRIVER_FILES: usize = 512;
pub const MAX_DRIVER_TREE_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_DRIVER_PATH_BYTES: usize = 200;

/// Directory nesting accepted inside the exported driver package.
const MAX_DRIVER_TREE_DEPTH: usize = 8;

const PAYLOAD_VERSION: &str = "1";

/// Authenticated length + SHA-256 of the network payload, mirrored into the signed config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeNetworkPayloadBinding {
    pub length_bytes: u64,
    pub sha256: String,
}

impl PeNetworkPayloadBinding {
    /// Derive the binding for an exact payload byte string.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() as u64 > PAYLOAD_MAX_BYTES {
            bail!("PE network payload size is outside its supported range");
        }
        Ok(Self {
            length_bytes: bytes.len() as u64,
            sha256: sha256_bytes(bytes),
        })
    }

    /// Confirm exact bytes match this binding by length and content hash.
    pub fn verify(&self, bytes: &[u8]) -> Result<()> {
        if self.length_bytes != bytes.len() as u64 {
            bail!("PE network payload length does not match its authenticated binding");
        }
        if !self.sha256.eq_ignore_ascii_case(&sha256_bytes(bytes)) {
            bail!("PE network payload SHA-256 does not match its authenticated binding");
        }
        Ok(())
    }

    /// The two authenticated config lines, each terminated by CRLF.
    pub fn to_config_lines(&self) -> String {
        format!(
            "PeNetworkPayloadLength={}\r\nPeNetworkPayloadSha256={}\r\n",
            self.length_bytes,
            self.sha256.to_ascii_lowercase()
        )
    }

    /// Read the optional binding from an authenticated config. The two fields are all-or-nothing:
    /// a partially written binding is rejected so a corrupt config can never silently disable the
    /// integrity check.
    pub fn from_config_text(content: &str) -> Result<Option<Self>> {
        let mut length = None;
        let mut sha256 = None;
        for raw in content.lines() {
            let line = raw.trim();
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                "PeNetworkPayloadLength" => {
                    if length.replace(value.trim().to_owned()).is_some() {
                        bail!("PE network payload length is repeated");
                    }
                }
                "PeNetworkPayloadSha256" => {
                    if sha256.replace(value.trim().to_owned()).is_some() {
                        bail!("PE network payload SHA-256 is repeated");
                    }
                }
                _ => {}
            }
        }
        match (length, sha256) {
            (None, None) => Ok(None),
            (Some(length), Some(sha256)) => {
                let length_bytes: u64 = length
                    .parse()
                    .context("PE network payload length is not an integer")?;
                if length_bytes == 0 || length_bytes > PAYLOAD_MAX_BYTES {
                    bail!("PE network payload length is outside its supported range");
                }
                // Validate the hex shape up front so PE never trusts a malformed digest.
                decode_hex_array::<32>(&sha256, "PE network payload SHA-256")?;
                Ok(Some(Self {
                    length_bytes,
                    sha256: sha256.to_ascii_lowercase(),
                }))
            }
            _ => bail!("PE network payload binding fields are incomplete"),
        }
    }
}

/// Whether the authenticated config authorizes the PE network runtime. Exactly one
/// `PeNetworkEnabled=true` must be present; anything else (absent, false, repeated) is off.
pub fn policy_enabled(content: &str) -> bool {
    let mut seen = false;
    for raw in content.lines() {
        let line = raw.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "PeNetworkEnabled" {
            continue;
        }
        if seen || !value.trim().eq_ignore_ascii_case("true") {
            return false;
        }
        seen = true;
    }
    seen
}

/// One captured Wi-Fi profile. The XML can contain a clear-text key, so it is held in zeroizing
/// memory and never logged.
#[derive(Clone)]
pub struct WifiProfileEntry {
    pub ssid: String,
    pub profile_xml: Zeroizing<String>,
}

impl WifiProfileEntry {
    pub fn new(ssid: impl Into<String>, profile_xml: impl Into<String>) -> Self {
        Self {
            ssid: ssid.into(),
            profile_xml: Zeroizing::new(profile_xml.into()),
        }
    }
}

/// One file of the exported host network driver package, authenticated through the payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverFileEntry {
    /// Backslash-separated path relative to the driver package root.
    pub relative_path: String,
    pub length_bytes: u64,
    /// Lowercase hexadecimal SHA-256 of the file contents.
    pub sha256: String,
}

impl DriverFileEntry {
    pub fn new(
        relative_path: impl Into<String>,
        length_bytes: u64,
        sha256: impl AsRef<str>,
    ) -> Result<Self> {
        let entry = Self {
            relative_path: relative_path.into(),
            length_bytes,
            sha256: sha256.as_ref().trim().to_ascii_lowercase(),
        };
        validate_driver_relative_path(&entry.relative_path)?;
        decode_hex_array::<32>(&entry.sha256, "driver file SHA-256")?;
        Ok(entry)
    }
}

/// The decoded network payload: a random nonce, the captured Wi-Fi profiles and the manifest of
/// the optional host driver package.
pub struct PeNetworkPayload {
    pub nonce: String,
    pub entries: Vec<WifiProfileEntry>,
    pub drivers: Vec<DriverFileEntry>,
}

impl PeNetworkPayload {
    pub fn new(nonce: impl Into<String>, entries: Vec<WifiProfileEntry>) -> Result<Self> {
        if entries.len() > MAX_WIFI_PROFILES {
            bail!("too many Wi-Fi profiles for the PE network payload");
        }
        Ok(Self {
            nonce: nonce.into(),
            entries,
            drivers: Vec::new(),
        })
    }

    /// Attach the manifest of the exported host network driver package.
    pub fn with_drivers(mut self, drivers: Vec<DriverFileEntry>) -> Result<Self> {
        validate_driver_manifest(&drivers)?;
        self.drivers = drivers;
        Ok(self)
    }

    /// Serialize to the authenticated INI byte string (CRLF, hex-encoded fields).
    pub fn to_bytes(&self) -> Zeroizing<Vec<u8>> {
        let mut text = String::new();
        text.push_str("[PeNetwork]\r\n");
        text.push_str(&format!("Version={PAYLOAD_VERSION}\r\n"));
        text.push_str(&format!("Nonce={}\r\n", self.nonce));
        text.push_str(&format!("WifiProfileCount={}\r\n", self.entries.len()));
        for (index, entry) in self.entries.iter().enumerate() {
            text.push_str(&format!(
                "WifiSsid{index}={}\r\n",
                encode_hex(entry.ssid.as_bytes())
            ));
            text.push_str(&format!(
                "WifiProfile{index}={}\r\n",
                encode_hex(entry.profile_xml.as_bytes())
            ));
        }
        if !self.drivers.is_empty() {
            text.push_str(&format!("DriverFileCount={}\r\n", self.drivers.len()));
            for (index, file) in self.drivers.iter().enumerate() {
                text.push_str(&format!(
                    "DriverPath{index}={}\r\n",
                    encode_hex(file.relative_path.as_bytes())
                ));
                text.push_str(&format!("DriverLength{index}={}\r\n", file.length_bytes));
                text.push_str(&format!("DriverHash{index}={}\r\n", file.sha256));
            }
        }
        Zeroizing::new(text.into_bytes())
    }

    /// Parse the authenticated INI byte string. The bytes are expected to have already passed
    /// their binding check, so this focuses on structure.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(bytes).context("PE network payload is not UTF-8")?;
        let mut version = None;
        let mut nonce = None;
        let mut count = None;
        let mut ssids: Vec<(usize, String)> = Vec::new();
        let mut xmls: Vec<(usize, String)> = Vec::new();
        let mut driver_count = None;
        let mut driver_paths: Vec<(usize, String)> = Vec::new();
        let mut driver_lengths: Vec<(usize, u64)> = Vec::new();
        let mut driver_hashes: Vec<(usize, String)> = Vec::new();
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('[') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            match key {
                "Version" => version = Some(value.to_owned()),
                "Nonce" => nonce = Some(value.to_owned()),
                "WifiProfileCount" => {
                    count = Some(
                        value
                            .parse::<usize>()
                            .context("Wi-Fi profile count is not an integer")?,
                    );
                }
                "DriverFileCount" => {
                    driver_count = Some(
                        value
                            .parse::<usize>()
                            .context("driver file count is not an integer")?,
                    );
                }
                other => {
                    if let Some(index) = other.strip_prefix("WifiSsid") {
                        let index: usize = index
                            .parse()
                            .context("Wi-Fi SSID index is not an integer")?;
                        ssids.push((index, decode_hex_string(value, "Wi-Fi SSID")?));
                    } else if let Some(index) = other.strip_prefix("WifiProfile") {
                        let index: usize = index
                            .parse()
                            .context("Wi-Fi profile index is not an integer")?;
                        xmls.push((index, decode_hex_string(value, "Wi-Fi profile")?));
                    } else if let Some(index) = other.strip_prefix("DriverPath") {
                        let index: usize = index
                            .parse()
                            .context("driver file path index is not an integer")?;
                        driver_paths.push((index, decode_hex_string(value, "driver path")?));
                    } else if let Some(index) = other.strip_prefix("DriverLength") {
                        let index: usize = index
                            .parse()
                            .context("driver file length index is not an integer")?;
                        let length = value
                            .parse::<u64>()
                            .context("driver file length is not an integer")?;
                        driver_lengths.push((index, length));
                    } else if let Some(index) = other.strip_prefix("DriverHash") {
                        let index: usize = index
                            .parse()
                            .context("driver file hash index is not an integer")?;
                        driver_hashes.push((index, value.to_owned()));
                    }
                }
            }
        }
        if version.as_deref() != Some(PAYLOAD_VERSION) {
            bail!("unsupported PE network payload version");
        }
        let nonce = nonce.context("PE network payload is missing its nonce")?;
        let count = count.context("PE network payload is missing its profile count")?;
        if count > MAX_WIFI_PROFILES {
            bail!("PE network payload declares too many Wi-Fi profiles");
        }
        let mut entries = Vec::with_capacity(count);
        for index in 0..count {
            let ssid = ssids
                .iter()
                .find(|(candidate, _)| *candidate == index)
                .map(|(_, value)| value.clone())
                .with_context(|| format!("PE network payload is missing Wi-Fi SSID {index}"))?;
            let xml = xmls
                .iter()
                .find(|(candidate, _)| *candidate == index)
                .map(|(_, value)| value.clone())
                .with_context(|| format!("PE network payload is missing Wi-Fi profile {index}"))?;
            entries.push(WifiProfileEntry::new(ssid, xml));
        }
        let driver_count = driver_count.unwrap_or(0);
        if driver_count > MAX_DRIVER_FILES {
            bail!("PE network payload declares too many driver files");
        }
        let mut drivers = Vec::with_capacity(driver_count);
        for index in 0..driver_count {
            let entry = DriverFileEntry::new(
                indexed_value(&driver_paths, index, "driver path")?,
                indexed_value(&driver_lengths, index, "driver length")?,
                indexed_value(&driver_hashes, index, "driver hash")?,
            )?;
            drivers.push(entry);
        }
        Self::new(nonce, entries)?.with_drivers(drivers)
    }
}

fn indexed_value<T: Clone>(values: &[(usize, T)], index: usize, field: &str) -> Result<T> {
    values
        .iter()
        .find(|(candidate, _)| *candidate == index)
        .map(|(_, value)| value.clone())
        .with_context(|| format!("PE network payload is missing {field} {index}"))
}

/// Validate a driver-package relative path: backslash-separated, bounded, and free of empty,
/// `.`/`..`, drive, stream, wildcard or trailing-dot/space components that could alias or escape.
pub fn validate_driver_relative_path(path: &str) -> Result<()> {
    if path.is_empty() || path.len() > MAX_DRIVER_PATH_BYTES {
        bail!("driver file path length is outside its supported range");
    }
    for component in path.split('\\') {
        let unsupported = component.is_empty()
            || component == "."
            || component == ".."
            || component.ends_with('.')
            || component.ends_with(' ')
            || component
                .chars()
                .any(|character| character.is_control() || "/:*?\"<>|".contains(character));
        if unsupported {
            bail!("driver file path contains an unsupported component");
        }
    }
    Ok(())
}

fn validate_driver_manifest(drivers: &[DriverFileEntry]) -> Result<()> {
    if drivers.len() > MAX_DRIVER_FILES {
        bail!("driver package has too many files for the PE network payload");
    }
    let mut total = 0_u64;
    let mut seen = std::collections::HashSet::new();
    for entry in drivers {
        validate_driver_relative_path(&entry.relative_path)?;
        decode_hex_array::<32>(&entry.sha256, "driver file SHA-256")?;
        total = total
            .checked_add(entry.length_bytes)
            .context("driver package size overflows")?;
        if !seen.insert(entry.relative_path.to_ascii_lowercase()) {
            bail!("driver package lists the same file twice");
        }
    }
    if total > MAX_DRIVER_TREE_BYTES {
        bail!("driver package exceeds its supported size");
    }
    Ok(())
}

/// Native path of a validated driver-package relative path under `root`.
pub fn driver_tree_path(root: &Path, relative_path: &str) -> PathBuf {
    relative_path
        .split('\\')
        .fold(root.to_path_buf(), |path, component| path.join(component))
}

/// Walk an exported driver package and build its manifest (sorted by path). Links, reparse
/// points, unsupported names and packages beyond the supported bounds are refused.
pub fn driver_tree_manifest(root: &Path) -> Result<Vec<DriverFileEntry>> {
    let mut found = Vec::new();
    collect_driver_tree_files(root, "", 0, &mut found)?;
    found.sort();
    let mut entries = Vec::with_capacity(found.len());
    for (relative_path, _) in found {
        let path = driver_tree_path(root, &relative_path);
        let (length_bytes, sha256) = hash_driver_file(&path)?;
        entries.push(DriverFileEntry::new(relative_path, length_bytes, sha256)?);
    }
    validate_driver_manifest(&entries)?;
    Ok(entries)
}

/// Confirm `root` holds exactly the manifest files (same paths, lengths and SHA-256) and nothing
/// else: no extra files, links or reparse points.
pub fn verify_driver_tree(root: &Path, entries: &[DriverFileEntry]) -> Result<()> {
    validate_driver_manifest(entries)?;
    let mut found = Vec::new();
    collect_driver_tree_files(root, "", 0, &mut found)?;
    if found.len() != entries.len() {
        bail!("driver package does not contain exactly the authenticated files");
    }
    for entry in entries {
        let listed = found
            .iter()
            .any(|(path, _)| path.eq_ignore_ascii_case(&entry.relative_path));
        if !listed {
            bail!("driver package is missing an authenticated file");
        }
        let path = driver_tree_path(root, &entry.relative_path);
        let (length_bytes, sha256) = hash_driver_file(&path)?;
        if length_bytes != entry.length_bytes || sha256 != entry.sha256 {
            bail!("driver package file does not match its authenticated length and SHA-256");
        }
    }
    Ok(())
}

fn collect_driver_tree_files(
    directory: &Path,
    prefix: &str,
    depth: usize,
    found: &mut Vec<(String, u64)>,
) -> Result<()> {
    if depth > MAX_DRIVER_TREE_DEPTH {
        bail!("driver package nests directories too deeply");
    }
    let listing = std::fs::read_dir(directory)
        .with_context(|| format!("cannot list driver directory {}", directory.display()))?;
    for item in listing {
        let item = item.context("cannot read a driver directory entry")?;
        let name = item
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("driver package contains a non-Unicode file name"))?;
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}\\{name}")
        };
        validate_driver_relative_path(&relative)?;
        let metadata = std::fs::symlink_metadata(item.path())
            .context("cannot inspect a driver package entry")?;
        if is_link_or_reparse_point(&metadata) {
            bail!("driver package contains a link or reparse point");
        }
        if metadata.is_dir() {
            collect_driver_tree_files(&item.path(), &relative, depth + 1, found)?;
        } else if metadata.is_file() {
            found.push((relative, metadata.len()));
            let total = found
                .iter()
                .fold(0_u64, |sum, (_, length)| sum.saturating_add(*length));
            if found.len() > MAX_DRIVER_FILES || total > MAX_DRIVER_TREE_BYTES {
                bail!("driver package exceeds its supported file count or size");
            }
        } else {
            bail!("driver package contains an unsupported file type");
        }
    }
    Ok(())
}

fn is_link_or_reparse_point(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_type().is_symlink()
            || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

/// Length and SHA-256 of the bytes actually read, bounded so a growing file cannot stall hashing.
fn hash_driver_file(path: &Path) -> Result<(u64, String)> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("cannot open driver file {}", path.display()))?;
    let mut length = 0_u64;
    let reader = file.take(MAX_DRIVER_TREE_BYTES + 1);
    let sha256 = crate::hash::sha256_reader(reader, |total| length = total)
        .with_context(|| format!("cannot hash driver file {}", path.display()))?;
    Ok((length, sha256))
}

fn decode_hex_string(value: &str, field: &str) -> Result<String> {
    let value = value.trim();
    if !value.len().is_multiple_of(2) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{field} is not valid hexadecimal");
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for index in (0..value.len()).step_by(2) {
        let byte = u8::from_str_radix(&value[index..index + 2], 16)
            .with_context(|| format!("invalid hexadecimal {field}"))?;
        bytes.push(byte);
    }
    String::from_utf8(bytes).with_context(|| format!("{field} is not UTF-8"))
}

/// Candidate profile XML strings to try in WinPE, most-compatible last. The original profile is
/// tried first; when the credentials allow, a minimal rebuilt profile follows so an older WinPE
/// WLAN stack that rejects extra elements still connects, plus WPA3->WPA2 and OWE->open fallbacks.
pub fn wifi_profile_candidates(profile_xml: &str) -> Vec<String> {
    let mut candidates = vec![profile_xml.to_owned()];
    let Some(details) = parse_profile_details(profile_xml) else {
        return candidates;
    };
    // Enterprise (802.1X) profiles cannot be rebuilt from the public fields; keep the original.
    if details.uses_one_x {
        return candidates;
    }
    for authentication in fallback_authentications(&details.authentication) {
        let encryption = fallback_encryption(authentication, &details.encryption);
        let rebuilt = build_minimal_profile(&details, authentication, encryption);
        if !candidates.contains(&rebuilt) {
            candidates.push(rebuilt);
        }
    }
    candidates
}

struct ProfileDetails {
    name: String,
    ssid_name: String,
    ssid_hex: Option<String>,
    authentication: String,
    encryption: String,
    key_material: Option<String>,
    uses_one_x: bool,
    non_broadcast: bool,
}

fn parse_profile_details(xml: &str) -> Option<ProfileDetails> {
    let document = roxmltree::Document::parse(xml).ok()?;
    let text_of = |name: &str| {
        document
            .descendants()
            .find(|node| node.tag_name().name() == name)
            .and_then(|node| node.text())
            .map(|value| value.trim().to_owned())
    };
    let ssid_name = document
        .descendants()
        .find(|node| node.tag_name().name() == "SSID")
        .and_then(|ssid| {
            ssid.children()
                .find(|child| child.tag_name().name() == "name")
                .and_then(|node| node.text())
        })
        .map(|value| value.trim().to_owned());
    let ssid_hex = document
        .descendants()
        .find(|node| node.tag_name().name() == "SSID")
        .and_then(|ssid| {
            ssid.children()
                .find(|child| child.tag_name().name() == "hex")
                .and_then(|node| node.text())
        })
        .map(|value| value.trim().to_owned());
    let name = text_of("name").or_else(|| ssid_name.clone())?;
    let ssid_name = ssid_name.unwrap_or_else(|| name.clone());
    let authentication = text_of("authentication").unwrap_or_default().to_uppercase();
    let encryption = text_of("encryption").unwrap_or_default().to_uppercase();
    let key_material = text_of("keyMaterial").filter(|value| !value.is_empty());
    let uses_one_x = document
        .descendants()
        .any(|node| node.tag_name().name() == "OneX")
        || text_of("useOneX").is_some_and(|value| value.eq_ignore_ascii_case("true"));
    let non_broadcast =
        text_of("nonBroadcast").is_some_and(|value| value.eq_ignore_ascii_case("true"));
    Some(ProfileDetails {
        name,
        ssid_name,
        ssid_hex,
        authentication,
        encryption,
        key_material,
        uses_one_x,
        non_broadcast,
    })
}

fn fallback_authentications(authentication: &str) -> Vec<&'static str> {
    match authentication {
        "WPA3SAE" | "WPA3" => vec!["WPA3SAE", "WPA2PSK"],
        "WPA2PSK" | "WPAPSK" | "WPA2" | "WPA" => vec!["WPA2PSK"],
        "OWE" => vec!["OWE", "open"],
        "OPEN" | "" => vec!["open"],
        _ => vec![],
    }
}

fn fallback_encryption(authentication: &str, original: &str) -> &'static str {
    match authentication {
        "open" => "none",
        "OWE" => "AES",
        _ if original.eq_ignore_ascii_case("TKIP") => "TKIP",
        _ => "AES",
    }
}

fn build_minimal_profile(
    details: &ProfileDetails,
    authentication: &str,
    encryption: &str,
) -> String {
    let name = xml_escape(&details.name);
    let ssid_name = xml_escape(&details.ssid_name);
    let hex_element = details
        .ssid_hex
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(|value| format!("<hex>{}</hex>", xml_escape(value)))
        .unwrap_or_default();
    let non_broadcast = if details.non_broadcast {
        "true"
    } else {
        "false"
    };
    let shared_key = if authentication == "open" || authentication == "OWE" {
        String::new()
    } else if let Some(key) = details.key_material.as_deref() {
        format!(
            "<sharedKey><keyType>passPhrase</keyType><protected>false</protected>\
             <keyMaterial>{}</keyMaterial></sharedKey>",
            xml_escape(key)
        )
    } else {
        String::new()
    };
    format!(
        "<?xml version=\"1.0\"?>\r\n\
         <WLANProfile xmlns=\"http://www.microsoft.com/networking/WLAN/profile/v1\">\
         <name>{name}</name>\
         <SSIDConfig><SSID>{hex_element}<name>{ssid_name}</name></SSID>\
         <nonBroadcast>{non_broadcast}</nonBroadcast></SSIDConfig>\
         <connectionType>ESS</connectionType>\
         <connectionMode>auto</connectionMode>\
         <MSM><security><authEncryption>\
         <authentication>{authentication}</authentication>\
         <encryption>{encryption}</encryption>\
         <useOneX>false</useOneX></authEncryption>{shared_key}</security></MSM>\
         </WLANProfile>"
    )
}

fn xml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Decode INF bytes for the `[Version]` scan. INF files are UTF-16LE (usually with a BOM), UTF-8,
/// or an ANSI code page; the keys the scan needs are ASCII, so other input is decoded lossily.
pub fn decode_inf_text(bytes: &[u8]) -> String {
    let utf16 = if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        Some(rest)
    } else if bytes.len() >= 4 && bytes[0] != 0 && bytes[1] == 0 && bytes[3] == 0 {
        Some(bytes)
    } else {
        None
    };
    match utf16 {
        Some(raw) => {
            let units: Vec<u16> = raw
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        None => {
            let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
            String::from_utf8_lossy(bytes).into_owned()
        }
    }
}

/// Whether an INF `[Version]` section declares the `Net` setup class (a network adapter driver).
pub fn inf_text_declares_net_class(text: &str) -> bool {
    let mut in_version = false;
    for raw_line in text.lines() {
        let line = raw_line.split(';').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            in_version = line
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim()
                .eq_ignore_ascii_case("version");
            continue;
        }
        if !in_version {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "class" if value.eq_ignore_ascii_case("net") => return true,
            "classguid" if value.eq_ignore_ascii_case(NET_CLASS_GUID) => return true,
            _ => {}
        }
    }
    false
}

/// Whether a token is a routable (usable in WinPE) unicast IPv4 literal: not unspecified,
/// loopback, link-local APIPA, multicast/reserved, or a subnet-mask-shaped value.
pub fn is_routable_ipv4(value: &str) -> bool {
    let octets: Vec<&str> = value.split('.').collect();
    if octets.len() != 4 {
        return false;
    }
    let mut parsed = [0u8; 4];
    for (slot, octet) in parsed.iter_mut().zip(octets) {
        if octet.is_empty() || octet.len() > 3 || !octet.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
        match octet.parse::<u16>() {
            Ok(number) if number <= 255 => *slot = number as u8,
            _ => return false,
        }
    }
    // `ipconfig` prints a subnet mask next to every address, APIPA ones included, so values with
    // contiguous leading one bits (255.255.0.0, 192.0.0.0, ...) must never count as an address.
    let bits = u32::from_be_bytes(parsed);
    let is_mask_shaped = bits != 0 && bits.leading_ones() + bits.trailing_zeros() == 32;
    !(parsed[0] == 0
        || parsed[0] == 127
        || parsed[0] >= 224
        || (parsed[0] == 169 && parsed[1] == 254)
        || is_mask_shaped)
}

/// Whether any dot/digit token in `text` is a routable IPv4 literal. Used to read `ipconfig`-style
/// output without depending on its (localized) labels; subnet masks and APIPA never match.
pub fn text_has_routable_ipv4(text: &str) -> bool {
    text.split(|character: char| !(character.is_ascii_digit() || character == '.'))
        .any(is_routable_ipv4)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_personal_profile() -> String {
        "<?xml version=\"1.0\"?>\
         <WLANProfile xmlns=\"http://www.microsoft.com/networking/WLAN/profile/v1\">\
         <name>HomeNet</name>\
         <SSIDConfig><SSID><name>HomeNet</name></SSID></SSIDConfig>\
         <connectionType>ESS</connectionType><connectionMode>auto</connectionMode>\
         <MSM><security><authEncryption>\
         <authentication>WPA2PSK</authentication><encryption>AES</encryption>\
         <useOneX>false</useOneX></authEncryption>\
         <sharedKey><keyType>passPhrase</keyType><protected>false</protected>\
         <keyMaterial>secret-pass</keyMaterial></sharedKey>\
         </security></MSM></WLANProfile>"
            .to_owned()
    }

    #[test]
    fn payload_round_trips_with_profiles() {
        let entries = vec![
            WifiProfileEntry::new("HomeNet", sample_personal_profile()),
            WifiProfileEntry::new("Café Wi-Fi", "<WLANProfile/>"),
        ];
        let nonce = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let payload = PeNetworkPayload::new(nonce, entries).unwrap();
        let bytes = payload.to_bytes();
        let parsed = PeNetworkPayload::parse(&bytes).unwrap();
        assert_eq!(parsed.nonce, nonce);
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(parsed.entries[0].ssid, "HomeNet");
        assert_eq!(parsed.entries[1].ssid, "Café Wi-Fi");
        assert_eq!(
            parsed.entries[0].profile_xml.as_str(),
            sample_personal_profile()
        );
    }

    #[test]
    fn empty_payload_round_trips() {
        let nonce = "a".repeat(64);
        let payload = PeNetworkPayload::new(nonce.clone(), Vec::new()).unwrap();
        let bytes = payload.to_bytes();
        let parsed = PeNetworkPayload::parse(&bytes).unwrap();
        assert_eq!(parsed.nonce, nonce);
        assert!(parsed.entries.is_empty());
    }

    #[test]
    fn binding_detects_tampering() {
        let bytes = b"[PeNetwork]\r\nVersion=1\r\n";
        let binding = PeNetworkPayloadBinding::from_bytes(bytes).unwrap();
        assert!(binding.verify(bytes).is_ok());
        assert!(binding.verify(b"[PeNetwork]\r\nVersion=2\r\n").is_err());
    }

    #[test]
    fn binding_parses_all_or_nothing() {
        let lines = "PeNetworkPayloadLength=12\r\n\
                     PeNetworkPayloadSha256=\
                     0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\r\n";
        let binding = PeNetworkPayloadBinding::from_config_text(lines)
            .unwrap()
            .unwrap();
        assert_eq!(binding.length_bytes, 12);
        assert!(PeNetworkPayloadBinding::from_config_text("")
            .unwrap()
            .is_none());
        assert!(
            PeNetworkPayloadBinding::from_config_text("PeNetworkPayloadLength=12\r\n").is_err()
        );
    }

    #[test]
    fn policy_requires_exactly_one_true() {
        assert!(policy_enabled("PeNetworkEnabled=true\r\n"));
        assert!(!policy_enabled("PeNetworkEnabled=false\r\n"));
        assert!(!policy_enabled("Language=zh-CN\r\n"));
        assert!(!policy_enabled(
            "PeNetworkEnabled=true\r\nPeNetworkEnabled=true\r\n"
        ));
    }

    #[test]
    fn candidates_add_minimal_and_downgrade_variants() {
        let candidates = wifi_profile_candidates(&sample_personal_profile());
        assert!(candidates.len() >= 2);
        assert_eq!(candidates[0], sample_personal_profile());
        assert!(candidates[1].contains("<authentication>WPA2PSK</authentication>"));
        assert!(candidates[1].contains("<keyMaterial>secret-pass</keyMaterial>"));
    }

    #[test]
    fn wpa3_downgrades_to_wpa2() {
        let profile = sample_personal_profile().replace("WPA2PSK", "WPA3SAE");
        let candidates = wifi_profile_candidates(&profile);
        assert!(candidates
            .iter()
            .any(|xml| xml.contains("<authentication>WPA3SAE</authentication>")));
        assert!(candidates
            .iter()
            .any(|xml| xml.contains("<authentication>WPA2PSK</authentication>")));
    }

    #[test]
    fn open_network_keeps_open_authentication() {
        let profile = "<?xml version=\"1.0\"?>\
             <WLANProfile xmlns=\"http://www.microsoft.com/networking/WLAN/profile/v1\">\
             <name>Guest</name>\
             <SSIDConfig><SSID><name>Guest</name></SSID></SSIDConfig>\
             <connectionType>ESS</connectionType>\
             <MSM><security><authEncryption>\
             <authentication>open</authentication><encryption>none</encryption>\
             <useOneX>false</useOneX></authEncryption></security></MSM></WLANProfile>";
        let candidates = wifi_profile_candidates(profile);
        assert!(candidates
            .iter()
            .any(|xml| xml.contains("<authentication>open</authentication>")));
    }

    #[test]
    fn enterprise_profile_keeps_only_original() {
        let profile = "<?xml version=\"1.0\"?>\
             <WLANProfile xmlns=\"http://www.microsoft.com/networking/WLAN/profile/v1\">\
             <name>Corp</name>\
             <SSIDConfig><SSID><name>Corp</name></SSID></SSIDConfig>\
             <MSM><security><authEncryption>\
             <authentication>WPA2</authentication><encryption>AES</encryption>\
             <useOneX>true</useOneX></authEncryption>\
             <OneX xmlns=\"http://www.microsoft.com/networking/OneX/v1\"/>\
             </security></MSM></WLANProfile>";
        let candidates = wifi_profile_candidates(profile);
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn detects_net_class_inf() {
        let inf = "[Version]\r\nClass=Net\r\nClassGuid={4d36e972-e325-11ce-bfc1-08002be10318}\r\n";
        assert!(inf_text_declares_net_class(inf));
        assert!(!inf_text_declares_net_class(
            "[Version]\r\nClass=Display\r\n"
        ));
    }

    #[test]
    fn routable_ipv4_filters_local_addresses() {
        assert!(is_routable_ipv4("192.168.1.20"));
        assert!(is_routable_ipv4("10.0.0.5"));
        assert!(!is_routable_ipv4("169.254.1.1"));
        assert!(!is_routable_ipv4("127.0.0.1"));
        assert!(!is_routable_ipv4("0.0.0.0"));
        assert!(!is_routable_ipv4("256.1.1.1"));
        assert!(!is_routable_ipv4("1.2.3"));
        assert!(!is_routable_ipv4("255.255.0.0"));
        assert!(!is_routable_ipv4("255.255.255.0"));
        assert!(!is_routable_ipv4("128.0.0.0"));
        assert!(!is_routable_ipv4("224.0.0.251"));
        assert!(text_has_routable_ipv4(
            "   IPv4 Address. . . . : 192.168.0.42(Preferred)"
        ));
        assert!(!text_has_routable_ipv4(
            "Autoconfiguration IPv4 Address: 169.254.9.9"
        ));
    }

    #[test]
    fn apipa_block_with_subnet_mask_is_not_connectivity() {
        let apipa = "Ethernet adapter Ethernet:\r\n\
                     Autoconfiguration IPv4 Address. . : 169.254.9.9\r\n\
                     Subnet Mask . . . . . . . . . . . : 255.255.0.0\r\n\
                     Default Gateway . . . . . . . . . :\r\n";
        assert!(!text_has_routable_ipv4(apipa));
        let dhcp = "IPv4 Address. . . . . . . . . . . : 192.168.1.20\r\n\
                    Subnet Mask . . . . . . . . . . . : 255.255.255.0\r\n\
                    Default Gateway . . . . . . . . . : 192.168.1.1\r\n";
        assert!(text_has_routable_ipv4(dhcp));
    }

    #[test]
    fn decodes_utf16_and_ansi_inf_text() {
        let text = "[Version]\r\nClass=Net\r\n";
        let mut with_bom = vec![0xFF, 0xFE];
        let mut without_bom = Vec::new();
        for unit in text.encode_utf16() {
            with_bom.extend_from_slice(&unit.to_le_bytes());
            without_bom.extend_from_slice(&unit.to_le_bytes());
        }
        assert!(inf_text_declares_net_class(&decode_inf_text(&with_bom)));
        assert!(inf_text_declares_net_class(&decode_inf_text(&without_bom)));
        let ansi = b"[Version]\r\nClass=Net\r\n; \xC4\xE3\xBA\xC3\r\n";
        assert!(inf_text_declares_net_class(&decode_inf_text(ansi)));
        let utf8_bom = b"\xEF\xBB\xBF[Version]\r\nClass=Net\r\n";
        assert!(inf_text_declares_net_class(&decode_inf_text(utf8_bom)));
    }

    fn driver_fixture() -> crate::scoped_temp_file::ScopedTempDir {
        use crate::scoped_temp_file::ScopedTempDir;
        let parent = std::env::temp_dir();
        let directory = ScopedTempDir::create_in(&parent, "lr-pe-network-driver-test").unwrap();
        let package = directory.path().join("package0");
        std::fs::create_dir(&package).unwrap();
        std::fs::write(package.join("netwifi.inf"), b"[Version]\r\nClass=Net").unwrap();
        std::fs::write(package.join("netwifi.sys"), [7_u8; 4096]).unwrap();
        std::fs::write(package.join("netwifi.cat"), b"catalog").unwrap();
        directory
    }

    #[test]
    fn driver_manifest_round_trips_through_the_payload_and_verifies() {
        let directory = driver_fixture();
        let manifest = driver_tree_manifest(directory.path()).unwrap();
        assert_eq!(manifest.len(), 3);
        let has_inf = manifest
            .iter()
            .any(|file| file.relative_path == "package0\\netwifi.inf");
        assert!(has_inf);
        verify_driver_tree(directory.path(), &manifest).unwrap();
        let payload = PeNetworkPayload::new("nonce", Vec::new())
            .unwrap()
            .with_drivers(manifest.clone())
            .unwrap();
        let parsed = PeNetworkPayload::parse(&payload.to_bytes()).unwrap();
        assert_eq!(parsed.drivers, manifest);
        assert!(parsed.entries.is_empty());
    }

    #[test]
    fn payload_without_drivers_keeps_the_original_layout() {
        let payload = PeNetworkPayload::new("nonce", Vec::new()).unwrap();
        let bytes = payload.to_bytes();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(!text.contains("Driver"));
        let parsed = PeNetworkPayload::parse(&bytes).unwrap();
        assert!(parsed.drivers.is_empty());
    }

    #[test]
    fn driver_tree_verification_rejects_tampering_and_extra_files() {
        let directory = driver_fixture();
        let manifest = driver_tree_manifest(directory.path()).unwrap();
        let sys = directory.path().join("package0").join("netwifi.sys");
        std::fs::write(&sys, [8_u8; 4096]).unwrap();
        assert!(verify_driver_tree(directory.path(), &manifest).is_err());
        std::fs::write(&sys, [7_u8; 4096]).unwrap();
        verify_driver_tree(directory.path(), &manifest).unwrap();
        std::fs::write(directory.path().join("extra.dll"), b"x").unwrap();
        assert!(verify_driver_tree(directory.path(), &manifest).is_err());
    }

    #[test]
    fn driver_paths_reject_traversal_and_aliasing_components() {
        assert!(validate_driver_relative_path("package0\\netwifi.inf").is_ok());
        let bad_paths = [
            "",
            "\\netwifi.inf",
            "..\\netwifi.inf",
            "package0\\..\\netwifi.inf",
            "C:\\netwifi.inf",
            "package0\\\\netwifi.inf",
            "package0/netwifi.inf",
            "netwifi.inf:stream",
            "package0.\\netwifi.inf",
        ];
        for bad in bad_paths {
            assert!(validate_driver_relative_path(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn payload_rejects_duplicate_or_oversized_driver_manifests() {
        let sha = "ab".repeat(32);
        let file = DriverFileEntry::new("package0\\netwifi.sys", 10, &sha).unwrap();
        let upper = DriverFileEntry::new("PACKAGE0\\NETWIFI.SYS", 10, &sha).unwrap();
        let duplicate = PeNetworkPayload::new("nonce", Vec::new())
            .unwrap()
            .with_drivers(vec![file, upper]);
        assert!(duplicate.is_err());
        let huge = DriverFileEntry::new("package0\\big.bin", MAX_DRIVER_TREE_BYTES + 1, &sha);
        let oversized = PeNetworkPayload::new("nonce", Vec::new())
            .unwrap()
            .with_drivers(vec![huge.unwrap()]);
        assert!(oversized.is_err());
        assert!(DriverFileEntry::new("package0\\netwifi.sys", 10, "not-a-hash").is_err());
    }

    #[test]
    fn handoff_validator_accepts_network_binding_keys() {
        let ini = "[Install]\r\n\
                   PeNetworkEnabled=true\r\n\
                   PeNetworkPayloadLength=42\r\n\
                   PeNetworkPayloadSha256=\
                   0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\r\n";
        assert!(crate::install_handoff::validate_install_handoff_ini(ini).is_ok());
    }
}
