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

if ! command -v curl >/dev/null 2>&1; then
    echo "minfm installer requires curl." >&2
    exit 1
fi

if ! command -v sha256sum >/dev/null 2>&1; then
    echo "minfm installer requires sha256sum (usually provided by coreutils)." >&2
    exit 1
fi

arch=$(uname -m)
if [ "$arch" != "x86_64" ]; then
    echo "No published static binary is available for architecture: $arch" >&2
    echo "Build manually with Rust or request an architecture-specific release." >&2
    exit 1
fi

temp_dir=$(mktemp -d)
trap 'rm -rf "$temp_dir"' EXIT HUP INT TERM

echo "Downloading minfm static binary..."
curl --proto '=https' --tlsv1.2 -fsSL "$binary_url" -o "$temp_dir/$binary_name"
curl --proto '=https' --tlsv1.2 -fsSL "$checksum_url" -o "$temp_dir/$binary_name.sha256"

expected=$(awk 'NF >= 1 { print $1; exit }' "$temp_dir/$binary_name.sha256")
if [ "${#expected}" -ne 64 ]; then
    echo "The downloaded checksum has an invalid format." >&2
    exit 1
fi
printf '%s  %s\n' "$expected" "$temp_dir/$binary_name" | sha256sum --check --status -

install_dir=${XDG_BIN_HOME:-${HOME}/.local/bin}
config_dir=${XDG_CONFIG_HOME:-${HOME}/.config}/minfm
mkdir -p "$install_dir" "$config_dir"
chmod 0755 "$temp_dir/$binary_name"
mv "$temp_dir/$binary_name" "$install_dir/minfm"

echo "Installed minfm to $install_dir/minfm"
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
