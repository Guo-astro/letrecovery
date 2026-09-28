//! 分散暂存：不新建数据分区，直接把经 PE 安装所需的文件放到已有分区上。
//!
//! 使用场景：
//! - `config.json` 中 `scattered_staging_enabled` 为 true，直接使用本模式，不调用 VDS 与存储管理 API；
//! - 或者缩卷新建数据分区失败（例如 VDS/存储管理 API 一调用就报错）、又没有任何分区能单独放下全部
//!   文件时，自动改用本模式。
//!
//! 规则：
//! - 目标分区一定排除（PE 里会被格式化或覆盖）；PE 环境中的 X: 也排除；
//! - 固定磁盘上的分区参与；读不到驱动器类型的分区也参与；已知是光驱、网络盘、可移动盘、内存盘的不参与；
//! - 先看有没有一个分区能装下全部文件，有就只用它，不分散；
//! - 否则按需分散：镜像优先放进一个能装下它的分区；实在没有，就把镜像按原始字节切成若干块分别放，
//!   PE 在格式化后的目标分区上按顺序拼回原文件并核对 SHA-256，不需要任何转换或中间文件；
//! - 驱动、用户驱动、预装软件以“一个驱动包/一个安装包”为单位按需放到有空间的分区。
//!
//! 所有探测类失败（读不到类型、读不到文件系统、读不到 BitLocker 状态等）都只记 warn，不中断安装。

use anyhow::{Context, Result};
use lr_core::data_staging::{
    choose_volume_for_unit, scatter_reserve_bytes, scatter_root_name, ScatterImagePlacement,
    ScatterPlan, ScatterVolume, SCATTER_DATA_DIRECTORY, SCATTER_MARKER_NAME,
};
use std::path::{Path, PathBuf};

use super::bitlocker::{BitLockerManager, VolumeStatus};

/// 一个可承载分散文件的已有分区（本次清点结果）。
#[derive(Clone, Debug)]
pub(crate) struct InventoryVolume {
    pub letter: char,
    pub free_bytes: u64,
    pub max_file_bytes: u64,
    pub file_system: Option<String>,
}

impl InventoryVolume {
    pub(crate) fn as_scatter_volume(&self) -> ScatterVolume {
        ScatterVolume {
            letter: self.letter,
            free_bytes: self.free_bytes,
            max_file_bytes: self.max_file_bytes,
        }
    }
}

#[cfg(windows)]
mod platform {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDriveTypeW, GetVolumeInformationW,
    };

    pub(super) const DRIVE_UNKNOWN: u32 = 0;
    pub(super) const DRIVE_NO_ROOT_DIR: u32 = 1;
    pub(super) const DRIVE_FIXED: u32 = 3;
    const FILE_READ_ONLY_VOLUME: u32 = 0x0008_0000;

    fn wide(path: &str) -> Vec<u16> {
        path.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub(super) fn drive_type(root: &str) -> u32 {
        let path = wide(root);
        unsafe { GetDriveTypeW(PCWSTR(path.as_ptr())) }
    }

    /// 调用者可用的空闲字节（配额感知）。
    pub(super) fn free_bytes(root: &str) -> Result<u64, String> {
        let path = wide(root);
        let mut available = 0_u64;
        unsafe { GetDiskFreeSpaceExW(PCWSTR(path.as_ptr()), Some(&mut available), None, None) }
            .map_err(|error| error.to_string())?;
        Ok(available)
    }

    /// 卷的文件系统总字节数。
    pub(super) fn total_bytes(root: &str) -> Result<u64, String> {
        let path = wide(root);
        let mut total = 0_u64;
        unsafe { GetDiskFreeSpaceExW(PCWSTR(path.as_ptr()), None, Some(&mut total), None) }
            .map_err(|error| error.to_string())?;
        Ok(total)
    }

    /// (文件系统名, 是否只读)。读不到时返回错误文本。
    pub(super) fn volume_information(root: &str) -> Result<(String, bool), String> {
        let path = wide(root);
        let mut file_system = [0_u16; 64];
        let mut flags = 0_u32;
        unsafe {
            GetVolumeInformationW(
                PCWSTR(path.as_ptr()),
                None,
                None,
                None,
                Some(&mut flags),
                Some(&mut file_system),
            )
        }
        .map_err(|error| error.to_string())?;
        let name = String::from_utf16_lossy(&file_system)
            .trim_end_matches('\0')
            .trim()
            .to_owned();
        Ok((name, flags & FILE_READ_ONLY_VOLUME != 0))
    }
}

#[cfg(not(windows))]
mod platform {
    pub(super) const DRIVE_UNKNOWN: u32 = 0;
    pub(super) const DRIVE_NO_ROOT_DIR: u32 = 1;
    pub(super) const DRIVE_FIXED: u32 = 3;

    pub(super) fn drive_type(_root: &str) -> u32 {
        DRIVE_NO_ROOT_DIR
    }

    pub(super) fn free_bytes(_root: &str) -> Result<u64, String> {
        Err("free-space query requires Windows".to_owned())
    }

    pub(super) fn volume_information(_root: &str) -> Result<(String, bool), String> {
        Err("volume information requires Windows".to_owned())
    }

    pub(super) fn total_bytes(_root: &str) -> Result<u64, String> {
        Err("volume size query requires Windows".to_owned())
    }
}

/// 分区文件系统总容量；读不到时返回 None（调用方只记 warn）。
pub(crate) fn volume_total_bytes(letter: char) -> Option<u64> {
    match platform::total_bytes(&format!("{letter}:\\")) {
        Ok(value) => Some(value),
        Err(error) => {
            log::warn!("[SCATTER] 读取 {letter}: 总容量失败: {error}");
            None
        }
    }
}

/// 分区当前空闲空间；读不到时返回 None。
pub(crate) fn volume_free_bytes(letter: char) -> Option<u64> {
    platform::free_bytes(&format!("{letter}:\\")).ok()
}

/// 驱动器类型是否允许承载分散文件：固定磁盘允许；类型读不到（DRIVE_UNKNOWN）也允许。
pub(crate) fn drive_type_is_eligible(drive_type: u32) -> bool {
    matches!(drive_type, platform::DRIVE_FIXED | platform::DRIVE_UNKNOWN)
}

/// 清点所有可用于分散暂存的已有分区。任何单项探测失败只记 warn：
/// 能确定不可用（不存在、光驱/网络盘/可移动盘、只读、已加密且可能在 PE 中锁定、读不到空闲空间）的才跳过。
pub(crate) fn inventory_existing_volumes(excluded: &[char]) -> Vec<InventoryVolume> {
    let bitlocker = BitLockerManager::new();
    let mut volumes = Vec::new();
    for letter in 'A'..='Z' {
        if excluded
            .iter()
            .any(|excluded| excluded.eq_ignore_ascii_case(&letter))
        {
            continue;
        }
        let root = format!("{letter}:\\");
        let drive_type = platform::drive_type(&root);
        if drive_type == platform::DRIVE_NO_ROOT_DIR {
            continue;
        }
        if !drive_type_is_eligible(drive_type) {
            log::info!(
                "[SCATTER] 跳过 {letter}:：驱动器类型 {drive_type} 不是固定磁盘，也不是未知类型"
            );
            continue;
        }
        if drive_type == platform::DRIVE_UNKNOWN {
            log::warn!("[SCATTER] {letter}: 的驱动器类型读取不到，按规则仍作为候选分区");
        }
        let free_bytes = match platform::free_bytes(&root) {
            Ok(value) => value,
            Err(error) => {
                log::warn!("[SCATTER] 跳过 {letter}:：读取空闲空间失败: {error}");
                continue;
            }
        };
        let file_system = match platform::volume_information(&root) {
            Ok((name, read_only)) => {
                if read_only {
                    log::warn!("[SCATTER] 跳过 {letter}:：卷为只读");
                    continue;
                }
                (!name.is_empty()).then_some(name)
            }
            Err(error) => {
                log::warn!(
                    "[SCATTER] {letter}: 的文件系统信息读取失败，按无单文件大小限制处理: {error}"
                );
                None
            }
        };
        match bitlocker.get_status(letter) {
            VolumeStatus::NotEncrypted => {}
            VolumeStatus::Unknown => {
                log::warn!("[SCATTER] {letter}: 的 BitLocker 状态未知，仍作为候选分区");
            }
            other => {
                log::warn!(
                    "[SCATTER] 跳过 {letter}:：BitLocker 状态为 {}，重启到 PE 后不能保证可访问",
                    other.as_str()
                );
                continue;
            }
        }
        let max_file_bytes =
            lr_core::data_staging::max_file_bytes_for_file_system(file_system.as_deref());
        log::info!(
            "[SCATTER] 候选分区 {letter}: 空闲={:.2} GB 文件系统={} 单文件上限={}",
            free_bytes as f64 / 1024.0 / 1024.0 / 1024.0,
            file_system.as_deref().unwrap_or("未知"),
            if max_file_bytes == u64::MAX {
                "无".to_owned()
            } else {
                max_file_bytes.to_string()
            }
        );
        volumes.push(InventoryVolume {
            letter,
            free_bytes,
            max_file_bytes,
            file_system,
        });
    }
    volumes
}

#[derive(Debug)]
struct StagingVolume {
    letter: char,
    max_file_bytes: u64,
    planned_free_bytes: u64,
    placed_bytes: u64,
    /// 次要分区的随机定位码；主分区始终为 None。
    token: Option<String>,
    /// 次要分区上本次创建的 `LetRecovery_Scatter_<token>` 目录。
    root: Option<PathBuf>,
}

/// 一次经 PE 安装的分散暂存状态。未提交就被释放时，会删除本次在次要分区上创建的全部目录。
#[derive(Debug)]
pub(crate) struct ScatterStaging {
    primary: char,
    volumes: Vec<StagingVolume>,
    image: ScatterImagePlacement,
    image_reservations: Vec<(char, u64)>,
    committed: bool,
}

impl ScatterStaging {
    pub(crate) fn new(plan: ScatterPlan, inventory: &[InventoryVolume]) -> Self {
        let volumes = inventory
            .iter()
            .map(|volume| StagingVolume {
                letter: volume.letter,
                max_file_bytes: volume.max_file_bytes,
                planned_free_bytes: volume.free_bytes,
                placed_bytes: 0,
                token: None,
                root: None,
            })
            .collect();
        Self {
            primary: plan.primary,
            volumes,
            image: plan.image,
            image_reservations: plan.image_reservations,
            committed: false,
        }
    }

    pub(crate) fn primary_letter(&self) -> char {
        self.primary
    }

    pub(crate) fn primary_partition(&self) -> String {
        format!("{}:", self.primary)
    }

    pub(crate) fn image_placement(&self) -> &ScatterImagePlacement {
        &self.image
    }

    pub(crate) fn letters(&self) -> Vec<char> {
        self.volumes.iter().map(|volume| volume.letter).collect()
    }

    /// 镜像已经暂存完毕后调用，后续组件可以使用原先为镜像预留的空间。
    pub(crate) fn release_image_reservations(&mut self) {
        self.image_reservations.clear();
    }

    /// 返回某分区上与主数据目录结构一致的数据目录；次要分区第一次使用时才创建分散目录和定位标记。
    pub(crate) fn data_dir_for(&mut self, letter: char) -> Result<PathBuf> {
        if letter.eq_ignore_ascii_case(&self.primary) {
            let directory = PathBuf::from(super::install_config::ConfigFileManager::get_data_dir(
                &self.primary_partition(),
            ));
            std::fs::create_dir_all(&directory)
                .with_context(|| format!("创建主数据目录 {}", directory.display()))?;
            return Ok(directory);
        }
        let volume = self
            .volumes
            .iter_mut()
            .find(|volume| volume.letter.eq_ignore_ascii_case(&letter))
            .with_context(|| format!("分区 {letter}: 不在本次分散暂存清单中"))?;
        if volume.root.is_none() {
            let token = lr_core::handoff_auth::generate_locator_token()
                .context("生成分散目录定位码")?
                .as_str()
                .to_owned();
            let root = PathBuf::from(format!("{}:\\{}", volume.letter, scatter_root_name(&token)));
            std::fs::create_dir(&root)
                .with_context(|| format!("创建分散目录 {}", root.display()))?;
            // Record ownership before the marker write so a failed marker still gets rolled back.
            volume.root = Some(root.clone());
            volume.token = Some(token.clone());
            let marker = root.join(SCATTER_MARKER_NAME);
            let bytes = lr_core::install_handoff::locator_marker_bytes(&token)
                .context("生成分散目录定位标记内容")?;
            write_synced_file(&marker, &bytes)
                .with_context(|| format!("写入分散目录定位标记 {}", marker.display()))?;
            log::info!("[SCATTER] 已在 {}: 创建分散目录 {}", volume.letter, root.display());
        }
        let root = volume
            .root
            .as_ref()
            .context("分散目录状态缺失")?
            .join(SCATTER_DATA_DIRECTORY);
        std::fs::create_dir_all(&root)
            .with_context(|| format!("创建分散数据目录 {}", root.display()))?;
        Ok(root)
    }

    /// 主数据目录加上所有本次已创建的次要数据目录（按盘符顺序）。
    pub(crate) fn existing_data_dirs(&self) -> Vec<PathBuf> {
        let mut directories = vec![PathBuf::from(
            super::install_config::ConfigFileManager::get_data_dir(&self.primary_partition()),
        )];
        directories.extend(
            self.volumes
                .iter()
                .filter_map(|volume| volume.root.as_ref())
                .map(|root| root.join(SCATTER_DATA_DIRECTORY)),
        );
        directories
    }

    /// 本次已创建分散目录的次要分区盘符（用于生成清单相对路径）。
    pub(crate) fn secondary_partitions(&self) -> Vec<String> {
        self.volumes
            .iter()
            .filter(|volume| volume.root.is_some())
            .map(|volume| format!("{}:", volume.letter))
            .collect()
    }

    fn live_free_bytes(&self, volume: &StagingVolume) -> u64 {
        match platform::free_bytes(&format!("{}:\\", volume.letter)) {
            Ok(value) => value,
            Err(error) => {
                let estimated = volume.planned_free_bytes.saturating_sub(volume.placed_bytes);
                log::warn!(
                    "[SCATTER] 重新读取 {}: 空闲空间失败，改用估算值 {estimated}: {error}",
                    volume.letter
                );
                estimated
            }
        }
    }

    /// 当前可继续写入的字节数：实时空闲减去保留余量，再减去为镜像预留的空间。
    pub(crate) fn available_bytes(&self, letter: char, include_image_reservation: bool) -> u64 {
        let Some(volume) = self
            .volumes
            .iter()
            .find(|volume| volume.letter.eq_ignore_ascii_case(&letter))
        else {
            return 0;
        };
        let reserved = if include_image_reservation {
            self.image_reservations
                .iter()
                .filter(|(reserved_letter, _)| reserved_letter.eq_ignore_ascii_case(&letter))
                .map(|(_, bytes)| *bytes)
                .fold(0_u64, u64::saturating_add)
        } else {
            0
        };
        self.live_free_bytes(volume)
            .saturating_sub(scatter_reserve_bytes(volume.letter == self.primary))
            .saturating_sub(reserved)
    }

    pub(crate) fn max_file_bytes(&self, letter: char) -> u64 {
        self.volumes
            .iter()
            .find(|volume| volume.letter.eq_ignore_ascii_case(&letter))
            .map_or(u64::MAX, |volume| volume.max_file_bytes)
    }

    /// `(盘符, 当前可用字节, 单文件上限)`。
    pub(crate) fn candidates(&self, include_image_reservation: bool) -> Vec<(char, u64, u64)> {
        self.volumes
            .iter()
            .map(|volume| {
                (
                    volume.letter,
                    self.available_bytes(volume.letter, include_image_reservation),
                    volume.max_file_bytes,
                )
            })
            .collect()
    }

    /// 为一个不可再分的单元（一个驱动包、一个安装包）选分区。优先主分区；放不下时选剩余最多且能放下的。
    /// 哪里都放不下时退回剩余最多的分区并记 warn，由实际写入结果决定成败。
    pub(crate) fn choose_for_unit(&self, unit_bytes: u64, largest_file_bytes: u64) -> char {
        let candidates = self.candidates(true);
        if let Some(letter) =
            choose_volume_for_unit(&candidates, unit_bytes, largest_file_bytes, Some(self.primary))
        {
            return letter;
        }
        let fallback = candidates
            .iter()
            .max_by_key(|(letter, available, _)| (*available, std::cmp::Reverse(*letter)))
            .map_or(self.primary, |(letter, _, _)| *letter);
        log::warn!(
            "[SCATTER] 没有分区能确定放下 {unit_bytes} 字节的单元，尝试写入剩余空间最多的 {fallback}:"
        );
        fallback
    }

    pub(crate) fn record_placed(&mut self, letter: char, bytes: u64) {
        if let Some(volume) = self
            .volumes
            .iter_mut()
            .find(|volume| volume.letter.eq_ignore_ascii_case(&letter))
        {
            volume.placed_bytes = volume.placed_bytes.saturating_add(bytes);
        }
    }

    /// PE 交接已提交：保留所有分散目录，交给 PE 使用和清理。
    pub(crate) fn mark_committed(&mut self) {
        self.committed = true;
    }

    pub(crate) fn describe(&self) -> String {
        let image = match &self.image {
            ScatterImagePlacement::Whole(letter) => format!("整体放在 {letter}:"),
            ScatterImagePlacement::PerFile(letters) => format!(
                "分卷分别放在 {}",
                letters
                    .iter()
                    .map(|letter| format!("{letter}:"))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            ScatterImagePlacement::Chunked => "按原始字节分块分散，PE 中拼回".to_owned(),
        };
        format!(
            "主数据分区={}: 候选分区={} 镜像{}",
            self.primary,
            self.volumes
                .iter()
                .map(|volume| format!("{}:", volume.letter))
                .collect::<Vec<_>>()
                .join(","),
            image
        )
    }
}

impl Drop for ScatterStaging {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for volume in &self.volumes {
            let Some(root) = volume.root.as_ref() else {
                continue;
            };
            match std::fs::remove_dir_all(root) {
                Ok(()) => log::info!("[SCATTER] 未提交，已删除分散目录 {}", root.display()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => log::warn!(
                    "[SCATTER] 未提交，删除分散目录 {} 失败，可手动删除: {error}",
                    root.display()
                ),
            }
        }
    }
}

pub(crate) fn write_synced_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// 递归统计目录内普通文件的逻辑字节数与最大单文件字节数。遇到读不到的条目只记 warn 并跳过。
pub(crate) fn measure_tree(path: &Path) -> (u64, u64) {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            log::warn!("[SCATTER] 读取 {} 失败: {error}", path.display());
            return (0, 0);
        }
    };
    if metadata.is_file() {
        return (metadata.len(), metadata.len());
    }
    let mut total = 0_u64;
    let mut largest = 0_u64;
    for entry in walkdir::WalkDir::new(path).follow_links(false) {
        match entry {
            Ok(entry) => {
                if let Ok(metadata) = entry.metadata() {
                    if metadata.is_file() {
                        total = total.saturating_add(metadata.len());
                        largest = largest.max(metadata.len());
                    }
                }
            }
            Err(error) => log::warn!("[SCATTER] 枚举 {} 时出错: {error}", path.display()),
        }
    }
    (total, largest)
}

/// 把 `source`（文件或目录）完整复制到 `destination`。同一分区时直接改名移动。
pub(crate) fn move_entry(source: &Path, destination: &Path) -> std::io::Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::rename(source, destination).is_ok() {
        return Ok(());
    }
    copy_entry(source, destination)?;
    // The copy is complete. Failing to delete the source only wastes space and must not fail the
    // installation.
    let removed = match std::fs::symlink_metadata(source) {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(source),
        Ok(_) => std::fs::remove_file(source),
        Err(error) => Err(error),
    };
    if let Err(error) = removed {
        log::warn!(
            "[SCATTER] 已复制到 {}，但删除源 {} 失败（可手动删除）: {error}",
            destination.display(),
            source.display()
        );
    }
    Ok(())
}

/// 复制文件或目录树（不跟随符号链接/联接点）。
pub(crate) fn copy_entry(source: &Path, destination: &Path) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.is_file() {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(source, destination)?;
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("不复制链接或特殊文件: {}", source.display()),
        ));
    }
    std::fs::create_dir_all(destination)?;
    for entry in walkdir::WalkDir::new(source).follow_links(false).min_depth(1) {
        let entry = entry.map_err(std::io::Error::other)?;
        let relative = entry
            .path()
            .strip_prefix(source)
            .map_err(std::io::Error::other)?;
        let target = destination.join(relative);
        let file_type = entry.file_type();
        if file_type.is_dir() {
            std::fs::create_dir_all(&target)?;
        } else if file_type.is_file() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(entry.path(), &target)?;
        } else {
            log::warn!("[SCATTER] 跳过链接或特殊文件: {}", entry.path().display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_fixed_and_unknown_drive_types_are_eligible() {
        assert!(drive_type_is_eligible(3));
        assert!(drive_type_is_eligible(0));
        for other in [1, 2, 4, 5, 6] {
            assert!(!drive_type_is_eligible(other));
        }
    }
}
