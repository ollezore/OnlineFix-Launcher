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
            if p.join("proton").is_file() {
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
        3
    } else if n.contains("experimental") {
        2
    } else if n.starts_with("proton") {
        1
    } else {
        0
    }
}

/// Скачивает свежий GE-Proton с GitHub и распаковывает в compatibilitytools.d.
/// Возвращает (версия, уже_была_установлена).
pub async fn install_ge_proton(progress: impl Fn(String)) -> anyhow::Result<(String, bool)> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    if IS_WINDOWS {
        anyhow::bail!("На Windows Proton не нужен — игры запускаются напрямую");
    }
    #[derive(serde::Deserialize)]
    struct Asset {
        name: String,
        browser_download_url: String,
        size: u64,
    }
    #[derive(serde::Deserialize)]
    struct Release {
        tag_name: String,
        assets: Vec<Asset>,
    }

    let client = reqwest::Client::builder()
        .user_agent("LinFix/0.1")
        .build()?;
    let rel: Release = client
        .get("https://api.github.com/repos/GloriousEggroll/proton-ge-custom/releases/latest")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let asset = rel
        .assets
        .iter()
        .find(|a| a.name.ends_with(".tar.gz"))
        .ok_or_else(|| anyhow::anyhow!("В релизе {} нет архива .tar.gz", rel.tag_name))?;

    let dir = compat_tools_dir();
    tokio::fs::create_dir_all(&dir).await?;
    if dir.join(&rel.tag_name).join("proton").is_file() {
        return Ok((rel.tag_name, true));
    }
    let tmp = dir.join(format!("{}.part", asset.name));
    progress(format!(
        "Скачиваю {} ({} МБ)…",
        rel.tag_name,
        asset.size / 1_048_576
    ));

    let resp = client
        .get(&asset.browser_download_url)
        .send()
        .await?
        .error_for_status()?;
    let mut file = tokio::fs::File::create(&tmp).await?;
    let mut stream = resp.bytes_stream();
    let (mut done, mut last_pct) = (0u64, 0u64);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        done += chunk.len() as u64;
        file.write_all(&chunk).await?;
        let pct = if asset.size > 0 {
            done * 100 / asset.size
        } else {
            0
        };
        if pct >= last_pct + 20 {
            last_pct = pct;
            progress(format!("GE-Proton: {pct}%"));
        }
    }
    file.flush().await?;
    drop(file);

    progress("Распаковываю GE-Proton…".into());
    let status = command("tar")
        .arg("-xzf")
        .arg(&tmp)
        .arg("-C")
        .arg(&dir)
        .status()?;
    let _ = tokio::fs::remove_file(&tmp).await;
    if !status.success() {
        anyhow::bail!("Не удалось распаковать архив GE-Proton");
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
    cmd.args(&spec.args)
        .current_dir(workdir)
        .stdin(Stdio::null());
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
            if !spec.args.is_empty() {
                let list: Vec<String> = spec.args.iter().map(|a| q(a)).collect();
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
    use super::natural_cmp;
    use std::cmp::Ordering::*;

    #[test]
    fn versions() {
        assert_eq!(natural_cmp("GE-Proton10-25", "GE-Proton10-9"), Greater);
        assert_eq!(natural_cmp("GE-Proton9-27", "GE-Proton10-1"), Less);
        assert_eq!(natural_cmp("Proton 9.0", "Proton 10.0"), Less);
        assert_eq!(natural_cmp("GE-Proton10-25", "GE-Proton10-25"), Equal);
    }
}
