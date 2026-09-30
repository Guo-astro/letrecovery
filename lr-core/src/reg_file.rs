//! Offline conversion of `.reg` files for LetRecovery's mounted hives (`pc-soft`, `pc-sys`,
//! `pc-default`). Shared by the desktop direct-install path and the WinPE path.
//!
//! Regedit exports are UTF-16LE with a BOM; hand-written and `REGEDIT4` files are UTF-8 or the
//! system ANSI code page. Only ASCII key-path prefixes are rewritten, so ANSI bytes, value data
//! and line endings are preserved exactly.

const OFFLINE_HIVE_MAP: &[(&str, &str)] = &[
    (
        "HKEY_LOCAL_MACHINE\\SYSTEM\\CurrentControlSet",
        "HKEY_LOCAL_MACHINE\\pc-sys\\ControlSet001",
    ),
    (
        "HKLM\\SYSTEM\\CurrentControlSet",
        "HKEY_LOCAL_MACHINE\\pc-sys\\ControlSet001",
    ),
    (
        "HKEY_LOCAL_MACHINE\\SOFTWARE",
        "HKEY_LOCAL_MACHINE\\pc-soft",
    ),
    ("HKLM\\SOFTWARE", "HKEY_LOCAL_MACHINE\\pc-soft"),
    ("HKEY_LOCAL_MACHINE\\SYSTEM", "HKEY_LOCAL_MACHINE\\pc-sys"),
    ("HKLM\\SYSTEM", "HKEY_LOCAL_MACHINE\\pc-sys"),
    ("HKEY_CURRENT_USER", "HKEY_LOCAL_MACHINE\\pc-default"),
    ("HKCU", "HKEY_LOCAL_MACHINE\\pc-default"),
    ("HKEY_USERS\\.DEFAULT", "HKEY_LOCAL_MACHINE\\pc-default"),
    ("HKEY_CLASSES_ROOT", "HKEY_LOCAL_MACHINE\\pc-soft\\Classes"),
    ("HKCR", "HKEY_LOCAL_MACHINE\\pc-soft\\Classes"),
];

fn convert_key_lines(content: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(content.len() + 256);
    for line in content.split_inclusive(|byte| *byte == b'\n') {
        let indent = line
            .iter()
            .take_while(|byte| **byte == b' ' || **byte == b'\t')
            .count();
        let trimmed = &line[indent..];
        let prefix = if trimmed.starts_with(b"[-") {
            2
        } else if trimmed.starts_with(b"[") {
            1
        } else {
            output.extend_from_slice(line);
            continue;
        };
        let body = &trimmed[prefix..];
        let mut replaced = false;
        for (from, to) in OFFLINE_HIVE_MAP {
            let from = from.as_bytes();
            if body.len() >= from.len()
                && body[..from.len()].eq_ignore_ascii_case(from)
                && matches!(
                    body.get(from.len()).copied(),
                    None | Some(b'\\') | Some(b']')
                )
            {
                output.extend_from_slice(&line[..indent + prefix]);
                output.extend_from_slice(to.as_bytes());
                output.extend_from_slice(&body[from.len()..]);
                replaced = true;
                break;
            }
        }
        if !replaced {
            output.extend_from_slice(line);
        }
    }
    output
}

fn decode_utf16(body: &[u8], little_endian: bool) -> String {
    let units = body
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

/// Convert a live-registry `.reg` file to the offline hive mount names. UTF-16 input is written
/// back as UTF-16LE with a BOM; any other input keeps its original byte encoding.
pub fn convert_reg_file_for_offline_hives(bytes: &[u8]) -> Vec<u8> {
    let text = if let Some(body) = bytes.strip_prefix(&[0xFF, 0xFE][..]) {
        decode_utf16(body, true)
    } else if let Some(body) = bytes.strip_prefix(&[0xFE, 0xFF][..]) {
        decode_utf16(body, false)
    } else {
        return convert_key_lines(bytes);
    };
    let converted = convert_key_lines(text.as_bytes());
    let converted = String::from_utf8_lossy(&converted);
    let mut output = vec![0xFF, 0xFE];
    for unit in converted.encode_utf16() {
        output.extend_from_slice(&unit.to_le_bytes());
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_only_key_prefixes_case_insensitively() {
        let input = b"REGEDIT4\r\n\r\n[HKEY_LOCAL_MACHINE\\Software\\Test]\r\n\"Path\"=\"HKEY_LOCAL_MACHINE\\\\SOFTWARE\"\r\n[-HKCU\\Old]\r\n[HKEY_LOCAL_MACHINE\\SYSTEM\\CurrentControlSet\\Services\\x]\r\n";
        let output = String::from_utf8(convert_reg_file_for_offline_hives(input)).unwrap();
        assert!(output.contains("[HKEY_LOCAL_MACHINE\\pc-soft\\Test]\r\n"));
        assert!(output.contains("\"Path\"=\"HKEY_LOCAL_MACHINE\\\\SOFTWARE\""));
        assert!(output.contains("[-HKEY_LOCAL_MACHINE\\pc-default\\Old]"));
        assert!(output.contains("[HKEY_LOCAL_MACHINE\\pc-sys\\ControlSet001\\Services\\x]"));
    }

    #[test]
    fn utf16_exports_stay_utf16() {
        let text = "Windows Registry Editor Version 5.00\r\n\r\n[HKEY_CURRENT_USER\\Test]\r\n";
        let mut input = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            input.extend_from_slice(&unit.to_le_bytes());
        }
        let output = convert_reg_file_for_offline_hives(&input);
        assert_eq!(&output[..2], &[0xFF, 0xFE]);
        let decoded = decode_utf16(&output[2..], true);
        assert!(decoded.contains("[HKEY_LOCAL_MACHINE\\pc-default\\Test]"));
    }
}
