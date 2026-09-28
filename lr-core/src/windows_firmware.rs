//! Documented firmware-mode probe shared by the desktop and WinPE endpoints.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareType {
    Bios,
    Uefi,
}

fn firmware_type_from_raw(raw_type: u32) -> anyhow::Result<FirmwareType> {
    match raw_type {
        1 => Ok(FirmwareType::Bios),
        2 => Ok(FirmwareType::Uefi),
        value => anyhow::bail!("unsupported firmware type {value}"),
    }
}

#[cfg(windows)]
pub fn detect_firmware_type() -> anyhow::Result<FirmwareType> {
    use anyhow::{bail, Context};
    use libloading::{Library, Symbol};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, SetLastError, ERROR_INVALID_FUNCTION, ERROR_NOT_ALL_ASSIGNED,
        ERROR_SUCCESS, HANDLE,
    };
    use windows::Win32::Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
        SE_SYSTEM_ENVIRONMENT_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::Win32::System::WindowsProgramming::GetFirmwareEnvironmentVariableW;

    type GetFirmwareTypeFn = unsafe extern "system" fn(*mut u32) -> i32;

    struct TokenHandle(HANDLE);
    impl Drop for TokenHandle {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    unsafe fn detect_with_get_firmware_type() -> anyhow::Result<Option<FirmwareType>> {
        // GetFirmwareType is available only on Windows 8+. Resolve it dynamically so the
        // executable still starts on supported Windows 7 systems.
        let kernel32 = Library::new("kernel32.dll").context("LoadLibraryW(kernel32.dll) failed")?;
        let function: Symbol<'_, GetFirmwareTypeFn> = match kernel32.get(b"GetFirmwareType\0") {
            Ok(function) => function,
            Err(_) => return Ok(None),
        };
        let mut raw_type = 0_u32;
        SetLastError(ERROR_SUCCESS);
        if function(&mut raw_type) == 0 {
            bail!(
                "GetFirmwareType failed: {}",
                std::io::Error::last_os_error()
            );
        }
        firmware_type_from_raw(raw_type)
            .map(Some)
            .context("GetFirmwareType returned an invalid result")
    }

    unsafe fn enable_system_environment_privilege() -> anyhow::Result<TokenHandle> {
        let mut token = HANDLE::default();
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
        .context("OpenProcessToken failed for firmware probe")?;
        let token = TokenHandle(token);
        let mut luid = Default::default();
        LookupPrivilegeValueW(PCWSTR::null(), SE_SYSTEM_ENVIRONMENT_NAME, &mut luid)
            .context("LookupPrivilegeValueW(SeSystemEnvironmentPrivilege) failed")?;
        let privileges = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        SetLastError(ERROR_SUCCESS);
        AdjustTokenPrivileges(token.0, false, Some(&privileges), 0, None, None)
            .context("AdjustTokenPrivileges(SeSystemEnvironmentPrivilege) failed")?;
        if GetLastError() == ERROR_NOT_ALL_ASSIGNED {
            bail!("the process token does not contain SeSystemEnvironmentPrivilege");
        }
        Ok(token)
    }

    unsafe {
        match detect_with_get_firmware_type() {
            Ok(Some(firmware_type)) => return Ok(firmware_type),
            Ok(None) => {}
            Err(error) => log::warn!(
                "[FIRMWARE] GetFirmwareType did not return a usable result ({error:#}); trying the documented fallbacks"
            ),
        }
    }

    // Microsoft documents this dummy empty-name/zero-GUID call as the
    // pre-Windows 8 firmware-type test. Enable the documented privilege first:
    // without it some updated systems return a permission error that cannot be
    // distinguished safely from the expected UEFI namespace error.
    let empty_name = [0_u16];
    let zero_guid: Vec<u16> = "{00000000-0000-0000-0000-000000000000}"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let probe = unsafe {
        match enable_system_environment_privilege() {
            Ok(_privilege) => {
                SetLastError(ERROR_SUCCESS);
                let result = GetFirmwareEnvironmentVariableW(
                    PCWSTR(empty_name.as_ptr()),
                    PCWSTR(zero_guid.as_ptr()),
                    None,
                    0,
                );
                let status = GetLastError();
                if result != 0 {
                    Ok(FirmwareType::Uefi)
                } else if status == ERROR_INVALID_FUNCTION {
                    Ok(FirmwareType::Bios)
                } else if status == ERROR_SUCCESS {
                    Err(anyhow::anyhow!(
                        "firmware probe returned zero without a Win32 error"
                    ))
                } else {
                    Ok(FirmwareType::Uefi)
                }
            }
            Err(error) => Err(error),
        }
    };
    match probe {
        Ok(firmware_type) => Ok(firmware_type),
        Err(probe_error) => {
            // WinPE and current Windows publish the boot firmware type here (1 = BIOS,
            // 2 = UEFI). It is used only when both documented API probes were inconclusive,
            // for example on systems whose token lacks SeSystemEnvironmentPrivilege.
            match crate::registry::OfflineRegistry::query_dword_optional(
                "HKLM\\SYSTEM\\CurrentControlSet\\Control",
                "PEFirmwareType",
            ) {
                Ok(Some(raw)) => firmware_type_from_raw(raw).map_err(|_| probe_error),
                _ => Err(probe_error),
            }
        }
    }
}

#[cfg(not(windows))]
pub fn detect_firmware_type() -> anyhow::Result<FirmwareType> {
    anyhow::bail!("Windows firmware APIs are unavailable on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_documented_get_firmware_type_values_only() {
        assert_eq!(firmware_type_from_raw(1).unwrap(), FirmwareType::Bios);
        assert_eq!(firmware_type_from_raw(2).unwrap(), FirmwareType::Uefi);
        assert!(firmware_type_from_raw(0).is_err());
        assert!(firmware_type_from_raw(3).is_err());
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "reads the host firmware boot mode; run explicitly on a test machine"]
    fn live_probe_returns_a_documented_firmware_mode() {
        assert!(matches!(
            detect_firmware_type().unwrap(),
            FirmwareType::Bios | FirmwareType::Uefi
        ));
    }
}
