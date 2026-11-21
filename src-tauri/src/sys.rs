// SPDX-License-Identifier: GPL-3.0-or-later
//! Платформенные мелочи: запуск процессов без консоли, поиск программ.

use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Command;

pub const IS_WINDOWS: bool = cfg!(target_os = "windows");

/// `Command` без всплывающего окна консоли на Windows.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// Ищет исполняемый файл в PATH.
pub fn which(bin: &str) -> Option<PathBuf> {
    let names: Vec<String> = if IS_WINDOWS {
        vec![format!("{bin}.exe"), bin.to_string()]
    } else {
        vec![bin.to_string()]
    };
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .flat_map(|p| names.iter().map(move |n| p.join(n)))
        .find(|p| p.is_file())
}

/// Тип файловой системы, на которой лежит путь (Linux, `stat -f`). Путь может
/// ещё не существовать — тогда смотрим ближайшую существующую папку выше.
pub fn fs_type(path: &std::path::Path) -> Option<String> {
    if IS_WINDOWS {
        return None;
    }
    let mut p = path;
    while !p.exists() {
        p = p.parent()?;
    }
    let out = command("stat").args(["-f", "-c", "%T"]).arg(p).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_lowercase())
}

/// Префикс Wine/Proton не работает на файловых системах без симлинков и прав Unix
/// (разделы Windows: NTFS через ntfs-3g/ntfs3, exFAT, FAT).
pub fn fs_unsuitable_for_prefix(fs: &str) -> bool {
    matches!(fs, "vfat" | "msdos" | "exfat" | "fuseblk" | "ntfs" | "ntfs3" | "fat" | "fat32")
}

fn program_files() -> Vec<PathBuf> {
    ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"]
        .iter()
        .filter_map(|v| std::env::var_os(v).map(PathBuf::from))
        .chain([
            PathBuf::from(r"C:\Program Files"),
            PathBuf::from(r"C:\Program Files (x86)"),
        ])
        .collect()
}

/// 7-Zip: PATH или стандартные папки установки на Windows.
pub fn seven_zip() -> Option<PathBuf> {
    for b in ["7z", "7zz", "7za"] {
        if let Some(p) = which(b) {
            return Some(p);
        }
    }
    if IS_WINDOWS {
        for pf in program_files() {
            let p = pf.join("7-Zip").join("7z.exe");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// WinRAR (умеет rar/zip/7z/iso) — только Windows.
pub fn winrar() -> Option<PathBuf> {
    if !IS_WINDOWS {
        return None;
    }
    which("WinRAR").or_else(|| {
        program_files()
            .into_iter()
            .map(|pf| pf.join("WinRAR").join("WinRAR.exe"))
            .find(|p| p.is_file())
    })
}
