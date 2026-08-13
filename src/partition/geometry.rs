use super::*;

pub fn parse_size(value: &str, disk_size: u64) -> Result<u64> {
    let value = value.trim();
    if let Some(percent) = value.strip_suffix('%') {
        let percent = percent
            .parse::<u64>()
            .map_err(|_| MinfmError::Message("invalid percentage".into()))?;
        if percent > 100 {
            return Err(MinfmError::Message(
                "percentage must be between 0 and 100".into(),
            ));
        }
        return disk_size
            .checked_mul(percent)
            .map(|value| value / 100)
            .ok_or_else(|| MinfmError::Message("size is too large".into()));
    }
    let split = value
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(value.len());
    let number = value[..split]
        .parse::<u64>()
        .map_err(|_| MinfmError::Message("size must begin with a whole number".into()))?;
    let multiplier = match value[split..].trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "kib" => 1024,
        "mib" => 1024 * 1024,
        "gib" => 1024 * 1024 * 1024,
        "tib" => 1024_u64.pow(4),
        _ => {
            return Err(MinfmError::Message(
                "use B, KiB, MiB, GiB, TiB, or %".into(),
            ))
        }
    };
    number
        .checked_mul(multiplier)
        .ok_or_else(|| MinfmError::Message("size is too large".into()))
}

pub fn size_input(value: u64) -> String {
    if value.is_multiple_of(1024 * 1024 * 1024) {
        format!("{}GiB", value / (1024 * 1024 * 1024))
    } else if value.is_multiple_of(1024 * 1024) {
        format!("{}MiB", value / (1024 * 1024))
    } else if value.is_multiple_of(1024) {
        format!("{}KiB", value / 1024)
    } else {
        format!("{value}B")
    }
}

pub fn free_regions(disk: &PartitionEntry, entries: &[PartitionEntry]) -> Vec<(u64, u64)> {
    if !disk.device.is_disk() || disk.device.size <= 2 * 1024 * 1024 {
        return Vec::new();
    }
    let margin = 1024 * 1024;
    let alignment = disk.device.logical_sector_size.max(1024 * 1024);
    let align_up = |value: u64| value.div_ceil(alignment) * alignment;
    let align_down = |value: u64| value - value % alignment;
    let mut extents = entries
        .iter()
        .filter(|entry| entry.device.parent.as_ref() == Some(&disk.device.path))
        .filter_map(|entry| entry.device.start_bytes().zip(entry.device.end_bytes()))
        .collect::<Vec<_>>();
    extents.sort_unstable();
    let mut regions = Vec::new();
    let mut cursor = align_up(margin);
    for (start, end) in extents {
        let region_end = align_down(start);
        if region_end.saturating_sub(cursor) >= margin {
            regions.push((cursor, region_end));
        }
        cursor = align_up(cursor.max(end));
    }
    let usable_end = align_down(disk.device.size.saturating_sub(margin));
    if usable_end.saturating_sub(cursor) >= margin {
        regions.push((cursor, usable_end));
    }
    regions
}

pub fn largest_free_region(
    disk: &PartitionEntry,
    entries: &[PartitionEntry],
) -> Option<(u64, u64)> {
    free_regions(disk, entries)
        .into_iter()
        .max_by_key(|(start, end)| end - start)
}
pub fn maximum_growth_end(partition: &PartitionEntry, entries: &[PartitionEntry]) -> Option<u64> {
    let parent = partition.device.parent.as_ref()?;
    let disk = entries
        .iter()
        .find(|entry| entry.device.path == *parent && entry.device.is_disk())?;
    let current_start = partition.device.start_bytes()?;
    let current_end = partition.device.end_bytes()?;
    let disk_end = disk.device.size.saturating_sub(1024 * 1024);
    let next_start = entries
        .iter()
        .filter(|entry| {
            entry.device.path != partition.device.path
                && entry.device.parent.as_ref() == Some(parent)
        })
        .filter_map(|entry| entry.device.start_bytes())
        .filter(|start| *start > current_start)
        .min();
    let maximum = next_start.unwrap_or(disk_end).min(disk_end);
    (maximum > current_end).then_some(maximum)
}

pub(super) fn format_bytes(value: u64) -> String {
    if value.is_multiple_of(1024 * 1024 * 1024) {
        format!("{} GiB", value / (1024 * 1024 * 1024))
    } else if value.is_multiple_of(1024 * 1024) {
        format!("{} MiB", value / (1024 * 1024))
    } else {
        format!("{value} B")
    }
}
