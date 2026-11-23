// SPDX-License-Identifier: GPL-3.0-or-later
//! Поиск Steam / Proton и запуск игр.
//! Linux: через Proton в отдельном префиксе. Windows: напрямую (Proton не нужен).

use crate::sys::{command, IS_WINDOWS};
use regex::Regex;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};

/// AppID Spacewar — через него Online-Fix даёт онлайн в Steam.
pub const SPACEWAR_APPID: &str = "480";

/// Стандартные оверрайды для Online-Fix под Wine/Proton.
pub const ONLINEFIX_DLL_OVERRIDES: &str =
    "OnlineFix64=n;SteamOverlay64=n;winmm=n,b;dnet=n;steam_api64=n,b;winhttp=n,b";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtonInfo {
    pub name: String,
    pub path: String,
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

// ───────────── Steam ─────────────

/// Корень установки Steam.
pub fn steam_root() -> Option<PathBuf> {
    if IS_WINDOWS {
        return steam_root_windows();
    }
    let h = home();
    [
        h.join(".steam/root"),
        h.join(".steam/steam"),
        h.join(".local/share/Steam"),
        h.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
        h.join("snap/steam/common/.local/share/Steam"),
    ]
    .into_iter()
    .find(|p| p.join("steamapps").is_dir())
    .map(|p| p.canonicalize().unwrap_or(p))
}

fn steam_root_windows() -> Option<PathBuf> {
    // HKCU\Software\Valve\Steam\SteamPath
    let from_reg = command("reg")
        .args(["query", r"HKCU\Software\Valve\Steam", "/v", "SteamPath"])
        .output()
        .ok()
        .and_then(|o| {
            let out = String::from_utf8_lossy(&o.stdout).to_string();
            out.lines()
                .find(|l| l.contains("SteamPath"))
                .and_then(|l| l.split("REG_SZ").nth(1))
                .map(|p| PathBuf::from(p.trim().replace('/', "\\")))
        });
    from_reg
        .into_iter()
        .chain([
            PathBuf::from(r"C:\Program Files (x86)\Steam"),
            PathBuf::from(r"C:\Program Files\Steam"),
        ])
        .find(|p| p.join("steam.exe").is_file())
}

pub fn is_steam_running() -> bool {
    if IS_WINDOWS {
        return command("tasklist")
            .args(["/FI", "IMAGENAME eq steam.exe", "/NH"])
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .to_lowercase()
                    .contains("steam.exe")
            })
            .unwrap_or(false);
    }
    let pid_file = home().join(".steam/steam.pid");
    if let Ok(pid) = std::fs::read_to_string(pid_file) {
        let pid = pid.trim();
        if !pid.is_empty() && Path::new(&format!("/proc/{pid}")).exists() {
            return true;
        }
    }
    command("pgrep")
        .args(["-x", "steam"])
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn start_steam() -> anyhow::Result<()> {
    if IS_WINDOWS {
        let exe = steam_root()
            .map(|r| r.join("steam.exe"))
            .ok_or_else(|| anyhow::anyhow!("Steam не найден"))?;
        command(exe).arg("-silent").spawn()?;
        return Ok(());
    }
    let try_spawn = |cmd: &str, args: &[&str]| {
        command(cmd)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    };
    try_spawn("steam", &["-silent"])
        .or_else(|_| try_spawn("flatpak", &["run", "com.valvesoftware.Steam", "-silent"]))
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("Не удалось запустить Steam: {e}"))
}

// ───────────── Proton (только Linux) ─────────────

fn steam_libraries(root: &Path) -> Vec<PathBuf> {
    let mut libs = vec![root.to_path_buf()];
    if let Ok(vdf) = std::fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) {
        let re = Regex::new(r#""path"\s+"([^"]+)""#).unwrap();
        for cap in re.captures_iter(&vdf) {
            let p = PathBuf::from(cap[1].replace("\\\\", "\\"));
            if !libs.contains(&p) {
                libs.push(p);
            }
        }
    }
    libs
}

/// Папка для пользовательских Proton (GE-Proton и т.п.).
pub fn compat_tools_dir() -> PathBuf {
    steam_root()
        .map(|r| r.join("compatibilitytools.d"))
        .unwrap_or_else(|| home().join(".steam/root/compatibilitytools.d"))
}

/// Все найденные версии Proton (официальные + GE/кастомные). На Windows — пусто.
pub fn list_protons() -> Vec<ProtonInfo> {
    if IS_WINDOWS {
        return vec![];
    }
    let mut found: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut dirs_to_scan: Vec<PathBuf> = Vec::new();

    if let Some(root) = steam_root() {
        for lib in steam_libraries(&root) {
            dirs_to_scan.push(lib.join("steamapps/common"));
        }
        dirs_to_scan.push(root.join("compatibilitytools.d"));
    }
    let h = home();
    dirs_to_scan.push(h.join(".steam/root/compatibilitytools.d"));
    dirs_to_scan.push(h.join(".local/share/Steam/compatibilitytools.d"));
    dirs_to_scan.push(h.join(".var/app/com.valvesoftware.Steam/.local/share/Steam/compatibilitytools.d"));
    dirs_to_scan.push(PathBuf::from("/usr/share/steam/compatibilitytools.d"));
    dirs_to_scan.push(PathBuf::from("/usr/local/share/steam/compatibilitytools.d"));

    for dir in dirs_to_scan {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.join("proton").is_file() && arch_ok(&p) {
                let name = e.file_name().to_string_lossy().to_string();
                found.entry(name).or_insert(p);
            }
        }
    }

    let mut list: Vec<ProtonInfo> = found
        .into_iter()
        .map(|(name, p)| ProtonInfo {
            name,
            path: p.to_string_lossy().to_string(),
        })
        .collect();
    // GE-Proton и Experimental — наверх, дальше по убыванию версии
    // (с учётом чисел: GE-Proton10-25 новее, чем GE-Proton10-9).
    list.sort_by(|a, b| rank(&b.name).cmp(&rank(&a.name)).then(natural_cmp(&b.name, &a.name)));
    list
}

/// Архитектура процессора, под которую собран Proton (по ELF-заголовку wine).
/// None — не удалось определить (тогда считаем, что подходит).
fn proton_arch(dir: &Path) -> Option<&'static str> {
    use std::io::Read;
    for rel in [
        "files/bin/wine64",
        "files/bin/wine",
        "files/bin/wineserver",
        "dist/bin/wine64",
        "dist/bin/wine",
        "dist/bin/wineserver",
    ] {
        let Ok(mut f) = std::fs::File::open(dir.join(rel)) else {
            continue;
        };
        let mut h = [0u8; 20];
        if f.read_exact(&mut h).is_err() || h[0] != 0x7F || &h[1..4] != b"ELF" {
            continue;
        }
        // e_machine, little-endian
        return match u16::from_le_bytes([h[18], h[19]]) {
            0x3E => Some("x86_64"),
            0xB7 => Some("aarch64"),
            0x03 => Some("x86"),
            _ => None,
        };
    }
    None
}

/// Архитектура этого компьютера в терминах архивов GE-Proton.
pub fn host_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        _ => "x86_64",
    }
}

/// Proton собран под этот процессор (32-битный wine на x86_64 тоже годится).
fn arch_ok(dir: &Path) -> bool {
    match proton_arch(dir) {
        None => true,
        Some(a) => a == host_arch() || (a == "x86" && host_arch() == "x86_64"),
    }
}

/// Сравнение строк с числами «по-человечески»: «10-25» > «10-9».
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    fn chunks(s: &str) -> Vec<(bool, String)> {
        let mut out: Vec<(bool, String)> = vec![];
        for c in s.chars() {
            let d = c.is_ascii_digit();
            match out.last_mut() {
                Some((pd, buf)) if *pd == d => buf.push(c),
                _ => out.push((d, c.to_string())),
            }
        }
        out
    }
    let (ca, cb) = (chunks(&a.to_lowercase()), chunks(&b.to_lowercase()));
    for (x, y) in ca.iter().zip(cb.iter()) {
        let ord = match (x.0, y.0) {
            (true, true) => {
                let (nx, ny) = (x.1.trim_start_matches('0'), y.1.trim_start_matches('0'));
                nx.len().cmp(&ny.len()).then(nx.cmp(ny))
            }
            _ => x.1.cmp(&y.1),
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    ca.len().cmp(&cb.len())
}

fn rank(name: &str) -> u8 {
    let n = name.to_lowercase();
    if n.contains("ge-proton") && n.ends_with(" latest") {
        5
    } else if n.contains("ge-proton") {
        4
    } else if n.contains("cachyos") || n.contains("dwproton") || n.contains("wineland") || n.contains("proton-em") {
        3
    } else if n.contains("experimental") {
        2
    } else if n.starts_with("proton") {
        1
    } else {
        0
    }
}

/// Сборки Proton, которые лаунчер умеет скачивать сам (менеджер Proton, как в Faugus).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProtonKind {
    /// GloriousEggroll/proton-ge-custom — по умолчанию.
    Ge,
    /// CachyOS/proton-cachyos — оптимизации CachyOS, свежий Wine. Обычная сборка x86_64.
    Cachyos,
    /// То же, собранное под x86-64-v3 (AVX2, FMA…): быстрее, но только на новых CPU
    /// (Intel Haswell 2013+, AMD Excavator/Zen).
    #[serde(rename = "cachyos-v3")]
    CachyosV3,
    /// Dawn Winery DW-Proton (форк CachyOS с фиксами для гача-игр), на своём Forgejo.
    Dw,
    /// Proton-Wineland (nanomatters, бывш. proton-cachyos → proton-salkim).
    Wineland,
    /// Proton-EM — от автора Wayland-драйвера Wine (Wayland, HDR).
    Em,
}

pub const ALL_KINDS: [ProtonKind; 6] = [
    ProtonKind::Ge,
    ProtonKind::Cachyos,
    ProtonKind::CachyosV3,
    ProtonKind::Dw,
    ProtonKind::Wineland,
    ProtonKind::Em,
];

impl ProtonKind {
    pub fn title(self) -> &'static str {
        match self {
            ProtonKind::Ge => "GE-Proton",
            ProtonKind::Cachyos => "Proton-CachyOS",
            ProtonKind::CachyosV3 => "Proton-CachyOS v3",
            ProtonKind::Dw => "DW-Proton",
            ProtonKind::Wineland => "Proton-Wineland",
            ProtonKind::Em => "Proton-EM",
        }
    }
    /// API релизов (GitHub или совместимый с ним Forgejo): `…/releases`.
    fn releases_api(self) -> &'static str {
        match self {
            ProtonKind::Ge => "https://api.github.com/repos/GloriousEggroll/proton-ge-custom/releases",
            ProtonKind::Cachyos | ProtonKind::CachyosV3 => {
                "https://api.github.com/repos/CachyOS/proton-cachyos/releases"
            }
            ProtonKind::Dw => "https://dawn.wine/api/v1/repos/dawn-winery/dwproton/releases",
            ProtonKind::Wineland => "https://api.github.com/repos/nanomatters/proton-salkim/releases",
            ProtonKind::Em => "https://api.github.com/repos/BananaWorks07/Proton/releases",
        }
    }
    fn v3(self) -> bool {
        self == ProtonKind::CachyosV3
    }
    /// Папка «<сборка> Latest» — всегда самая свежая версия, обновляется сама.
    pub fn latest_dir_name(self) -> String {
        format!("{} Latest", self.title())
    }
}

/// Файл в папке «… Latest» с тегом установленной версии.
const LATEST_MARK: &str = ".ofl-latest";

#[derive(Debug, Clone, serde::Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

fn http() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent("OnlineFix-Launcher")
        .build()?)
}

async fn get_json<T: serde::de::DeserializeOwned>(url: &str) -> anyhow::Result<T> {
    let resp = http()?.get(url).send().await?;
    if resp.status() == reqwest::StatusCode::FORBIDDEN || resp.status().as_u16() == 429 {
        anyhow::bail!("GitHub временно ограничил запросы — попробуйте через несколько минут");
    }
    Ok(resp.error_for_status()?.json().await?)
}

/// Кэш списков релизов: GitHub без токена даёт 60 запросов в час.
static RELEASES_CACHE: parking_lot::Mutex<
    Option<std::collections::HashMap<&'static str, (std::time::Instant, Vec<GhRelease>)>>,
> = parking_lot::Mutex::new(None);

async fn fetch_releases(kind: ProtonKind, refresh: bool) -> anyhow::Result<Vec<GhRelease>> {
    let api = kind.releases_api();
    if !refresh {
        if let Some((t, list)) = RELEASES_CACHE.lock().as_ref().and_then(|m| m.get(api)) {
            if t.elapsed() < std::time::Duration::from_secs(600) {
                return Ok(list.clone());
            }
        }
    }
    // per_page — GitHub, limit — Forgejo; лишний параметр каждый из них игнорирует.
    let list: Vec<GhRelease> = get_json(&format!("{api}?per_page=60&limit=50")).await?;
    let list: Vec<GhRelease> = list.into_iter().filter(|r| !r.draft).collect();
    RELEASES_CACHE
        .lock()
        .get_or_insert_with(Default::default)
        .insert(api, (std::time::Instant::now(), list.clone()));
    Ok(list)
}

async fn fetch_release(kind: ProtonKind, tag: Option<&str>) -> anyhow::Result<GhRelease> {
    let api = kind.releases_api();
    match tag {
        Some(t) => get_json(&format!("{api}/tags/{t}")).await,
        None => get_json(&format!("{api}/latest")).await,
    }
}

const ARCHIVE_EXTS: [&str; 3] = [".tar.gz", ".tar.xz", ".tar.zst"];

fn archive_stem(name: &str) -> Option<&str> {
    ARCHIVE_EXTS.iter().find_map(|e| name.strip_suffix(e))
}

/// Выбирает архив под процессор из ассетов релиза.
/// GE: -x86_64 / -aarch64; CachyOS/Wineland: -x86_64, -x86_64_v3, -x86_64_wow64, -arm64;
/// DW: -x86_64; EM: без суффикса (x86_64). `v3` — нужна именно сборка x86_64_v3.
fn pick_asset<'a, A>(assets: &'a [A], name: impl Fn(&A) -> &str, v3: bool) -> Option<&'a A> {
    let arch = host_arch();
    let tars: Vec<&A> = assets.iter().filter(|a| archive_stem(name(a)).is_some()).collect();
    let is_arm = |n: &str| n.contains("aarch64") || n.contains("arm64");
    if v3 {
        if arch != "x86_64" {
            return None;
        }
        return tars
            .into_iter()
            .find(|a| archive_stem(name(a)).is_some_and(|s| s.ends_with("x86_64_v3")));
    }
    if arch == "aarch64" {
        return tars.into_iter().find(|a| is_arm(name(a)));
    }
    // Точное «-x86_64» без _v2/_v3/_wow64 — работает на любом x86_64.
    tars.iter()
        .find(|a| archive_stem(name(a)).is_some_and(|s| s.ends_with("x86_64")))
        .or_else(|| tars.iter().find(|a| !is_arm(name(a)) && !name(a).contains("x86_64_")))
        .copied()
}

/// Поддерживает ли процессор уровень x86-64-v3 (нужен для сборок *_v3).
/// Смотрим флаги в /proc/cpuinfo; если прочитать не удалось — false.
pub fn cpu_x86_64_v3() -> bool {
    if host_arch() != "x86_64" {
        return false;
    }
    let Ok(info) = std::fs::read_to_string("/proc/cpuinfo") else {
        return false;
    };
    let Some(flags) = info.lines().find(|l| l.starts_with("flags")) else {
        return false;
    };
    let have: std::collections::HashSet<&str> = flags.split_whitespace().collect();
    // abm = lzcnt (так флаг называется и у AMD, и у Intel).
    ["avx", "avx2", "bmi1", "bmi2", "f16c", "fma", "abm", "movbe", "xsave"]
        .iter()
        .all(|f| have.contains(f))
}

/// Папки compatibilitytools.d в доме пользователя (туда можно ставить и откуда удалять).
fn user_compat_dirs() -> Vec<PathBuf> {
    let h = home();
    let mut out: Vec<PathBuf> = Vec::new();
    for d in [
        compat_tools_dir(),
        h.join(".steam/root/compatibilitytools.d"),
        h.join(".local/share/Steam/compatibilitytools.d"),
        h.join(".var/app/com.valvesoftware.Steam/.local/share/Steam/compatibilitytools.d"),
    ] {
        let key = std::fs::canonicalize(&d).unwrap_or_else(|_| d.clone());
        if !out
            .iter()
            .any(|o| std::fs::canonicalize(o).unwrap_or_else(|_| o.clone()) == key)
        {
            out.push(d);
        }
    }
    out
}

/// «x» входит в «name» и сразу за ним не цифра (GE-Proton10-2 ≠ GE-Proton10-25).
fn contains_version(name: &str, x: &str) -> bool {
    if x.is_empty() {
        return false;
    }
    let mut from = 0;
    while let Some(pos) = name[from..].find(x) {
        let end = from + pos + x.len();
        if !name[end..].starts_with(|c: char| c.is_ascii_digit()) {
            return true;
        }
        from = end;
    }
    false
}

/// Ищет установленную версию: по точному имени папки (тег или имя архива),
/// затем по вхождению тега/имени архива в имя папки.
fn find_installed(kind: ProtonKind, tag: &str, stem: Option<&str>) -> Option<PathBuf> {
    let (tag_l, stem_l) = (tag.to_lowercase(), stem.map(|s| s.to_lowercase()));
    let mut fuzzy = None;
    for dir in user_compat_dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if !p.join("proton").is_file() || !arch_ok(&p) {
                continue;
            }
            let n = e.file_name().to_string_lossy().to_lowercase();
            if n.ends_with(" latest") {
                continue;
            }
            if n == tag_l || stem_l.as_deref() == Some(n.as_str()) {
                return Some(p);
            }
            let v3_ok = n.contains("_v3") == kind.v3();
            let hit = stem_l.as_deref().is_some_and(|s| contains_version(&n, s))
                || (v3_ok && contains_version(&n, &tag_l));
            if hit && fuzzy.is_none() {
                fuzzy = Some(p);
            }
        }
    }
    fuzzy
}

fn latest_dir(kind: ProtonKind) -> PathBuf {
    compat_tools_dir().join(kind.latest_dir_name())
}

/// Тег версии в папке «… Latest» (None — не установлена).
fn latest_installed_tag(kind: ProtonKind) -> Option<String> {
    let d = latest_dir(kind);
    if !d.join("proton").is_file() {
        return None;
    }
    Some(
        std::fs::read_to_string(d.join(LATEST_MARK))
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
    )
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseInfo {
    pub tag: String,
    pub published: Option<String>,
    pub size: u64,
    /// Путь к установленной папке, если эта версия уже стоит.
    pub installed: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseList {
    pub kind: ProtonKind,
    pub title: String,
    /// Имя папки «… Latest».
    pub latest_name: String,
    /// Тег, который стоит в «… Latest» (пустая строка — тег неизвестен).
    pub latest_installed: Option<String>,
    pub latest_path: String,
    pub releases: Vec<ReleaseInfo>,
}

/// Список версий сборки для менеджера Proton.
pub async fn releases(kind: ProtonKind, refresh: bool) -> anyhow::Result<ReleaseList> {
    let list = fetch_releases(kind, refresh).await?;
    let releases = list
        .iter()
        .filter_map(|r| {
            let a = pick_asset(&r.assets, |a| a.name.as_str(), kind.v3())?;
            Some(ReleaseInfo {
                tag: r.tag_name.clone(),
                published: r.published_at.clone(),
                size: a.size,
                installed: find_installed(kind, &r.tag_name, archive_stem(&a.name))
                    .map(|p| p.to_string_lossy().into_owned()),
            })
        })
        .collect();
    Ok(ReleaseList {
        kind,
        title: kind.title().into(),
        latest_name: kind.latest_dir_name(),
        latest_installed: latest_installed_tag(kind),
        latest_path: latest_dir(kind).to_string_lossy().into_owned(),
        releases,
    })
}

/// Скачивает сборку Proton и распаковывает в compatibilitytools.d.
/// `tag` — конкретная версия (None — последняя). `as_latest` — поставить в папку
/// «… Latest» (её потом обновляет `update_latest_all`).
/// `progress(pct)` — проценты загрузки, `status(text)` — этапы для уведомлений,
/// `cancel` — флаг отмены. Возвращает (версия, уже_была_установлена).
pub async fn install_proton(
    kind: ProtonKind,
    tag: Option<&str>,
    as_latest: bool,
    progress: impl Fn(u8),
    status: impl Fn(String),
    cancel: &std::sync::atomic::AtomicBool,
) -> anyhow::Result<(String, bool)> {
    use futures_util::StreamExt;
    use std::sync::atomic::Ordering;
    use tokio::io::AsyncWriteExt;

    if IS_WINDOWS {
        anyhow::bail!("На Windows Proton не нужен — игры запускаются напрямую");
    }
    let title = kind.title();
    if kind.v3() && !cpu_x86_64_v3() {
        anyhow::bail!("Процессор не поддерживает x86-64-v3 (AVX2/FMA) — скачайте обычный Proton-CachyOS");
    }
    let rel = fetch_release(kind, if as_latest { None } else { tag }).await?;
    let arch = host_arch();
    let asset = pick_asset(&rel.assets, |a| a.name.as_str(), kind.v3())
        .ok_or_else(|| anyhow::anyhow!("В релизе {} нет сборки под {arch}", rel.tag_name))?;
    let stem = archive_stem(&asset.name).unwrap_or(&asset.name).to_string();

    let dir = compat_tools_dir();
    tokio::fs::create_dir_all(&dir).await?;
    if as_latest {
        if latest_installed_tag(kind).as_deref() == Some(rel.tag_name.as_str()) {
            return Ok((rel.tag_name, true));
        }
    } else if find_installed(kind, &rel.tag_name, Some(&stem)).is_some() {
        return Ok((rel.tag_name, true));
    }

    let tmp = dir.join(format!("{}.part", asset.name));
    status(format!("Скачиваю {} ({} МБ)…", rel.tag_name, asset.size / 1_048_576));
    progress(0);
    let resp = http()?
        .get(&asset.browser_download_url)
        .send()
        .await?
        .error_for_status()?;
    let total = resp.content_length().filter(|&n| n > 0).unwrap_or(asset.size);
    let mut file = tokio::fs::File::create(&tmp).await?;
    let mut stream = resp.bytes_stream();
    let (mut done, mut last_pct) = (0u64, 0u64);
    let mut failed: Option<anyhow::Error> = None;
    while let Some(chunk) = stream.next().await {
        if cancel.load(Ordering::Relaxed) {
            failed = Some(anyhow::anyhow!("Загрузка {title} отменена"));
            break;
        }
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                failed = Some(e.into());
                break;
            }
        };
        done += chunk.len() as u64;
        if let Err(e) = file.write_all(&chunk).await {
            failed = Some(e.into());
            break;
        }
        let pct = if total > 0 { (done * 100 / total).min(100) } else { 0 };
        if pct > last_pct {
            last_pct = pct;
            progress(pct as u8);
        }
    }
    let _ = file.flush().await;
    drop(file);
    if let Some(e) = failed {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }

    status(format!("Распаковываю {title}…"));
    // Распаковываем во временную папку, потом переносим — так не остаётся
    // полураспакованных версий и не смешиваются файлы старой и новой.
    let work = dir.join(format!(".ofl-extract-{}", stem));
    let _ = tokio::fs::remove_dir_all(&work).await;
    tokio::fs::create_dir_all(&work).await?;
    // GNU tar сам определяет gzip/xz/zstd по содержимому.
    let ok = command("tar")
        .arg("-xf")
        .arg(&tmp)
        .arg("-C")
        .arg(&work)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let _ = tokio::fs::remove_file(&tmp).await;
    let top = std::fs::read_dir(&work)
        .ok()
        .and_then(|rd| rd.flatten().map(|e| e.path()).find(|p| p.join("proton").is_file()));
    let Some(top) = top.filter(|_| ok) else {
        let _ = tokio::fs::remove_dir_all(&work).await;
        anyhow::bail!("Не удалось распаковать архив {title} (нужен tar с поддержкой xz)");
    };
    let target = if as_latest {
        latest_dir(kind)
    } else {
        dir.join(top.file_name().unwrap_or_default())
    };
    if target.exists() {
        let _ = tokio::fs::remove_dir_all(&target).await;
    }
    let moved = tokio::fs::rename(&top, &target).await;
    let _ = tokio::fs::remove_dir_all(&work).await;
    moved?;
    if as_latest {
        let name = kind.latest_dir_name();
        let _ = std::fs::write(target.join(LATEST_MARK), &rel.tag_name);
        // Своё имя для Steam, иначе он покажет «… Latest» под именем версии
        // и спутает её с отдельно установленной.
        let vdf = format!(
            "\"compatibilitytools\"\n{{\n  \"compat_tools\"\n  {{\n    \"{name}\"\n    {{\n      \"install_path\" \".\"\n      \"display_name\" \"{name}\"\n      \"from_oslist\" \"windows\"\n      \"to_oslist\" \"linux\"\n    }}\n  }}\n}}\n"
        );
        let _ = std::fs::write(target.join("compatibilitytool.vdf"), vdf);
    }
    Ok((rel.tag_name, false))
}

/// Удаляет версию Proton. Только из пользовательских compatibilitytools.d —
/// официальные Proton из Steam не трогаем.
pub fn remove_proton(path: &str) -> anyhow::Result<()> {
    let p = std::fs::canonicalize(path)?;
    let parent = p.parent().ok_or_else(|| anyhow::anyhow!("Неверный путь"))?;
    let allowed = user_compat_dirs()
        .iter()
        .filter_map(|d| std::fs::canonicalize(d).ok())
        .any(|d| d == parent);
    if !allowed || !p.join("proton").is_file() {
        anyhow::bail!("Удалять можно только версии из compatibilitytools.d");
    }
    std::fs::remove_dir_all(&p)?;
    Ok(())
}

/// Обновляет все установленные «… Latest» до свежих версий (при запуске лаунчера).
pub async fn update_latest_all(status: impl Fn(String)) {
    if IS_WINDOWS {
        return;
    }
    let never = std::sync::atomic::AtomicBool::new(false);
    for kind in ALL_KINDS {
        let Some(have) = latest_installed_tag(kind) else { continue };
        let Ok(rel) = fetch_release(kind, None).await else { continue };
        if rel.tag_name == have {
            continue;
        }
        status(format!("Обновляю {} до {}…", kind.latest_dir_name(), rel.tag_name));
        match install_proton(kind, None, true, |_| {}, |_| {}, &never).await {
            Ok((tag, _)) => status(format!("{} обновлён: {}", kind.latest_dir_name(), tag)),
            Err(e) => status(format!("Не удалось обновить {}: {}", kind.latest_dir_name(), e)),
        }
    }
}

pub fn umu_available() -> bool {
    !IS_WINDOWS && crate::sys::which("umu-run").is_some()
}

// ───────────── Запуск ─────────────

pub struct RunSpec<'a> {
    /// Папка Proton (Linux). На Windows не используется.
    pub proton_dir: &'a Path,
    /// Папка совместимости игры; сам префикс Wine будет в `<compat>/pfx`.
    pub compat_dir: &'a Path,
    pub exe: &'a Path,
    pub args: Vec<String>,
    pub use_umu: bool,
    /// Включить окружение Online-Fix (Spacewar 480 + DLL overrides).
    pub online_fix: bool,
    pub dll_overrides: Option<String>,
    pub extra_env: Vec<(String, String)>,
    pub log_file: Option<PathBuf>,
    /// Подключить оверлей Steam (Shift+Tab).
    pub steam_overlay: bool,
}

pub fn spawn(spec: RunSpec) -> anyhow::Result<Child> {
    if IS_WINDOWS {
        spawn_windows(spec)
    } else {
        spawn_proton(spec)
    }
}

/// Windows: exe запускается напрямую. Если нужен администратор (типично для
/// установщиков) — повторяем через PowerShell с UAC и ждём завершения.
fn spawn_windows(spec: RunSpec) -> anyhow::Result<Child> {
    let workdir = spec.exe.parent().unwrap_or(Path::new("."));
    let mut cmd = command(spec.exe);
    // Командную строку собираем сами: установщики разбирают её по-своему
    // (NSIS ждёт `/D=C:\My Games\X` последним и без кавычек).
    let args = windows_cmdline_args(&spec.args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        for a in &args {
            cmd.raw_arg(a);
        }
    }
    #[cfg(not(windows))]
    cmd.args(&args);
    cmd.current_dir(workdir).stdin(Stdio::null());
    if spec.online_fix {
        cmd.env("SteamAppId", SPACEWAR_APPID)
            .env("SteamGameId", SPACEWAR_APPID);
    }
    for (k, v) in &spec.extra_env {
        cmd.env(k, v);
    }
    match cmd.spawn() {
        Ok(c) => Ok(c),
        // ERROR_ELEVATION_REQUIRED
        Err(e) if e.raw_os_error() == Some(740) => {
            let q = |s: &str| format!("'{}'", s.replace('\'', "''"));
            let mut ps = format!(
                "Start-Process -FilePath {} -WorkingDirectory {} -Verb RunAs -Wait",
                q(&spec.exe.to_string_lossy()),
                q(&workdir.to_string_lossy())
            );
            if !args.is_empty() {
                let list: Vec<String> = args.iter().map(|a| q(a)).collect();
                ps.push_str(&format!(" -ArgumentList {}", list.join(",")));
            }
            Ok(command("powershell")
                .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
                .stdin(Stdio::null())
                .spawn()?)
        }
        Err(e) => Err(e.into()),
    }
}

/// Окружение для установщиков под Wine: не открывать сайт в браузере
/// (winebrowser) и не создавать ярлыки/пункты меню (winemenubuilder).
pub fn installer_env() -> Vec<(String, String)> {
    if IS_WINDOWS {
        return vec![];
    }
    vec![(
        "WINEDLLOVERRIDES".into(),
        "winebrowser.exe=d;winemenubuilder.exe=d".into(),
    )]
}

/// Аргументы для командной строки Windows: с пробелами — в кавычках,
/// кроме уже закавыченных (`/DIR="…"`) и NSIS-овского `/D=…`.
pub fn windows_cmdline_args(args: &[String]) -> Vec<String> {
    args.iter()
        .map(|a| {
            if a.contains(' ') && !a.contains('"') && !a.starts_with("/D=") {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect()
}

fn spawn_proton(spec: RunSpec) -> anyhow::Result<Child> {
    std::fs::create_dir_all(spec.compat_dir)?;
    let workdir = spec.exe.parent().unwrap_or(spec.compat_dir);

    let mut cmd = if spec.use_umu && umu_available() {
        let mut c = command("umu-run");
        c.env("PROTONPATH", spec.proton_dir)
            .env("WINEPREFIX", spec.compat_dir.join("pfx"))
            .env(
                "GAMEID",
                if spec.online_fix {
                    format!("umu-{SPACEWAR_APPID}")
                } else {
                    "0".into()
                },
            )
            .arg(spec.exe);
        c
    } else {
        let mut c = command(spec.proton_dir.join("proton"));
        c.arg(if spec.online_fix {
            "waitforexitandrun"
        } else {
            "run"
        })
        .arg(spec.exe);
        c
    };
    cmd.args(&spec.args).current_dir(workdir);

    cmd.env("STEAM_COMPAT_DATA_PATH", spec.compat_dir);
    if let Some(root) = steam_root() {
        // Оверлей Steam (Shift+Tab): Steam сам подгружает его в игры через LD_PRELOAD
        // (OpenGL) и Vulkan-слой (DXVK/VKD3D). При запуске мимо Steam делаем то же самое.
        if spec.steam_overlay && spec.online_fix {
            let libs: Vec<String> = ["ubuntu12_32", "ubuntu12_64"]
                .iter()
                .map(|d| root.join(d).join("gameoverlayrenderer.so"))
                .filter(|p| p.is_file())
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            if !libs.is_empty() {
                let mut pre = std::env::var("LD_PRELOAD").unwrap_or_default();
                for l in libs {
                    if !pre.is_empty() {
                        pre.push(':');
                    }
                    pre.push_str(&l);
                }
                cmd.env("LD_PRELOAD", pre);
            }
            cmd.env("ENABLE_VK_LAYER_VALVE_steam_overlay_1", "1");
        }
        cmd.env("STEAM_COMPAT_CLIENT_INSTALL_PATH", root);
    }
    if let Some(parent) = spec.exe.parent() {
        cmd.env("STEAM_COMPAT_INSTALL_PATH", parent);
    }

    if spec.online_fix {
        cmd.env("SteamAppId", SPACEWAR_APPID)
            .env("SteamGameId", SPACEWAR_APPID)
            .env("SteamOverlayGameId", SPACEWAR_APPID);
        let ov = spec
            .dll_overrides
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| ONLINEFIX_DLL_OVERRIDES.to_string());
        cmd.env("WINEDLLOVERRIDES", ov);
    }
    for (k, v) in spec.extra_env {
        cmd.env(k, v);
    }

    match spec.log_file.and_then(|p| std::fs::File::create(p).ok()) {
        Some(f) => {
            let f2 = f.try_clone()?;
            cmd.stdout(f).stderr(f2);
        }
        None => {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    cmd.stdin(Stdio::null());
    Ok(cmd.spawn()?)
}

/// Префикс уже создан (Proton кладёт его в `<compat>/pfx`).
pub fn prefix_ready(compat_dir: &Path) -> bool {
    IS_WINDOWS || compat_dir.join("pfx/system.reg").is_file()
}

/// Создаёт префикс отдельным шагом: Proton сам готовит префикс перед первой
/// командой, поэтому запускаем безобидный `cmd /c exit` и ждём завершения.
/// Блокирующая; вызывать из spawn_blocking. На Windows ничего не делает.
pub fn init_prefix(proton_dir: &Path, compat_dir: &Path, use_umu: bool, log: Option<PathBuf>) -> anyhow::Result<()> {
    if prefix_ready(compat_dir) {
        return Ok(());
    }
    std::fs::create_dir_all(compat_dir)?;
    let mut cmd = if use_umu && umu_available() {
        let mut c = command("umu-run");
        c.env("PROTONPATH", proton_dir)
            .env("WINEPREFIX", compat_dir.join("pfx"))
            .env("GAMEID", "0")
            .arg("cmd.exe");
        c
    } else {
        let mut c = command(proton_dir.join("proton"));
        c.arg("run").arg("cmd.exe");
        c
    };
    cmd.args(["/c", "exit"]).current_dir(compat_dir);
    cmd.env("STEAM_COMPAT_DATA_PATH", compat_dir)
        .env("STEAM_COMPAT_INSTALL_PATH", compat_dir);
    if let Some(root) = steam_root() {
        cmd.env("STEAM_COMPAT_CLIENT_INSTALL_PATH", root);
    }
    for (k, v) in installer_env() {
        cmd.env(k, v);
    }
    // Вывод всегда сохраняем: при ошибке показываем его пользователю.
    let log = log.unwrap_or_else(|| compat_dir.join("prefix.log"));
    let f = std::fs::File::create(&log)?;
    cmd.stdout(f.try_clone()?).stderr(f);
    let st = cmd.stdin(Stdio::null()).status()?;
    if !prefix_ready(compat_dir) {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        let tail: Vec<&str> = text.lines().rev().filter(|l| !l.trim().is_empty()).take(6).collect();
        let tail = tail.into_iter().rev().collect::<Vec<_>>().join(" | ");
        anyhow::bail!(
            "Префикс не создался (код {:?}). Лог: {}. {}",
            st.code(),
            log.display(),
            tail
        );
    }
    Ok(())
}

/// Принудительно завершает игру: на Linux — все процессы Wine в префиксе,
/// на Windows — дерево процессов по pid.
pub fn kill(proton_dir: &Path, compat_dir: &Path, pid: Option<u32>) {
    if IS_WINDOWS {
        if let Some(pid) = pid {
            let _ = command("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .status();
        }
        return;
    }
    let candidates = [
        proton_dir.join("files/bin/wineserver"),
        proton_dir.join("dist/bin/wineserver"),
    ];
    let ws = candidates
        .iter()
        .find(|p| p.is_file())
        .cloned()
        .unwrap_or_else(|| "wineserver".into());
    let _ = command(ws)
        .arg("-k")
        .env("WINEPREFIX", compat_dir.join("pfx"))
        .status();
}

/// Разбирает строку `KEY=VAL KEY2=VAL2` из настроек игры.
pub fn parse_env(s: &str) -> Vec<(String, String)> {
    s.split_whitespace()
        .filter_map(|kv| {
            kv.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{contains_version, host_arch, natural_cmp, pick_asset, windows_cmdline_args};
    use std::cmp::Ordering::*;

    #[test]
    fn installer_args() {
        let a: Vec<String> = ["/SILENT", r#"/DIR="D:\My Games\x\game""#, r"/D=D:\My Games\x", "a b"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            windows_cmdline_args(&a),
            vec!["/SILENT", r#"/DIR="D:\My Games\x\game""#, r"/D=D:\My Games\x", "\"a b\""]
        );
    }

    #[test]
    fn proton_assets() {
        fn nm<'a>(n: &'a &str) -> &'a str {
            n
        }
        let pick = |names: &[&'static str]| pick_asset(names, nm, false).copied();
        let cachy = [
            "proton-cachyos-11.0-20261015-slr-arm64.sha512sum",
            "proton-cachyos-11.0-20261015-slr-arm64.tar.xz",
            "proton-cachyos-11.0-20261015-slr-x86_64.sha512sum",
            "proton-cachyos-11.0-20261015-slr-x86_64_v3.tar.xz",
            "proton-cachyos-11.0-20261015-slr-x86_64.tar.xz",
        ];
        let ge = ["GE-Proton11-12-aarch64.tar.gz", "GE-Proton11-12-x86_64.tar.gz", "GE-Proton11-12.sha512sum"];
        let dw = ["dwproton-11.0-17-x86_64.sha512sum", "dwproton-11.0-17-x86_64.tar.xz"];
        let em = ["proton-EM-10.0-37-HDR.sha256sum", "proton-EM-10.0-37-HDR.tar.xz"];
        if host_arch() == "x86_64" {
            assert_eq!(pick(&cachy), Some("proton-cachyos-11.0-20261015-slr-x86_64.tar.xz"));
            assert_eq!(
                pick_asset(&cachy, nm, true).copied(),
                Some("proton-cachyos-11.0-20261015-slr-x86_64_v3.tar.xz")
            );
            assert_eq!(pick_asset(&dw, nm, true).copied(), None);
            let wl = [
                "proton-wineland-11.0-20261119-x86_64.tar.xz",
                "proton-wineland-11.0-20261119-x86_64_v3.tar.xz",
                "proton-wineland-11.0-20261119-x86_64_wow64.tar.xz",
            ];
            assert_eq!(pick(&wl), Some("proton-wineland-11.0-20261119-x86_64.tar.xz"));
            assert_eq!(pick(&ge), Some("GE-Proton11-12-x86_64.tar.gz"));
            assert_eq!(pick(&dw), Some("dwproton-11.0-17-x86_64.tar.xz"));
            assert_eq!(pick(&em), Some("proton-EM-10.0-37-HDR.tar.xz"));
            assert_eq!(pick(&["GE-Proton9-1.tar.gz"]), Some("GE-Proton9-1.tar.gz"));
        }
    }

    /// Живой запрос к GitHub/Forgejo: cargo test --lib live_releases -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_releases() {
        for kind in super::ALL_KINDS {
            match super::releases(kind, true).await {
                Ok(l) => println!(
                    "{:<18} {:>3} версий, свежая: {}",
                    l.title,
                    l.releases.len(),
                    l.releases.first().map(|r| r.tag.as_str()).unwrap_or("—")
                ),
                Err(e) => panic!("{kind:?}: {e}"),
            }
        }
    }

    #[test]
    fn version_match() {
        assert!(contains_version("ge-proton10-25", "ge-proton10-25"));
        assert!(!contains_version("ge-proton10-25", "ge-proton10-2"));
        assert!(contains_version("proton-cachyos-11.0-20261015-slr-x86_64", "cachyos-11.0-20261015-slr"));
        assert!(contains_version("игры ge-proton9-1", "ge-proton9-1"));
    }

    #[test]
    fn versions() {
        assert_eq!(natural_cmp("GE-Proton10-25", "GE-Proton10-9"), Greater);
        assert_eq!(natural_cmp("GE-Proton9-27", "GE-Proton10-1"), Less);
        assert_eq!(natural_cmp("Proton 9.0", "Proton 10.0"), Less);
        assert_eq!(natural_cmp("GE-Proton10-25", "GE-Proton10-25"), Equal);
    }
}
