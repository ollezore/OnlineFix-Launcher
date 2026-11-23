// SPDX-License-Identifier: GPL-3.0-or-later
//! Библиотека установленных игр и настройки (JSON на диске).

use crate::catalog::{Addon, CatalogEntry};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    /// Торрент качается.
    Downloading,
    /// Скачано: проверяем файлы и создаём префикс Wine (Linux).
    Preparing,
    /// Скачано, нужно установить (запустить установщик / выбрать exe).
    Downloaded,
    /// Распаковка архивов / работа установщика.
    Installing,
    /// Установщик закрылся, нужно выбрать exe игры.
    NeedsExe,
    /// Готово к запуску.
    Installed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Game {
    pub id: String,
    pub title: String,
    pub release: String,
    pub magnet: String,
    pub info_hash: String,
    pub file_size: String,
    pub article_url: Option<String>,
    pub install_kind: u8,
    #[serde(default)]
    pub addons: Vec<Addon>,
    #[serde(default)]
    pub addons_applied: bool,
    /// Какой из вариантов фикса поставлен (название из базы).
    #[serde(default)]
    pub addon_applied: Option<String>,
    pub status: Status,
    /// Откуда раздача: "freetp" (пусто в старых записях) или "onlinefix".
    #[serde(default)]
    pub source: String,
    /// Найдены ли файлы Online-Fix в папке игры (вычисляется при каждом запросе).
    #[serde(default, skip_deserializing)]
    pub fix_detected: bool,
    /// Корень игры: <games_dir>/<id>. Внутри: download/, prefix/, extracted/.
    pub dir: PathBuf,
    pub exe: Option<PathBuf>,
    #[serde(default)]
    pub launch_args: String,
    /// Дополнительные переменные окружения `KEY=VAL KEY2=VAL2`.
    #[serde(default)]
    pub env: String,
    /// Свои DLL overrides (пусто — стандартные для Online-Fix).
    #[serde(default)]
    pub dll_overrides: String,
    /// Запуск с окружением Spacewar (480) — для онлайна через Steam.
    #[serde(default = "yes")]
    pub spacewar: bool,
    /// Имя версии Proton; None — из общих настроек.
    #[serde(default)]
    pub proton: Option<String>,
    #[serde(default)]
    pub playtime_secs: u64,
    #[serde(default)]
    pub last_played: Option<u64>,
    pub added_at: u64,
    #[serde(default)]
    pub last_error: Option<String>,
    /// Когда запускали установщик (unix-время) — чтобы найти, куда он поставил игру.
    #[serde(default)]
    pub install_started: Option<u64>,
    /// Дата раздачи из базы — чтобы понять, что вышла версия новее.
    #[serde(default)]
    pub upload_date: String,
    /// Название в базе (пользователь может переименовать игру — сравниваем по нему).
    #[serde(default)]
    pub catalog_title: String,
    /// Идёт обновление: имя прошлой раздачи (для подписи «v1 → v2»).
    #[serde(default)]
    pub update_from: Option<String>,
    /// Префикс Proton не в папке игры (выбран отдельно или папка игры на NTFS/exFAT).
    #[serde(default)]
    pub prefix_dir: Option<PathBuf>,
    /// Куда установщик поставил игру (папка, которую мы ему передали, или найденная
    /// в реестре Windows, если пользователь выбрал свою).
    #[serde(default)]
    pub install_dir: Option<PathBuf>,
}

/// Название для сравнения: только буквы и цифры в нижнем регистре.
pub fn norm_title(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

fn yes() -> bool {
    true
}

impl Game {
    pub fn from_entry(e: &CatalogEntry, games_dir: &Path) -> Self {
        let id = format!(
            "{}-{}",
            crate::files::sanitize(&e.title).to_lowercase(),
            &e.info_hash[..8.min(e.info_hash.len())]
        );
        Game {
            dir: games_dir.join(&id),
            id,
            title: e.title.clone(),
            release: e.release.clone(),
            magnet: e.magnet.clone(),
            info_hash: e.info_hash.clone(),
            file_size: e.file_size.clone(),
            article_url: e.article_url.clone(),
            install_kind: e.install_kind,
            addons: e.addons.clone(),
            addons_applied: false,
            addon_applied: None,
            status: Status::Downloading,
            source: e.source.clone(),
            fix_detected: false,
            exe: None,
            launch_args: String::new(),
            env: String::new(),
            dll_overrides: String::new(),
            spacewar: true,
            proton: None,
            playtime_secs: 0,
            last_played: None,
            added_at: now(),
            last_error: None,
            install_started: None,
            upload_date: e.upload_date.clone(),
            catalog_title: e.title.clone(),
            update_from: None,
            prefix_dir: None,
            install_dir: None,
        }
    }
    pub fn download_dir(&self) -> PathBuf {
        self.dir.join("download")
    }
    pub fn extracted_dir(&self) -> PathBuf {
        self.dir.join("extracted")
    }
    pub fn compat_dir(&self) -> PathBuf {
        self.prefix_dir.clone().unwrap_or_else(|| self.dir.join("prefix"))
    }
    /// Папка, в которую лаунчер просит установщик поставить игру.
    pub fn target_install_dir(&self) -> PathBuf {
        self.dir.join("game")
    }
    pub fn drive_c(&self) -> PathBuf {
        self.compat_dir().join("pfx/drive_c")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub games_dir: PathBuf,
    /// "arachnel" (по умолчанию, с фиксами и ссылками на статьи) или "hydra".
    pub source: String,
    /// Имя версии Proton по умолчанию; None — первая найденная.
    pub proton: Option<String>,
    /// Запускать через umu-run, если установлен.
    pub use_umu: bool,
    /// Ограничение скорости загрузки, МБ/с (0 — без лимита). Применяется при старте.
    #[serde(default)]
    pub download_limit_mbps: u32,
    /// Писать логи Proton в <игра>/proton.log (по умолчанию выключено).
    #[serde(default)]
    pub proton_log: bool,
    /// Подключать оверлей Steam (Shift+Tab) к игре, как это делает сам Steam.
    #[serde(default = "yes")]
    pub steam_overlay: bool,
    /// Общая папка для префиксов Proton (внутри — папка на каждую игру).
    /// None — префикс лежит в папке игры.
    #[serde(default)]
    pub prefixes_dir: Option<PathBuf>,
    /// Запускать игры через Wayland-драйвер Wine (PROTON_ENABLE_WAYLAND=1).
    #[serde(default)]
    pub wayland: bool,
    /// Версия формата настроек — для однократных миграций.
    #[serde(default)]
    pub rev: u32,
}

/// Текущая версия формата настроек.
pub const SETTINGS_REV: u32 = 1;

impl Settings {
    /// Однократные исправления старых настроек.
    pub fn migrate(&mut self) {
        if self.rev < 1 {
            // Раньше логи Proton писались всегда — теперь по умолчанию выключены.
            self.proton_log = false;
        }
        self.rev = SETTINGS_REV;
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            games_dir: dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("Games/OnlineFix-Launcher"),
            source: "arachnel".into(),
            proton: None,
            use_umu: false,
            download_limit_mbps: 0,
            steam_overlay: true,
            proton_log: false,
            prefixes_dir: None,
            wayland: false,
            rev: SETTINGS_REV,
        }
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn load_json<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> T {
    std::fs::read(path)
        .ok()
        .and_then(|b| {
            // Блокнот / PowerShell 5 пишут UTF-8 с BOM — пропускаем его.
            let b = b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&b);
            serde_json::from_slice(b).ok()
        })
        .unwrap_or_default()
}

pub fn save_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}
