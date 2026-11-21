// SPDX-License-Identifier: GPL-3.0-or-later
//! Каталог игр из открытых баз freetp-hydra-link (авторизация не нужна).

use serde::{Deserialize, Serialize};
use std::path::Path;

pub const SOURCE_ARACHNEL: &str =
    "https://gitlab.com/BadKiko/freetp-hydra-link/-/raw/main/games-arachnel.json?ref_type=heads";
pub const SOURCE_HYDRA: &str =
    "https://gitlab.com/BadKiko/freetp-hydra-link/-/raw/main/games.json?ref_type=heads";

/// База Online-Fix в формате Hydra. Оригинал закрыт Cloudflare (качаем через
/// WebView), зеркала на GitHub отстают, но доступны всегда.
pub const ONLINEFIX_ORIGINAL: &str = "https://hydralinks.cloud/sources/onlinefix.json";
pub const ONLINEFIX_MIRRORS: &[&str] = &[
    "https://raw.githubusercontent.com/ArnamentGames/HydraLinks/main/onlinefix.json",
    "https://raw.githubusercontent.com/TypeHintsFun/HydraRepacks/main/onlinefix.json",
];

/// Каталог и откуда он взят — для подписи в интерфейсе.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogResp {
    pub entries: Vec<CatalogEntry>,
    /// Человекочитаемое происхождение базы.
    pub origin: String,
    /// Дата самой свежей раздачи в базе.
    pub newest: String,
}

pub fn source_url(source: &str) -> &'static str {
    match source {
        "hydra" => SOURCE_HYDRA,
        _ => SOURCE_ARACHNEL,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Addon {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub uris: Vec<String>,
    #[serde(default)]
    pub referer: Option<String>,
    #[serde(default)]
    pub getfile_url: Option<String>,
    #[serde(default)]
    pub delivery: Option<String>,
    /// Необязательное дополнение (мод-пак и т.п.) — автоматически не ставим.
    #[serde(default)]
    pub optional: Option<bool>,
    #[serde(default)]
    pub file_size: Option<String>,
}

impl Addon {
    /// Онлайн-фикс, который можно скачать по прямой ссылке.
    /// Несколько таких у одной игры — это ВАРИАНТЫ (разные версии, Steam/LAN),
    /// ставится только один.
    pub fn is_fix(&self) -> bool {
        let kind_ok = self.kind.is_empty() || self.kind == "addon" || self.kind == "fix";
        let direct = !matches!(self.delivery.as_deref(), Some("magnet"))
            && self.uris.first().is_some_and(|u| u.starts_with("http"));
        let modpack = self.title.to_lowercase().contains("mod pack");
        kind_ok && direct && !self.optional.unwrap_or(false) && !modpack
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEntry {
    title: String,
    #[serde(default)]
    uris: Vec<String>,
    #[serde(default)]
    file_size: String,
    #[serde(default)]
    upload_date: String,
    #[serde(default)]
    article_url: Option<String>,
    #[serde(default)]
    install_kind: Option<u8>,
    #[serde(default)]
    addons: Vec<Addon>,
}

#[derive(Debug, Deserialize)]
struct RawCatalog {
    #[serde(default)]
    downloads: Vec<RawEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub title: String,
    /// Имя релиза из magnet (dn=) — обычно содержит версию и автора сборки.
    pub release: String,
    pub magnet: String,
    pub info_hash: String,
    pub file_size: String,
    pub upload_date: String,
    pub article_url: Option<String>,
    /// 0 — обычная раздача, 1 — нужен отдельный фикс (addons), 2 — раздача уже с фиксом.
    pub install_kind: u8,
    pub addons: Vec<Addon>,
    /// "freetp" или "onlinefix".
    #[serde(default)]
    pub source: String,
}

pub async fn load(cache_dir: &Path, source: &str, refresh: bool) -> anyhow::Result<CatalogResp> {
    let cache = cache_dir.join(format!("catalog-{source}.json"));
    let fresh_enough = std::fs::metadata(&cache)
        .and_then(|m| m.modified())
        .map(|t| t.elapsed().map(|e| e.as_secs() < 6 * 3600).unwrap_or(false))
        .unwrap_or(false);

    let bytes = if !refresh && fresh_enough {
        tokio::fs::read(&cache).await?
    } else {
        match download(source_url(source)).await {
            Ok(b) => {
                let _ = tokio::fs::create_dir_all(cache_dir).await;
                let _ = tokio::fs::write(&cache, &b).await;
                b
            }
            // Нет сети — работаем с кэшем; если кэша нет — пробуем урезанную базу Hydra
            // (те же игры, но без фиксов и ссылок на статьи).
            Err(e) => match tokio::fs::read(&cache).await {
                Ok(b) => b,
                Err(_) if source != "hydra" => download(SOURCE_HYDRA)
                    .await
                    .map_err(|_| anyhow::anyhow!("Не удалось скачать каталог: {e}"))?,
                Err(_) => anyhow::bail!("Не удалось скачать каталог: {e}"),
            },
        }
    };

    let entries = parse(&bytes, "freetp")?;
    Ok(CatalogResp {
        newest: newest(&entries),
        origin: "GitLab · BadKiko/freetp-hydra-link".into(),
        entries,
    })
}

pub fn newest(entries: &[CatalogEntry]) -> String {
    entries
        .iter()
        .map(|e| e.upload_date.as_str())
        .max()
        .unwrap_or("")
        .to_string()
}

/// Онлайн-фикс раздачи называют «Game Name 1.5.3151.0» / «Game Build 01102024» —
/// версию убираем из названия, оставляя её в имени релиза.
pub fn clean_title(title: &str) -> String {
    let re = regex::Regex::new(
        r"(?i)\s+(v?\d+(\.\d+)+[\w.\-]*|build[\s.]*\d+[\w.\-]*|b\d{3,}|\d{8,}|\(build[^)]*\)|rev\.?\s*\d+)\s*$",
    )
    .unwrap();
    let mut t = title.trim().to_string();
    loop {
        let n = re.replace(&t, "").trim().to_string();
        if n == t || n.is_empty() {
            break;
        }
        t = n;
    }
    t
}

/// Разбор базы в формате Hydra (`downloads: [{title, uris, fileSize, uploadDate…}]`).
pub fn parse(bytes: &[u8], source: &str) -> anyhow::Result<Vec<CatalogEntry>> {
    let raw: RawCatalog = serde_json::from_slice(bytes)?;
    let onlinefix = source == "onlinefix";
    let mut out: Vec<CatalogEntry> = raw
        .downloads
        .into_iter()
        .filter_map(|e| {
            let magnet = e.uris.into_iter().find(|u| u.starts_with("magnet:"))?;
            let info_hash = magnet_info_hash(&magnet)?;
            let release = magnet_param(&magnet, "dn").unwrap_or_else(|| e.title.clone());
            let (title, release) = if onlinefix {
                (clean_title(&e.title), e.title.clone())
            } else {
                (e.title, release)
            };
            Some(CatalogEntry {
                title,
                release,
                magnet,
                info_hash,
                file_size: e.file_size,
                upload_date: e.upload_date,
                article_url: e.article_url,
                // У Online-Fix фикс всегда уже в раздаче.
                install_kind: if onlinefix {
                    2
                } else {
                    e.install_kind.unwrap_or(0)
                },
                addons: e.addons,
                source: source.to_string(),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        a.title
            .to_lowercase()
            .cmp(&b.title.to_lowercase())
            .then(b.upload_date.cmp(&a.upload_date))
    });
    Ok(out)
}

pub async fn download(url: &str) -> anyhow::Result<Vec<u8>> {
    let resp = reqwest::Client::builder()
        .user_agent("OnlineFix-Launcher/0.1")
        .build()?
        .get(url)
        .send()
        .await?
        .error_for_status()?;
    Ok(resp.bytes().await?.to_vec())
}

fn magnet_param(magnet: &str, key: &str) -> Option<String> {
    let query = magnet.split_once('?')?.1;
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| {
            urlencoding::decode(&v.replace('+', " "))
                .map(|s| s.into_owned())
                .unwrap_or_else(|_| v.to_string())
        })
    })
}

/// Возвращает info-hash в hex (в games.json он в base32 — конвертируем).
pub fn magnet_info_hash(magnet: &str) -> Option<String> {
    let xt = magnet_param(magnet, "xt")?;
    let h = xt.strip_prefix("urn:btih:")?;
    match h.len() {
        40 => Some(h.to_lowercase()),
        32 => base32_to_hex(h),
        _ => None,
    }
}

/// Приводит magnet к hex-хешу — так его гарантированно примет любой парсер.
pub fn normalize_magnet(magnet: &str) -> String {
    let Some(hex) = magnet_info_hash(magnet) else {
        return magnet.to_string();
    };
    let Some((_, query)) = magnet.split_once('?') else {
        return magnet.to_string();
    };
    let rest: Vec<&str> = query
        .split('&')
        .filter(|kv| !kv.starts_with("xt="))
        .collect();
    let mut s = format!("magnet:?xt=urn:btih:{hex}");
    for kv in rest {
        s.push('&');
        s.push_str(kv);
    }
    s
}

fn base32_to_hex(s: &str) -> Option<String> {
    const ALPHA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut bits: u64 = 0;
    let mut nbits = 0u32;
    let mut out = Vec::with_capacity(20);
    for c in s.to_ascii_uppercase().bytes() {
        let v = ALPHA.iter().position(|&a| a == c)? as u64;
        bits = (bits << 5) | v;
        nbits += 5;
        if nbits >= 8 {
            nbits -= 8;
            out.push(((bits >> nbits) & 0xff) as u8);
        }
    }
    (out.len() == 20).then(|| out.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_hash() {
        let m = "magnet:?xt=urn:btih:IN3KNR7DPZ33UFMN4OTEOL7HCAY3P2KM&tr=x";
        let h = magnet_info_hash(m).unwrap();
        assert_eq!(h.len(), 40);
        assert!(normalize_magnet(m).starts_with(&format!("magnet:?xt=urn:btih:{h}&tr=x")));
    }
}

#[cfg(test)]
mod title_tests {
    use super::clean_title;

    #[test]
    fn clean_titles() {
        assert_eq!(
            clean_title("Warhammer 40000 Darktide 1.5.3151.0"),
            "Warhammer 40000 Darktide"
        );
        assert_eq!(
            clean_title("Backrooms Escape Together Build 01102024"),
            "Backrooms Escape Together"
        );
        assert_eq!(
            clean_title("Nine-Ball Roulette 0.7.17"),
            "Nine-Ball Roulette"
        );
        assert_eq!(clean_title("Left 4 Dead 2"), "Left 4 Dead 2");
        assert_eq!(
            clean_title("Mars First Logistics 202306211528"),
            "Mars First Logistics"
        );
        assert_eq!(clean_title("F1 2019 1.22"), "F1 2019");
        assert_eq!(
            clean_title("Hearts of Iron IV v1.12.4"),
            "Hearts of Iron IV"
        );
    }
}
