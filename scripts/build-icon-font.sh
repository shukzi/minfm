#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
    echo "Usage: $0 OUTPUT_DIRECTORY" >&2
    exit 1
fi

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
mkdir -p "$1"
output_dir=$(CDPATH= cd -- "$1" && pwd)
temp_dir=$(mktemp -d)
trap 'rm -rf "$temp_dir"' EXIT HUP INT TERM

python=${MINFM_FONT_PYTHON:-python3}
"$python" -m venv "$temp_dir/tools"
"$temp_dir/tools/bin/python" -m pip install --disable-pip-version-check \
    --only-binary=:all: --no-deps --platform any --target "$temp_dir/packages" \
    --require-hashes -r "$repo_dir/scripts/font-requirements.txt"

archive="$temp_dir/NerdFontsSymbolsOnly.tar.xz"
curl --proto '=https' --tlsv1.2 -fsSL \
    https://github.com/ryanoasis/nerd-fonts/releases/download/v3.5.0/NerdFontsSymbolsOnly.tar.xz \
    -o "$archive"
printf '%s  %s\n' b7ef2283462b435f1fe91d729dc412d5dbe34269dd2c7f4e1d803e4105c8d883 "$archive" \
    | sha256sum --check --status -
tar -xJf "$archive" -C "$temp_dir" SymbolsNerdFontMono-Regular.ttf
PYTHONPATH="$temp_dir/packages" "$temp_dir/tools/bin/python" \
    "$repo_dir/scripts/prepare_icon_font.py" \
    "$temp_dir/SymbolsNerdFontMono-Regular.ttf" "$output_dir/minfm-icons.ttf"
cp "$repo_dir/licenses/minfm-icons-font.txt" "$output_dir/minfm-icons-LICENSE.txt"
cd "$output_dir"
sha256sum minfm-icons.ttf > minfm-icons.ttf.sha256
