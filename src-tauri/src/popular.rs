// SPDX-License-Identifier: GPL-3.0-or-later
//! Рейтинг популярности игр: раздел «Сетевые игры» FreeTP, отсортированный
//! по посещаемости. Возвращает id статей по порядку (самые посещаемые первыми).

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

/// Сколько страниц списка брать (по ~13 игр на странице → ~250 игр).
const PAGES: u32 = 20;
const TTL_SECS: u64 = 24 * 3600;
const BASE: &str = "https://freetp.org/po-seti/";

#[derive(Serialize, Deserialize, Default)]
struct Cache {
    fetched: u64,
    ids: Vec<u32>,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub async fn ranking(cache_dir: &Path, refresh: bool) -> anyhow::Result<Vec<u32>> {
    let file = cache_dir.join("popular.json");
    let cached: Option<Cache> = std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    if let Some(c) = &cached {
        if !refresh && !c.ids.is_empty() && now().saturating_sub(c.fetched) < TTL_SECS {
            return Ok(c.ids.clone());
        }
    }
    match fetch().await {
        Ok(ids) if !ids.is_empty() => {
            let _ = std::fs::create_dir_all(cache_dir);
            let _ = std::fs::write(
                &file,
                serde_json::to_vec(&Cache {
                    fetched: now(),
                    ids: ids.clone(),
                })?,
            );
            Ok(ids)
        }
        // Нет сети или сайт поменялся — отдаём старый рейтинг, если он есть.
        other => match cached {
            Some(c) if !c.ids.is_empty() => Ok(c.ids),
            _ => other,
        },
    }
}

async fn fetch() -> anyhow::Result<Vec<u32>> {
    let c = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36")
        .cookie_store(true)
        .timeout(Duration::from_secs(15))
        .build()?;

    // Сортировка в DLE хранится в сессии: берём cookie и hash, затем выставляем «по посещаемости».
    let first = c.get(BASE).send().await?.error_for_status()?.text().await?;
    let hash = regex::Regex::new(r#"dle_login_hash\s*=\s*['"]([0-9a-f]*)['"]"#)?
        .captures(&first)
        .map(|m| m[1].to_string())
        .unwrap_or_default();
    let form = reqwest::multipart::Form::new()
        .text("dlenewssortby", "news_read")
        .text("dledirection", "desc")
        .text("set_new_sort", "dle_sort_cat_21")
        .text("set_direction_sort", "dle_direction_cat_21")
        .text("user_hash", hash);
    c.post("https://freetp.org/index.php?controller=ajax&mod=adminfunction&action=setsort")
        .header("X-Requested-With", "XMLHttpRequest")
        .header(reqwest::header::REFERER, BASE)
        .multipart(form)
        .send()
        .await?
        .error_for_status()?;

    let re = regex::Regex::new(
        r#"class="game-card__media-link"\s+href="https://freetp\.org/po-seti/(\d+)-"#,
    )?;
    let mut ids: Vec<u32> = vec![];
    for page in 1..=PAGES {
        let url = if page == 1 {
            BASE.to_string()
        } else {
            format!("{BASE}page/{page}/")
        };
        let html = match c.get(&url).send().await.and_then(|r| r.error_for_status()) {
            Ok(r) => r.text().await?,
            Err(e) if !ids.is_empty() => {
                eprintln!("popular: страница {page}: {e}");
                break;
            }
            Err(e) => return Err(e.into()),
        };
        for m in re.captures_iter(&html) {
            if let Ok(id) = m[1].parse::<u32>() {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    /// cargo test --lib popular -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn fetch_live() {
        let ids = super::fetch().await.unwrap();
        println!("{} ids, first: {:?}", ids.len(), &ids[..ids.len().min(10)]);
        assert!(ids.len() > 100);
    }
}
