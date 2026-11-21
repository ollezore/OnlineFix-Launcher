// SPDX-License-Identifier: GPL-3.0-or-later
//! Обложки игр: магазин Steam (поиск по названию, без ключа API),
//! а если игры там нет — постер со страницы игры на FreeTP.

use serde::Deserialize;
use std::time::Duration;

#[derive(Deserialize)]
struct SearchResp {
    #[serde(default)]
    items: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    id: u64,
    name: String,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    tiny_image: Option<String>,
}

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) OnlineFix-Launcher/0.1")
        .timeout(Duration::from_secs(10))
        .build()?)
}

/// Варианты названия для поиска: «Age Of Mythology (Эпоха Мифологии)» →
/// «Age Of Mythology»; «Aground v2.0.5» → «Aground»; «Anarchy / Анархия» → «Anarchy».
fn title_variants(title: &str) -> Vec<String> {
    let mut out = vec![title.trim().to_string()];
    let mut t = title.to_string();
    if let Some(i) = t.find(" / ") {
        t.truncate(i);
    }
    let re_paren = regex::Regex::new(r"\s*[\(\[][^\)\]]*[\)\]]").unwrap();
    t = re_paren.replace_all(&t, "").to_string();
    let re_ver = regex::Regex::new(r"(?i)\s+(v|build\s*)?\d+(\.\d+)+\w*.*$").unwrap();
    t = re_ver.replace(&t, "").to_string();
    // Слова на кириллице в конце («Armies of Exigo:Хроники великой войны»).
    if let Some(i) = t.find(|c: char| {
        ('а'..='я').contains(&c.to_lowercase().next().unwrap_or(' ')) || c == 'ё' || c == 'Ё'
    }) {
        t.truncate(i);
    }
    let t = t
        .trim_matches(|c: char| c.is_whitespace() || c == ':' || c == '-')
        .replace(" :", ":");
    if !t.is_empty() && !out.contains(&t) {
        out.push(t);
    }
    out
}

async fn steam_search(c: &reqwest::Client, term: &str) -> anyhow::Result<Vec<Item>> {
    let resp: SearchResp = c
        .get(format!(
            "https://store.steampowered.com/api/storesearch/?term={}&l=english&cc=US",
            urlencoding::encode(term)
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp
        .items
        .into_iter()
        .filter(|i| i.kind.is_empty() || i.kind == "app")
        .collect())
}

/// Ищет подходящую игру в результатах. `strict` — только точное совпадение названия.
fn pick<'a>(items: &'a [Item], want: &str, strict: bool) -> Option<&'a Item> {
    let want = norm(want);
    items.iter().find(|i| norm(&i.name) == want).or_else(|| {
        if strict {
            return None;
        }
        items.iter().find(|i| {
            let n = norm(&i.name);
            !n.is_empty()
                && n.len() * 2 >= want.len()
                && (n.starts_with(&want) || want.starts_with(&n))
        })
    })
}

/// Рабочая ссылка на шапку игры. У новых игр Steam хранит картинки по пути
/// с хешем, и старый `apps/<id>/header.jpg` отдаёт 404.
async fn steam_header(c: &reqwest::Client, item: &Item) -> Option<String> {
    let plain = format!(
        "https://shared.akamai.steamstatic.com/store_item_assets/steam/apps/{}/header.jpg",
        item.id
    );
    if let Ok(r) = c.head(&plain).send().await {
        if r.status().is_success() {
            return Some(plain);
        }
    }
    let details = async {
        let v: serde_json::Value = c
            .get(format!(
                "https://store.steampowered.com/api/appdetails?appids={}&filters=basic",
                item.id
            ))
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        v.get(item.id.to_string())?
            .get("data")?
            .get("header_image")?
            .as_str()
            .map(String::from)
    };
    if let Some(h) = details.await {
        return Some(h);
    }
    item.tiny_image.clone()
}

/// Постер из `og:image` страницы игры на FreeTP.
async fn freetp_poster(c: &reqwest::Client, article: &str) -> Option<String> {
    let html = c
        .get(article)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .text()
        .await
        .ok()?;
    let re = regex::Regex::new(r#"<meta\s+property="og:image"\s+content="([^"]+)""#).ok()?;
    let url = re.captures(&html)?.get(1)?.as_str().to_string();
    let url = if url.starts_with("//") {
        format!("https:{url}")
    } else {
        url
    };
    url.starts_with("http").then_some(url)
}

/// Ok(None) — обложки нет нигде; Err — сетевая ошибка (не кэшируется).
pub async fn lookup(title: &str, article: Option<&str>) -> anyhow::Result<Option<String>> {
    let c = client()?;
    let variants = title_variants(title);
    for v in &variants {
        let items = steam_search(&c, v).await?;
        // Совпадение — точное или по началу названия; иначе берём постер с FreeTP,
        // чтобы не показать обложку чужой игры.
        if let Some(item) = pick(&items, v, false) {
            if let Some(url) = steam_header(&c, item).await {
                return Ok(Some(url));
            }
        }
    }
    if let Some(a) = article.filter(|a| a.starts_with("http")) {
        if let Some(p) = freetp_poster(&c, a).await {
            return Ok(Some(p));
        }
    }
    Ok(None)
}

/// Ссылка на страницу игры в магазине Steam. Если игру не нашли —
/// поиск Steam по названию (там почти всегда найдётся нужная).
pub async fn steam_store_url(title: &str) -> anyhow::Result<String> {
    let c = client()?;
    for v in &title_variants(title) {
        let items = steam_search(&c, v).await?;
        if let Some(item) = pick(&items, v, false) {
            return Ok(format!("https://store.steampowered.com/app/{}/", item.id));
        }
    }
    let clean = title_variants(title).pop().unwrap_or_else(|| title.to_string());
    Ok(format!(
        "https://store.steampowered.com/search/?term={}",
        urlencoding::encode(&clean)
    ))
}

#[cfg(test)]
mod tests {
    use super::title_variants;

    #[test]
    fn variants() {
        assert_eq!(
            title_variants("Age Of Mythology (Эпоха Мифологии)")[1],
            "Age Of Mythology"
        );
        assert_eq!(title_variants("Aground v2.0.5")[1], "Aground");
        assert_eq!(
            title_variants("Anarchy: Wolf's Law / Анархия: Волчьи законы")[1],
            "Anarchy: Wolf's Law"
        );
        assert_eq!(
            title_variants("Armies of Exigo:Хроники великой войны")[1],
            "Armies of Exigo"
        );
        assert_eq!(
            title_variants("Battle of Empires : 1914-1918")[1],
            "Battle of Empires: 1914-1918"
        );
        assert_eq!(title_variants("Schedule I").len(), 1);
    }
}

#[cfg(test)]
mod net_tests {
    /// cargo test --lib covers::net_tests -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn lookup_problem_titles() {
        let cases = [
            ("Alchemy Factory", None),
            ("Unicycle Together", None),
            ("Schedule I", None),
            ("Age Of Mythology (Эпоха Мифологии)", None),
            ("Aground v2.0.5", None),
            (
                "Battlefield 1942",
                Some("https://freetp.org/po-seti/111-007-legends-igra-v-setevoi-rejim.html"),
            ),
        ];
        for (t, a) in cases {
            println!("{t} => {:?}", super::lookup(t, a).await);
        }
    }
}
