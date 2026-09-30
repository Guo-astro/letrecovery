//! Read-only analysis boundary for the native "lossless expand C:" dialog.
//!
//! This preserves the legacy capacity calculation, including the distinction between adjacent
//! unallocated space (pure extend) and space which requires moving the following data partition.
//! It never writes a partition table, shrinks or moves a volume, prepares PE, or restarts Windows.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NativeExpandCAnalysis {
    pub found: bool,
    pub disk: Option<crate::core::native_quick_partition::DiskFingerprint>,
    pub partition_number: u32,
    pub current_size_mb: u64,
    pub used_mb: u64,
    pub free_mb: u64,
    pub max_size_mb: u64,
    pub no_move_max_mb: u64,
    pub can_expand: bool,
    pub reason: String,
    /// Ways to reach beyond the adjacent space, nearest donor first. Each option moves its donor
    /// (and every partition between it and the target) to the right, shrinking the donor first
    /// when needed. Empty when only adjacent space is available.
    pub move_options: Vec<NativeExpandMoveDonor>,
}

/// A partition between the target and the donor that is carried along by the same distance.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NativeExpandMovedPartition {
    pub partition_number: u32,
    pub offset_bytes: u64,
    pub size_bytes: u64,
    pub drive_letter: Option<char>,
    pub is_recovery: bool,
}

/// At most this many partitions are carried along between the target and the donor.
const MAX_MOVED_PARTITIONS: usize = 3;

/// Identity and capacity of the movable data partition behind the target, as analysed in
/// Windows. The WinPE executor refuses to write unless it finds exactly this partition again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NativeExpandMoveDonor {
    pub partition_number: u32,
    pub offset_bytes: u64,
    pub size_bytes: u64,
    pub drive_letter: char,
    /// Bytes the donor file system can give up: Windows' own shrink limit, capped so the donor
    /// keeps a comfortable amount of free space.
    pub shrinkable_bytes: u64,
    /// Unallocated bytes directly behind the donor.
    pub free_after_bytes: u64,
    /// Partitions between the target and this donor that move along, in disk order.
    pub moved_before: Vec<NativeExpandMovedPartition>,
    /// Largest final target size (MB) this option can reach.
    pub reach_mb: u64,
}

impl NativeExpandMoveDonor {
    /// (number, offset, size) of the carried partitions, the identity pinned for WinPE.
    pub fn moved_identity(&self) -> Vec<(u32, u64, u64)> {
        self.moved_before
            .iter()
            .map(|moved| (moved.partition_number, moved.offset_bytes, moved.size_bytes))
            .collect()
    }

    /// Human-readable list of every partition this option moves, in disk order.
    pub fn describe(&self) -> String {
        let mut names: Vec<String> = self
            .moved_before
            .iter()
            .map(|moved| match moved.drive_letter {
                Some(letter) if !moved.is_recovery => format!("{}:", letter),
                _ => crate::tr!("恢复分区"),
            })
            .collect();
        names.push(format!("{}:", self.drive_letter));
        names.join(&crate::tr!("、"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NativeExpandCAnalysisError {
    #[error("开发测试构建禁止读取宿主磁盘扩容布局")]
    DisabledInDevelopment,
    #[error("无法确定当前运行的 Windows 卷: {0}")]
    CurrentWindowsVolume(String),
}

/// Reads the current disk inventory and computes the established expand-C safety limits.
/// The returned snapshot is advisory: the eventual PE handoff must enumerate again and compare a
/// stable disk/partition fingerprint before writing anything.
#[cfg(feature = "non-elevated-tests")]
pub fn analyze_expand_c() -> Result<NativeExpandCAnalysis, NativeExpandCAnalysisError> {
    Err(NativeExpandCAnalysisError::DisabledInDevelopment)
}

#[cfg(feature = "non-elevated-tests")]
pub fn analyze_expand_partition(
    _target_letter: char,
) -> Result<NativeExpandCAnalysis, NativeExpandCAnalysisError> {
    Err(NativeExpandCAnalysisError::DisabledInDevelopment)
}

#[cfg(not(feature = "non-elevated-tests"))]
pub fn analyze_expand_c() -> Result<NativeExpandCAnalysis, NativeExpandCAnalysisError> {
    let drive = lr_core::windows_storage::current_windows_drive_letter()
        .map_err(|error| NativeExpandCAnalysisError::CurrentWindowsVolume(error.to_string()))?;
    analyze_expand_partition(drive)
}

#[cfg(not(feature = "non-elevated-tests"))]
pub fn analyze_expand_partition(
    target_letter: char,
) -> Result<NativeExpandCAnalysis, NativeExpandCAnalysisError> {
    use crate::core::quick_partition::get_physical_disks;

    const BYTES_PER_MB: u64 = 1024 * 1024;
    let target_letter = target_letter.to_ascii_uppercase();
    let disks = get_physical_disks();
    let Some((disk, target_index)) = disks.iter().find_map(|disk| {
        disk.partitions
            .iter()
            .position(|partition| {
                partition
                    .drive_letter
                    .is_some_and(|letter| letter.eq_ignore_ascii_case(&target_letter))
            })
            .map(|index| (disk, index))
    }) else {
        return Ok(NativeExpandCAnalysis {
            reason: crate::tr!("未找到目标分区 {}:", target_letter),
            ..Default::default()
        });
    };

    let target_partition = &disk.partitions[target_index];
    let current_size_mb = target_partition.size_bytes / BYTES_PER_MB;
    let target_end = target_partition
        .offset_bytes
        .saturating_add(target_partition.size_bytes);
    let mut following: Vec<_> = disk
        .partitions
        .iter()
        .filter(|partition| partition.offset_bytes >= target_end)
        .collect();
    following.sort_by_key(|partition| partition.offset_bytes);

    let unallocated_after_bytes = following.first().map_or_else(
        || disk.size_bytes.saturating_sub(target_end),
        |next| next.offset_bytes.saturating_sub(target_end),
    );
    let unallocated_after_mb = unallocated_after_bytes / BYTES_PER_MB;
    let no_move_max_mb = current_size_mb.saturating_add(unallocated_after_mb);
    // Space inside or behind a data partition further back is reached by moving that partition
    // to the right (shrinking it first when needed) together with every partition between it and
    // the target, for example the recovery partition Windows places right behind C:. Each donor
    // candidate is one option; the dialog takes the first that reaches the requested size, which
    // moves the least data. The moves themselves run in WinPE.
    let mut move_options: Vec<NativeExpandMoveDonor> = Vec::new();
    let mut move_blocker = None;
    let mut carried: Vec<NativeExpandMovedPartition> = Vec::new();
    for next in following.iter().take(MAX_MOVED_PARTITIONS + 1) {
        if let Ok(mut donor) = plan_move_donor(disk, next, &following) {
            donor.moved_before = carried.clone();
            donor.reach_mb = current_size_mb.saturating_add(
                unallocated_after_bytes
                    .saturating_add(donor.free_after_bytes)
                    .saturating_add(donor.shrinkable_bytes)
                    / BYTES_PER_MB,
            );
            // Moving data is only worth its risk when it gains at least 1 GB, and a donor further
            // back is only offered when it reaches further than the nearer ones.
            if donor.reach_mb > no_move_max_mb.saturating_add(1024)
                && move_options
                    .last()
                    .is_none_or(|nearer| donor.reach_mb > nearer.reach_mb)
            {
                move_options.push(donor);
            }
        }
        match carry_along(disk, next) {
            Ok(moved) => carried.push(moved),
            Err(blocker) => {
                if move_options.is_empty() {
                    move_blocker = Some(blocker);
                }
                break;
            }
        }
    }
    let max_size_mb = move_options
        .iter()
        .map(|option| option.reach_mb)
        .max()
        .unwrap_or(0)
        .max(no_move_max_mb);
    let can_expand = max_size_mb > current_size_mb.saturating_add(1024);
    let reason = if can_expand {
        String::new()
    } else if let Some(blocker) = move_blocker {
        crate::tr!(
            "分区 {}: 后方没有未分配空间，而且{}。",
            target_letter,
            blocker
        )
    } else {
        crate::tr!("分区 {}: 后方没有可用于扩容的空间。", target_letter)
    };

    Ok(NativeExpandCAnalysis {
        found: true,
        disk: Some(crate::core::native_quick_partition::DiskFingerprint::from(
            disk,
        )),
        partition_number: target_partition.partition_number,
        current_size_mb,
        used_mb: target_partition.used_bytes / BYTES_PER_MB,
        free_mb: target_partition.free_bytes / BYTES_PER_MB,
        max_size_mb,
        no_move_max_mb,
        can_expand,
        reason,
        move_options,
    })
}

/// Checks whether a partition behind the target may be moved along to reach a donor further
/// back: a GPT recovery partition (moved as a block with its GPT identity kept) or a movable
/// NTFS data partition.
#[cfg(not(feature = "non-elevated-tests"))]
fn carry_along(
    disk: &crate::core::quick_partition::PhysicalDisk,
    partition: &crate::core::quick_partition::DiskPartitionInfo,
) -> Result<NativeExpandMovedPartition, String> {
    if partition.is_recovery && !partition.is_esp && !partition.is_msr {
        if !matches!(disk.partition_style, crate::core::disk::PartitionStyle::GPT) {
            return Err(crate::tr!(
                "紧挨在后面的恢复分区在 MBR 磁盘上，暂不支持连同它一起移动"
            ));
        }
    } else {
        movable_data_letter(disk, partition)?;
    }
    Ok(NativeExpandMovedPartition {
        partition_number: partition.partition_number,
        offset_bytes: partition.offset_bytes,
        size_bytes: partition.size_bytes,
        drive_letter: partition
            .drive_letter
            .map(|letter| letter.to_ascii_uppercase()),
        is_recovery: partition.is_recovery,
    })
}

/// The checks every moved data partition must pass; returns its drive letter.
#[cfg(not(feature = "non-elevated-tests"))]
fn movable_data_letter(
    disk: &crate::core::quick_partition::PhysicalDisk,
    next: &crate::core::quick_partition::DiskPartitionInfo,
) -> Result<char, String> {
    if next.is_esp || next.is_msr || next.is_recovery {
        return Err(crate::tr!(
            "紧挨在后面的是 ESP/MSR/恢复这类系统分区，为安全起见不会移动它"
        ));
    }
    let Some(letter) = next.drive_letter.map(|letter| letter.to_ascii_uppercase()) else {
        return Err(crate::tr!("紧挨在后面的分区没有盘符，无法安全移动"));
    };
    if !next.file_system.eq_ignore_ascii_case("NTFS") {
        return Err(crate::tr!("后方分区 {}: 不是 NTFS 分区，无法移动", letter));
    }
    if matches!(disk.partition_style, crate::core::disk::PartitionStyle::MBR)
        && !next.partition_type.eq_ignore_ascii_case("0x07")
    {
        return Err(crate::tr!(
            "后方分区 {}: 在扩展分区里或类型不受支持，无法移动",
            letter
        ));
    }
    if let Ok(api) = lr_core::fveapi::FveApi::instance() {
        if let Ok(info) = api.get_status_by_path(&format!("{}:\\", letter)) {
            if !matches!(
                crate::core::bitlocker::VolumeStatus::from(&info),
                crate::core::bitlocker::VolumeStatus::NotEncrypted
            ) {
                return Err(crate::tr!(
                    "后方分区 {}: 启用了 BitLocker，请先解密后再扩容",
                    letter
                ));
            }
        }
    }
    Ok(letter)
}

/// Checks whether the partition right behind the target may be moved, and how much it can give.
#[cfg(not(feature = "non-elevated-tests"))]
fn plan_move_donor(
    disk: &crate::core::quick_partition::PhysicalDisk,
    next: &crate::core::quick_partition::DiskPartitionInfo,
    following: &[&crate::core::quick_partition::DiskPartitionInfo],
) -> Result<NativeExpandMoveDonor, String> {
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;
    let letter = movable_data_letter(disk, next)?;
    let next_end = next.offset_bytes.saturating_add(next.size_bytes);
    let boundary = following
        .iter()
        .filter(|partition| partition.offset_bytes > next.offset_bytes)
        .map(|partition| partition.offset_bytes)
        .min()
        .unwrap_or(disk.size_bytes);
    let free_after_bytes = boundary.saturating_sub(next_end);
    // The donor stays comfortably usable: at least 2 GB or 5% of its size remains free.
    let reserve = (2 * GIB).max(next.size_bytes / 20);
    let by_free_space = next.free_bytes.saturating_sub(reserve);
    // Files NTFS cannot move (MFT, page file, ...) make the real shrink limit smaller than the
    // free space. Ask Windows for its own limit; without it, plan only half of the free space.
    let shrinkable_bytes = match lr_core::windows_storage::query_max_reclaimable_bytes(letter) {
        Ok(reclaimable) => reclaimable.min(by_free_space),
        Err(error) => {
            log::warn!("[EXPAND] shrink capacity of {letter}: unavailable: {error}");
            by_free_space / 2
        }
    } / MIB
        * MIB;
    Ok(NativeExpandMoveDonor {
        partition_number: next.partition_number,
        offset_bytes: next.offset_bytes,
        size_bytes: next.size_bytes,
        drive_letter: letter,
        shrinkable_bytes,
        free_after_bytes,
        moved_before: Vec::new(),
        reach_mb: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_analysis_is_fail_closed() {
        let analysis = NativeExpandCAnalysis::default();
        assert!(!analysis.found);
        assert!(!analysis.can_expand);
        assert_eq!(analysis.max_size_mb, 0);
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn development_build_refuses_host_disk_inventory() {
        assert_eq!(
            analyze_expand_c(),
            Err(NativeExpandCAnalysisError::DisabledInDevelopment)
        );
    }
}
