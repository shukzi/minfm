#!/bin/sh
set -eu

repo="shukzi/minfm"
if [ -n "${MINFM_VERSION:-}" ]; then
    release_base="https://github.com/${repo}/releases/download/${MINFM_VERSION}"
else
    release_base="https://github.com/${repo}/releases/latest/download"
fi
binary_name="minfm-linux-x86_64"
binary_url="${release_base}/${binary_name}"
checksum_url="${release_base}/${binary_name}.sha256"
font_name="minfm-icons.ttf"
font_url="${release_base}/${font_name}"
font_checksum_url="${release_base}/${font_name}.sha256"

if ! command -v curl >/dev/null 2>&1; then
    echo "minfm installer requires curl." >&2
    exit 1
fi

if ! command -v sha256sum >/dev/null 2>&1; then
    echo "minfm installer requires sha256sum (usually provided by coreutils)." >&2
    exit 1
fi

if ! command -v fc-cache >/dev/null 2>&1; then
    echo "minfm installer requires fc-cache (provided by fontconfig) for its icon font." >&2
    exit 1
fi

arch=$(uname -m)
if [ "$arch" != "x86_64" ]; then
    echo "No published static binary is available for architecture: $arch" >&2
    echo "Build manually with Rust or request an architecture-specific release." >&2
    exit 1
fi

temp_dir=$(mktemp -d)
staged_binary=""
staged_font=""
cleanup() {
    rm -rf "$temp_dir"
    if [ -n "$staged_binary" ]; then
        rm -f "$staged_binary"
    fi
    if [ -n "$staged_font" ]; then
        rm -f "$staged_font"
    fi
}
trap cleanup EXIT HUP INT TERM

echo "Downloading minfm static binary..."
curl --proto '=https' --tlsv1.2 -fsSL "$binary_url" -o "$temp_dir/$binary_name"
curl --proto '=https' --tlsv1.2 -fsSL "$checksum_url" -o "$temp_dir/$binary_name.sha256"
curl --proto '=https' --tlsv1.2 -fsSL "$font_url" -o "$temp_dir/$font_name"
curl --proto '=https' --tlsv1.2 -fsSL "$font_checksum_url" -o "$temp_dir/$font_name.sha256"

verify_download() {
    expected=$(awk 'NF >= 1 { print $1; exit }' "$2")
    if [ "${#expected}" -ne 64 ]; then
        echo "The downloaded checksum for $1 has an invalid format." >&2
        exit 1
    fi
    printf '%s  %s\n' "$expected" "$1" | sha256sum --check --status -
}
verify_download "$temp_dir/$binary_name" "$temp_dir/$binary_name.sha256"
verify_download "$temp_dir/$font_name" "$temp_dir/$font_name.sha256"

install_dir=${XDG_BIN_HOME:-${HOME}/.local/bin}
config_dir=${XDG_CONFIG_HOME:-${HOME}/.config}/minfm
font_dir=${XDG_DATA_HOME:-${HOME}/.local/share}/fonts/minfm
mkdir -p "$install_dir" "$config_dir" "$font_dir"
staged_binary=$(mktemp "$install_dir/.minfm-install.XXXXXX")
staged_font=$(mktemp "$font_dir/.minfm-font-install.XXXXXX")
cp "$temp_dir/$binary_name" "$staged_binary"
cp "$temp_dir/$font_name" "$staged_font"
chmod 0755 "$staged_binary"
chmod 0644 "$staged_font"
mv "$staged_font" "$font_dir/$font_name"
staged_font=""
fc-cache -f "$font_dir"
mv "$staged_binary" "$install_dir/minfm"
staged_binary=""

echo "Installed minfm to $install_dir/minfm"
echo "Installed minfm icon font to $font_dir/$font_name"
echo "Configuration directory: $config_dir"
if ! command -v minfm >/dev/null 2>&1; then
    echo "If needed, add $install_dir to PATH, then run: minfm"
fi

missing=""
for tool in lsblk findmnt udisksctl cryptsetup; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        missing="$missing $tool"
    fi
done

has_tty() {
    [ -r /dev/tty ] && (: </dev/tty) 2>/dev/null
}

if ! command -v xdg-open >/dev/null 2>&1; then
    echo "xdg-open is missing; opening files with the default application is unavailable."
    if has_tty; then
        printf "Install xdg-utils now? [y/N] " > /dev/tty
        read answer < /dev/tty || answer=""
        case "$answer" in
            y|Y|yes|YES)
                if command -v dnf >/dev/null 2>&1; then
                    sudo dnf install -y xdg-utils
                elif command -v apt-get >/dev/null 2>&1; then
                    sudo apt-get update
                    sudo apt-get install -y xdg-utils
                elif command -v pacman >/dev/null 2>&1; then
                    sudo pacman -S --needed xdg-utils
                else
                    echo "No supported package manager found; install xdg-utils manually."
                fi
                ;;
            *)
                echo "Skipped xdg-utils. Configure [open] with another installed application if needed."
                ;;
        esac
    else
        echo "Install xdg-utils or configure [open] with another installed application if needed."
    fi
fi

if [ -n "$missing" ]; then
    echo "Optional LUKS device-management tools are missing:$missing"
    if has_tty; then
        printf "Install the missing LUKS tools now? [y/N] " > /dev/tty
        read answer < /dev/tty || answer=""
        case "$answer" in
            y|Y|yes|YES)
                if command -v dnf >/dev/null 2>&1; then
                    sudo dnf install -y util-linux udisks2 cryptsetup
                elif command -v apt-get >/dev/null 2>&1; then
                    sudo apt-get update
                    sudo apt-get install -y util-linux udisks2 cryptsetup
                elif command -v pacman >/dev/null 2>&1; then
                    sudo pacman -S --needed util-linux udisks2 cryptsetup
                else
                    echo "No supported package manager found; install:$missing manually."
                fi
                ;;
            *)
                echo "Skipped optional LUKS tools. File management remains available."
                ;;
        esac
    else
        echo "Run your distribution's package manager to install them if needed."
    fi
fi

add_samba_package() {
    case " $samba_packages " in
        *" $1 "*) ;;
        *) samba_packages="$samba_packages $1" ;;
    esac
}

samba_packages=""
if command -v dnf >/dev/null 2>&1; then
    command -v gio >/dev/null 2>&1 || add_samba_package glib2
    rpm -q gvfs-smb >/dev/null 2>&1 || add_samba_package gvfs-smb
    command -v secret-tool >/dev/null 2>&1 || add_samba_package libsecret
elif command -v apt-get >/dev/null 2>&1; then
    command -v gio >/dev/null 2>&1 || add_samba_package libglib2.0-bin
    dpkg-query -W -f='${Status}' gvfs-backends 2>/dev/null | grep -q 'install ok installed' || add_samba_package gvfs-backends
    command -v secret-tool >/dev/null 2>&1 || add_samba_package libsecret-tools
elif command -v pacman >/dev/null 2>&1; then
    command -v gio >/dev/null 2>&1 || add_samba_package glib2
    pacman -Q gvfs-smb >/dev/null 2>&1 || add_samba_package gvfs-smb
    command -v secret-tool >/dev/null 2>&1 || add_samba_package libsecret
else
    command -v gio >/dev/null 2>&1 || add_samba_package gio
    command -v secret-tool >/dev/null 2>&1 || add_samba_package secret-tool
fi

if [ -n "$samba_packages" ]; then
    for package in $samba_packages; do
        echo "$package is missing."
    done
    if has_tty; then
        printf "Install the required packages for Samba functionality? [y/N] " > /dev/tty
        read answer < /dev/tty || answer=""
        case "$answer" in
            y|Y|yes|YES)
                if command -v dnf >/dev/null 2>&1; then
                    sudo dnf install -y $samba_packages
                elif command -v apt-get >/dev/null 2>&1; then
                    sudo apt-get update
                    sudo apt-get install -y $samba_packages
                elif command -v pacman >/dev/null 2>&1; then
                    sudo pacman -S --needed $samba_packages
                else
                    echo "No supported package manager found; install:$samba_packages manually."
                fi
                ;;
            *)
                echo "Skipped Samba packages. Local file management remains available."
                ;;
        esac
    else
        echo "Run your distribution's package manager to install them if needed."
    fi
fi

add_partition_package() {
    case " $partition_packages " in
        *" $1 "*) ;;
        *) partition_packages="$partition_packages $1" ;;
    esac
}

missing_any() {
    for tool in "$@"; do
        command -v "$tool" >/dev/null 2>&1 || return 0
    done
    return 1
}

partition_packages=""
if command -v dnf >/dev/null 2>&1 || command -v apt-get >/dev/null 2>&1 || command -v pacman >/dev/null 2>&1; then
    missing_any parted && add_partition_package parted
    missing_any mount umount wipefs sfdisk blockdev mkswap swaplabel && add_partition_package util-linux
    missing_any chown dd cp install mkdir mv && add_partition_package coreutils
    missing_any sudo && add_partition_package sudo
    missing_any mkfs.ext4 e2fsck resize2fs e2label && add_partition_package e2fsprogs
    missing_any mkfs.xfs xfs_repair xfs_admin && add_partition_package xfsprogs
    missing_any mkfs.btrfs btrfs && add_partition_package btrfs-progs
    missing_any mkfs.fat fsck.fat fatlabel && add_partition_package dosfstools
    missing_any mkfs.exfat fsck.exfat exfatlabel && add_partition_package exfatprogs
    missing_any mkfs.ntfs ntfsfix ntfslabel && add_partition_package ntfs-3g
    missing_any mkfs.f2fs fsck.f2fs f2fslabel && add_partition_package f2fs-tools
    missing_any mkudffs && add_partition_package udftools
    missing_any smartctl && add_partition_package smartmontools
    missing_any hdparm && add_partition_package hdparm
else
    for tool in parted mount umount wipefs sfdisk blockdev chown dd cp install mkdir mv sudo mkfs.ext4 e2fsck resize2fs e2label mkfs.xfs xfs_repair xfs_admin mkfs.btrfs btrfs mkfs.fat fsck.fat fatlabel mkfs.exfat fsck.exfat exfatlabel mkfs.ntfs ntfsfix ntfslabel mkfs.f2fs fsck.f2fs f2fslabel mkudffs smartctl hdparm mkswap swaplabel; do
        command -v "$tool" >/dev/null 2>&1 || add_partition_package "$tool"
    done
fi

if [ -n "$partition_packages" ]; then
    echo "Device-manager support is incomplete; missing packages/tools:$partition_packages"
    if has_tty; then
        printf "Install the required packages for complete device-manager functionality? [y/N] " > /dev/tty
        read answer < /dev/tty || answer=""
        case "$answer" in
            y|Y|yes|YES)
                if command -v dnf >/dev/null 2>&1; then
                    sudo dnf install -y $partition_packages
                elif command -v apt-get >/dev/null 2>&1; then
                    sudo apt-get update
                    sudo apt-get install -y $partition_packages
                elif command -v pacman >/dev/null 2>&1; then
                    sudo pacman -S --needed $partition_packages
                else
                    echo "No supported package manager found; install:$partition_packages manually."
                fi
                ;;
            *)
                echo "Skipped device-manager packages. Available operations depend on installed tools."
                ;;
        esac
    else
        echo "Run your distribution's package manager to install them for complete device-manager functionality."
    fi
fi
