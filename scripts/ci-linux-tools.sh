#!/usr/bin/env bash
# Reuse preinstalled runner tools; install only missing runtime/display packages.
set -euo pipefail
tools=(mksquashfs desktop-file-validate Xvfb xauth dbus-run-session)
tool_packages=(squashfs-tools desktop-file-utils xvfb xauth dbus-x11)
runtime_packages=(libvulkan1 mesa-vulkan-drivers libxkbcommon-x11-0 libxcb-shape0
                  libxcb-xfixes0 libfontconfig1 libx11-xcb1)
missing=()
for index in "${!tools[@]}"; do
    if ! command -v "${tools[index]}" >/dev/null; then
        missing+=("${tool_packages[index]}")
    fi
done
for package in "${runtime_packages[@]}"; do
    if [[ $(dpkg-query -W -f='${Status}' "$package" 2>/dev/null || true) != 'install ok installed' ]]; then
        missing+=("$package")
    fi
done
if [[ ${#missing[@]} == 0 ]]; then
    printf '%s\n' 'Native package/display tools are already installed; skipping APT.'
    exit 0
fi
printf 'Installing missing runner packages: %s\n' "${missing[*]}"
sudo apt-get update -o Acquire::Retries=3 -o Acquire::http::Timeout=45 -o Acquire::https::Timeout=45
sudo env DEBIAN_FRONTEND=noninteractive apt-get install --no-install-recommends -y "${missing[@]}"
for tool in "${tools[@]}"; do command -v "$tool" >/dev/null; done
