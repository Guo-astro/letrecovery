//! PE execution boundary for authenticated full-disk and dual-boot installation plans.
//!
//! Cross-reboot selection is random-marker based. Disk numbers below are obtained from the marker
//! volume's current `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS` result and are never compared with the
//! normal-endpoint diagnostic disk numbers.

use anyhow::{bail, Context, Result};

use lr_core::custom_install::{
    plan_full_disk_layout_around_staging, plan_full_disk_layout_for_disk,
    validate_existing_staging_extent, CustomInstallPlan, FullDiskRole, PlannedPartition,
    PlannedPartitionRole, RepartitionAllDisksPlan, RequestedPartitionStyle,
    BIOS_SYSTEM_FUNCTIONAL_MINIMUM_BYTES, ESP_4KN_MINIMUM_BYTES, ESP_512_MINIMUM_BYTES, MIB,
    MIN_USEFUL_DATA_BYTES, MSR_WINDOWS_7_MINIMUM_BYTES,
};
use lr_core::windows_storage::{
    CreatePartitionRequest, CreatedPartition, DiskStyle, FileSystem, FreeExtent, PartitionKind,
    VolumeIdentity,
};

use super::config::FullDiskExecutionTarget;

pub struct PreparedFullDiskInstall {
    plan: RepartitionAllDisksPlan,
    disks: Vec<PreparedDisk>,
}

struct PreparedDisk {
    locator_token: String,
    role: FullDiskRole,
    current_disk_number: u32,
    diagnostic_disk_number: u32,
    layout: Vec<PlannedPartition>,
    disk_size_bytes: u64,
    /// `[offset, end)` of the preserved same-disk staging extent, when this disk holds it.
    staging_extent: Option<(u64, u64)>,
}

pub struct PreparedInstallTarget {
    pub partition: String,
    pub identity: VolumeIdentity,
    pub staging_cleanup: Option<FullDiskStagingCleanup>,
    /// Optional layout steps that were skipped. The installation itself is complete.
    pub layout_warnings: Vec<String>,
}

/// Current access path and extent of the volume that receives the staging space.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StagingRecipient {
    letter: char,
    identity: VolumeIdentity,
}

/// Move-only post-install authority for deleting the preserved same-disk staging extent.
///
/// This intentionally contains only the current extents that participate in the topology change.
/// Historical disk numbers, GUIDs, labels, capacities and layout fingerprints are not
/// authorization inputs.
pub struct FullDiskStagingCleanup {
    disk_number: u32,
    staging_offset_bytes: u64,
    staging_length_bytes: u64,
    /// Nearest mounted ordinary volume in front of the staging extent. `None` only when the
    /// staging extent sits at the start of the disk; its space then becomes a new data volume.
    recipient: Option<StagingRecipient>,
    /// True when no partition was planned behind the staging extent, so the recipient also
    /// receives the free tail. False keeps that tail for its own planned data volume.
    reclaim_trailing_free: bool,
}

/// Bounded retries for post-install topology cleanup. Every attempt re-reads the current layout,
/// so transient VDS cache, volume lock and provider errors do not leave a staging partition
/// behind, and an attempt interrupted after the deletion resumes with the space reclamation.
pub(crate) const POST_INSTALL_CLEANUP_ATTEMPTS: u32 = 6;

pub(crate) fn post_install_cleanup_retry_delay(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_millis(u64::from(attempt.clamp(1, 5)) * 1_500)
}

/// Required partitions are created again only while the disk layout is provably unchanged.
const PARTITION_CREATE_ATTEMPTS: u32 = 3;
/// Passes over the old partitions of a staging disk (logical partitions, transient locks).
const OLD_PARTITION_DELETE_PASSES: u32 = 5;
/// A freshly wiped disk is cleaned and initialized again after a transient failure.
const DISK_CLEAN_ATTEMPTS: u32 = 3;

fn staging_reclaim_length(
    recipient_offset: u64,
    recipient_length: u64,
    staging_offset: u64,
    staging_length: u64,
) -> Result<u64> {
    let recipient_end = recipient_offset
        .checked_add(recipient_length)
        .context("full-disk staging recipient end overflows")?;
    if recipient_end > staging_offset || staging_length == 0 {
        bail!("the preserved staging extent overlaps or precedes its recipient volume");
    }
    staging_offset
        .checked_add(staging_length)
        .and_then(|staging_end| staging_end.checked_sub(recipient_end))
        .context("full-disk staging reclaim length overflows")
}

/// Keep the mounted ordinary volume that ends closest to the preserved staging extent.
///
/// GPT infrastructure partitions such as ESP and MSR intentionally have no DOS access path. They
/// are valid layout members, but can never receive the later filesystem extend, so they are skipped
/// rather than being misreported as a missing recipient. Volumes behind the staging extent are
/// skipped as well: NTFS can only grow towards the end of the disk.
fn consider_staging_cleanup_recipient(
    current: &mut Option<StagingRecipient>,
    staging_offset_bytes: u64,
    created_offset_bytes: u64,
    created_length_bytes: u64,
    created_identity: Option<(char, VolumeIdentity)>,
) -> Result<()> {
    let created_end = created_offset_bytes
        .checked_add(created_length_bytes)
        .context("created partition end overflows")?;
    if created_end > staging_offset_bytes {
        return Ok(());
    }
    let Some((letter, identity)) = created_identity else {
        return Ok(());
    };
    let replace = match current.as_ref() {
        None => true,
        Some(existing) => {
            existing
                .identity
                .offset_bytes
                .checked_add(existing.identity.extent_length_bytes)
                .context("current staging recipient end overflows")?
                < created_end
        }
    };
    if replace {
        *current = Some(StagingRecipient { letter, identity });
    }
    Ok(())
}

/// Byte range a planned partition may be created in: in front of or behind the preserved staging
/// extent, or the whole disk when the disk holds no staging extent.
fn partition_segment(
    partition: &PlannedPartition,
    disk_size_bytes: u64,
    staging_extent: Option<(u64, u64)>,
) -> (u64, u64) {
    match staging_extent {
        Some((_, staging_end)) if partition.offset_bytes >= staging_end => {
            (staging_end, disk_size_bytes.max(staging_end))
        }
        Some((staging_offset, _)) => (0, staging_offset),
        None => (0, disk_size_bytes),
    }
}

fn ensure_rollback_source_unchanged(
    expected: VolumeIdentity,
    current: VolumeIdentity,
) -> Result<()> {
    if !lr_core::windows_storage::same_volume_identity(expected, current) {
        bail!("dual-boot rollback source changed while deleting the task-owned tail volumes");
    }
    Ok(())
}

fn select_current_free_extent(
    extents: &[FreeExtent],
    segment_start_bytes: u64,
    segment_end_bytes: u64,
    minimum_bytes: u64,
    reserved_tail_bytes: u64,
) -> Result<Option<FreeExtent>> {
    let required_bytes = minimum_bytes
        .checked_add(reserved_tail_bytes)
        .context("current and following partition minimums overflow")?;
    let mut selected = None;
    for extent in extents {
        if extent.length_bytes == 0 {
            continue;
        }
        let end = extent
            .offset_bytes
            .checked_add(extent.length_bytes)
            .context("current full-disk free extent end overflows")?;
        // A provider extent may extend past a preserved staging boundary. Authorize only its
        // exact intersection with the segment instead of rejecting the legal part.
        let authorized_start = extent.offset_bytes.max(segment_start_bytes);
        let authorized_end = end.min(segment_end_bytes);
        let Some(authorized_length) = authorized_end.checked_sub(authorized_start) else {
            continue;
        };
        if authorized_length < required_bytes {
            continue;
        }
        let candidate = FreeExtent {
            offset_bytes: authorized_start,
            // Keep enough provider-reported space outside this operation's hard envelope for all
            // later boot-critical partitions. Provider geometry may differ from the request, but
            // it cannot consume capacity already required by a later partition.
            length_bytes: authorized_length - reserved_tail_bytes,
        };
        if selected.is_none_or(|current: FreeExtent| {
            candidate.length_bytes > current.length_bytes
                || (candidate.length_bytes == current.length_bytes
                    && candidate.offset_bytes > current.offset_bytes)
        }) {
            selected = Some(candidate);
        }
    }
    Ok(selected)
}

fn partition_functional_minimum(
    role: PlannedPartitionRole,
    windows_minimum_bytes: u64,
    logical_sector_bytes: Option<u32>,
) -> Result<u64> {
    match role {
        PlannedPartitionRole::EfiSystem => match logical_sector_bytes {
            // Microsoft distinguishes 512-native/512e from 4K-native by the logical sector
            // exposed to Windows. Physical=4096 with logical=512 is 512e, not 4Kn.
            Some(512) => Ok(ESP_512_MINIMUM_BYTES),
            Some(4096) => Ok(ESP_4KN_MINIMUM_BYTES),
            Some(value) => {
                bail!("unsupported logical sector size {value} bytes for an EFI system partition")
            }
            None => bail!("logical sector geometry is required for an EFI system partition"),
        },
        // Windows 7 requires 128 MiB on GPT disks >=16 GiB. A 16-MiB MSR is sufficient only for
        // newer Windows layouts, so it is not the Windows 7-11 functional minimum.
        PlannedPartitionRole::MicrosoftReserved => Ok(MSR_WINDOWS_7_MINIMUM_BYTES),
        // 100 MiB is Microsoft's boot-only BIOS minimum. LetRecovery also supports BitLocker,
        // whose separate NTFS system volume requirement is approximately 350 MiB.
        PlannedPartitionRole::SystemReserved => Ok(BIOS_SYSTEM_FUNCTIONAL_MINIMUM_BYTES),
        PlannedPartitionRole::Windows => Ok(windows_minimum_bytes),
        // This is the existing product threshold for a useful data volume, not an alignment rule.
        // Optional data is skipped if current provider space falls below it.
        PlannedPartitionRole::Data => Ok(MIN_USEFUL_DATA_BYTES),
    }
}

fn following_required_minimum(
    partitions: &[PlannedPartition],
    windows_minimum_bytes: u64,
    logical_sector_bytes: Option<u32>,
    data_is_required: bool,
) -> Result<u64> {
    partitions
        .iter()
        // Data after the Windows partition is optional and must not reduce the image-derived
        // Windows minimum. On a selected data-only disk it is the core result and is reserved.
        .filter(|partition| data_is_required || partition.role != PlannedPartitionRole::Data)
        .try_fold(0_u64, |total, partition| {
            total
                .checked_add(partition_functional_minimum(
                    partition.role,
                    windows_minimum_bytes,
                    logical_sector_bytes,
                )?)
                .context("following partition functional minimums overflow")
        })
}

fn validate_preserved_staging_bounds(
    staging: &lr_core::custom_install::PreservedStagingExtent,
    disk_size_bytes: u64,
    staging_disk_number: u32,
    original_target: VolumeIdentity,
) -> Result<()> {
    if staging_disk_number == original_target.disk_number {
        return validate_existing_staging_extent(
            staging,
            disk_size_bytes,
            original_target.offset_bytes,
            original_target.extent_length_bytes,
        )
        .map_err(anyhow::Error::msg);
    }
    let staging_end = staging
        .offset_bytes
        .checked_add(staging.length_bytes)
        .context("preserved staging extent end overflows")?;
    if staging.offset_bytes == 0 || staging.length_bytes == 0 || staging_end > disk_size_bytes {
        bail!("the preserved staging extent is outside its current disk");
    }
    Ok(())
}

fn rebind_current_preserved_staging(
    plan: &RepartitionAllDisksPlan,
    targets: &[FullDiskExecutionTarget],
    data_identity: VolumeIdentity,
) -> Result<RepartitionAllDisksPlan> {
    let mut current = plan.clone();
    let matching: Vec<_> = targets
        .iter()
        .filter(|target| target.expected.disk_number == data_identity.disk_number)
        .collect();
    match current.preserved_staging.as_mut() {
        Some(staging) => {
            let [target] = matching.as_slice() else {
                bail!(
                    "the preserved staging locator does not resolve uniquely to the current data volume disk"
                );
            };
            if target.locator_token != staging.disk_locator_token {
                bail!("the preserved staging locator names a different current disk");
            }
            if staging.offset_bytes != data_identity.offset_bytes
                || staging.length_bytes != data_identity.extent_length_bytes
            {
                log::info!(
                    "full-disk data marker rebound to current staging extent disk={} offset={} length={}; normal-endpoint diagnostic offset={} length={}",
                    data_identity.disk_number,
                    data_identity.offset_bytes,
                    data_identity.extent_length_bytes,
                    staging.offset_bytes,
                    staging.length_bytes
                );
            }
            staging.offset_bytes = data_identity.offset_bytes;
            staging.length_bytes = data_identity.extent_length_bytes;
        }
        None if !matching.is_empty() => {
            bail!("the data staging volume is on a selected disk but the plan does not preserve it")
        }
        None => {}
    }
    Ok(current)
}

pub fn validate_dual_boot_target(
    plan: &CustomInstallPlan,
    current_target: VolumeIdentity,
    current_data: VolumeIdentity,
) -> Result<()> {
    let CustomInstallPlan::DualBoot(plan) = plan else {
        return Ok(());
    };

    let target_end = current_target
        .offset_bytes
        .checked_add(current_target.extent_length_bytes)
        .context("the current dual-boot target extent overflows")?;
    let data_end = current_data
        .offset_bytes
        .checked_add(current_data.extent_length_bytes)
        .context("the current installation-data extent overflows")?;
    if current_target.extent_length_bytes == 0 || current_data.extent_length_bytes == 0 {
        bail!("the current dual-boot target or installation-data extent is empty");
    }
    if current_target == current_data
        || (current_target.disk_number == current_data.disk_number
            && current_target.offset_bytes < data_end
            && current_data.offset_bytes < target_end)
    {
        bail!("the current dual-boot target overlaps the installation-data extent");
    }

    // The random target marker is the cross-reboot binding. Disk numbers and historical geometry
    // can legitimately change across firmware/provider/WinPE enumeration, so retain them only as
    // diagnostics. Destructive rollback remains a separate transaction and deliberately keeps its
    // exact normal-endpoint extent checks below before deleting any pre-created partition.
    if current_target.offset_bytes != plan.target_offset_bytes
        || current_target.extent_length_bytes != plan.target_length_bytes
    {
        log::info!(
            "dual-boot marker rebound to current extent disk={} offset={} length={}; normal-endpoint diagnostic offset={} length={}",
            current_target.disk_number,
            current_target.offset_bytes,
            current_target.extent_length_bytes,
            plan.target_offset_bytes,
            plan.target_length_bytes
        );
    }
    Ok(())
}

/// Roll back a normal-Windows dual-boot preparation only while the original source system has not
/// been deleted, formatted or handed to an image engine. The current random target marker has
/// already rebound `current_target`; historical drive letters and disk numbers are not used.
pub fn rollback_dual_boot_before_write(
    plan: &CustomInstallPlan,
    current_target: VolumeIdentity,
) -> Result<bool> {
    let CustomInstallPlan::DualBoot(plan) = plan else {
        return Ok(false);
    };
    if current_target.offset_bytes != plan.target_offset_bytes
        || current_target.extent_length_bytes != plan.target_length_bytes
    {
        bail!("dual-boot rollback target differs from the authenticated pre-created extent");
    }
    let disk_number = current_target.disk_number;
    let source_letters = lr_core::windows_storage::assigned_drive_letters_for_partition(
        disk_number,
        plan.source_offset_bytes,
    )?;
    // A volume may legitimately expose more than one DOS drive-letter alias. Every alias in this
    // list already comes from the exact current disk/partition extent, so requiring exactly one
    // adds no wrong-volume protection and can turn a normal mount layout into a rollback failure.
    let source_letter = source_letters
        .first()
        .copied()
        .context("dual-boot rollback source has no current drive-letter access path")?;
    let source_before_cleanup = lr_core::windows_storage::volume_identity(source_letter)?;
    if source_before_cleanup.disk_number != disk_number
        || source_before_cleanup.offset_bytes != plan.source_offset_bytes
        || source_before_cleanup.extent_length_bytes != plan.source_length_after_bytes
    {
        bail!("dual-boot rollback source no longer has the authenticated post-shrink extent");
    }
    let reclaimed = plan
        .source_length_before_bytes
        .checked_sub(plan.source_length_after_bytes)
        .context("dual-boot rollback length underflow")?;
    let tail_offset = plan
        .source_offset_bytes
        .checked_add(plan.source_length_after_bytes)
        .context("dual-boot rollback tail offset overflow")?;
    let partitions = lr_core::windows_storage::partitions(disk_number)?;
    let target_matches = partitions
        .iter()
        .filter(|partition| {
            partition.offset_bytes == plan.target_offset_bytes
                && partition.size_bytes == plan.target_length_bytes
                && partition.kind == PartitionKind::BasicData
        })
        .count();
    let data_matches = plan.data_offset_bytes.map_or(0, |offset| {
        partitions
            .iter()
            .filter(|partition| {
                partition.offset_bytes == offset
                    && partition.size_bytes == plan.data_length_bytes
                    && partition.kind == PartitionKind::BasicData
            })
            .count()
    });
    if target_matches != 1 || data_matches != usize::from(plan.data_offset_bytes.is_some()) {
        bail!("dual-boot rollback target/data extent is absent or ambiguous");
    }
    for partition in &partitions {
        let overlap = lr_core::custom_install::ranges_overlap(
            partition.offset_bytes,
            partition.size_bytes,
            tail_offset,
            reclaimed,
        )
        .map_err(anyhow::Error::msg)?;
        let owned_target = partition.offset_bytes == plan.target_offset_bytes
            && partition.size_bytes == plan.target_length_bytes;
        let owned_data = plan.data_offset_bytes.is_some_and(|offset| {
            partition.offset_bytes == offset && partition.size_bytes == plan.data_length_bytes
        });
        if overlap && !owned_target && !owned_data {
            bail!("dual-boot rollback tail contains a partition not created by this task");
        }
    }
    if let Some(offset) = plan.data_offset_bytes {
        let snapshot = lr_core::windows_storage::disk_layout_snapshot(disk_number)?;
        lr_core::windows_storage::delete_partition_checked(disk_number, offset, false, &snapshot)?;
    }
    let snapshot = lr_core::windows_storage::disk_layout_snapshot(disk_number)?;
    lr_core::windows_storage::delete_partition_checked(
        disk_number,
        plan.target_offset_bytes,
        false,
        &snapshot,
    )?;
    let source_after_deletes = lr_core::windows_storage::volume_identity(source_letter)?;
    ensure_rollback_source_unchanged(source_before_cleanup, source_after_deletes)?;
    lr_core::windows_storage::extend_volume_checked(
        source_letter,
        source_after_deletes,
        reclaimed,
    )?;
    let restored = lr_core::windows_storage::volume_identity(source_letter)?;
    if restored.disk_number != disk_number
        || restored.offset_bytes != plan.source_offset_bytes
        || restored.extent_length_bytes != plan.source_length_before_bytes
    {
        bail!("dual-boot rollback source readback differs from the original extent");
    }
    Ok(true)
}

pub fn preflight_full_disk_install(
    plan: &CustomInstallPlan,
    targets: Vec<FullDiskExecutionTarget>,
    data_identity: VolumeIdentity,
    original_target: VolumeIdentity,
) -> Result<Option<PreparedFullDiskInstall>> {
    let CustomInstallPlan::RepartitionAllDisks(plan) = plan else {
        return Ok(None);
    };
    if targets.len() != plan.disks.len() {
        bail!("authenticated full-disk locator count does not match the plan");
    }
    let current_plan = rebind_current_preserved_staging(plan, &targets, data_identity)?;
    let staging_selection = current_plan.preserved_staging.as_ref();

    let mut seen_disks = std::collections::BTreeSet::new();
    let mut prepared = Vec::with_capacity(targets.len());
    for target in targets {
        let selection = current_plan
            .disks
            .iter()
            .find(|selection| selection.locator_token == target.locator_token)
            .context("full-disk marker is not present in the authenticated plan")?;
        if target.role != selection.role {
            bail!("full-disk marker role differs from the authenticated plan");
        }
        if !seen_disks.insert(target.expected.disk_number) {
            bail!("two selected locators resolved to volumes on the same current disk");
        }
        let initial_layout =
            lr_core::windows_storage::disk_layout_snapshot(target.expected.disk_number)
                .context("read selected disk layout before full-disk write")?;
        let partitions = lr_core::windows_storage::partitions(target.expected.disk_number)
            .context("read selected disk partitions before full-disk write")?;
        let preserves_staging = staging_selection
            .is_some_and(|staging| staging.disk_locator_token == target.locator_token);
        let staging_extent = if preserves_staging {
            let staging = staging_selection.expect("presence checked");
            validate_preserved_staging_bounds(
                staging,
                initial_layout.disk_size_bytes,
                data_identity.disk_number,
                original_target,
            )?;
            let exact = partitions
                .iter()
                .filter(|partition| {
                    partition.offset_bytes == staging.offset_bytes
                        && partition.size_bytes == staging.length_bytes
                        && partition.kind == PartitionKind::BasicData
                })
                .count();
            if exact != 1
                || data_identity.disk_number != target.expected.disk_number
                || data_identity.offset_bytes != staging.offset_bytes
                || data_identity.extent_length_bytes != staging.length_bytes
            {
                bail!("the existing staging extent is absent, ambiguous or no longer basic data");
            }
            let staging_end = staging
                .offset_bytes
                .checked_add(staging.length_bytes)
                .context("preserved staging extent end overflows")?;
            Some((staging.offset_bytes, staging_end))
        } else {
            None
        };
        // With a preserved staging extent the layout may use both sides of it. The choice is made
        // by the Windows size after the post-install cleanup, so a staging volume carved from the
        // old system volume no longer shrinks the new Windows volume.
        let layout = match staging_selection.filter(|_| preserves_staging) {
            Some(staging) => plan_full_disk_layout_around_staging(
                selection.style,
                selection.role,
                initial_layout.disk_size_bytes,
                staging.offset_bytes,
                staging.length_bytes,
                current_plan.windows_partition_bytes,
            ),
            None => plan_full_disk_layout_for_disk(
                selection.style,
                selection.role,
                initial_layout.disk_size_bytes,
                initial_layout.disk_size_bytes,
                current_plan.windows_partition_bytes,
            ),
        }
        .map_err(anyhow::Error::msg)?;
        for partition in &layout {
            log::info!(
                "[FULL DISK] planned {:?} offset={} length={} (disk {} bytes, image minimum {} bytes, staging {:?})",
                partition.role,
                partition.offset_bytes,
                partition.length_bytes,
                initial_layout.disk_size_bytes,
                current_plan.windows_partition_bytes,
                staging_extent
            );
        }
        prepared.push(PreparedDisk {
            locator_token: target.locator_token,
            role: target.role,
            current_disk_number: target.expected.disk_number,
            diagnostic_disk_number: target.diagnostic_disk_number,
            layout,
            disk_size_bytes: initial_layout.disk_size_bytes,
            staging_extent,
        });
    }
    Ok(Some(PreparedFullDiskInstall {
        plan: current_plan,
        disks: prepared,
    }))
}

pub fn execute_full_disk_install(
    prepared: PreparedFullDiskInstall,
    released: &[FullDiskExecutionTarget],
) -> Result<PreparedInstallTarget> {
    if released.len() != prepared.disks.len() {
        bail!("full-disk locator release set changed after preflight");
    }
    let mut drive_mask = lr_core::windows_storage::assigned_drive_letter_mask()
        .context("read assigned drive letters")?;
    let mut windows_target = None;
    let mut staging_cleanup: Option<FullDiskStagingCleanup> = None;
    let mut layout_warnings = Vec::new();
    for disk in prepared.disks {
        let released_target = released
            .iter()
            .find(|target| target.locator_token == disk.locator_token)
            .context("full-disk locator was not released for execution")?;
        if released_target.expected.disk_number != disk.current_disk_number {
            bail!("full-disk locator resolved to a different disk after preflight");
        }
        log::info!(
            "[FULL DISK] confirmed diagnostic disk {} currently resolves to disk {} role={:?}",
            disk.diagnostic_disk_number,
            disk.current_disk_number,
            disk.role
        );
        if disk.staging_extent.is_some() {
            let staging = prepared
                .plan
                .preserved_staging
                .as_ref()
                .context("preserved staging disappeared from the plan")?;
            delete_old_partitions_except_staging(
                disk.current_disk_number,
                staging.offset_bytes,
                staging.length_bytes,
            )?;
        } else {
            let style = requested_style(
                prepared
                    .plan
                    .disks
                    .iter()
                    .find(|value| value.locator_token == disk.locator_token)
                    .expect("validated plan contains locator")
                    .style,
            );
            clean_and_initialize_selected_disk(disk.current_disk_number, style)?;
        }

        let layout = disk.layout;
        let logical_sector_bytes = if layout
            .iter()
            .any(|partition| partition.role == PlannedPartitionRole::EfiSystem)
        {
            // StorageAccessAlignmentProperty is available on Windows 7 and distinguishes 512e
            // from 4Kn without guessing. The planned ESP already has the 4K-native size, so when
            // the query fails the stricter 4K-native functional minimum is safe on every disk.
            Some(
                match lr_core::windows_storage::physical_disk_sector_geometry(
                    disk.current_disk_number,
                ) {
                    Ok(geometry) => geometry.logical_sector_bytes,
                    Err(error) => {
                        log::warn!(
                            "[FULL DISK] logical sector size of disk {} is unavailable; using the 4K-native ESP minimum: {error}",
                            disk.current_disk_number
                        );
                        4096
                    }
                },
            )
        } else {
            None
        };
        let mut recipient: Option<StagingRecipient> = None;
        for partition_index in 0..layout.len() {
            let partition = layout[partition_index];
            let segment = partition_segment(&partition, disk.disk_size_bytes, disk.staging_extent);
            let later_in_segment = layout[partition_index + 1..]
                .iter()
                .copied()
                .filter(|later| {
                    partition_segment(later, disk.disk_size_bytes, disk.staging_extent) == segment
                })
                .collect::<Vec<_>>();
            // The last partition of its segment (in front of or behind a preserved staging
            // extent, or on the whole disk) consumes the provider's current remaining extent.
            let is_final = later_in_segment.is_empty();
            // Data volumes, and the MSR of a data-only disk, never decide whether Windows can be
            // installed. Skipping one leaves free space that can be partitioned later.
            let optional = partition.role == PlannedPartitionRole::Data
                || (disk.role == FullDiskRole::Data
                    && partition.role == PlannedPartitionRole::MicrosoftReserved);
            let letter = match partition.role {
                PlannedPartitionRole::Windows | PlannedPartitionRole::Data => {
                    let value = match first_free_letter(drive_mask) {
                        Ok(value) => value,
                        Err(error) if optional => {
                            log::warn!(
                                "[FULL DISK] optional {:?} volume skipped because no drive letter is available: {error:#}",
                                partition.role
                            );
                            layout_warnings.push(format!(
                                "disk {} {:?}: {error:#}",
                                disk.current_disk_number, partition.role
                            ));
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    drive_mask |= letter_bit(value);
                    Some(value)
                }
                _ => None,
            };
            let functional_minimum = partition_functional_minimum(
                partition.role,
                prepared.plan.windows_partition_bytes,
                logical_sector_bytes,
            )?;
            let following_minimum = following_required_minimum(
                &later_in_segment,
                prepared.plan.windows_partition_bytes,
                logical_sector_bytes,
                disk.role == FullDiskRole::Data,
            )?;
            let created = match create_planned_partition(
                disk.current_disk_number,
                partition,
                segment,
                is_final,
                letter,
                functional_minimum,
                following_minimum,
            ) {
                Ok(created) => created,
                Err(error) if optional => {
                    log::warn!(
                        "[FULL DISK] optional {:?} volume was not created; the Windows installation continues: {error:#}",
                        partition.role
                    );
                    layout_warnings.push(format!(
                        "disk {} {:?}: {error:#}",
                        disk.current_disk_number, partition.role
                    ));
                    continue;
                }
                Err(error) => return Err(error),
            };
            if partition.role == PlannedPartitionRole::Windows
                && created.size_bytes < prepared.plan.windows_partition_bytes
            {
                bail!(
                    "provider-created Windows volume is {} bytes, below the required {} bytes",
                    created.size_bytes,
                    prepared.plan.windows_partition_bytes
                );
            }
            log::info!(
                "[FULL DISK] created {:?} on disk {} offset={} length={} letter={:?}",
                partition.role,
                disk.current_disk_number,
                created.offset_bytes,
                created.size_bytes,
                letter
            );
            // `create_partition_checked_in_envelope` already binds the requested access path and
            // verifies its post-create canonical extent. Do not add a second whole-machine
            // inventory gate after that checked boundary.
            let created_identity = letter.map(|letter| {
                (
                    letter,
                    VolumeIdentity {
                        disk_number: disk.current_disk_number,
                        offset_bytes: created.offset_bytes,
                        extent_length_bytes: created.size_bytes,
                    },
                )
            });
            if let Some((staging_offset, _)) = disk.staging_extent {
                // Only the last planned volume in front of the staging extent is its planned
                // recipient. When that optional volume was skipped, an earlier volume must not
                // take its place (Windows would otherwise absorb the staging extent and the whole
                // free tail); the cleanup then turns the freed range into a data volume instead.
                if is_final && segment.1 == staging_offset {
                    consider_staging_cleanup_recipient(
                        &mut recipient,
                        staging_offset,
                        created.offset_bytes,
                        created.size_bytes,
                        created_identity,
                    )?;
                }
            }
            if partition.role == PlannedPartitionRole::Windows {
                let (letter, identity) =
                    created_identity.expect("Windows partition receives a drive letter");
                windows_target = Some(PreparedInstallTarget {
                    partition: format!("{letter}:"),
                    identity,
                    staging_cleanup: None,
                    layout_warnings: Vec::new(),
                });
            }
        }
        if let Some((staging_offset, staging_end)) = disk.staging_extent {
            if recipient.is_none() {
                log::warn!(
                    "[FULL DISK] no mounted ordinary volume precedes the preserved staging extent; the cleanup turns its space into a data volume"
                );
            }
            staging_cleanup = Some(FullDiskStagingCleanup {
                disk_number: disk.current_disk_number,
                staging_offset_bytes: staging_offset,
                staging_length_bytes: staging_end - staging_offset,
                recipient,
                reclaim_trailing_free: !layout
                    .iter()
                    .any(|partition| partition.offset_bytes >= staging_end),
            });
        }
    }
    let mut windows_target =
        windows_target.context("full-disk transaction created no Windows target")?;
    windows_target.staging_cleanup = staging_cleanup;
    windows_target.layout_warnings = layout_warnings;
    Ok(windows_target)
}

/// Create one planned partition inside its current segment.
///
/// The attempt is repeated with fresh provider extents only while the canonical partition table is
/// provably unchanged by the failed attempt, so a retry can never stack a second partition on top
/// of a partially created one.
fn create_planned_partition(
    disk_number: u32,
    partition: PlannedPartition,
    segment: (u64, u64),
    is_final: bool,
    letter: Option<char>,
    functional_minimum: u64,
    following_minimum: u64,
) -> Result<CreatedPartition> {
    let (kind, file_system, label, active) = partition_parameters(partition.role);
    let mut attempt = 1;
    loop {
        let before = lr_core::windows_storage::disk_layout_snapshot(disk_number)
            .context("read the selected disk before creating a partition")?;
        let result = (|| -> Result<CreatedPartition> {
            let extents = lr_core::windows_storage::current_free_extents(disk_number)
                .context("query the remaining provider extents")?;
            let envelope = select_current_free_extent(
                &extents,
                segment.0,
                segment.1,
                functional_minimum,
                following_minimum,
            )?
            .with_context(|| {
                format!(
                    "no current provider free extent can satisfy the {:?} volume minimum of {} bytes",
                    partition.role, functional_minimum
                )
            })?;
            let request = CreatePartitionRequest {
                disk_number,
                // The final volume of a segment consumes the provider's current remaining extent.
                // Earlier volumes keep their requested capacities, while `functional_minimum`
                // remains a separate success condition so legal provider alignment does not become
                // a false exact-size requirement.
                offset_bytes: envelope.offset_bytes,
                size_bytes: if is_final {
                    envelope.length_bytes
                } else {
                    partition.length_bytes.min(envelope.length_bytes)
                },
                kind,
                file_system,
                label: label.to_owned(),
                drive_letter: letter,
                active,
                preserve_gpt_metadata: None,
            };
            Ok(
                lr_core::windows_storage::create_partition_checked_in_envelope(
                    &request,
                    envelope,
                    functional_minimum,
                    &before,
                )?,
            )
        })();
        match result {
            Ok(created) => return Ok(created),
            Err(error) => {
                let unchanged = lr_core::windows_storage::disk_layout_snapshot(disk_number)
                    .is_ok_and(|after| after == before);
                if attempt >= PARTITION_CREATE_ATTEMPTS || !unchanged {
                    return Err(error);
                }
                log::warn!(
                    "[FULL DISK] creating the {:?} volume failed (attempt {attempt}/{PARTITION_CREATE_ATTEMPTS}); the disk layout is unchanged, retrying: {error:#}",
                    partition.role
                );
                std::thread::sleep(post_install_cleanup_retry_delay(attempt));
                attempt += 1;
            }
        }
    }
}

pub(crate) fn is_mbr_container(
    partition: &lr_core::windows_storage::DiskLayoutPartitionSnapshot,
) -> bool {
    matches!(
        partition.token,
        lr_core::windows_storage::DiskLayoutPartitionToken::Mbr {
            partition_type: 0x05 | 0x0F | 0x85,
            ..
        }
    )
}

pub(crate) fn partition_contains_range(
    partition: &lr_core::windows_storage::DiskLayoutPartitionSnapshot,
    offset_bytes: u64,
    end_bytes: u64,
) -> bool {
    partition.offset_bytes <= offset_bytes
        && partition
            .offset_bytes
            .checked_add(partition.size_bytes)
            .is_some_and(|end| end >= end_bytes)
}

/// Delete one exact partition whose content is disposable: the post-install staging extent, or an
/// old partition of a disk the user confirmed for a complete wipe.
///
/// The volume is flushed, locked and dismounted first when possible; VDS then deletes the
/// partition even if some process still has a handle open. A partition that is already gone (for
/// example deleted by an attempt that reported a late error) counts as deleted.
pub(crate) fn delete_disposable_partition(disk_number: u32, offset_bytes: u64) -> Result<()> {
    let present = |snapshot: &lr_core::windows_storage::DiskLayoutSnapshot| {
        snapshot
            .partitions
            .iter()
            .any(|partition| partition.offset_bytes == offset_bytes)
    };
    let before = lr_core::windows_storage::disk_layout_snapshot(disk_number)
        .context("read the disk before deleting a partition")?;
    if !present(&before) {
        return Ok(());
    }
    match lr_core::windows_storage::force_dismount_partition_volume(disk_number, offset_bytes) {
        Ok(Some(true)) | Ok(None) => {}
        Ok(Some(false)) => log::warn!(
            "[FULL DISK] the volume at disk {disk_number} offset {offset_bytes} was still in use; it was dismounted by force before deletion"
        ),
        Err(error) => log::warn!(
            "[FULL DISK] pre-dismount of the volume at disk {disk_number} offset {offset_bytes} failed; the forced deletion continues: {error}"
        ),
    }
    let snapshot = lr_core::windows_storage::disk_layout_snapshot(disk_number)
        .context("re-read the disk before deleting a partition")?;
    if !present(&snapshot) {
        return Ok(());
    }
    lr_core::windows_storage::delete_disposable_partition_checked(
        disk_number,
        offset_bytes,
        true,
        &snapshot,
    )?;
    Ok(())
}

/// Delete every old partition of the staging disk except the exact preserved staging extent.
///
/// The user confirmed that this disk is wiped, so an old volume that some process still has open
/// is dismounted instead of failing the transaction. Several passes handle MBR logical partitions
/// (their container can only go once it is empty) and transient locks. An MBR container that
/// still holds the staging extent is kept until the post-install cleanup.
fn delete_old_partitions_except_staging(
    disk_number: u32,
    staging_offset: u64,
    staging_length: u64,
) -> Result<()> {
    let fresh = lr_core::windows_storage::partitions(disk_number)
        .context("re-read preserved staging disk before deleting old partitions")?;
    let exact = fresh
        .iter()
        .filter(|partition| {
            partition.offset_bytes == staging_offset
                && partition.size_bytes == staging_length
                && partition.kind == PartitionKind::BasicData
        })
        .count();
    if exact != 1 {
        bail!("preserved staging extent changed before the first partition delete");
    }
    let staging_end = staging_offset
        .checked_add(staging_length)
        .context("preserved staging extent end overflows")?;
    let canonical = lr_core::windows_storage::disk_layout_snapshot(disk_number)
        .context("read the canonical layout before deleting old partitions")?;
    let mut pending = canonical
        .partitions
        .iter()
        .filter(|partition| {
            let is_staging =
                partition.offset_bytes == staging_offset && partition.size_bytes == staging_length;
            let holds_staging = is_mbr_container(partition)
                && partition_contains_range(partition, staging_offset, staging_end);
            !(is_staging || holds_staging)
        })
        .map(|partition| partition.offset_bytes)
        .collect::<Vec<_>>();
    // Delete from the end of the disk towards its start: logical partitions go before their
    // container, and the partition numbers of the remaining entries stay stable.
    pending.sort_unstable_by(|left, right| right.cmp(left));
    let mut pass = 1;
    while !pending.is_empty() {
        let mut failures = Vec::new();
        for offset in std::mem::take(&mut pending) {
            if let Err(error) = delete_disposable_partition(disk_number, offset) {
                failures.push((offset, error));
            }
        }
        if failures.is_empty() {
            break;
        }
        if pass >= OLD_PARTITION_DELETE_PASSES {
            let (offset, error) = failures.remove(0);
            return Err(error).with_context(|| {
                format!("delete old partition at offset {offset} on selected disk {disk_number}")
            });
        }
        for (offset, error) in &failures {
            log::warn!(
                "[FULL DISK] old partition at offset {offset} on disk {disk_number} was not deleted in pass {pass}/{OLD_PARTITION_DELETE_PASSES}; retrying: {error:#}"
            );
        }
        pending = failures.into_iter().map(|(offset, _)| offset).collect();
        std::thread::sleep(post_install_cleanup_retry_delay(pass));
        pass += 1;
    }
    Ok(())
}

/// Clean and initialize a selected disk that holds no staging extent. A transient failure is
/// retried against a freshly captured canonical snapshot.
fn clean_and_initialize_selected_disk(disk_number: u32, style: DiskStyle) -> Result<()> {
    let mut attempt = 1;
    loop {
        let result =
            lr_core::windows_storage::disk_layout_snapshot(disk_number).and_then(|snapshot| {
                lr_core::windows_storage::clean_and_initialize_checked(
                    disk_number,
                    &snapshot,
                    style,
                )
            });
        match result {
            Ok(()) => return Ok(()),
            Err(error) if attempt < DISK_CLEAN_ATTEMPTS => {
                log::warn!(
                    "[FULL DISK] cleaning disk {disk_number} failed (attempt {attempt}/{DISK_CLEAN_ATTEMPTS}); retrying with a fresh snapshot: {error}"
                );
                std::thread::sleep(post_install_cleanup_retry_delay(attempt));
                attempt += 1;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("clean and initialize selected disk {disk_number}"))
            }
        }
    }
}

/// Delete the exact preserved staging partition and give its space back.
///
/// This is called only after image application, drivers, unattended setup and boot creation have
/// succeeded. A failure is therefore a post-install warning, never an installation failure. The
/// work is retried with a fresh view of the disk, and every attempt resumes where an earlier one
/// stopped (partition already deleted, recipient already extended).
pub fn cleanup_full_disk_staging(authorization: &FullDiskStagingCleanup) -> Result<()> {
    let mut last_error = None;
    for attempt in 1..=POST_INSTALL_CLEANUP_ATTEMPTS {
        match cleanup_full_disk_staging_attempt(authorization) {
            Ok(()) => {
                log::info!(
                    "[FULL DISK] preserved staging cleanup completed on attempt {attempt}/{POST_INSTALL_CLEANUP_ATTEMPTS}"
                );
                return Ok(());
            }
            Err(error) => {
                log::warn!(
                    "[FULL DISK] preserved staging cleanup attempt {attempt}/{POST_INSTALL_CLEANUP_ATTEMPTS} failed: {error:#}"
                );
                last_error = Some(error);
                if attempt < POST_INSTALL_CLEANUP_ATTEMPTS {
                    std::thread::sleep(post_install_cleanup_retry_delay(attempt));
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("preserved staging cleanup did not run")))
}

fn cleanup_full_disk_staging_attempt(authorization: &FullDiskStagingCleanup) -> Result<()> {
    let disk_number = authorization.disk_number;
    let staging_offset = authorization.staging_offset_bytes;
    let staging_end = staging_offset
        .checked_add(authorization.staging_length_bytes)
        .context("preserved staging extent end overflows")?;
    let recipient = authorization
        .recipient
        .map(|expected| resolve_staging_recipient(disk_number, expected))
        .transpose()?;
    if let Some(recipient) = recipient {
        let recipient_end = recipient
            .identity
            .offset_bytes
            .checked_add(recipient.identity.extent_length_bytes)
            .context("full-disk staging recipient end overflows")?;
        if recipient_end >= staging_end {
            log::info!(
                "[FULL DISK] {}: already covers the former staging extent",
                recipient.letter
            );
            if !authorization.reclaim_trailing_free {
                create_data_volume_best_effort(disk_number, recipient_end);
            }
            return Ok(());
        }
        if recipient_end > staging_offset {
            bail!("the staging recipient volume overlaps the preserved staging extent");
        }
    }
    remove_preserved_staging_partition(
        disk_number,
        staging_offset,
        authorization.staging_length_bytes,
    )?;
    match recipient {
        Some(recipient) => {
            extend_recipient_over_staging(authorization, recipient)?;
            if !authorization.reclaim_trailing_free {
                // A data volume was planned behind the staging extent. If its creation was
                // skipped earlier, create it now instead of leaving that space unallocated.
                create_data_volume_best_effort(disk_number, staging_end);
            }
        }
        None => create_data_volume_best_effort(disk_number, staging_offset),
    }
    Ok(())
}

/// Re-bind the recipient through its drive letter, or through any current access path of the
/// same partition when the letter no longer maps to it. Only the start of the extent must match:
/// an earlier attempt may already have extended the volume.
fn resolve_staging_recipient(
    disk_number: u32,
    expected: StagingRecipient,
) -> Result<StagingRecipient> {
    let same_partition = |identity: VolumeIdentity| {
        identity.disk_number == disk_number
            && identity.offset_bytes == expected.identity.offset_bytes
    };
    if let Ok(identity) = lr_core::windows_storage::volume_identity(expected.letter) {
        if same_partition(identity) {
            return Ok(StagingRecipient {
                letter: expected.letter,
                identity,
            });
        }
    }
    let letters = lr_core::windows_storage::assigned_drive_letters_for_partition(
        disk_number,
        expected.identity.offset_bytes,
    )
    .context("find the current access path of the staging recipient volume")?;
    for letter in letters {
        if let Ok(identity) = lr_core::windows_storage::volume_identity(letter) {
            if same_partition(identity) {
                return Ok(StagingRecipient { letter, identity });
            }
        }
    }
    bail!(
        "the staging recipient volume {}: is no longer reachable through a drive letter",
        expected.letter
    )
}

/// Delete the staging partition (when it still exists) and an MBR container that becomes empty.
fn remove_preserved_staging_partition(
    disk_number: u32,
    staging_offset: u64,
    staging_length: u64,
) -> Result<()> {
    let staging_end = staging_offset
        .checked_add(staging_length)
        .context("preserved staging extent end overflows")?;
    let layout = lr_core::windows_storage::disk_layout_snapshot(disk_number)
        .context("read the staging disk before post-install cleanup")?;
    let mut exact = 0_usize;
    for partition in &layout.partitions {
        if partition.offset_bytes == staging_offset && partition.size_bytes == staging_length {
            exact += 1;
            continue;
        }
        if partition.size_bytes == 0
            || (is_mbr_container(partition)
                && partition_contains_range(partition, staging_offset, staging_end))
        {
            continue;
        }
        if lr_core::custom_install::ranges_overlap(
            partition.offset_bytes,
            partition.size_bytes,
            staging_offset,
            staging_length,
        )
        .unwrap_or(true)
        {
            bail!("another partition now occupies part of the preserved staging extent");
        }
    }
    match exact {
        0 => log::info!(
            "[FULL DISK] the preserved staging partition is already gone; continuing with the space reclamation"
        ),
        1 => delete_disposable_partition(disk_number, staging_offset)
            .context("delete the preserved same-disk staging partition")?,
        _ => bail!("the preserved staging extent is ambiguous"),
    }

    delete_empty_mbr_containers_over(disk_number, staging_offset, staging_end)
}

/// A staging volume that was an MBR logical partition leaves an empty extended container behind.
/// Delete such a container so it cannot block the following extension.
pub(crate) fn delete_empty_mbr_containers_over(
    disk_number: u32,
    range_offset: u64,
    range_end: u64,
) -> Result<()> {
    let layout = lr_core::windows_storage::disk_layout_snapshot(disk_number)
        .context("read the disk after deleting the staging partition")?;
    for container in layout.partitions.iter().filter(|partition| {
        is_mbr_container(partition) && partition_contains_range(partition, range_offset, range_end)
    }) {
        let container_end = container.offset_bytes.saturating_add(container.size_bytes);
        let occupied = layout.partitions.iter().any(|partition| {
            !is_mbr_container(partition)
                && partition.offset_bytes >= container.offset_bytes
                && partition.offset_bytes < container_end
        });
        if !occupied {
            delete_disposable_partition(disk_number, container.offset_bytes)
                .context("delete the empty extended partition that held the staging volume")?;
        }
    }
    Ok(())
}

/// Extend the recipient over the former staging extent (and over the free tail when no partition
/// was planned behind it). Several legal end boundaries are tried, from the largest to the exact
/// staging end, so a provider that rejects the disk-end boundary still returns the staging space.
fn extend_recipient_over_staging(
    authorization: &FullDiskStagingCleanup,
    recipient: StagingRecipient,
) -> Result<()> {
    let disk_number = authorization.disk_number;
    let staging_end = authorization
        .staging_offset_bytes
        .checked_add(authorization.staging_length_bytes)
        .context("preserved staging extent end overflows")?;
    let reclaim = staging_reclaim_length(
        recipient.identity.offset_bytes,
        recipient.identity.extent_length_bytes,
        authorization.staging_offset_bytes,
        authorization.staging_length_bytes,
    )?;
    let canonical_end =
        lr_core::windows_storage::adjacent_free_end_after_volume(recipient.identity)
            .context("read the free range behind the staging recipient")?;
    if canonical_end < staging_end {
        bail!("the former staging range is not completely free after its deletion");
    }
    let mut target_ends = Vec::new();
    if authorization.reclaim_trailing_free && canonical_end > staging_end {
        // VDS reports the last usable sector; a GPT backup table is never part of a free extent.
        let provider_end = lr_core::windows_storage::current_free_extents(disk_number)
            .ok()
            .and_then(|extents| {
                extents
                    .iter()
                    .filter_map(|extent| {
                        let end = extent.offset_bytes.checked_add(extent.length_bytes)?;
                        (extent.offset_bytes <= staging_end && end > staging_end).then_some(end)
                    })
                    .max()
            });
        let trailing_end = provider_end.map_or(canonical_end, |end| end.min(canonical_end));
        for end in [
            trailing_end,
            trailing_end / MIB * MIB,
            trailing_end.saturating_sub(MIB) / MIB * MIB,
        ] {
            if end > staging_end && !target_ends.contains(&end) {
                target_ends.push(end);
            }
        }
    }
    target_ends.push(staging_end);
    log::info!(
        "[FULL DISK] returning {} staging bytes to {}: (candidate ends {:?})",
        reclaim,
        recipient.letter,
        target_ends
    );
    let mut last_error = None;
    for target_end in target_ends {
        let current = lr_core::windows_storage::volume_identity(recipient.letter)
            .context("re-read the staging recipient volume")?;
        if current.disk_number != disk_number
            || current.offset_bytes != recipient.identity.offset_bytes
        {
            bail!("the staging recipient drive letter now maps to a different volume");
        }
        let current_end = current
            .offset_bytes
            .checked_add(current.extent_length_bytes)
            .context("full-disk staging recipient end overflows")?;
        if current_end >= staging_end {
            return Ok(());
        }
        match lr_core::windows_storage::extend_volume_checked(
            recipient.letter,
            current,
            target_end - current_end,
        ) {
            Ok(()) => {
                let actual = lr_core::windows_storage::volume_identity(recipient.letter)
                    .context("read back the extended full-disk recipient volume")?;
                let actual_end = actual
                    .offset_bytes
                    .checked_add(actual.extent_length_bytes)
                    .context("extended recipient end overflows")?;
                if actual.disk_number != disk_number
                    || actual.offset_bytes != current.offset_bytes
                    || actual_end < staging_end
                {
                    bail!("full-disk staging cleanup finished with an unexpected recipient extent");
                }
                log::info!(
                    "[FULL DISK] {}: now ends at {} (former staging end {})",
                    recipient.letter,
                    actual_end,
                    staging_end
                );
                return Ok(());
            }
            Err(error) => {
                log::warn!(
                    "[FULL DISK] extending {}: up to {} failed; trying the next boundary: {error}",
                    recipient.letter,
                    target_end
                );
                last_error = Some(error);
            }
        }
    }
    match last_error {
        Some(error) => Err(error).context("return preserved staging space to the adjacent volume"),
        None => bail!("no extension boundary was attempted for the staging recipient"),
    }
}

/// Turn the free range that contains (or starts right at) `hint_offset` into an NTFS data volume
/// when it holds at least the useful data size. Best effort: a failure only leaves it free.
fn create_data_volume_best_effort(disk_number: u32, hint_offset: u64) {
    let result = (|| -> Result<Option<(char, CreatedPartition)>> {
        let extents = lr_core::windows_storage::current_free_extents(disk_number)
            .context("query free extents for the data volume")?;
        let Some(extent) = extents
            .into_iter()
            .filter(|extent| {
                extent.offset_bytes <= hint_offset.saturating_add(MIB)
                    && extent
                        .offset_bytes
                        .checked_add(extent.length_bytes)
                        .is_some_and(|end| end > hint_offset)
            })
            .max_by_key(|extent| extent.length_bytes)
        else {
            return Ok(None);
        };
        if extent.length_bytes < MIN_USEFUL_DATA_BYTES {
            return Ok(None);
        }
        let letter = first_free_letter(
            lr_core::windows_storage::assigned_drive_letter_mask()
                .context("read assigned drive letters")?,
        )?;
        let snapshot = lr_core::windows_storage::disk_layout_snapshot(disk_number)
            .context("read the disk before creating the data volume")?;
        let (kind, file_system, label, active) = partition_parameters(PlannedPartitionRole::Data);
        let request = CreatePartitionRequest {
            disk_number,
            offset_bytes: extent.offset_bytes,
            size_bytes: extent.length_bytes,
            kind,
            file_system,
            label: label.to_owned(),
            drive_letter: Some(letter),
            active,
            preserve_gpt_metadata: None,
        };
        let created = lr_core::windows_storage::create_partition_checked_in_envelope(
            &request,
            extent,
            MIN_USEFUL_DATA_BYTES,
            &snapshot,
        )?;
        Ok(Some((letter, created)))
    })();
    match result {
        Ok(Some((letter, created))) => log::info!(
            "[FULL DISK] created data volume {letter}: offset={} length={}",
            created.offset_bytes,
            created.size_bytes
        ),
        Ok(None) => {}
        Err(error) => log::warn!(
            "[FULL DISK] the remaining free space could not be turned into a data volume; it stays unallocated: {error:#}"
        ),
    }
}

fn requested_style(style: RequestedPartitionStyle) -> DiskStyle {
    match style {
        RequestedPartitionStyle::Gpt => DiskStyle::Gpt,
        RequestedPartitionStyle::Mbr => DiskStyle::Mbr,
    }
}

fn partition_parameters(
    role: PlannedPartitionRole,
) -> (PartitionKind, Option<FileSystem>, &'static str, bool) {
    match role {
        PlannedPartitionRole::EfiSystem => (
            PartitionKind::EfiSystem,
            Some(FileSystem::Fat32),
            "EFI",
            false,
        ),
        PlannedPartitionRole::MicrosoftReserved => {
            (PartitionKind::MicrosoftReserved, None, "MSR", false)
        }
        PlannedPartitionRole::SystemReserved => (
            PartitionKind::BasicData,
            Some(FileSystem::Ntfs),
            "System Reserved",
            true,
        ),
        PlannedPartitionRole::Windows => (
            PartitionKind::BasicData,
            Some(FileSystem::Ntfs),
            "Windows",
            false,
        ),
        PlannedPartitionRole::Data => (
            PartitionKind::BasicData,
            Some(FileSystem::Ntfs),
            "Data",
            false,
        ),
    }
}

fn letter_bit(letter: char) -> u32 {
    1_u32 << u32::from(letter.to_ascii_uppercase() as u8 - b'A')
}

fn first_free_letter(mask: u32) -> Result<char> {
    (b'C'..=b'Z')
        .map(char::from)
        .find(|letter| mask & letter_bit(*letter) == 0)
        .context("no unused drive letter is available for the full-disk layout")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dual_boot_target_binding_ignores_cross_boot_disk_guid_number_and_geometry_changes() {
        let plan = CustomInstallPlan::DualBoot(lr_core::custom_install::DualBootPlan {
            source_drive_letter: 'C',
            source_offset_bytes: 1_048_576,
            source_length_before_bytes: 300_000_000_000,
            source_length_after_bytes: 200_000_000_123,
            target_offset_bytes: 200_001_049_211,
            target_length_bytes: 80_000_000_321,
            data_offset_bytes: None,
            data_length_bytes: 0,
        });
        assert!(validate_dual_boot_target(
            &plan,
            VolumeIdentity {
                disk_number: 17,
                offset_bytes: 321_123,
                extent_length_bytes: 79_999_999_777,
            },
            VolumeIdentity {
                disk_number: 23,
                offset_bytes: 8_192,
                extent_length_bytes: 9_000_000_123,
            }
        )
        .is_ok());
    }

    #[test]
    fn dual_boot_target_binding_rejects_empty_overflowing_or_overlapping_current_extents() {
        let plan = CustomInstallPlan::DualBoot(lr_core::custom_install::DualBootPlan {
            source_drive_letter: 'C',
            source_offset_bytes: 1_048_576,
            source_length_before_bytes: 300_000_000_000,
            source_length_after_bytes: 200_000_000_123,
            target_offset_bytes: 200_001_049_211,
            target_length_bytes: 80_000_000_321,
            data_offset_bytes: None,
            data_length_bytes: 0,
        });
        let data = VolumeIdentity {
            disk_number: 9,
            offset_bytes: 10_000,
            extent_length_bytes: 20_000,
        };
        assert!(validate_dual_boot_target(
            &plan,
            VolumeIdentity {
                disk_number: 8,
                offset_bytes: 1,
                extent_length_bytes: 0,
            },
            data,
        )
        .is_err());
        assert!(validate_dual_boot_target(
            &plan,
            VolumeIdentity {
                disk_number: 8,
                offset_bytes: u64::MAX - 10,
                extent_length_bytes: 20,
            },
            data,
        )
        .is_err());
        assert!(validate_dual_boot_target(
            &plan,
            VolumeIdentity {
                disk_number: 9,
                offset_bytes: 25_000,
                extent_length_bytes: 10_000,
            },
            data,
        )
        .is_err());
    }

    #[test]
    fn full_disk_staging_rebinds_to_the_random_data_markers_current_extent() {
        let plan = RepartitionAllDisksPlan {
            disks: vec![lr_core::custom_install::FullDiskSelection {
                diagnostic_disk_number: 1,
                locator_token: "selected-disk-token".into(),
                style: RequestedPartitionStyle::Gpt,
                role: FullDiskRole::Windows,
            }],
            windows_partition_bytes: 80 * lr_core::custom_install::GIB,
            preserved_staging: Some(lr_core::custom_install::PreservedStagingExtent {
                disk_locator_token: "selected-disk-token".into(),
                offset_bytes: 900_000_000_000,
                length_bytes: 10_000_000_000,
            }),
        };
        let targets = vec![FullDiskExecutionTarget {
            locator_token: "selected-disk-token".into(),
            diagnostic_disk_number: 1,
            role: FullDiskRole::Windows,
            partition: r"\\?\Volume{test}\".into(),
            expected: VolumeIdentity {
                disk_number: 17,
                offset_bytes: 4096,
                extent_length_bytes: 123_456_789,
            },
        }];
        let current_data = VolumeIdentity {
            disk_number: 17,
            offset_bytes: 700_000_321,
            extent_length_bytes: 9_999_999_777,
        };
        let rebound = rebind_current_preserved_staging(&plan, &targets, current_data).unwrap();
        let staging = rebound.preserved_staging.unwrap();
        assert_eq!(staging.offset_bytes, current_data.offset_bytes);
        assert_eq!(staging.length_bytes, current_data.extent_length_bytes);
        assert_eq!(staging.disk_locator_token, "selected-disk-token");
    }

    #[test]
    fn full_disk_staging_rebind_rejects_an_unrelated_or_ambiguous_current_disk() {
        let plan = RepartitionAllDisksPlan {
            disks: vec![],
            windows_partition_bytes: 1,
            preserved_staging: Some(lr_core::custom_install::PreservedStagingExtent {
                disk_locator_token: "wanted".into(),
                offset_bytes: 1,
                length_bytes: 1,
            }),
        };
        let target = |token: &str| FullDiskExecutionTarget {
            locator_token: token.into(),
            diagnostic_disk_number: 1,
            role: FullDiskRole::Windows,
            partition: r"\\?\Volume{test}\".into(),
            expected: VolumeIdentity {
                disk_number: 17,
                offset_bytes: 4096,
                extent_length_bytes: 123_456_789,
            },
        };
        let data = VolumeIdentity {
            disk_number: 17,
            offset_bytes: 700_000_321,
            extent_length_bytes: 9_999_999_777,
        };
        assert!(rebind_current_preserved_staging(&plan, &[target("other")], data).is_err());
        assert!(rebind_current_preserved_staging(
            &plan,
            &[target("wanted"), target("wanted")],
            data,
        )
        .is_err());
    }

    #[test]
    fn staging_cleanup_includes_a_legal_non_mib_tail_gap() {
        let recipient_offset = 1_048_576;
        let recipient_length = 60 * 1024 * 1024 * 1024;
        let gap = 63 * 1024;
        let staging_offset = recipient_offset + recipient_length + gap;
        let staging_length = 8 * 1024 * 1024 * 1024 + 512;
        assert_eq!(
            staging_reclaim_length(
                recipient_offset,
                recipient_length,
                staging_offset,
                staging_length,
            )
            .unwrap(),
            gap + staging_length
        );
    }

    #[test]
    fn hidden_gpt_partitions_do_not_block_the_mounted_staging_recipient() {
        let disk_number = 0;
        let staging_offset = 127_307_612_160;
        let mut selected = None;

        // ESP and MSR intentionally have no drive letter. The production failure captured on
        // 2026-08-14 stopped on the first of these instead of continuing to the Windows volume.
        consider_staging_cleanup_recipient(
            &mut selected,
            staging_offset,
            1_048_576,
            300 * 1024 * 1024,
            None,
        )
        .unwrap();
        consider_staging_cleanup_recipient(
            &mut selected,
            staging_offset,
            315_638_271,
            128 * 1024 * 1024,
            None,
        )
        .unwrap();
        assert!(selected.is_none());

        let windows = VolumeIdentity {
            disk_number,
            offset_bytes: 449_855_999,
            extent_length_bytes: 21_126_799_329,
        };
        consider_staging_cleanup_recipient(
            &mut selected,
            staging_offset,
            windows.offset_bytes,
            windows.extent_length_bytes,
            Some(('C', windows)),
        )
        .unwrap();

        // A later hidden recovery/infrastructure partition must not erase the valid ordinary
        // recipient already selected.
        consider_staging_cleanup_recipient(
            &mut selected,
            staging_offset,
            126_900_000_123,
            300_000_321,
            None,
        )
        .unwrap();
        // A volume behind the staging extent can never receive it.
        consider_staging_cleanup_recipient(
            &mut selected,
            staging_offset,
            staging_offset + 10_130_292_736,
            90_000_000_000,
            Some((
                'D',
                VolumeIdentity {
                    disk_number,
                    offset_bytes: staging_offset + 10_130_292_736,
                    extent_length_bytes: 90_000_000_000,
                },
            )),
        )
        .unwrap();
        let selected = selected.unwrap();
        assert_eq!(selected.letter, 'C');
        assert_eq!(selected.identity, windows);
    }

    #[test]
    fn later_mounted_volume_replaces_an_earlier_staging_recipient() {
        let mut selected = None;
        let windows = VolumeIdentity {
            disk_number: 3,
            offset_bytes: 449_855_999,
            extent_length_bytes: 21_126_799_329,
        };
        let data = VolumeIdentity {
            disk_number: 3,
            offset_bytes: 21_576_655_777,
            extent_length_bytes: 90_000_000_123,
        };
        for (letter, identity) in [('C', windows), ('D', data)] {
            consider_staging_cleanup_recipient(
                &mut selected,
                127_307_612_160,
                identity.offset_bytes,
                identity.extent_length_bytes,
                Some((letter, identity)),
            )
            .unwrap();
        }
        let selected = selected.unwrap();
        assert_eq!(selected.letter, 'D');
        assert_eq!(selected.identity, data);
    }

    #[test]
    fn partition_segments_follow_the_preserved_staging_extent() {
        let staging = Some((200 * MIB, 300 * MIB));
        let front = PlannedPartition {
            offset_bytes: MIB,
            length_bytes: 100 * MIB,
            role: PlannedPartitionRole::Windows,
        };
        let back = PlannedPartition {
            offset_bytes: 300 * MIB,
            length_bytes: 100 * MIB,
            role: PlannedPartitionRole::Data,
        };
        assert_eq!(
            partition_segment(&front, 1_000 * MIB, staging),
            (0, 200 * MIB)
        );
        assert_eq!(
            partition_segment(&back, 1_000 * MIB, staging),
            (300 * MIB, 1_000 * MIB)
        );
        assert_eq!(
            partition_segment(&back, 1_000 * MIB, None),
            (0, 1_000 * MIB)
        );
    }

    #[test]
    fn current_extent_is_limited_to_the_segment_behind_staging() {
        let front = FreeExtent {
            offset_bytes: MIB,
            length_bytes: 199 * MIB,
        };
        let back = FreeExtent {
            offset_bytes: 300 * MIB + 512,
            length_bytes: 700 * MIB - 512,
        };
        assert_eq!(
            select_current_free_extent(&[front, back], 300 * MIB, 1_000 * MIB, 10 * MIB, 0)
                .unwrap()
                .unwrap(),
            back
        );
        assert_eq!(
            select_current_free_extent(&[front, back], 0, 200 * MIB, 10 * MIB, 0)
                .unwrap()
                .unwrap(),
            front
        );
        // A provider extent that still spans the staging boundary is clipped to the segment.
        let spanning = FreeExtent {
            offset_bytes: 150 * MIB,
            length_bytes: 400 * MIB,
        };
        assert_eq!(
            select_current_free_extent(&[spanning], 300 * MIB, 1_000 * MIB, 10 * MIB, 0)
                .unwrap()
                .unwrap(),
            FreeExtent {
                offset_bytes: 300 * MIB,
                length_bytes: 250 * MIB,
            }
        );
    }

    #[test]
    fn final_volume_uses_the_largest_current_provider_extent_within_the_authorized_end() {
        let extents = [
            FreeExtent {
                offset_bytes: 4096,
                length_bytes: 63 * 1024,
            },
            FreeExtent {
                offset_bytes: 2_000_123,
                length_bytes: 90_000_321,
            },
            FreeExtent {
                offset_bytes: 200_000_000,
                length_bytes: 900_000_000,
            },
        ];
        assert_eq!(
            select_current_free_extent(&extents, 0, 100_000_444, 80_000_000, 0)
                .unwrap()
                .unwrap(),
            extents[1]
        );
        assert!(
            select_current_free_extent(&extents, 0, 100_000_444, 91_000_000, 0)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn current_extent_is_intersected_and_reserves_following_boot_minimums() {
        let extent = FreeExtent {
            offset_bytes: 4096,
            length_bytes: 1_000_000,
        };
        assert_eq!(
            select_current_free_extent(&[extent], 0, 900_123, 300_000, 400_000)
                .unwrap()
                .unwrap(),
            FreeExtent {
                offset_bytes: 4096,
                length_bytes: 496_027,
            }
        );
        assert!(
            select_current_free_extent(&[extent], 0, 700_000, 300_000, 400_000)
                .unwrap()
                .is_none()
        );
        assert!(select_current_free_extent(&[extent], 0, u64::MAX, u64::MAX, 1).is_err());
    }

    #[test]
    fn full_disk_functional_minimum_uses_sector_and_cross_version_requirements() {
        let image_minimum = 20 * 1024 * 1024 * 1024;
        assert_eq!(
            partition_functional_minimum(PlannedPartitionRole::Windows, image_minimum, None)
                .unwrap(),
            image_minimum
        );
        assert_eq!(
            partition_functional_minimum(PlannedPartitionRole::EfiSystem, image_minimum, Some(512))
                .unwrap(),
            ESP_512_MINIMUM_BYTES
        );
        assert_eq!(
            partition_functional_minimum(
                PlannedPartitionRole::EfiSystem,
                image_minimum,
                Some(4096)
            )
            .unwrap(),
            ESP_4KN_MINIMUM_BYTES
        );
        assert!(
            partition_functional_minimum(PlannedPartitionRole::EfiSystem, image_minimum, None)
                .is_err()
        );
        assert!(partition_functional_minimum(
            PlannedPartitionRole::EfiSystem,
            image_minimum,
            Some(2048)
        )
        .is_err());
        assert_eq!(
            partition_functional_minimum(
                PlannedPartitionRole::MicrosoftReserved,
                image_minimum,
                None
            )
            .unwrap(),
            MSR_WINDOWS_7_MINIMUM_BYTES
        );
        assert_eq!(
            partition_functional_minimum(PlannedPartitionRole::SystemReserved, image_minimum, None)
                .unwrap(),
            BIOS_SYSTEM_FUNCTIONAL_MINIMUM_BYTES
        );
        assert_eq!(
            partition_functional_minimum(PlannedPartitionRole::Data, image_minimum, None).unwrap(),
            MIN_USEFUL_DATA_BYTES
        );
    }

    #[test]
    fn optional_data_does_not_take_budget_from_windows() {
        let future = [PlannedPartition {
            offset_bytes: 123,
            length_bytes: MIN_USEFUL_DATA_BYTES,
            role: PlannedPartitionRole::Data,
        }];
        assert_eq!(
            following_required_minimum(&future, 20 * 1024 * 1024 * 1024, None, false).unwrap(),
            0
        );
        assert_eq!(
            following_required_minimum(&future, 20 * 1024 * 1024 * 1024, None, true).unwrap(),
            MIN_USEFUL_DATA_BYTES
        );
    }

    #[test]
    fn dual_boot_rollback_rejects_source_rebinding_after_tail_deletes() {
        let expected = VolumeIdentity {
            disk_number: 7,
            offset_bytes: 1_048_576 + 512,
            extent_length_bytes: 80_000_000_321,
        };
        assert!(ensure_rollback_source_unchanged(expected, expected).is_ok());
        assert!(ensure_rollback_source_unchanged(
            expected,
            VolumeIdentity {
                disk_number: 8,
                ..expected
            }
        )
        .is_err());
        assert!(ensure_rollback_source_unchanged(
            expected,
            VolumeIdentity {
                extent_length_bytes: expected.extent_length_bytes - 512,
                ..expected
            }
        )
        .is_err());
    }

    #[test]
    fn preserved_staging_offsets_are_compared_only_on_the_same_current_disk() {
        let staging = lr_core::custom_install::PreservedStagingExtent {
            disk_locator_token: "data".into(),
            offset_bytes: 10_000,
            length_bytes: 20_000,
        };
        let target = VolumeIdentity {
            disk_number: 7,
            offset_bytes: 15_000,
            extent_length_bytes: 2_000,
        };
        assert!(validate_preserved_staging_bounds(&staging, 100_000, 8, target).is_ok());
        assert!(validate_preserved_staging_bounds(&staging, 100_000, 7, target).is_err());
        assert!(validate_preserved_staging_bounds(&staging, 29_999, 8, target).is_err());
    }
}
