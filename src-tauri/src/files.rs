// SPDX-License-Identifier: GPL-3.0-or-later
//! Поиск установщиков / exe, распаковка архивов, скачивание фиксов.

use crate::catalog::Addon;
use crate::sys::command;
use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tokio::io::AsyncWriteExt;
use walkdir::WalkDir;

const ARCHIVE_EXT: &[&str] = &["zip", "rar", "7z", "iso"];
/// Пароли, которые обычно ставят на архивы с фиксами.
const ARCHIVE_PASSWORDS: &[&str] = &["", "online-fix.me", "freetp.org", "freetp"];

const JUNK_EXE: &[&str] = &[
    "unins",
    "uninstall",
    "redist",
    "vcredist",
    "vc_redist",
    "dxsetup",
    "dxwebsetup",
    "directx",
    "dotnet",
    "ndp",
    "physx",
    "crashhandler",
    "crashreport",
    "unitycrashhandler",
    "ue4prereq",
    "prereq",
    "easyanticheat",
    "eac_",
    "battleye",
    "be_",
    "launcherpatcher",
    "update",
    "7z",
    "winrar",
    "unrar",
    "quicksfv",
    "oalinst",
    "xnafx",
    "cefprocess",
    "crs-",
    "notification_helper",
    "wine",
    "rundll",
    "explorer",
    "iexplore",
    "wmplayer",
    "winecfg",
    "regedit",
];

fn lower_name(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn is_exe(p: &Path) -> bool {
    p.extension()
        .map(|e| e.eq_ignore_ascii_case("exe"))
        .unwrap_or(false)
}

fn is_junk(p: &Path) -> bool {
    let n = lower_name(p);
    JUNK_EXE
        .iter()
        .any(|j| n.starts_with(j) || n.contains(&format!("_{j}")))
        || p.components().any(|c| {
            let s = c.as_os_str().to_string_lossy().to_lowercase();
            matches!(
                s.as_str(),
                "_commonredist"
                    | "redist"
                    | "redistributables"
                    | "directx"
                    | "__installer"
                    | "support"
                    | "_redist"
                    | "prerequisites"
            )
        })
}

/// Ищет установщик (setup.exe и т.п.) в скачанной раздаче.
pub fn find_installer(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(u8, usize, PathBuf)> = None;
    for e in WalkDir::new(dir).max_depth(3).into_iter().flatten() {
        let p = e.path();
        if !e.file_type().is_file() || !is_exe(p) {
            continue;
        }
        let n = lower_name(p);
        let score = if n == "setup.exe" {
            3
        } else if n.starts_with("setup") || n.starts_with("install") {
            2
        } else if n.contains("setup") || n.contains("installer") {
            1
        } else {
            continue;
        };
        if is_junk(p) && score < 3 {
            continue;
        }
        let depth = e.depth();
        let better = match &best {
            None => true,
            Some((s, d, _)) => score > *s || (score == *s && depth < *d),
        };
        if better {
            best = Some((score, depth, p.to_path_buf()));
        }
    }
    best.map(|b| b.2)
}

/// Архивы в корне раздачи (если игра лежит в .zip/.rar/.7z/.iso).
pub fn find_archives(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = WalkDir::new(dir)
        .max_depth(2)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| {
            let ext = p
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if !ARCHIVE_EXT.contains(&ext.as_str()) {
                return false;
            }
            // Многотомные rar: берём только первую часть.
            let n = lower_name(p);
            !(n.contains(".part")
                && !n.contains(".part1.")
                && !n.contains(".part01.")
                && !n.contains(".part001."))
        })
        .collect();
    v.sort();
    v
}

/// Кандидаты на исполняемый файл игры, самые вероятные — первыми.
/// `since` — брать только exe, созданные/изменённые после этого момента
/// (на Windows так находим, куда установщик поставил игру).
pub fn find_game_exes(
    roots: &[PathBuf],
    max_depth: usize,
    since: Option<SystemTime>,
) -> Vec<String> {
    let mut found: Vec<(u64, usize, PathBuf)> = Vec::new();
    for root in roots {
        if !root.exists() {
            continue;
        }
        let walker = WalkDir::new(root)
            .max_depth(max_depth)
            .into_iter()
            .filter_entry(|e| {
                let n = e.file_name().to_string_lossy().to_lowercase();
                // В префиксе пропускаем системные каталоги Windows.
                !matches!(
                    n.as_str(),
                    "windows"
                        | "programdata"
                        | "appdata"
                        | "temp"
                        | "internet explorer"
                        | "windows nt"
                        | "windows media player"
                        | "common files"
                        | "microsoft.net"
                )
            });
        for e in walker.flatten() {
            let p = e.path();
            if !e.file_type().is_file() || !is_exe(p) || is_junk(p) {
                continue;
            }
            let n = lower_name(p);
            if n.contains("setup") || n.contains("install") {
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            let size = meta.len();
            if size < 64 * 1024 {
                continue;
            }
            if let Some(since) = since {
                let newest = [meta.created().ok(), meta.modified().ok()]
                    .into_iter()
                    .flatten()
                    .max();
                if newest.map_or(true, |t| t < since) {
                    continue;
                }
            }
            found.push((size, e.depth(), p.to_path_buf()));
        }
    }
    // Shipping-сборки UE и крупные exe — вероятнее всего сама игра.
    found.sort_by(|a, b| {
        let sa = lower_name(&a.2).contains("shipping") as u8;
        let sb = lower_name(&b.2).contains("shipping") as u8;
        sb.cmp(&sa).then(b.0.cmp(&a.0)).then(a.1.cmp(&b.1))
    });
    found.dedup_by(|a, b| a.2 == b.2);
    found
        .into_iter()
        .take(40)
        .map(|f| f.2.to_string_lossy().to_string())
        .collect()
}

/// Распаковывает архив через 7-Zip, WinRAR (Windows) или unrar/unzip, перебирая пароли.
pub fn extract(archive: &Path, dest: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest)?;
    let ext = archive
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut last_err = String::from("не найден архиватор (установите 7-Zip или WinRAR)");

    if let Some(bin) = crate::sys::seven_zip() {
        for pw in ARCHIVE_PASSWORDS {
            let o = command(&bin)
                .arg("x")
                .arg("-y")
                .arg(format!("-p{pw}"))
                .arg(format!("-o{}", dest.display()))
                .arg(archive)
                .output()?;
            if o.status.success() {
                return Ok(());
            }
            last_err = String::from_utf8_lossy(&o.stderr).trim().to_string();
        }
    }
    if let Some(bin) = crate::sys::winrar() {
        // WinRAR.exe x -ibck (без окна) -y -o+ -p<пароль> архив папка\
        let mut target = dest.as_os_str().to_owned();
        target.push("\\");
        for pw in ARCHIVE_PASSWORDS {
            let pw_arg = if pw.is_empty() {
                "-p-".to_string()
            } else {
                format!("-p{pw}")
            };
            let st = command(&bin)
                .args(["x", "-ibck", "-y", "-o+", &pw_arg])
                .arg(archive)
                .arg(&target)
                .status()?;
            if st.success() {
                return Ok(());
            }
            last_err = format!("WinRAR вернул код {}", st.code().unwrap_or(-1));
        }
    }
    if ext == "rar" {
        for pw in ARCHIVE_PASSWORDS {
            let pw_arg = if pw.is_empty() {
                "-p-".to_string()
            } else {
                format!("-p{pw}")
            };
            if let Ok(o) = command("unrar")
                .args(["x", "-o+", &pw_arg])
                .arg(archive)
                .arg(format!("{}/", dest.display()))
                .output()
            {
                if o.status.success() {
                    return Ok(());
                }
                last_err = String::from_utf8_lossy(&o.stderr).trim().to_string();
            }
        }
    }
    if ext == "zip" {
        if let Ok(o) = command("unzip")
            .args(["-o", "-q"])
            .arg(archive)
            .arg("-d")
            .arg(dest)
            .output()
        {
            if o.status.success() {
                return Ok(());
            }
        }
    }
    anyhow::bail!("Не удалось распаковать {}: {last_err}", archive.display())
}

/// Скачивает файл фикса по прямой ссылке. Возвращает путь к файлу.
pub async fn download_addon(addon: &Addon, dest_dir: &Path) -> anyhow::Result<PathBuf> {
    let url = addon
        .uris
        .first()
        .ok_or_else(|| anyhow::anyhow!("У фикса нет ссылки"))?;
    tokio::fs::create_dir_all(dest_dir).await?;

    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) OnlineFix-Launcher/0.1")
        .cookie_store(true)
        .build()?;

    // Сайт иногда требует сначала открыть страницу getfile (ставит cookie).
    if let Some(gf) = &addon.getfile_url {
        let _ = client.get(gf).send().await;
    }
    let mut req = client.get(url);
    if let Some(r) = &addon.referer {
        req = req.header(reqwest::header::REFERER, r);
    }
    let resp = req.send().await?.error_for_status()?;

    let ctype = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    if ctype.contains("text/html") {
        anyhow::bail!("Сайт вернул страницу вместо файла (возможно, защита от ботов). Скачайте фикс вручную со страницы игры.");
    }

    let filename = resp
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(|cd| {
            let re = regex::Regex::new(r#"filename\*?=(?:UTF-8'')?"?([^";]+)"?"#).ok()?;
            re.captures(cd).map(|c| {
                urlencoding::decode(&c[1])
                    .map(|s| s.into_owned())
                    .unwrap_or_else(|_| c[1].to_string())
            })
        })
        .map(|n| n.replace(['/', '\\'], "_"))
        .unwrap_or_else(|| format!("{}.zip", sanitize(&addon.title)));

    let path = dest_dir.join(filename);
    let mut file = tokio::fs::File::create(&path).await?;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        file.write_all(&chunk?).await?;
    }
    file.flush().await?;
    Ok(path)
}

/// Копирует содержимое `src` поверх `dst` (для установки фикса в папку игры).
pub fn copy_over(src: &Path, dst: &Path) -> anyhow::Result<usize> {
    // Если архив содержит единственную папку — заходим внутрь.
    let mut root = src.to_path_buf();
    loop {
        let entries: Vec<_> = std::fs::read_dir(&root)?.flatten().collect();
        if entries.len() == 1
            && entries[0].path().is_dir()
            && !dst.join(entries[0].file_name()).exists()
        {
            root = entries[0].path();
        } else {
            break;
        }
    }
    let mut n = 0;
    for e in WalkDir::new(&root).min_depth(1).into_iter().flatten() {
        let rel = e.path().strip_prefix(&root)?;
        let target = dst.join(rel);
        if e.file_type().is_dir() {
            std::fs::create_dir_all(&target)?;
        } else {
            if let Some(p) = target.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::copy(e.path(), &target)?;
            n += 1;
        }
    }
    Ok(n)
}

pub fn sanitize(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches('_').chars().take(60).collect()
}

/// Фикс в виде установщика Inno Setup (так часто раздаёт freetp.org) —
/// его не распаковать архиватором, нужно запускать.
pub fn is_inno_installer(path: &Path) -> bool {
    let is_exe = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("exe"));
    if !is_exe {
        return false;
    }
    let Ok(data) = std::fs::read(path) else {
        return false;
    };
    let head = &data[..data.len().min(4 * 1024 * 1024)];
    head.windows(10).any(|w| w == b"Inno Setup")
}

/// Есть ли в папке игры файлы фикса: Online-Fix (OnlineFix64.dll, OnlineFix.ini)
/// или FreeTP (SteamFix64.dll, SteamFix.ini).
pub fn fix_present(dir: &Path) -> bool {
    walkdir::WalkDir::new(dir)
        .max_depth(3)
        .into_iter()
        .flatten()
        .any(|e| {
            let n = e.file_name().to_string_lossy().to_lowercase();
            (n.starts_with("onlinefix") || n.starts_with("steamfix"))
                && (n.ends_with(".dll") || n.ends_with(".ini"))
        })
}
