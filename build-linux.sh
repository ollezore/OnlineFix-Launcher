#!/usr/bin/env bash
# OnlineFix-Launcher — сборка под Linux одной командой.  SPDX-License-Identifier: GPL-3.0-or-later
#
#   chmod +x build-linux.sh && ./build-linux.sh
#
# Ставит зависимости (Debian/Ubuntu/Mint, Arch/Manjaro/CachyOS, Fedora/Nobara),
# Rust и Node.js при необходимости, собирает лаунчер и кладёт его в ./dist/onlinefix-launcher.
set -euo pipefail
cd "$(dirname "$0")"

say() { printf '\n\033[1;35m==> %s\033[0m\n' "$*"; }

say "Ставлю системные зависимости (понадобится пароль sudo)"
if command -v apt-get >/dev/null; then
  sudo apt-get update
  sudo apt-get install -y libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev \
    libssl-dev libayatana-appindicator3-dev librsvg2-dev cmake clang p7zip-full nodejs npm
  # RAR (фиксы часто в .rar) — пакеты из non-free/multiverse, могут быть недоступны
  sudo apt-get install -y unrar 2>/dev/null || sudo apt-get install -y p7zip-rar 2>/dev/null || true
elif command -v pacman >/dev/null; then
  sudo pacman -S --needed --noconfirm webkit2gtk-4.1 base-devel curl wget file openssl \
    appmenu-gtk-module libappindicator-gtk3 librsvg cmake clang 7zip nodejs npm
  sudo pacman -S --needed --noconfirm unrar 2>/dev/null || true
elif command -v dnf >/dev/null; then
  sudo dnf install -y webkit2gtk4.1-devel openssl-devel curl wget file libappindicator-gtk3-devel \
    librsvg2-devel cmake clang p7zip p7zip-plugins nodejs npm
  sudo dnf group install -y "c-development" || sudo dnf groupinstall -y "Development Tools"
  sudo dnf install -y unrar 2>/dev/null || true   # из RPM Fusion, если подключён
else
  echo "Не знаю менеджер пакетов этого дистрибутива. Поставьте вручную: webkit2gtk 4.1 (dev), openssl (dev),"
  echo "librsvg, appindicator, cmake, clang, p7zip, nodejs, npm — и запустите скрипт снова."
  exit 1
fi

if ! command -v cargo >/dev/null; then
  say "Ставлю Rust (rustup)"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
fi
# shellcheck disable=SC1091
source "$HOME/.cargo/env" 2>/dev/null || true

say "Ставлю Tauri CLI"
npm install

say "Собираю OnlineFix-Launcher (первый раз 5–15 минут)"
npx tauri build --no-bundle

mkdir -p dist
cp src-tauri/target/release/onlinefix-launcher dist/onlinefix-launcher
chmod +x dist/onlinefix-launcher

# Ярлык в меню приложений (старый ярлык LinFix — убираем)
rm -f "$HOME/.local/share/applications/linfix.desktop" "$HOME/.local/share/icons/hicolor/128x128/apps/linfix.png"
mkdir -p "$HOME/.local/share/applications" "$HOME/.local/share/icons/hicolor/128x128/apps"
cp src-tauri/icons/128x128.png "$HOME/.local/share/icons/hicolor/128x128/apps/onlinefix-launcher.png"
cat > "$HOME/.local/share/applications/onlinefix-launcher.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=OnlineFix-Launcher
Comment=Лаунчер игр FreeTP / Online-Fix (Proton + Steam)
Exec=$(pwd)/dist/onlinefix-launcher
Icon=onlinefix-launcher
Categories=Game;
Terminal=false
EOF

say "Готово!"
echo "Запуск:  $(pwd)/dist/onlinefix-launcher"
echo "Также OnlineFix-Launcher появился в меню приложений."
echo "Для онлайна нужен запущенный Steam; Proton (GE-Proton) можно скачать в «Настройки → Система»."
