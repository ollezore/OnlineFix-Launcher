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
    if n.contains("ge-proton") {
        4
    } else if n.contains("cachyos") || n.contains("dwproton") || n.contains("proton-em") {
        3
    } else if n.contains("experimental") {
        2
    } else if n.starts_with("proton") {
        1
    } else {
        0
    }
}

/// Сборки Proton, которые лаунчер умеет скачивать сам.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, Serialize)]
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
    /// Proton-EM — от автора Wayland-драйвера Wine (Wayland, HDR).
    Em,
}

impl ProtonKind {
    pub fn title(self) -> &'static str {
        match self {
            ProtonKind::Ge => "GE-Proton",
            ProtonKind::Cachyos => "Proton-CachyOS",
            ProtonKind::CachyosV3 => "Proton-CachyOS v3",
            ProtonKind::Dw => "DW-Proton",
            ProtonKind::Em => "Proton-EM",
        }
    }
    /// API последнего релиза (GitHub или совместимый с ним Forgejo).
    fn latest_url(self) -> &'static str {
        match self {
            ProtonKind::Ge => "https://api.github.com/repos/GloriousEggroll/proton-ge-custom/releases/latest",
            ProtonKind::Cachyos | ProtonKind::CachyosV3 => {
                "https://api.github.com/repos/CachyOS/proton-cachyos/releases/latest"
            }
            ProtonKind::Dw => "https://dawn.wine/api/v1/repos/dawn-winery/dwproton/releases/latest",
            ProtonKind::Em => "https://api.github.com/repos/BananaWorks07/Proton/releases/latest",
        }
    }
}

const ARCHIVE_EXTS: [&str; 3] = [".tar.gz", ".tar.xz", ".tar.zst"];

fn archive_stem(name: &str) -> Option<&str> {
    ARCHIVE_EXTS.iter().find_map(|e| name.strip_suffix(e))
}

/// Выбирает архив под процессор из ассетов релиза.
/// GE: -x86_64 / -aarch64; CachyOS: -x86_64, -x86_64_v3 (только новые CPU), -arm64;
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
    // Точное «-x86_64» без _v2/_v3 — работает на любом x86_64.
    tars.iter()
        .find(|a| archive_stem(name(a)).is_some_and(|s| s.ends_with("x86_64")))
        .or_else(|| tars.iter().find(|a| !is_arm(name(a)) && !name(a).contains("x86_64_v")))
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

/// Скачивает свежую сборку Proton и распаковывает в compatibilitytools.d.
/// `progress(pct)` — проценты загрузки (0..=100), `status(text)` — этапы для уведомлений.
/// Возвращает (версия, уже_была_установлена).
pub async fn install_proton(
    kind: ProtonKind,
    progress: impl Fn(u8),
    status: impl Fn(String),
) -> anyhow::Result<(String, bool)> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    if IS_WINDOWS {
        anyhow::bail!("На Windows Proton не нужен — игры запускаются напрямую");
    }
    #[derive(serde::Deserialize)]
    struct Asset {
        name: String,
        browser_download_url: String,
        #[serde(default)]
        size: u64,
    }
    #[derive(serde::Deserialize)]
    struct Release {
        tag_name: String,
        assets: Vec<Asset>,
    }

    let title = kind.title();
    let client = reqwest::Client::builder()
        .user_agent("OnlineFix-Launcher/0.3")
        .build()?;
    let rel: Release = client
        .get(kind.latest_url())
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let arch = host_arch();
    let v3 = kind == ProtonKind::CachyosV3;
    if v3 && !cpu_x86_64_v3() {
        anyhow::bail!("Процессор не поддерживает x86-64-v3 (AVX2/FMA) — скачайте обычный Proton-CachyOS");
    }
    let asset = pick_asset(&rel.assets, |a| a.name.as_str(), v3)
        .ok_or_else(|| anyhow::anyhow!("В релизе {} нет сборки под {arch}", rel.tag_name))?;

    let dir = compat_tools_dir();
    tokio::fs::create_dir_all(&dir).await?;
    let stem = archive_stem(&asset.name).unwrap_or(&asset.name).to_string();
    let have = [rel.tag_name.as_str(), stem.as_str()].iter().any(|name| {
        let p = dir.join(name);
        p.join("proton").is_file() && arch_ok(&p)
    });
    if have {
        return Ok((rel.tag_name, true));
    }
    let tmp = dir.join(format!("{}.part", asset.name));
    status(format!(
        "Скачиваю {} ({} МБ)…",
        rel.tag_name,
        asset.size / 1_048_576
    ));
    progress(0);

    let resp = client
        .get(&asset.browser_download_url)
        .send()
        .await?
        .error_for_status()?;
    let total = resp.content_length().filter(|&n| n > 0).unwrap_or(asset.size);
    let mut file = tokio::fs::File::create(&tmp).await?;
    let mut stream = resp.bytes_stream();
    let (mut done, mut last_pct) = (0u64, 0u64);
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                drop(file);
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(e.into());
            }
        };
        done += chunk.len() as u64;
        file.write_all(&chunk).await?;
        let pct = if total > 0 { (done * 100 / total).min(100) } else { 0 };
        if pct > last_pct {
            last_pct = pct;
            progress(pct as u8);
        }
    }
    file.flush().await?;
    drop(file);

    status(format!("Распаковываю {title}…"));
    // Если раньше была поставлена сборка с тем же именем (например, не под тот
    // процессор) — убираем её, иначе файлы смешаются.
    if let Ok(o) = command("sh")
        .arg("-c")
        .arg("tar -tf \"$1\" | head -n 1")
        .arg("sh")
        .arg(&tmp)
        .output()
    {
        let top = String::from_utf8_lossy(&o.stdout);
        let top = top.trim().trim_start_matches("./").split('/').next().unwrap_or("").to_string();
        if !top.is_empty() && dir.join(&top).is_dir() {
            let _ = tokio::fs::remove_dir_all(dir.join(&top)).await;
        }
    }
    // GNU tar сам определяет gzip/xz/zstd по содержимому.
    let status_code = command("tar")
        .arg("-xf")
        .arg(&tmp)
        .arg("-C")
        .arg(&dir)
        .status()?;
    let _ = tokio::fs::remove_file(&tmp).await;
    if !status_code.success() {
        anyhow::bail!("Не удалось распаковать архив {title} (нужен tar с поддержкой xz)");
    }
    Ok((rel.tag_name, false))
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
    use super::{host_arch, natural_cmp, pick_asset, windows_cmdline_args};
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
            assert_eq!(pick(&ge), Some("GE-Proton11-12-x86_64.tar.gz"));
            assert_eq!(pick(&dw), Some("dwproton-11.0-17-x86_64.tar.xz"));
            assert_eq!(pick(&em), Some("proton-EM-10.0-37-HDR.tar.xz"));
            assert_eq!(pick(&["GE-Proton9-1.tar.gz"]), Some("GE-Proton9-1.tar.gz"));
        }
    }

    #[test]
    fn versions() {
        assert_eq!(natural_cmp("GE-Proton10-25", "GE-Proton10-9"), Greater);
        assert_eq!(natural_cmp("GE-Proton9-27", "GE-Proton10-1"), Less);
        assert_eq!(natural_cmp("Proton 9.0", "Proton 10.0"), Less);
        assert_eq!(natural_cmp("GE-Proton10-25", "GE-Proton10-25"), Equal);
    }
}
