//! Windows build-number policy shared by the desktop and WinPE endpoints.
//!
//! LetRecovery keeps installing Windows builds that were published after a release was
//! validated (Insider Dev/Beta/Experimental and "Future Platforms" flights such as 29671).
//! Behaviour that depends on undocumented or retired Windows internals is limited to the
//! validated ranges below; an unknown newer build takes the conservative path and the
//! installation continues instead of failing.

/// Newest client build family that has been validated end-to-end (the 28000 / 26H1 family).
pub const NEWEST_VALIDATED_CLIENT_BUILD: u32 = 28_999;

/// Microsoft retired the `BypassNRO` (`oobe\bypassnro`) local-account path in OOBE starting with
/// Insider build 26220.6772. Newer Dev/Beta/Experimental and Future Platforms OOBE either ignores
/// the value or routes it back to the Microsoft-account flow, which can leave setup looping on
/// "Just a moment". The built-in answer file creates the local account through the supported
/// `UserAccounts` path, so the legacy value is not written there.
pub const BYPASS_NRO_RETIRED_FROM_BUILD: u32 = 26_220;

/// First build of the 28000 (26H1) family, which was validated with the legacy value present.
const VALIDATED_26H1_FIRST_BUILD: u32 = 28_000;

/// `true` for client builds newer than every validated build family.
pub fn is_future_client_build(build: u32) -> bool {
    build > NEWEST_VALIDATED_CLIENT_BUILD
}

/// `true` where OOBE still honours the legacy `BypassNRO` value. Validated builds keep their
/// historical behaviour.
pub fn bypass_nro_is_honored(build: u32) -> bool {
    build < BYPASS_NRO_RETIRED_FROM_BUILD
        || (VALIDATED_26H1_FIRST_BUILD..=NEWEST_VALIDATED_CLIENT_BUILD).contains(&build)
}

/// Decide whether `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\OOBE\BypassNRO` is written.
/// An unreadable target version keeps the historical behaviour.
pub fn should_write_bypass_nro(target_build: Option<u32>) -> bool {
    target_build.is_none_or(bypass_nro_is_honored)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn future_platform_builds_take_the_conservative_path() {
        assert!(!is_future_client_build(26_100));
        assert!(!is_future_client_build(28_000));
        assert!(is_future_client_build(29_671));
    }

    #[test]
    fn bypass_nro_is_written_only_where_oobe_still_honours_it() {
        for build in [19_045, 22_621, 26_100, 26_200, 28_000] {
            assert!(should_write_bypass_nro(Some(build)), "{build}");
        }
        for build in [26_220, 26_300, 27_000, 29_671] {
            assert!(!should_write_bypass_nro(Some(build)), "{build}");
        }
        assert!(should_write_bypass_nro(None));
    }
}
