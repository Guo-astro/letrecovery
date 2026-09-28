//! Minimal read-only probe for the setup class declared in an INF `[Version]` section.
//!
//! Used to keep optional third-party display packages away from pre-release Windows builds whose
//! graphics stack has not been validated. Any read or decode problem reports "not display", so
//! callers keep their historical behaviour.

use std::path::Path;

/// `Display` setup class GUID.
pub const DISPLAY_CLASS_GUID: &str = "{4d36e968-e325-11ce-bfc1-08002be10318}";

const MAX_INF_BYTES: u64 = 16 * 1024 * 1024;

fn decode_utf16(bytes: &[u8], little_endian: bool) -> String {
    let units = bytes
        .chunks_exact(2)
        .map(|pair| {
            if little_endian {
                u16::from_le_bytes([pair[0], pair[1]])
            } else {
                u16::from_be_bytes([pair[0], pair[1]])
            }
        })
        .collect::<Vec<_>>();
    String::from_utf16_lossy(&units)
}

/// Decode INF text: UTF-16 with BOM, BOM-less UTF-16LE, UTF-8 (with or without BOM) or ANSI.
pub fn decode_inf_text(bytes: &[u8]) -> String {
    if let Some(body) = bytes.strip_prefix(&[0xFF, 0xFE][..]) {
        return decode_utf16(body, true);
    }
    if let Some(body) = bytes.strip_prefix(&[0xFE, 0xFF][..]) {
        return decode_utf16(body, false);
    }
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes);
    let sample = &body[..body.len().min(4096)];
    if sample.len() >= 8 {
        let odd_zero = sample.iter().skip(1).step_by(2).filter(|byte| **byte == 0).count();
        let even_zero = sample.iter().step_by(2).filter(|byte| **byte == 0).count();
        if odd_zero * 4 >= sample.len() && even_zero * 16 < sample.len() {
            return decode_utf16(body, true);
        }
    }
    String::from_utf8_lossy(body).into_owned()
}

/// Returns `true` only when the INF `[Version]` section declares the `Display` class.
pub fn inf_text_declares_display_class(text: &str) -> bool {
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
            "class" if value.eq_ignore_ascii_case("display") => return true,
            "classguid" if value.eq_ignore_ascii_case(DISPLAY_CLASS_GUID) => return true,
            _ => {}
        }
    }
    false
}

/// Reads an INF file and reports whether it is a `Display` class package.
pub fn inf_declares_display_class(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() || metadata.len() > MAX_INF_BYTES {
        return false;
    }
    match std::fs::read(path) {
        Ok(bytes) => inf_text_declares_display_class(&decode_inf_text(&bytes)),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_display_class_by_name_or_guid() {
        assert!(inf_text_declares_display_class(
            "[Version]\r\nSignature=\"$Windows NT$\"\r\nClass=Display\r\nClassGUID={4D36E968-E325-11CE-BFC1-08002BE10318}\r\n"
        ));
        assert!(inf_text_declares_display_class(
            "[version]\nClassGuid = \"{4d36e968-e325-11ce-bfc1-08002be10318}\" ; display\n"
        ));
    }

    #[test]
    fn other_classes_and_other_sections_are_not_display() {
        assert!(!inf_text_declares_display_class(
            "[Version]\nClass=SCSIAdapter\nClassGuid={4D36E97B-E325-11CE-BFC1-08002BE10318}\n[Strings]\nClass=Display\n"
        ));
        assert!(!inf_text_declares_display_class("Class=Display\n"));
    }

    #[test]
    fn decodes_utf16_inf_text() {
        let text = "[Version]\r\nClass=Display\r\n";
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        assert!(inf_text_declares_display_class(&decode_inf_text(&bytes)));
        let without_bom = bytes[2..].to_vec();
        assert!(inf_text_declares_display_class(&decode_inf_text(&without_bom)));
    }
}
