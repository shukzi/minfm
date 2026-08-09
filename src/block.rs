use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::error::{MinfmError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockDevice {
    pub path: PathBuf,
    pub parent: Option<PathBuf>,
    pub kind: String,
    pub size: u64,
    pub filesystem: Option<String>,
    pub filesystem_version: Option<String>,
    pub label: Option<String>,
    pub uuid: Option<String>,
    pub table_type: Option<String>,
    pub partition_type: Option<String>,
    pub partition_label: Option<String>,
    pub partition_flags: Vec<String>,
    pub mountpoints: Vec<PathBuf>,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub transport: Option<String>,
    pub major_minor: Option<String>,
    pub partition_number: Option<u32>,
    pub start_sector: Option<u64>,
    pub logical_sector_size: u64,
    pub removable: bool,
    pub read_only: bool,
    pub rotational: bool,
}

impl BlockDevice {
    pub fn is_mounted(&self) -> bool {
        !self.mountpoints.is_empty()
    }

    pub fn is_disk(&self) -> bool {
        self.kind == "disk"
    }

    pub fn name(&self) -> String {
        record_name(&self.path)
    }

    pub fn start_bytes(&self) -> Option<u64> {
        self.start_sector
            .and_then(|start| start.checked_mul(self.logical_sector_size))
    }

    pub fn end_bytes(&self) -> Option<u64> {
        self.start_bytes()
            .and_then(|start| start.checked_add(self.size))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockInventory {
    pub devices: Vec<BlockDevice>,
    protected: HashSet<PathBuf>,
}

impl BlockInventory {
    pub fn is_protected(&self, path: &Path) -> bool {
        self.protected.contains(path)
    }

    pub fn descendants_mounted(&self, path: &Path) -> bool {
        let names = descendant_names(&self.devices, path);
        self.devices
            .iter()
            .any(|device| names.contains(&device.name()) && !device.mountpoints.is_empty())
    }
}

pub fn discover() -> Result<BlockInventory> {
    let output = Command::new("lsblk")
        .args([
            "--pairs",
            "--bytes",
            "--paths",
            "--output",
            "PATH,TYPE,SIZE,FSTYPE,FSVER,LABEL,UUID,MOUNTPOINTS,PKNAME,PTTYPE,PARTTYPE,PARTLABEL,PARTFLAGS,PARTN,START,LOG-SEC,MODEL,SERIAL,RM,RO,ROTA,TRAN,MAJ:MIN",
        ])
        .output()
        .map_err(|error| crate::error::io_error("could not run lsblk", error))?;
    if !output.status.success() {
        return Err(MinfmError::Message(format!(
            "lsblk failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    parse_lsblk(
        &String::from_utf8_lossy(&output.stdout),
        &system_mount_sources(),
    )
}

pub(crate) fn parse_lsblk(text: &str, protected_sources: &[PathBuf]) -> Result<BlockInventory> {
    let devices = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_device)
        .collect::<Result<Vec<_>>>()?;
    let protected_names = protected_record_names(&devices, protected_sources);
    let protected = devices
        .iter()
        .filter(|device| protected_names.contains(&device.name()))
        .map(|device| device.path.clone())
        .collect();
    Ok(BlockInventory { devices, protected })
}

fn parse_device(line: &str) -> Result<BlockDevice> {
    let fields = parse_pairs(line)?;
    let get = |key: &str| fields.get(key).cloned().unwrap_or_default();
    let optional = |key: &str| match get(key) {
        value if value.is_empty() => None,
        value => Some(value),
    };
    let path = get("PATH");
    if path.is_empty() {
        return Err(MinfmError::Message(
            "lsblk returned a record without PATH".into(),
        ));
    }
    let parent = optional("PKNAME").map(|parent| {
        if parent.starts_with('/') {
            PathBuf::from(parent)
        } else {
            Path::new("/dev").join(parent)
        }
    });
    Ok(BlockDevice {
        path: PathBuf::from(path),
        parent,
        kind: get("TYPE"),
        size: get("SIZE").parse().unwrap_or(0),
        filesystem: optional("FSTYPE"),
        filesystem_version: optional("FSVER"),
        label: optional("LABEL"),
        uuid: optional("UUID"),
        table_type: optional("PTTYPE"),
        partition_type: optional("PARTTYPE"),
        partition_label: optional("PARTLABEL"),
        partition_flags: get("PARTFLAGS")
            .split(',')
            .map(str::trim)
            .filter(|flag| !flag.is_empty())
            .map(str::to_owned)
            .collect(),
        mountpoints: get("MOUNTPOINTS")
            .lines()
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .collect(),
        model: optional("MODEL").map(|value| value.trim().to_owned()),
        serial: optional("SERIAL").map(|value| value.trim().to_owned()),
        transport: optional("TRAN"),
        major_minor: optional("MAJ:MIN"),
        partition_number: get("PARTN").parse().ok(),
        start_sector: get("START").parse().ok(),
        logical_sector_size: get("LOG-SEC").parse().unwrap_or(512),
        removable: get("RM") == "1",
        read_only: get("RO") == "1",
        rotational: get("ROTA") == "1",
    })
}

pub(crate) fn parse_pairs(line: &str) -> Result<HashMap<String, String>> {
    let bytes = line.as_bytes();
    let mut fields = HashMap::new();
    let mut index = 0;
    while index < bytes.len() {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let key_start = index;
        while index < bytes.len() && bytes[index] != b'=' {
            index += 1;
        }
        if index >= bytes.len() || index + 1 >= bytes.len() || bytes[index + 1] != b'"' {
            return Err(MinfmError::Message(format!(
                "could not parse lsblk output: {line}"
            )));
        }
        let key = String::from_utf8_lossy(&bytes[key_start..index]).into_owned();
        index += 2;
        let mut value = Vec::new();
        while index < bytes.len() && bytes[index] != b'"' {
            if bytes[index] == b'\\' && index + 3 < bytes.len() && bytes[index + 1] == b'x' {
                let hex = &line[index + 2..index + 4];
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    value.push(byte);
                    index += 4;
                    continue;
                }
            }
            if bytes[index] == b'\\' && index + 1 < bytes.len() {
                index += 1;
            }
            value.push(bytes[index]);
            index += 1;
        }
        if index >= bytes.len() {
            return Err(MinfmError::Message(format!(
                "unterminated lsblk value: {line}"
            )));
        }
        index += 1;
        fields.insert(key, String::from_utf8_lossy(&value).into_owned());
    }
    Ok(fields)
}

pub(crate) fn system_mount_sources() -> Vec<PathBuf> {
    let mut sources = ["/", "/boot", "/boot/efi", "/usr", "/var", "/home"]
        .iter()
        .filter_map(|target| {
            Command::new("findmnt")
                .args(["-n", "-o", "SOURCE", "--target", target])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| {
                    let source = String::from_utf8_lossy(&output.stdout);
                    normalize_mount_source(&source)
                })
        })
        .collect::<Vec<_>>();
    if let Ok(swaps) = fs::read_to_string("/proc/swaps") {
        sources.extend(
            swaps
                .lines()
                .skip(1)
                .filter_map(|line| line.split_whitespace().next())
                .filter(|source| source.starts_with('/'))
                .map(PathBuf::from),
        );
    }
    sources.sort();
    sources.dedup();
    sources
}

fn normalize_mount_source(source: &str) -> Option<PathBuf> {
    let source = source.trim();
    let source = source
        .strip_suffix(']')
        .and_then(|source| source.rsplit_once('[').map(|(device, _)| device))
        .unwrap_or(source);
    (!source.is_empty() && source.starts_with('/')).then(|| PathBuf::from(source))
}

pub(crate) fn record_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn protected_record_names(devices: &[BlockDevice], sources: &[PathBuf]) -> HashSet<String> {
    let mut protected = sources
        .iter()
        .flat_map(|source| [source.to_string_lossy().into_owned(), record_name(source)])
        .collect::<HashSet<_>>();
    let mut changed = true;
    while changed {
        changed = false;
        for device in devices {
            let path = device.path.to_string_lossy().into_owned();
            let name = device.name();
            let parent = device.parent.as_ref().map(|path| record_name(path));
            if protected.contains(&path)
                || protected.contains(&name)
                || parent.as_ref().is_some_and(|name| protected.contains(name))
            {
                changed |= protected.insert(path);
                changed |= protected.insert(name);
                if let Some(parent) = parent {
                    changed |= protected.insert(parent);
                }
            }
        }
    }
    protected
}

fn descendant_names(devices: &[BlockDevice], path: &Path) -> HashSet<String> {
    let mut names = HashSet::from([record_name(path)]);
    let mut changed = true;
    while changed {
        changed = false;
        for device in devices {
            if device
                .parent
                .as_ref()
                .is_some_and(|parent| names.contains(&record_name(parent)))
            {
                changed |= names.insert(device.name());
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = concat!(
        "PATH=\"/dev/nvme0n1\" TYPE=\"disk\" SIZE=\"100000\" FSTYPE=\"\" FSVER=\"\" LABEL=\"\" UUID=\"\" MOUNTPOINTS=\"\" PKNAME=\"\" PTTYPE=\"gpt\" PARTTYPE=\"\" PARTLABEL=\"\" PARTFLAGS=\"\" MODEL=\"Fast\\x20Disk\" SERIAL=\"ABC\" RM=\"0\" RO=\"0\" TRAN=\"nvme\" MAJ:MIN=\"259:0\"\n",
        "PATH=\"/dev/nvme0n1p1\" TYPE=\"part\" SIZE=\"90000\" FSTYPE=\"ext4\" FSVER=\"1.0\" LABEL=\"System\" UUID=\"uuid-1\" MOUNTPOINTS=\"/\" PKNAME=\"nvme0n1\" PTTYPE=\"\" PARTTYPE=\"linux\" PARTLABEL=\"root\" PARTFLAGS=\"\" MODEL=\"\" SERIAL=\"\" RM=\"0\" RO=\"0\" TRAN=\"\" MAJ:MIN=\"259:1\"\n",
        "PATH=\"/dev/sdb\" TYPE=\"disk\" SIZE=\"200000\" FSTYPE=\"\" FSVER=\"\" LABEL=\"\" UUID=\"\" MOUNTPOINTS=\"\" PKNAME=\"\" PTTYPE=\"gpt\" PARTTYPE=\"\" PARTLABEL=\"\" PARTFLAGS=\"\" MODEL=\"USB\\x20Drive\" SERIAL=\"XYZ\" RM=\"1\" RO=\"0\" TRAN=\"usb\" MAJ:MIN=\"8:16\"\n",
        "PATH=\"/dev/sdb1\" TYPE=\"part\" SIZE=\"190000\" FSTYPE=\"exfat\" FSVER=\"1.0\" LABEL=\"Data\" UUID=\"uuid-2\" MOUNTPOINTS=\"/run/media/user/Data\" PKNAME=\"sdb\" PTTYPE=\"\" PARTTYPE=\"data\" PARTLABEL=\"files\" PARTFLAGS=\"msftdata\" MODEL=\"\" SERIAL=\"\" RM=\"1\" RO=\"0\" TRAN=\"\" MAJ:MIN=\"8:17\"\n",
    );

    #[test]
    fn parses_inventory_and_decodes_values() {
        let inventory = parse_lsblk(FIXTURE, &[]).unwrap();
        assert_eq!(inventory.devices.len(), 4);
        assert_eq!(inventory.devices[0].model.as_deref(), Some("Fast Disk"));
        assert_eq!(inventory.devices[3].partition_flags, ["msftdata"]);
        assert_eq!(inventory.devices[3].parent, Some(PathBuf::from("/dev/sdb")));
    }

    #[test]
    fn protects_system_partition_its_disk_and_siblings() {
        let inventory = parse_lsblk(FIXTURE, &[PathBuf::from("/dev/nvme0n1p1")]).unwrap();
        assert!(inventory.is_protected(Path::new("/dev/nvme0n1")));
        assert!(inventory.is_protected(Path::new("/dev/nvme0n1p1")));
        assert!(!inventory.is_protected(Path::new("/dev/sdb")));
    }

    #[test]
    fn finds_mounted_descendants() {
        let inventory = parse_lsblk(FIXTURE, &[]).unwrap();
        assert!(inventory.descendants_mounted(Path::new("/dev/sdb")));
        assert!(!inventory.descendants_mounted(Path::new("/dev/unknown")));
    }

    #[test]
    fn malformed_pair_output_is_rejected() {
        assert!(parse_pairs("PATH=/dev/sda").is_err());
        assert!(parse_pairs("PATH=\"/dev/sda").is_err());
    }

    #[test]
    fn strips_findmnt_subvolume_suffixes_from_device_identity() {
        assert_eq!(
            normalize_mount_source("/dev/mapper/system[/root]\n"),
            Some(PathBuf::from("/dev/mapper/system"))
        );
        assert_eq!(
            normalize_mount_source("/dev/nvme0n1p2\n"),
            Some(PathBuf::from("/dev/nvme0n1p2"))
        );
        assert_eq!(normalize_mount_source("overlay"), None);
    }
}
