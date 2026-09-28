//! Pure policy for choosing the temporary data volume used by a ViaPE install.
//!
//! This module never probes disks and never executes DiskPart.  Callers provide a fresh inventory
//! from the current boot session, then execute a returned shrink plan only after revalidating the
//! exact target volume and physical disk.

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
pub const STAGING_OPERATIONAL_HEADROOM_BYTES: u64 = 2 * GIB;

/// Logical-byte budget for the ViaPE preparation workflow's data volume.
///
/// The caller measures every component from its existing source. A producer such as DISM may
/// legally materialize a different package tree than its read-only Driver Store inventory; after
/// that required producer runs, the caller replaces the provisional component with the observed
/// logical bytes and rechecks the same budget. No component is copied merely to discover its size.
/// The fixed 2 GiB headroom is added once by [`required_staging_bytes`] for filesystem allocation
/// rounding, the bounded handoff log/config, and transactional metadata.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StagingPayloadBudget {
    pub image_bytes: u64,
    pub exported_driver_bytes: u64,
    pub pca_bytes: u64,
    pub user_driver_bytes: u64,
    pub uefiseven_bytes: u64,
    /// Exact bytes of already downloaded, user-selected unattended installers.
    pub preinstalled_software_bytes: u64,
}

impl StagingPayloadBudget {
    pub fn payload_bytes(self) -> Option<u64> {
        self.image_bytes
            .checked_add(self.exported_driver_bytes)?
            .checked_add(self.pca_bytes)?
            .checked_add(self.user_driver_bytes)?
            .checked_add(self.uefiseven_bytes)?
            .checked_add(self.preinstalled_software_bytes)
    }

    pub fn required_bytes(self) -> Option<u64> {
        required_staging_bytes(self.payload_bytes()?)
    }

    /// Remaining free bytes required after `materialized_payload_bytes` from this same budget are
    /// already present on the selected volume. This preserves the one fixed operational headroom
    /// instead of silently consuming it when an authoritative producer exceeds its preflight
    /// inventory.
    pub fn remaining_required_bytes_after(self, materialized_payload_bytes: u64) -> Option<u64> {
        self.required_bytes()?
            .checked_sub(materialized_payload_bytes)
    }

    /// Remaining payload after `materialized_payload_bytes` is already present on the selected
    /// volume. The fixed headroom is allocated with the partition and is not required again after
    /// a producer materializes part of the payload.
    pub fn remaining_payload_bytes_after(self, materialized_payload_bytes: u64) -> Option<u64> {
        self.payload_bytes()?
            .checked_sub(materialized_payload_bytes)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageMedia {
    SolidState,
    Rotational,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageAttachment {
    Internal,
    External,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StagingCandidate {
    pub letter: char,
    pub disk_number: Option<u32>,
    pub media: StorageMedia,
    pub attachment: StorageAttachment,
    pub free_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShrinkCandidate {
    pub letter: char,
    pub disk_number: Option<u32>,
    pub media: StorageMedia,
    pub attachment: StorageAttachment,
    pub free_bytes: u64,
    /// Set only after the caller confirms NTFS/basic-volume and a stable BitLocker state. Windows
    /// permits shrinking the currently unlocked system volume without first decrypting it.
    pub shrink_is_safe: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StagingPlan {
    Existing {
        letter: char,
        required_bytes: u64,
    },
    ShrinkTarget {
        letter: char,
        size_mb: u64,
        required_bytes: u64,
    },
    Unavailable {
        required_bytes: u64,
    },
}

/// Adds exactly 2 GiB of operational headroom to already-complete payload accounting.
pub fn required_staging_bytes(payload_bytes: u64) -> Option<u64> {
    payload_bytes.checked_add(STAGING_OPERATIONAL_HEADROOM_BYTES)
}

pub fn select_staging_plan(
    payload_bytes: u64,
    target_disk_number: Option<u32>,
    candidates: &[StagingCandidate],
    shrink_target: Option<ShrinkCandidate>,
) -> StagingPlan {
    let Some(required_bytes) = required_staging_bytes(payload_bytes) else {
        return StagingPlan::Unavailable {
            required_bytes: u64::MAX,
        };
    };
    let best_existing = candidates
        .iter()
        .copied()
        .filter(|candidate| candidate_has_room(*candidate, required_bytes))
        .max_by_key(|candidate| candidate_rank(*candidate, target_disk_number, required_bytes));

    let safe_shrink = shrink_target.filter(|target| shrink_has_room(*target, required_bytes));
    let prefer_shrink = match (best_existing, safe_shrink) {
        (None, Some(_)) => true,
        (Some(existing), Some(target)) => should_prefer_fixed_shrink(existing, target),
        _ => false,
    };

    if prefer_shrink {
        let target = safe_shrink.expect("prefer_shrink requires a safe target");
        return StagingPlan::ShrinkTarget {
            letter: target.letter,
            size_mb: required_bytes.div_ceil(MIB),
            required_bytes,
        };
    }

    if let Some(existing) = best_existing {
        return StagingPlan::Existing {
            letter: existing.letter,
            required_bytes,
        };
    }

    if let Some(target) = safe_shrink {
        return StagingPlan::ShrinkTarget {
            letter: target.letter,
            size_mb: required_bytes.div_ceil(MIB),
            required_bytes,
        };
    }

    StagingPlan::Unavailable { required_bytes }
}

fn candidate_has_room(candidate: StagingCandidate, required_bytes: u64) -> bool {
    candidate.free_bytes >= required_bytes
}

fn shrink_has_room(target: ShrinkCandidate, required_bytes: u64) -> bool {
    // QueryMaxReclaimableBytes is only an estimate and Microsoft explicitly documents that it may
    // exceed what Shrink can reclaim. Keeping that estimate in this pure selection data structure
    // invites it to become a false hard gate. Current free space is a necessary capacity bound;
    // the real VDS Shrink call and its post-operation extent readback remain authoritative.
    target.shrink_is_safe && target.free_bytes >= required_bytes
}

fn candidate_rank(
    candidate: StagingCandidate,
    target_disk_number: Option<u32>,
    required_bytes: u64,
) -> (u16, u8, u64, std::cmp::Reverse<char>) {
    let media_score = match (candidate.attachment, candidate.media) {
        (StorageAttachment::Internal, StorageMedia::SolidState) => 600,
        (StorageAttachment::Internal, StorageMedia::Unknown) => 450,
        (StorageAttachment::Internal, StorageMedia::Rotational) => 350,
        (StorageAttachment::Unknown, StorageMedia::SolidState) => 325,
        (StorageAttachment::Unknown, StorageMedia::Unknown) => 275,
        (StorageAttachment::Unknown, StorageMedia::Rotational) => 250,
        (StorageAttachment::External, StorageMedia::SolidState) => 225,
        (StorageAttachment::External, StorageMedia::Unknown) => 175,
        (StorageAttachment::External, StorageMedia::Rotational) => 150,
    };
    let different_disk = u8::from(
        candidate.disk_number.is_some()
            && target_disk_number.is_some()
            && candidate.disk_number != target_disk_number,
    );
    let remaining = candidate.free_bytes.saturating_sub(required_bytes);
    // `max_by_key()` uses the final tuple item as the last tie-breaker. `Reverse` keeps a stable
    // preference for the earlier drive letter without relying on scan order.
    (
        media_score,
        different_disk,
        remaining,
        std::cmp::Reverse(candidate.letter.to_ascii_uppercase()),
    )
}

fn should_prefer_fixed_shrink(existing: StagingCandidate, target: ShrinkCandidate) -> bool {
    // An already suitable fixed/internal volume is preferred regardless of payload size. A former
    // 8-GiB threshold abruptly introduced a destructive Shrink transaction for otherwise identical
    // valid inventories and did not describe any storage-provider capability. Keep only the stable
    // distinction that avoids depending on removable/external media across the PE reboot.
    target.attachment != StorageAttachment::External
        && existing.attachment == StorageAttachment::External
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gib(value: u64) -> u64 {
        value * GIB
    }

    fn candidate(
        letter: char,
        disk_number: u32,
        media: StorageMedia,
        attachment: StorageAttachment,
        free_gib: u64,
    ) -> StagingCandidate {
        StagingCandidate {
            letter,
            disk_number: Some(disk_number),
            media,
            attachment,
            free_bytes: gib(free_gib),
        }
    }

    fn shrink_target(media: StorageMedia, free_gib: u64) -> ShrinkCandidate {
        ShrinkCandidate {
            letter: 'C',
            disk_number: Some(1),
            media,
            attachment: StorageAttachment::Internal,
            free_bytes: gib(free_gib),
            shrink_is_safe: true,
        }
    }

    #[test]
    fn exact_payload_gets_one_fixed_two_gib_headroom() {
        assert_eq!(required_staging_bytes(gib(1)), Some(gib(3)));
        assert_eq!(required_staging_bytes(gib(20)), Some(gib(22)));
        assert_eq!(required_staging_bytes(gib(100)), Some(gib(102)));
        assert_eq!(required_staging_bytes(u64::MAX), None);
    }

    #[test]
    fn component_budget_counts_every_payload_once() {
        let budget = StagingPayloadBudget {
            image_bytes: gib(9),
            exported_driver_bytes: gib(4),
            pca_bytes: 128 * MIB,
            user_driver_bytes: 64 * MIB,
            uefiseven_bytes: 8 * MIB,
            preinstalled_software_bytes: 32 * MIB,
        };
        assert_eq!(budget.payload_bytes(), Some(gib(13) + 232 * MIB));
        assert_eq!(budget.required_bytes(), Some(gib(15) + 232 * MIB));
        assert_eq!(
            budget.remaining_required_bytes_after(gib(4) + 128 * MIB),
            Some(gib(11) + 104 * MIB)
        );
        assert_eq!(
            budget.remaining_payload_bytes_after(gib(4) + 128 * MIB),
            Some(gib(9) + 104 * MIB)
        );
        assert_eq!(budget.remaining_required_bytes_after(gib(16)), None);
    }

    #[test]
    fn existing_internal_ssd_wins_over_a_larger_hdd() {
        let candidates = [
            candidate(
                'D',
                0,
                StorageMedia::Rotational,
                StorageAttachment::Internal,
                500,
            ),
            candidate(
                'E',
                2,
                StorageMedia::SolidState,
                StorageAttachment::Internal,
                40,
            ),
        ];
        assert!(matches!(
            select_staging_plan(gib(6), Some(1), &candidates, None),
            StagingPlan::Existing { letter: 'E', .. }
        ));
    }

    #[test]
    fn small_image_uses_existing_hdd_instead_of_shrinking_system_ssd() {
        let hdd = candidate(
            'D',
            0,
            StorageMedia::Rotational,
            StorageAttachment::Internal,
            100,
        );
        assert!(matches!(
            select_staging_plan(
                gib(6),
                Some(1),
                &[hdd],
                Some(shrink_target(StorageMedia::SolidState, 100))
            ),
            StagingPlan::Existing { letter: 'D', .. }
        ));
    }

    #[test]
    fn large_image_reuses_suitable_internal_hdd_without_a_size_triggered_shrink() {
        let hdd = candidate(
            'D',
            0,
            StorageMedia::Rotational,
            StorageAttachment::Internal,
            100,
        );
        assert!(matches!(
            select_staging_plan(
                gib(20),
                Some(1),
                &[hdd],
                Some(shrink_target(StorageMedia::SolidState, 100))
            ),
            StagingPlan::Existing { letter: 'D', .. }
        ));
    }

    #[test]
    fn fixed_two_gib_headroom_does_not_force_a_size_triggered_shrink() {
        let hdd = candidate(
            'D',
            0,
            StorageMedia::Rotational,
            StorageAttachment::Internal,
            100,
        );
        assert!(matches!(
            select_staging_plan(
                gib(20),
                Some(1),
                &[hdd],
                Some(shrink_target(StorageMedia::SolidState, 40))
            ),
            StagingPlan::Existing { letter: 'D', .. }
        ));
    }

    #[test]
    fn crossing_the_old_eight_gib_boundary_does_not_change_to_destructive_shrink() {
        let hdd = candidate(
            'D',
            0,
            StorageMedia::Rotational,
            StorageAttachment::Internal,
            100,
        );
        let target = Some(shrink_target(StorageMedia::SolidState, 100));
        assert!(matches!(
            select_staging_plan(gib(8) - 1, Some(1), &[hdd], target),
            StagingPlan::Existing { letter: 'D', .. }
        ));
        assert!(matches!(
            select_staging_plan(gib(8), Some(1), &[hdd], target),
            StagingPlan::Existing { letter: 'D', .. }
        ));
    }

    #[test]
    fn caller_marked_unsafe_target_is_never_selected_for_shrink() {
        let mut target = shrink_target(StorageMedia::SolidState, 100);
        target.shrink_is_safe = false;
        assert_eq!(
            select_staging_plan(gib(6), Some(1), &[], Some(target)),
            StagingPlan::Unavailable {
                required_bytes: required_staging_bytes(gib(6)).expect("bounded")
            }
        );
    }

    #[test]
    fn external_disk_is_only_a_fallback() {
        let candidates = [
            candidate(
                'D',
                0,
                StorageMedia::SolidState,
                StorageAttachment::External,
                100,
            ),
            candidate(
                'E',
                2,
                StorageMedia::Rotational,
                StorageAttachment::Internal,
                50,
            ),
        ];
        assert!(matches!(
            select_staging_plan(gib(6), Some(1), &candidates, None),
            StagingPlan::Existing { letter: 'E', .. }
        ));
    }

    #[test]
    fn external_ssd_is_used_when_fixed_storage_lacks_safe_room() {
        let candidates = [
            candidate(
                'D',
                0,
                StorageMedia::Rotational,
                StorageAttachment::Internal,
                8,
            ),
            candidate(
                'U',
                3,
                StorageMedia::SolidState,
                StorageAttachment::External,
                200,
            ),
        ];
        assert!(matches!(
            select_staging_plan(gib(10), Some(1), &candidates, None),
            StagingPlan::Existing { letter: 'U', .. }
        ));
    }

    #[test]
    fn safe_fixed_shrink_wins_over_external_ssd_even_for_a_small_image() {
        let external_ssd = candidate(
            'U',
            3,
            StorageMedia::SolidState,
            StorageAttachment::External,
            200,
        );
        assert!(matches!(
            select_staging_plan(
                gib(4),
                Some(1),
                &[external_ssd],
                Some(shrink_target(StorageMedia::Rotational, 100))
            ),
            StagingPlan::ShrinkTarget { letter: 'C', .. }
        ));
    }

    #[test]
    fn unknown_virtual_disks_choose_the_one_with_more_safe_space() {
        let candidates = [
            candidate(
                'D',
                0,
                StorageMedia::Unknown,
                StorageAttachment::Unknown,
                30,
            ),
            candidate(
                'E',
                2,
                StorageMedia::Unknown,
                StorageAttachment::Unknown,
                80,
            ),
        ];
        assert!(matches!(
            select_staging_plan(gib(6), Some(1), &candidates, None),
            StagingPlan::Existing { letter: 'E', .. }
        ));
    }

    #[test]
    fn fragmented_free_space_is_never_misrepresented_as_one_basic_partition() {
        // Microsoft defines the largest creatable basic partition by the largest contiguous free
        // extent, not by adding unrelated tails. Model the reported extreme layout with 5 GiB on
        // C:, D: and E:. The aggregate 15 GiB exceeds the 12 GiB requirement, but no individual
        // extent can hold it, so the safe result remains unavailable. A future multi-volume
        // carrier must be a separately authenticated design; it must not silently turn disks into
        // dynamic/spanned volumes.
        let candidates = [
            candidate(
                'D',
                1,
                StorageMedia::SolidState,
                StorageAttachment::Internal,
                5,
            ),
            candidate(
                'E',
                1,
                StorageMedia::SolidState,
                StorageAttachment::Internal,
                5,
            ),
        ];
        let c = shrink_target(StorageMedia::SolidState, 5);
        assert_eq!(
            select_staging_plan(gib(10), Some(1), &candidates, Some(c)),
            StagingPlan::Unavailable {
                required_bytes: gib(12)
            }
        );
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.free_bytes)
                .sum::<u64>()
                + c.free_bytes,
            gib(15)
        );
    }
}

// ================================================================================================
// Scattered staging on already existing volumes
// ================================================================================================
//
// When no dedicated staging volume can be created (VDS and the Storage Management API both fail,
// or the user enabled `scattered_staging_enabled`), the ViaPE payload is stored on the volumes
// that already exist. One volume is the *primary* data volume: it carries the authenticated
// control files, the PCA package, UefiSeven and every component that fits there. Other volumes
// receive a directory named `LetRecovery_Scatter_<locator token>` whose inner layout mirrors the
// primary `LetRecovery_Data` directory. The locator token is also written into a marker file in
// that directory, so WinPE can find the volume again even when drive letters change.
//
// A single image file that fits on no volume is split into raw byte chunks. Chunks never need a
// conversion, recompression or temporary copy: WinPE concatenates them onto the freshly formatted
// target partition and checks the exact SHA-256 of the whole stream before the image engine opens
// it. This works for WIM, ESD (solid) and any future single-file format alike.

/// Directory prefix of a scattered-staging root. The full name is the prefix plus the
/// 64-character lowercase hexadecimal locator token of that volume.
pub const SCATTER_DIRECTORY_PREFIX: &str = "LetRecovery_Scatter_";
/// Locator marker inside every scattered-staging root. Its exact content is the token.
pub const SCATTER_MARKER_NAME: &str = "LetRecovery_Scatter.marker";
/// Data directory mirrored below every scattered-staging root.
pub const SCATTER_DATA_DIRECTORY: &str = "LetRecovery_Data";
/// Directory (below the data directory) that holds raw chunks of a single image file.
pub const IMAGE_CHUNK_DIRECTORY: &str = "image_chunks";
/// Headroom left free on the primary scattered volume (control files, logs, rounding).
pub const SCATTER_PRIMARY_RESERVE_BYTES: u64 = STAGING_OPERATIONAL_HEADROOM_BYTES;
/// Headroom left free on every other scattered volume (allocation rounding, NTFS metadata).
pub const SCATTER_SECONDARY_RESERVE_BYTES: u64 = 512 * MIB;
/// Chunks smaller than this are only used for the final remainder of an image.
pub const IMAGE_CHUNK_MIN_BYTES: u64 = 64 * MIB;
/// FAT12/16/32 cannot store a file of 4 GiB or more.
pub const FAT_MAX_FILE_BYTES: u64 = 4 * GIB - 1;
/// Extra free space required on the formatted target in WinPE besides the reassembled image and
/// the expanded Windows image (file-system metadata, page file creation during first boot).
pub const IMAGE_REASSEMBLY_HEADROOM_BYTES: u64 = 2 * GIB;

/// Name of the scattered-staging root directory for `token`.
pub fn scatter_root_name(token: &str) -> String {
    format!("{SCATTER_DIRECTORY_PREFIX}{token}")
}

fn is_locator_token(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Split `LetRecovery_Scatter_<token>\<rest>` into `(token, rest)`.
///
/// Any other form, including an unexpected token length, returns `None` so the caller keeps its
/// historical single-volume interpretation of the path.
pub fn split_scatter_prefix(relative_path: &str) -> Option<(&str, &str)> {
    let without_prefix = relative_path.get(..SCATTER_DIRECTORY_PREFIX.len()).and_then(|head| {
        head.eq_ignore_ascii_case(SCATTER_DIRECTORY_PREFIX)
            .then(|| &relative_path[SCATTER_DIRECTORY_PREFIX.len()..])
    })?;
    let separator = without_prefix.find(['\\', '/'])?;
    let token = &without_prefix[..separator];
    let rest = &without_prefix[separator + 1..];
    (is_locator_token(token) && !rest.is_empty()).then_some((token, rest))
}

/// Return the data-volume-relative form of a manifest path: scattered paths lose their
/// `LetRecovery_Scatter_<token>\` prefix, every other path is returned unchanged.
pub fn strip_scatter_prefix(relative_path: &str) -> &str {
    split_scatter_prefix(relative_path).map_or(relative_path, |(_, rest)| rest)
}

/// Largest single file a file system can store. Unknown names are treated as unlimited; the real
/// write still reports an error if that assumption is wrong.
pub fn max_file_bytes_for_file_system(file_system: Option<&str>) -> u64 {
    match file_system.map(str::trim) {
        Some(name)
            if ["FAT", "FAT12", "FAT16", "FAT32"]
                .iter()
                .any(|fat| name.eq_ignore_ascii_case(fat)) =>
        {
            FAT_MAX_FILE_BYTES
        }
        _ => u64::MAX,
    }
}

/// File name of raw image chunk `ordinal` (zero based).
pub fn image_chunk_file_name(image_file_name: &str, ordinal: u32) -> String {
    format!("{image_file_name}.lrpart{:03}", u64::from(ordinal) + 1)
}

/// One existing volume that may receive scattered payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScatterVolume {
    pub letter: char,
    /// Free bytes available to the caller (`GetDiskFreeSpaceExW`, caller quota aware).
    pub free_bytes: u64,
    /// Largest file the volume's file system can store.
    pub max_file_bytes: u64,
}

/// Shape of the installation source for scattered planning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScatterImageDemand {
    /// One image file. `chunkable` is true only when WinPE will format the target and can
    /// therefore reassemble raw chunks there.
    SingleFile { bytes: u64, chunkable: bool },
    /// Split WIM parts. Every part is an independent file and may live on any volume.
    IndependentFiles { files: Vec<u64> },
    /// Files that an external engine discovers by directory (GHO/GHS); they stay together.
    TogetherFiles { files: Vec<u64> },
    /// Directory tree that must remain on the primary volume (XP/2003 text mode source).
    PrimaryTree { bytes: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScatterRequest {
    pub volumes: Vec<ScatterVolume>,
    pub image: ScatterImageDemand,
    /// Components that are always stored on the primary volume (PCA package, UefiSeven).
    pub primary_fixed_bytes: u64,
    /// Components that may be distributed in whole units (drivers, user drivers, installers).
    pub flexible_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScatterImagePlacement {
    /// The complete image (single file, SWM set, GHO set or XP tree) on one volume.
    Whole(char),
    /// One volume per split WIM part, in source order.
    PerFile(Vec<char>),
    /// Raw byte chunks placed while copying; WinPE reassembles them on the formatted target.
    Chunked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScatterPlan {
    pub primary: char,
    pub image: ScatterImagePlacement,
    /// Bytes that later components must leave free on a volume because the image is assigned
    /// there. Chunked images reserve nothing: chunks use whatever is left when they are copied.
    pub image_reservations: Vec<(char, u64)>,
}

/// Reserve that a volume keeps free in scattered mode.
pub const fn scatter_reserve_bytes(is_primary: bool) -> u64 {
    if is_primary {
        SCATTER_PRIMARY_RESERVE_BYTES
    } else {
        SCATTER_SECONDARY_RESERVE_BYTES
    }
}

/// Pick one existing volume that can hold the complete payload plus the usual 2 GiB headroom.
/// The volume with the most free space wins; ties keep the lowest drive letter.
pub fn select_single_existing_volume(
    volumes: &[ScatterVolume],
    payload_bytes: u64,
    largest_file_bytes: u64,
) -> Option<char> {
    let required = required_staging_bytes(payload_bytes)?;
    volumes
        .iter()
        .filter(|volume| volume.free_bytes >= required && volume.max_file_bytes >= largest_file_bytes)
        .max_by_key(|volume| (volume.free_bytes, std::cmp::Reverse(volume.letter)))
        .map(|volume| volume.letter)
}

/// Choose the volume for one indivisible unit (one driver package, one installer, one chunk).
///
/// `candidates` holds `(letter, currently available bytes, largest storable file)`. The preferred
/// volume is used when the unit fits there, keeping components together; otherwise the volume
/// with the most available space that fits is used. `None` means the unit fits nowhere.
pub fn choose_volume_for_unit(
    candidates: &[(char, u64, u64)],
    unit_bytes: u64,
    largest_file_bytes: u64,
    preferred: Option<char>,
) -> Option<char> {
    let fits = |(_, available, max_file): &(char, u64, u64)| {
        *available >= unit_bytes && *max_file >= largest_file_bytes
    };
    if let Some(preferred) = preferred {
        if candidates
            .iter()
            .any(|candidate| candidate.0 == preferred && fits(candidate))
        {
            return Some(preferred);
        }
    }
    candidates
        .iter()
        .filter(|candidate| fits(candidate))
        .max_by_key(|(letter, available, _)| (*available, std::cmp::Reverse(*letter)))
        .map(|(letter, _, _)| *letter)
}

/// Length of the next raw image chunk, or `None` when the chosen volume is too small to be worth
/// a chunk (smaller than [`IMAGE_CHUNK_MIN_BYTES`] while more data than that remains).
pub fn next_image_chunk_len(remaining: u64, available: u64, max_file_bytes: u64) -> Option<u64> {
    if remaining == 0 {
        return None;
    }
    let length = remaining.min(available).min(max_file_bytes);
    if length == 0 || (length < IMAGE_CHUNK_MIN_BYTES && length < remaining) {
        None
    } else {
        Some(length)
    }
}

/// Free bytes the target volume must retain after in-place staging, so the reassembled image,
/// the applied Windows image and first-boot artifacts still fit once the old system is deleted.
pub const IN_PLACE_TARGET_RESERVE_BYTES: u64 = 3 * GIB;

/// Whether the target volume, with the old system still present, can host the entire payload
/// in-place. `old_system_bytes` is what will be reclaimed by the pre-write old-system deletion;
/// only that reclaimable space plus current free space is counted, and the applied image plus a
/// fixed reserve must still fit afterwards.
pub fn target_can_host_in_place(
    target_free_bytes: u64,
    old_system_bytes: u64,
    payload_bytes: u64,
    expanded_image_bytes: u64,
) -> bool {
    let reclaimable = target_free_bytes.saturating_add(old_system_bytes);
    let required = match required_staging_bytes(payload_bytes) {
        Some(value) => value,
        None => return false,
    };
    // Payload must fit beside the old system that is still on disk at staging time.
    if target_free_bytes < required {
        return false;
    }
    // After deletion the volume must still hold the applied image plus a fixed reserve; the
    // staged payload is consumed as the image is applied, so it is not double-counted here.
    let after_delete_required = expanded_image_bytes
        .saturating_add(IN_PLACE_TARGET_RESERVE_BYTES);
    reclaimable >= after_delete_required
}

/// Plan a scattered layout from a fresh volume inventory.
///
/// The image, the least flexible component, is placed first (best fit, so the largest volumes
/// remain available for drivers and installers). The plan fails only when the payload cannot fit
/// in the combined free space at all, or when a component that must stay whole fits nowhere.
pub fn plan_scattered_staging(request: &ScatterRequest) -> Result<ScatterPlan, String> {
    let mut volumes = request.volumes.clone();
    volumes.sort_by_key(|volume| (std::cmp::Reverse(volume.free_bytes), volume.letter));
    let largest = volumes
        .first()
        .copied()
        .ok_or_else(|| "no existing volume is available for scattered staging".to_owned())?;

    let primary_needs = match &request.image {
        ScatterImageDemand::PrimaryTree { bytes } => request
            .primary_fixed_bytes
            .checked_add(*bytes)
            .ok_or_else(|| "primary payload size overflows u64".to_owned())?,
        _ => request.primary_fixed_bytes,
    };
    let primary = volumes
        .iter()
        .find(|volume| {
            volume.free_bytes.saturating_sub(scatter_reserve_bytes(true)) >= primary_needs
        })
        .map(|volume| volume.letter)
        .ok_or_else(|| {
            format!(
                "no existing volume can hold the primary payload of {primary_needs} bytes plus {} bytes headroom (largest volume {}: has {} free bytes)",
                scatter_reserve_bytes(true),
                largest.letter,
                largest.free_bytes
            )
        })?;

    let mut capacity: Vec<(char, u64, u64)> = volumes
        .iter()
        .map(|volume| {
            let is_primary = volume.letter == primary;
            let usable = volume
                .free_bytes
                .saturating_sub(scatter_reserve_bytes(is_primary))
                .saturating_sub(if is_primary { primary_needs } else { 0 });
            (volume.letter, usable, volume.max_file_bytes)
        })
        .collect();
    let best_fit = |capacity: &[(char, u64, u64)], total: u64, largest_file: u64| {
        capacity
            .iter()
            .filter(|(_, usable, max_file)| *usable >= total && *max_file >= largest_file)
            .min_by_key(|(letter, usable, _)| (*usable, *letter))
            .map(|(letter, _, _)| *letter)
    };
    let take = |capacity: &mut Vec<(char, u64, u64)>, letter: char, bytes: u64| {
        if let Some(entry) = capacity.iter_mut().find(|entry| entry.0 == letter) {
            entry.1 = entry.1.saturating_sub(bytes);
        }
    };

    let (image, image_reservations) = match &request.image {
        ScatterImageDemand::PrimaryTree { .. } => (ScatterImagePlacement::Whole(primary), Vec::new()),
        ScatterImageDemand::SingleFile { bytes, chunkable } => {
            if let Some(letter) = best_fit(&capacity, *bytes, *bytes) {
                take(&mut capacity, letter, *bytes);
                (ScatterImagePlacement::Whole(letter), vec![(letter, *bytes)])
            } else if *chunkable {
                let total: u64 = capacity
                    .iter()
                    .map(|(_, usable, _)| *usable)
                    .fold(0_u64, u64::saturating_add);
                if total < *bytes {
                    return Err(format!(
                        "the image needs {bytes} bytes but all existing volumes together only have {total} usable bytes"
                    ));
                }
                // Chunks consume the largest volumes first; account for that in the remaining
                // capacity so the flexible-payload check below stays honest.
                let mut remaining = *bytes;
                let mut order: Vec<usize> = (0..capacity.len()).collect();
                order.sort_by_key(|index| std::cmp::Reverse(capacity[*index].1));
                for index in order {
                    let used = remaining.min(capacity[index].1);
                    capacity[index].1 -= used;
                    remaining -= used;
                    if remaining == 0 {
                        break;
                    }
                }
                (ScatterImagePlacement::Chunked, Vec::new())
            } else {
                return Err(format!(
                    "the {bytes}-byte image fits on no single existing volume and this installation mode cannot reassemble chunks on the target"
                ));
            }
        }
        ScatterImageDemand::TogetherFiles { files } | ScatterImageDemand::IndependentFiles { files } => {
            let total = files
                .iter()
                .try_fold(0_u64, |sum, file| sum.checked_add(*file))
                .ok_or_else(|| "image set size overflows u64".to_owned())?;
            let largest_file = files.iter().copied().max().unwrap_or(0);
            if let Some(letter) = best_fit(&capacity, total, largest_file) {
                take(&mut capacity, letter, total);
                (ScatterImagePlacement::Whole(letter), vec![(letter, total)])
            } else if matches!(request.image, ScatterImageDemand::TogetherFiles { .. }) {
                return Err(format!(
                    "the {total}-byte GHO/GHS set must stay in one directory but fits on no single existing volume"
                ));
            } else {
                let mut order: Vec<usize> = (0..files.len()).collect();
                order.sort_by_key(|index| std::cmp::Reverse(files[*index]));
                let mut assigned = vec![primary; files.len()];
                let mut reservations: Vec<(char, u64)> = Vec::new();
                for index in order {
                    let size = files[index];
                    let letter = capacity
                        .iter()
                        .filter(|(_, usable, max_file)| *usable >= size && *max_file >= size)
                        .max_by_key(|(letter, usable, _)| (*usable, std::cmp::Reverse(*letter)))
                        .map(|(letter, _, _)| *letter)
                        .ok_or_else(|| {
                            format!("split image part {} ({size} bytes) fits on no existing volume", index + 1)
                        })?;
                    take(&mut capacity, letter, size);
                    assigned[index] = letter;
                    match reservations.iter_mut().find(|entry| entry.0 == letter) {
                        Some(entry) => entry.1 = entry.1.saturating_add(size),
                        None => reservations.push((letter, size)),
                    }
                }
                (ScatterImagePlacement::PerFile(assigned), reservations)
            }
        }
    };

    let remaining: u64 = capacity
        .iter()
        .map(|(_, usable, _)| *usable)
        .fold(0_u64, u64::saturating_add);
    if remaining < request.flexible_bytes {
        return Err(format!(
            "drivers and installers need {} bytes but only {remaining} bytes remain on existing volumes after placing the image",
            request.flexible_bytes
        ));
    }
    Ok(ScatterPlan {
        primary,
        image,
        image_reservations,
    })
}

#[cfg(test)]
mod scatter_tests {
    use super::*;

    fn volume(letter: char, free_gib: u64) -> ScatterVolume {
        ScatterVolume {
            letter,
            free_bytes: free_gib * GIB,
            max_file_bytes: u64::MAX,
        }
    }

    #[test]
    fn scatter_prefix_round_trips_only_exact_tokens() {
        let token = "a".repeat(64);
        let path = format!("{}\\LetRecovery_Data\\drivers\\x.inf", scatter_root_name(&token));
        assert_eq!(
            split_scatter_prefix(&path),
            Some((token.as_str(), "LetRecovery_Data\\drivers\\x.inf"))
        );
        assert_eq!(strip_scatter_prefix(&path), "LetRecovery_Data\\drivers\\x.inf");
        assert_eq!(strip_scatter_prefix("LetRecovery_Data\\a.wim"), "LetRecovery_Data\\a.wim");
        assert_eq!(split_scatter_prefix("LetRecovery_Scatter_abc\\x"), None);
        let upper = format!("letrecovery_scatter_{}/y", "B".repeat(64));
        assert_eq!(split_scatter_prefix(&upper).map(|(_, rest)| rest), Some("y"));
        assert_eq!(split_scatter_prefix(&format!("{}\\", scatter_root_name(&token))), None);
    }

    #[test]
    fn fat_volumes_limit_single_files() {
        assert_eq!(max_file_bytes_for_file_system(Some("FAT32")), FAT_MAX_FILE_BYTES);
        assert_eq!(max_file_bytes_for_file_system(Some("fat")), FAT_MAX_FILE_BYTES);
        assert_eq!(max_file_bytes_for_file_system(Some("NTFS")), u64::MAX);
        assert_eq!(max_file_bytes_for_file_system(Some("exFAT")), u64::MAX);
        assert_eq!(max_file_bytes_for_file_system(None), u64::MAX);
        assert_eq!(image_chunk_file_name("a.esd", 0), "a.esd.lrpart001");
        assert_eq!(image_chunk_file_name("a.esd", 11), "a.esd.lrpart012");
    }

    #[test]
    fn a_single_volume_that_holds_everything_is_preferred() {
        let volumes = [volume('D', 449), volume('E', 20)];
        assert_eq!(select_single_existing_volume(&volumes, 10 * GIB, 5 * GIB), Some('D'));
        assert_eq!(select_single_existing_volume(&volumes, 500 * GIB, 5 * GIB), None);
        let fat = [ScatterVolume {
            letter: 'F',
            free_bytes: 100 * GIB,
            max_file_bytes: FAT_MAX_FILE_BYTES,
        }];
        assert_eq!(select_single_existing_volume(&fat, 6 * GIB, 5 * GIB), None);
    }

    #[test]
    fn single_image_uses_best_fit_and_keeps_large_volume_for_drivers() {
        let plan = plan_scattered_staging(&ScatterRequest {
            volumes: vec![volume('D', 30), volume('E', 8)],
            image: ScatterImageDemand::SingleFile {
                bytes: 5 * GIB,
                chunkable: true,
            },
            primary_fixed_bytes: 100 * MIB,
            flexible_bytes: 20 * GIB,
        })
        .unwrap();
        assert_eq!(plan.primary, 'D');
        assert_eq!(plan.image, ScatterImagePlacement::Whole('E'));
        assert_eq!(plan.image_reservations, vec![('E', 5 * GIB)]);
    }

    #[test]
    fn oversized_single_image_is_chunked_only_when_the_target_is_formatted() {
        let request = |chunkable| ScatterRequest {
            volumes: vec![volume('D', 5), volume('E', 5), volume('F', 5)],
            image: ScatterImageDemand::SingleFile {
                bytes: 9 * GIB,
                chunkable,
            },
            primary_fixed_bytes: 0,
            flexible_bytes: GIB,
        };
        let plan = plan_scattered_staging(&request(true)).unwrap();
        assert_eq!(plan.image, ScatterImagePlacement::Chunked);
        assert!(plan.image_reservations.is_empty());
        assert!(plan_scattered_staging(&request(false)).is_err());
    }

    #[test]
    fn split_wim_parts_are_distributed_and_ghost_sets_stay_together() {
        let plan = plan_scattered_staging(&ScatterRequest {
            volumes: vec![volume('D', 7), volume('E', 7)],
            image: ScatterImageDemand::IndependentFiles {
                files: vec![4 * GIB, 4 * GIB],
            },
            primary_fixed_bytes: 0,
            flexible_bytes: 0,
        })
        .unwrap();
        let ScatterImagePlacement::PerFile(letters) = plan.image else {
            panic!("expected per-file placement");
        };
        assert_eq!(letters.len(), 2);
        assert_ne!(letters[0], letters[1]);
        assert!(plan_scattered_staging(&ScatterRequest {
            volumes: vec![volume('D', 5), volume('E', 5)],
            image: ScatterImageDemand::TogetherFiles {
                files: vec![4 * GIB, 4 * GIB],
            },
            primary_fixed_bytes: 0,
            flexible_bytes: 0,
        })
        .is_err());
    }

    #[test]
    fn plan_rejects_payload_larger_than_total_free_space() {
        assert!(plan_scattered_staging(&ScatterRequest {
            volumes: vec![volume('D', 5), volume('E', 5)],
            image: ScatterImageDemand::SingleFile {
                bytes: 3 * GIB,
                chunkable: true,
            },
            primary_fixed_bytes: 0,
            flexible_bytes: 20 * GIB,
        })
        .is_err());
        assert!(plan_scattered_staging(&ScatterRequest {
            volumes: Vec::new(),
            image: ScatterImageDemand::SingleFile {
                bytes: 1,
                chunkable: true,
            },
            primary_fixed_bytes: 0,
            flexible_bytes: 0,
        })
        .is_err());
    }

    #[test]
    fn unit_placement_prefers_the_requested_volume_then_the_largest() {
        let candidates = [('D', 10 * GIB, u64::MAX), ('E', 20 * GIB, FAT_MAX_FILE_BYTES)];
        assert_eq!(choose_volume_for_unit(&candidates, GIB, GIB, Some('D')), Some('D'));
        assert_eq!(choose_volume_for_unit(&candidates, GIB, GIB, None), Some('E'));
        assert_eq!(choose_volume_for_unit(&candidates, 12 * GIB, 5 * GIB, None), None);
        assert_eq!(choose_volume_for_unit(&candidates, 5 * GIB, 5 * GIB, None), Some('D'));
    }

    #[test]
    fn chunk_lengths_respect_space_file_limits_and_minimum() {
        assert_eq!(next_image_chunk_len(10 * GIB, 3 * GIB, u64::MAX), Some(3 * GIB));
        assert_eq!(next_image_chunk_len(10 * GIB, 8 * GIB, FAT_MAX_FILE_BYTES), Some(FAT_MAX_FILE_BYTES));
        assert_eq!(next_image_chunk_len(10 * GIB, MIB, u64::MAX), None);
        assert_eq!(next_image_chunk_len(MIB, 5 * MIB, u64::MAX), Some(MIB));
        assert_eq!(next_image_chunk_len(0, 5 * MIB, u64::MAX), None);
    }
}
