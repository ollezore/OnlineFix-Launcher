// OnlineFix-Launcher — лаунчер игр FreeTP / Online-Fix для Linux
// Copyright (C) 2026  OnlineFix-Launcher contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

mod catalog;
mod covers;
mod files;
mod library;
mod popular;
mod proton;
mod sys;

use catalog::CatalogEntry;
use library::{Game, Settings, Status};
use librqbit::api::TorrentIdOrHash;
use librqbit::{
    AddTorrent, AddTorrentOptions, ListenerOptions, ManagedTorrent, Session, SessionOptions,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

type CmdResult<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

pub struct AppState {
    data_dir: PathBuf,
    settings: Mutex<Settings>,
    games: Mutex<Vec<Game>>,
    session: tokio::sync::OnceCell<Arc<Session>>,
    torrents: Mutex<HashMap<String, Arc<ManagedTorrent>>>,
    /// Торренты, для которых ещё ищутся метаданные: id игры -> время старта.
    pending: Mutex<HashMap<String, std::time::Instant>>,
    /// id игры -> pid запущенного процесса (игра или установщик).
    running: Mutex<HashMap<String, u32>>,
    /// Кэш обложек: название игры -> URL картинки (None — не найдено в Steam).
    covers: Mutex<HashMap<String, Option<String>>>,
}

impl AppState {
    fn games_file(&self) -> PathBuf {
        self.data_dir.join("library.json")
    }
    fn settings_file(&self) -> PathBuf {
        self.data_dir.join("settings.json")
    }
    fn save_games(&self) {
        let games = self.games.lock().clone();
        if let Err(e) = library::save_json(&self.games_file(), &games) {
            eprintln!("library save error: {e}");
        }
    }
    fn game(&self, id: &str) -> CmdResult<Game> {
        self.games
            .lock()
            .iter()
            .find(|g| g.id == id)
            .cloned()
            .ok_or_else(|| "Игра не найдена".to_string())
    }
    fn update<F: FnOnce(&mut Game)>(&self, id: &str, f: F) {
        if let Some(g) = self.games.lock().iter_mut().find(|g| g.id == id) {
            f(g);
        }
        self.save_games();
    }
    async fn session(&self) -> CmdResult<Arc<Session>> {
        let settings = self.settings.lock().clone();
        self.session
            .get_or_try_init(|| async move {
                std::fs::create_dir_all(&settings.games_dir).map_err(err)?;
                let mut opts = SessionOptions {
                    listen: Some(ListenerOptions {
                        enable_upnp_port_forwarding: true,
                        ..Default::default()
                    }),
                    client_name_and_version: Some("OnlineFix-Launcher/0.1".into()),
                    ..Default::default()
                };
                if settings.download_limit_mbps > 0 {
                    opts.ratelimits.download_bps =
                        NonZeroU32::new(settings.download_limit_mbps.saturating_mul(1024 * 1024));
                }
                Session::new_with_opts(settings.games_dir.clone(), opts)
                    .await
                    .map_err(err)
            })
            .await
            .cloned()
    }
    fn resolve_proton(&self, game: &Game) -> CmdResult<PathBuf> {
        if sys::IS_WINDOWS {
            return Ok(PathBuf::new()); // на Windows игры запускаются напрямую
        }
        let list = proton::list_protons();
        let want = game
            .proton
            .clone()
            .or_else(|| self.settings.lock().proton.clone());
        let chosen = want
            .and_then(|w| list.iter().find(|p| p.name == w).cloned())
            .or_else(|| list.first().cloned())
            .ok_or_else(|| {
                "Proton не найден. Установите Proton через Steam или GE-Proton в ~/.steam/root/compatibilitytools.d".to_string()
            })?;
        Ok(PathBuf::from(chosen.path))
    }
}

fn notify(app: &AppHandle) {
    let _ = app.emit("library-changed", ());
}

fn toast(app: &AppHandle, msg: impl Into<String>) {
    let _ = app.emit("toast", msg.into());
}

// ───────────────────────── Загрузки ─────────────────────────

async fn start_torrent(app: AppHandle, game: Game) -> CmdResult<()> {
    let state = app.state::<AppState>();
    let session = state.session().await?;
    std::fs::create_dir_all(game.download_dir()).map_err(err)?;
    let magnet = catalog::normalize_magnet(&game.magnet);
    state
        .pending
        .lock()
        .insert(game.id.clone(), std::time::Instant::now());
    notify(&app);
    let resp = session
        .add_torrent(
            AddTorrent::from_url(magnet),
            Some(AddTorrentOptions {
                output_folder: Some(game.download_dir().to_string_lossy().to_string()),
                overwrite: true,
                ..Default::default()
            }),
        )
        .await;
    state.pending.lock().remove(&game.id);
    let handle = resp
        .map_err(err)?
        .into_handle()
        .ok_or("Торрент не добавлен")?;
    // Игру могли удалить, пока искались метаданные.
    if state.game(&game.id).is_err() {
        let _ = session
            .delete(TorrentIdOrHash::Id(handle.id()), false)
            .await;
        return Ok(());
    }
    state
        .torrents
        .lock()
        .insert(game.id.clone(), handle.clone());
    notify(&app);

    let id = game.id.clone();
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let res = handle.wait_until_completed().await;
        let state = app2.state::<AppState>();
        match res {
            Ok(()) => {
                // Раздачу останавливаем: установщик/игра могут менять файлы.
                if let Ok(s) = state.session().await {
                    let _ = s.delete(TorrentIdOrHash::Id(handle.id()), false).await;
                }
                state.torrents.lock().remove(&id);
                let updating = state.game(&id).map(|g| g.update_from.is_some()).unwrap_or(false);
                if updating {
                    finish_update_download(&app2, &id).await;
                } else {
                    state.update(&id, |g| {
                        if g.status == Status::Downloading {
                            g.status = Status::Downloaded;
                        }
                    });
                    let title = state.game(&id).map(|g| g.title).unwrap_or_default();
                    toast(&app2, format!("«{title}» скачана — можно устанавливать"));
                }
            }
            Err(e) => {
                state.update(&id, |g| g.last_error = Some(e.to_string()));
            }
        }
        notify(&app2);
    });
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadInfo {
    id: String,
    state: String,
    progress_bytes: u64,
    total_bytes: u64,
    speed_bps: u64,
    eta_secs: Option<u64>,
    /// Сколько секунд уже ищутся метаданные (для state = "metadata").
    waiting_secs: Option<u64>,
    error: Option<String>,
}

#[tauri::command]
fn downloads(state: State<AppState>) -> Vec<DownloadInfo> {
    let pending: Vec<DownloadInfo> = state
        .pending
        .lock()
        .iter()
        .map(|(id, t)| DownloadInfo {
            id: id.clone(),
            state: "metadata".into(),
            progress_bytes: 0,
            total_bytes: 0,
            speed_bps: 0,
            eta_secs: None,
            waiting_secs: Some(t.elapsed().as_secs()),
            error: None,
        })
        .collect();
    let mut list: Vec<DownloadInfo> = state
        .torrents
        .lock()
        .iter()
        .map(|(id, h)| {
            let s = h.stats();
            let speed = s
                .live
                .as_ref()
                .map(|l| (l.download_speed.mbps * 1024.0 * 1024.0) as u64)
                .unwrap_or(0);
            let left = s.total_bytes.saturating_sub(s.progress_bytes);
            DownloadInfo {
                id: id.clone(),
                state: if h.is_paused() {
                    "paused".into()
                } else {
                    s.state.to_string()
                },
                progress_bytes: s.progress_bytes,
                total_bytes: s.total_bytes,
                speed_bps: speed,
                eta_secs: (speed > 0).then(|| left / speed),
                waiting_secs: None,
                error: s.error,
            }
        })
        .collect();
    list.extend(pending);
    list
}

#[tauri::command]
async fn pause_download(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let h = state
        .torrents
        .lock()
        .get(&id)
        .cloned()
        .ok_or("Загрузка не найдена")?;
    state.session().await?.pause(&h).await.map_err(err)
}

#[tauri::command]
async fn resume_download(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let h = state
        .torrents
        .lock()
        .get(&id)
        .cloned()
        .ok_or("Загрузка не найдена")?;
    state.session().await?.unpause(&h).await.map_err(err)
}

// ───────────────────────── Каталог ─────────────────────────

#[tauri::command]
async fn get_catalog(
    app: AppHandle,
    state: State<'_, AppState>,
    refresh: bool,
    catalog: Option<String>,
) -> CmdResult<catalog::CatalogResp> {
    let cache = state.data_dir.join("cache");
    if catalog.as_deref() == Some("onlinefix") {
        return load_onlinefix(&app, &cache, refresh).await;
    }
    let source = state.settings.lock().source.clone();
    catalog::load(&cache, &source, refresh).await.map_err(err)
}

/// База Online-Fix: оригинал (через WebView) → зеркала GitHub → кэш.
async fn load_onlinefix(
    app: &AppHandle,
    cache: &Path,
    refresh: bool,
) -> CmdResult<catalog::CatalogResp> {
    #[derive(Serialize, Deserialize, Default)]
    struct Meta {
        origin: String,
    }
    let file = cache.join("catalog-onlinefix.json");
    let meta_file = cache.join("catalog-onlinefix.meta.json");
    let fresh = std::fs::metadata(&file)
        .and_then(|m| m.modified())
        .map(|t| t.elapsed().map(|e| e.as_secs() < 6 * 3600).unwrap_or(false))
        .unwrap_or(false);
    let parse = |bytes: &[u8], origin: String| -> CmdResult<catalog::CatalogResp> {
        let entries = catalog::parse(bytes, "onlinefix").map_err(err)?;
        if entries.is_empty() {
            return Err("база пустая".into());
        }
        Ok(catalog::CatalogResp {
            newest: catalog::newest(&entries),
            origin,
            entries,
        })
    };
    let from_cache = || -> CmdResult<catalog::CatalogResp> {
        let bytes = std::fs::read(&file).map_err(err)?;
        let meta: Meta = library::load_json(&meta_file);
        parse(
            &bytes,
            if meta.origin.is_empty() {
                "кэш".into()
            } else {
                meta.origin
            },
        )
    };
    if !refresh && fresh {
        if let Ok(r) = from_cache() {
            return Ok(r);
        }
    }

    let _ = std::fs::create_dir_all(cache);
    let mut errors = vec![];
    let mut candidates: Vec<(Vec<u8>, String)> = vec![];
    match fetch_json_via_webview(app, catalog::ONLINEFIX_ORIGINAL, &cache.join("wv")).await {
        Ok(b) => candidates.push((b, "hydralinks.cloud (оригинал)".into())),
        Err(e) => errors.push(format!("hydralinks.cloud: {e}")),
    }
    if candidates.is_empty() {
        for m in catalog::ONLINEFIX_MIRRORS {
            match catalog::download(m).await {
                Ok(b) => {
                    let name = m.split('/').skip(3).take(2).collect::<Vec<_>>().join("/");
                    candidates.push((b, format!("зеркало GitHub · {name}")));
                }
                Err(e) => errors.push(format!("{m}: {e}")),
            }
        }
    }
    // Из доступных берём самую свежую базу.
    let best = candidates
        .into_iter()
        .filter_map(|(b, origin)| parse(&b, origin.clone()).ok().map(|r| (b, r)))
        .max_by(|a, b| a.1.newest.cmp(&b.1.newest));
    match best {
        Some((bytes, resp)) => {
            let _ = std::fs::write(&file, &bytes);
            let _ = library::save_json(
                &meta_file,
                &Meta {
                    origin: resp.origin.clone(),
                },
            );
            Ok(resp)
        }
        None => from_cache().map_err(|_| {
            format!(
                "Не удалось загрузить базу Online-Fix: {}",
                errors.join("; ")
            )
        }),
    }
}

#[tauri::command]
async fn add_game(
    app: AppHandle,
    state: State<'_, AppState>,
    entry: CatalogEntry,
    dir: Option<String>,
    prefix_dir: Option<String>,
) -> CmdResult<Game> {
    // dir — папка, выбранная пользователем в диалоге; внутри неё создаётся папка игры.
    let games_dir = match dir.map(|d| d.trim().to_string()).filter(|d| !d.is_empty()) {
        Some(d) => {
            let p = PathBuf::from(d);
            if !p.is_absolute() {
                return Err("Укажите полный путь к папке".into());
            }
            std::fs::create_dir_all(&p).map_err(|e| format!("Не удалось создать папку: {e}"))?;
            p
        }
        None => state.settings.lock().games_dir.clone(),
    };
    let mut game = Game::from_entry(&entry, &games_dir);
    // Папка для префиксов, выбранная отдельно (только Linux). Внутри — папка игры.
    let prefixes = prefix_dir
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty() && !sys::IS_WINDOWS)
        .map(|d| expand_home(PathBuf::from(d)));
    if let Some(p) = prefixes {
        if !p.is_absolute() {
            return Err("Укажите полный путь к папке префиксов".into());
        }
        if let Some(fs) = sys::fs_type(&p).filter(|f| sys::fs_unsuitable_for_prefix(f)) {
            return Err(format!(
                "Папка префиксов на {fs}: там Proton не работает (нужны симлинки). Выберите папку на разделе Linux (ext4, btrfs…)"
            ));
        }
        std::fs::create_dir_all(&p).map_err(|e| format!("Не удалось создать папку префиксов: {e}"))?;
        game.prefix_dir = Some(p.join(&game.id));
    } else if let Some(fs) = sys::fs_type(&games_dir).filter(|f| sys::fs_unsuitable_for_prefix(f)) {
        // Папка игр на разделе Windows (NTFS/exFAT/FAT): игра может лежать там,
        // а префикс Proton — нет. Переносим префикс в домашнюю папку.
        game.prefix_dir = Some(state.data_dir.join("prefixes").join(&game.id));
        toast(
            &app,
            format!("Диск с играми — {fs}: префикс Proton будет в домашней папке, игра — на выбранном диске"),
        );
    }
    if state
        .games
        .lock()
        .iter()
        .any(|g| g.info_hash == game.info_hash)
    {
        return Err("Эта раздача уже в библиотеке".into());
    }
    state.games.lock().push(game.clone());
    state.save_games();
    notify(&app);
    // Метаданные по magnet могут искаться долго — не блокируем интерфейс.
    let (app2, g2) = (app.clone(), game.clone());
    tauri::async_runtime::spawn(async move {
        if let Err(e) = start_torrent(app2.clone(), g2.clone()).await {
            app2.state::<AppState>()
                .update(&g2.id, |g| g.last_error = Some(e));
            notify(&app2);
        }
    });
    Ok(game)
}

// ───────────────────────── Библиотека ─────────────────────────

// ───────────────────────── Обновление самого лаунчера ─────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppUpdate {
    version: String,
    current: String,
    notes: Option<String>,
    date: Option<String>,
}

/// Найденное обновление ждёт, пока пользователь нажмёт «Обновить».
static PENDING_APP_UPDATE: std::sync::Mutex<Option<tauri_plugin_updater::Update>> = std::sync::Mutex::new(None);

#[tauri::command]
fn app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

/// Проверяет GitHub Releases (latest.json, подписанный ключом обновлений).
#[tauri::command]
async fn check_app_update(app: AppHandle) -> CmdResult<Option<AppUpdate>> {
    use tauri_plugin_updater::UpdaterExt;
    let upd = app.updater().map_err(err)?.check().await.map_err(err)?;
    Ok(upd.map(|u| {
        let info = AppUpdate {
            version: u.version.clone(),
            current: u.current_version.clone(),
            notes: u.body.clone(),
            date: u.date.map(|d| d.date().to_string()),
        };
        *PENDING_APP_UPDATE.lock().unwrap() = Some(u);
        info
    }))
}

/// Скачивает, проверяет подпись, ставит обновление и перезапускает лаунчер.
#[tauri::command]
async fn install_app_update(app: AppHandle) -> CmdResult<()> {
    let u = PENDING_APP_UPDATE
        .lock()
        .unwrap()
        .take()
        .ok_or("Сначала проверьте обновления")?;
    let app2 = app.clone();
    let (mut got, mut last) = (0u64, 0u64);
    u.download_and_install(
        move |chunk, total| {
            got += chunk as u64;
            if let Some(t) = total.filter(|t| *t > 0) {
                let pct = got * 100 / t;
                if pct >= last + 10 {
                    last = pct;
                    let _ = app2.emit("app-update-progress", pct);
                }
            }
        },
        || {},
    )
    .await
    .map_err(|e| format!("Не удалось обновить: {e}. Скачайте новую версию со страницы релизов."))?;
    app.restart()
}

// ───────────────────────── Обновления ─────────────────────────

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct UpdateInfo {
    id: String,
    /// "game" — вышла новая раздача, "fix" — новый фикс.
    kind: String,
    /// Новая раздача (для kind = game).
    entry: Option<CatalogEntry>,
    /// Название нового фикса (для kind = fix).
    fix_title: Option<String>,
}

/// Сравнивает библиотеку с базами. Заодно подтягивает свежий список фиксов
/// к играм и дату раздачи к старым записям.
#[tauri::command]
async fn check_updates(app: AppHandle, state: State<'_, AppState>) -> CmdResult<Vec<UpdateInfo>> {
    let games = state.games.lock().clone();
    if games.is_empty() {
        return Ok(vec![]);
    }
    let cache = state.data_dir.join("cache");
    let source = state.settings.lock().source.clone();
    let mut entries: Vec<CatalogEntry> = vec![];
    if games.iter().any(|g| g.source != "onlinefix") {
        if let Ok(r) = catalog::load(&cache, &source, false).await {
            entries.extend(r.entries);
        }
    }
    if games.iter().any(|g| g.source == "onlinefix") {
        if let Ok(r) = load_onlinefix(&app, &cache, false).await {
            entries.extend(r.entries);
        }
    }
    if entries.is_empty() {
        return Err("Базы недоступны — проверить обновления не получилось".into());
    }
    let src = |s: &str| if s == "onlinefix" { "onlinefix" } else { "freetp" };

    let mut out = vec![];
    let mut patches: Vec<(String, Option<String>, Option<Vec<catalog::Addon>>)> = vec![];
    for g in &games {
        if g.status == Status::Downloading || g.update_from.is_some() {
            continue;
        }
        let key = library::norm_title(if g.catalog_title.is_empty() { &g.title } else { &g.catalog_title });
        let same: Vec<&CatalogEntry> = entries
            .iter()
            .filter(|e| src(&e.source) == src(&g.source) && library::norm_title(&e.title) == key)
            .collect();
        if same.is_empty() {
            continue;
        }
        let own = same.iter().find(|e| e.info_hash == g.info_hash).copied();
        let my_date = if !g.upload_date.is_empty() {
            g.upload_date.clone()
        } else {
            own.map(|e| e.upload_date.clone()).unwrap_or_default()
        };
        let newest = same.iter().max_by(|a, b| a.upload_date.cmp(&b.upload_date)).copied();
        if let Some(n) = newest {
            let newer = if my_date.is_empty() { own.is_none() } else { n.upload_date > my_date };
            if n.info_hash != g.info_hash && newer {
                out.push(UpdateInfo { id: g.id.clone(), kind: "game".into(), entry: Some(n.clone()), fix_title: None });
                continue;
            }
        }
        // Свежий список вариантов фикса для нашей раздачи.
        let addons = own.map(|e| e.addons.clone());
        patches.push((
            g.id.clone(),
            g.upload_date.is_empty().then(|| my_date.clone()).filter(|d| !d.is_empty()),
            addons.clone(),
        ));
        let list = addons.unwrap_or_else(|| g.addons.clone());
        if let (true, Some(t)) = (g.addons_applied, &g.addon_applied) {
            let fixes: Vec<&catalog::Addon> = list.iter().filter(|a| a.is_fix()).collect();
            if !fixes.is_empty() && !fixes.iter().any(|a| &a.title == t) {
                out.push(UpdateInfo {
                    id: g.id.clone(),
                    kind: "fix".into(),
                    entry: None,
                    fix_title: Some(fixes[0].title.clone()),
                });
            }
        }
    }
    if !patches.is_empty() {
        let mut list = state.games.lock();
        for (id, date, addons) in patches {
            if let Some(g) = list.iter_mut().find(|g| g.id == id) {
                if let Some(d) = date {
                    g.upload_date = d;
                }
                if let Some(a) = addons {
                    g.addons = a;
                }
            }
        }
        drop(list);
        state.save_games();
    }
    Ok(out)
}

/// Обновляет игру до новой раздачи. Качаем в ту же папку: librqbit проверит
/// уже лежащие файлы и докачает только изменившиеся. Префикс (сохранения,
/// настройки) не трогаем.
#[tauri::command]
async fn start_game_update(app: AppHandle, state: State<'_, AppState>, id: String, entry: CatalogEntry) -> CmdResult<()> {
    let g = state.game(&id)?;
    if state.running.lock().contains_key(&id) {
        return Err("Сначала закройте игру".into());
    }
    if state.games.lock().iter().any(|x| x.id != id && x.info_hash == entry.info_hash) {
        return Err("Эта раздача уже есть в библиотеке отдельной игрой".into());
    }
    // Старый торрент, если он ещё в сессии, убираем (файлы оставляем).
    let old_handle = state.torrents.lock().remove(&id);
    if let Some(h) = old_handle {
        if let Ok(s) = state.session().await {
            let _ = s.delete(TorrentIdOrHash::Id(h.id()), false).await;
        }
    }
    let old = g.release.clone();
    state.update(&id, |g| {
        g.magnet = entry.magnet.clone();
        g.info_hash = entry.info_hash.clone();
        g.release = entry.release.clone();
        g.file_size = entry.file_size.clone();
        g.upload_date = entry.upload_date.clone();
        if entry.article_url.is_some() {
            g.article_url = entry.article_url.clone();
        }
        g.install_kind = entry.install_kind;
        g.addons = entry.addons.clone();
        g.addons_applied = false; // addon_applied оставляем — поставим тот же вариант
        g.update_from = Some(old);
        g.status = Status::Downloading;
        g.last_error = None;
    });
    notify(&app);
    let game = state.game(&id)?;
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = start_torrent(app2.clone(), game.clone()).await {
            app2.state::<AppState>().update(&game.id, |g| g.last_error = Some(e));
            notify(&app2);
        }
    });
    Ok(())
}

/// Обновление скачано. Портативная игра (exe прямо в раздаче) сразу готова —
/// переставляем фикс; игре с установщиком нужно запустить установку снова.
async fn finish_update_download(app: &AppHandle, id: &str) {
    let state = app.state::<AppState>();
    let Ok(g) = state.game(id) else { return };
    let portable = g
        .exe
        .as_ref()
        .is_some_and(|e| e.starts_with(g.download_dir()) && e.is_file());
    if portable {
        state.update(id, |g| {
            g.status = Status::Installed;
            g.update_from = None;
        });
        notify(app);
        let fix = g
            .addon_applied
            .as_ref()
            .and_then(|t| g.addons.iter().position(|a| a.is_fix() && &a.title == t))
            .or_else(|| (g.install_kind != 2).then(|| g.addons.iter().position(|a| a.is_fix())).flatten());
        match fix {
            Some(i) => match apply_addons(app.clone(), state.clone(), id.to_string(), Some(i)).await {
                Ok(_) => toast(app, format!("«{}» обновлена, фикс поставлен заново", g.title)),
                Err(e) => toast(app, format!("«{}» обновлена, но фикс не поставился: {e}", g.title)),
            },
            None => toast(app, format!("«{}» обновлена", g.title)),
        }
    } else {
        state.update(id, |g| g.status = Status::Downloaded);
        toast(app, format!("Обновление «{}» скачано — нажмите «Установить»", g.title));
    }
    notify(app);
}

/// Страница игры в Steam (для кнопки в окне раздачи).
#[tauri::command]
async fn steam_page(title: String) -> CmdResult<String> {
    covers::steam_store_url(&title).await.map_err(err)
}

/// id статей FreeTP по популярности (посещаемости), самые популярные первыми.
#[tauri::command]
async fn popular(state: State<'_, AppState>, refresh: bool) -> CmdResult<Vec<u32>> {
    popular::ranking(&state.data_dir.join("cache"), refresh)
        .await
        .map_err(err)
}

#[tauri::command]
fn library(state: State<AppState>) -> Vec<Game> {
    let mut games = state.games.lock().clone();
    for g in games.iter_mut() {
        g.fix_detected = g
            .exe
            .as_deref()
            .is_some_and(|exe| files::fix_present(&game_root_for_fix(exe)));
    }
    games
}

#[tauri::command]
fn running_games(state: State<AppState>) -> Vec<String> {
    state.running.lock().keys().cloned().collect()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GamePatch {
    launch_args: Option<String>,
    env: Option<String>,
    dll_overrides: Option<String>,
    spacewar: Option<bool>,
    proton: Option<Option<String>>,
    title: Option<String>,
}

#[tauri::command]
fn update_game(
    app: AppHandle,
    state: State<AppState>,
    id: String,
    patch: GamePatch,
) -> CmdResult<()> {
    state.game(&id)?;
    state.update(&id, |g| {
        if let Some(v) = patch.launch_args {
            g.launch_args = v;
        }
        if let Some(v) = patch.env {
            g.env = v;
        }
        if let Some(v) = patch.dll_overrides {
            g.dll_overrides = v;
        }
        if let Some(v) = patch.spacewar {
            g.spacewar = v;
        }
        if let Some(v) = patch.proton {
            g.proton = v.filter(|s| !s.is_empty());
        }
        if let Some(v) = patch.title.filter(|s| !s.trim().is_empty()) {
            g.title = v;
        }
    });
    notify(&app);
    Ok(())
}

#[tauri::command]
async fn remove_game(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    delete_files: bool,
) -> CmdResult<()> {
    let game = state.game(&id)?;
    state.pending.lock().remove(&id);
    let handle = state.torrents.lock().remove(&id);
    if let Some(h) = handle {
        if let Ok(s) = state.session().await {
            let _ = s.delete(TorrentIdOrHash::Id(h.id()), false).await;
        }
    }
    let pid = state.running.lock().get(&id).copied();
    if let Ok(p) = state.resolve_proton(&game) {
        proton::kill(&p, &game.compat_dir(), pid);
    }
    state.games.lock().retain(|g| g.id != id);
    state.save_games();
    let games_dir = state.settings.lock().games_dir.clone();
    // Удаляем только папку, созданную лаунчером: внутри папки игр или названную по id игры.
    let ours = game.dir.starts_with(&games_dir)
        || game
            .dir
            .file_name()
            .is_some_and(|n| n.to_string_lossy() == game.id);
    if delete_files && ours && game.dir.exists() {
        tokio::fs::remove_dir_all(&game.dir).await.map_err(err)?;
    }
    // Префикс, вынесенный из папки игры (см. add_game): удаляем только папку этой игры.
    if let Some(p) = game.prefix_dir.as_ref().filter(|p| {
        p.starts_with(state.data_dir.join("prefixes"))
            || p.file_name().is_some_and(|n| n.to_string_lossy() == game.id)
    }) {
        if delete_files && p.exists() {
            let _ = tokio::fs::remove_dir_all(p).await;
        }
    }
    notify(&app);
    Ok(())
}

// ───────────────────────── Установка ─────────────────────────

/// Распаковывает архивы (если есть) и запускает установщик (на Linux — через Proton
/// в префиксе игры). Если установщика нет — игра портативная.
/// `installer` — установщик, который пользователь указал сам.
#[tauri::command]
async fn install_game(
    app: AppHandle,
    id: String,
    installer: Option<String>,
) -> CmdResult<String> {
    install_game_impl(&app, &id, installer.map(PathBuf::from)).await
}

async fn install_game_impl(app: &AppHandle, id: &str, chosen: Option<PathBuf>) -> CmdResult<String> {
    let state = app.state::<AppState>();
    let id = id.to_string();
    let game = state.game(&id)?;
    if state.running.lock().contains_key(&id) {
        return Err("Установщик уже запущен".into());
    }
    state.update(&id, |g| {
        g.status = Status::Installing;
        g.install_started = Some(library::now());
        g.last_error = None;
    });
    notify(app);

    let mut installer = chosen.filter(|p| p.is_file());
    if installer.is_none() {
        let dl = game.download_dir();
        let ex = game.extracted_dir();
        installer = tokio::task::spawn_blocking({
            let dl = dl.clone();
            move || files::find_installer(&dl)
        })
        .await
        .map_err(err)?;
        if installer.is_none() {
            let archives = files::find_archives(&dl);
            // Распаковываем и при повторной установке (например, после обновления) — 7z -y перезапишет.
            if !archives.is_empty() {
                toast(app, "Распаковка архивов…");
                let ex2 = ex.clone();
                let res = tokio::task::spawn_blocking(move || {
                    for a in archives {
                        files::extract(&a, &ex2)?;
                    }
                    anyhow::Ok(())
                })
                .await
                .map_err(err)?;
                if let Err(e) = res {
                    state.update(&id, |g| {
                        g.status = Status::Downloaded;
                        g.last_error = Some(e.to_string());
                    });
                    notify(app);
                    return Err(e.to_string());
                }
            }
            installer = tokio::task::spawn_blocking(move || files::find_installer(&ex))
                .await
                .map_err(err)?;
        }
    }

    let Some(installer) = installer else {
        state.update(&id, |g| g.status = Status::NeedsExe);
        notify(app);
        return Ok("portable".into());
    };

    let proton_dir = match state.resolve_proton(&game) {
        Ok(p) => p,
        Err(e) => {
            state.update(&id, |g| g.status = Status::Downloaded);
            notify(app);
            return Err(e);
        }
    };
    let log = state
        .settings
        .lock()
        .proton_log
        .then(|| game.dir.join("installer.log"));
    let use_umu = state.settings.lock().use_umu;

    // Ставим игру сами в <папка игры>/game: тихо, если установщик это умеет
    // (Inno Setup, NSIS), иначе — обычным мастером, а куда он поставил игру,
    // узнаём по реестру (Windows) или по папке (Linux).
    let target = game.target_install_dir();
    let _ = std::fs::create_dir_all(&target);
    let kind = files::installer_kind(&installer);
    let win_target = windows_path(&game, &target);
    let attempts: Vec<(bool, Vec<String>)> = match kind {
        Some(files::InstallerKind::Inno) => {
            let dir = if sys::IS_WINDOWS {
                format!("/DIR=\"{win_target}\"")
            } else {
                format!("/DIR={win_target}")
            };
            // /TASKS= с пустым списком снимает все галочки доп. задач:
            // ярлык на рабочем столе, ярлык сайта и т.п.
            let tasks = if sys::IS_WINDOWS { "/TASKS=\"\"" } else { "/TASKS=" }.to_string();
            vec![
                (
                    true,
                    ["/SILENT", "/SP-", "/SUPPRESSMSGBOXES", "/NORESTART", "/NOICONS"]
                        .into_iter()
                        .map(String::from)
                        .chain([tasks.clone(), dir.clone()])
                        .collect(),
                ),
                (false, vec!["/SP-".into(), "/NORESTART".into(), "/NOICONS".into(), tasks, dir]),
            ]
        }
        Some(files::InstallerKind::Nsis) => vec![
            (true, vec!["/S".into(), format!("/D={win_target}")]),
            (false, vec![format!("/D={win_target}")]),
        ],
        None => vec![(false, vec![])],
    };
    if attempts[0].0 {
        toast(app, format!("Устанавливаю автоматически в {}", target.display()));
    }
    let before = if sys::IS_WINDOWS {
        tokio::task::spawn_blocking(sys::uninstall_locations)
            .await
            .unwrap_or_default()
    } else {
        Default::default()
    };

    let compat = game.compat_dir();
    let mut first = match proton::spawn(proton::RunSpec {
        proton_dir: &proton_dir,
        compat_dir: &compat,
        exe: &installer,
        args: attempts[0].1.clone(),
        use_umu,
        online_fix: false,
        dll_overrides: None,
        extra_env: proton::installer_env(),
        log_file: log.clone(),
        steam_overlay: false,
    }) {
        Ok(c) => c,
        Err(e) => {
            state.update(&id, |g| g.status = Status::Downloaded);
            notify(app);
            return Err(e.to_string());
        }
    };
    state.running.lock().insert(id.clone(), first.id());
    notify(app);

    let app2 = app.clone();
    let silent = attempts[0].0;
    let started = std::time::SystemTime::now() - std::time::Duration::from_secs(5);
    std::thread::spawn(move || {
        let state = app2.state::<AppState>();
        let has_exes = |dir: &Path| !files::find_game_exes(&[dir.to_path_buf()], 7, None).is_empty();
        let mut status = first.wait().ok();
        for (i, (_, args)) in attempts.iter().enumerate().skip(1) {
            if has_exes(&target) {
                break;
            }
            // Inno: 2 — отменили до начала, 5 — во время установки. Не навязываемся.
            let code = status.and_then(|s| s.code());
            if matches!(code, Some(2) | Some(5)) && kind == Some(files::InstallerKind::Inno) {
                break;
            }
            if i == 1 {
                toast(&app2, "Автоматическая установка не удалась — открываю обычный установщик (папка уже подставлена)");
            }
            match proton::spawn(proton::RunSpec {
                proton_dir: &proton_dir,
                compat_dir: &compat,
                exe: &installer,
                args: args.clone(),
                use_umu,
                online_fix: false,
                dll_overrides: None,
                extra_env: proton::installer_env(),
                log_file: log.clone(),
                steam_overlay: false,
            }) {
                Ok(mut c) => {
                    state.running.lock().insert(id.clone(), c.id());
                    status = c.wait().ok();
                }
                Err(e) => {
                    toast(&app2, format!("Не удалось запустить установщик: {e}"));
                    break;
                }
            }
        }
        // Куда встала игра: наша папка → новая запись в реестре (пользователь выбрал свою).
        let mut found = has_exes(&target).then(|| target.clone());
        if found.is_none() && sys::IS_WINDOWS {
            let after = sys::uninstall_locations();
            found = after
                .iter()
                .filter(|(k, v)| before.get(*k) != Some(*v))
                .map(|(_, v)| v.clone())
                .find(|v| v.is_dir());
        }
        if found.is_none() {
            let _ = std::fs::remove_dir(&target); // пустую папку убираем
        }
        let removed = remove_site_shortcuts(&state, &id, started);
        if removed > 0 {
            toast(&app2, "Убрал ярлык сайта, который создал установщик");
        }
        state.running.lock().remove(&id);
        state.update(&id, |g| {
            g.status = Status::NeedsExe;
            if found.is_some() {
                g.install_dir = found.clone();
            }
        });
        if silent && found.as_deref() == Some(target.as_path()) {
            toast(&app2, "Игра установлена — выберите exe");
        }
        notify(&app2);
        let _ = app2.emit("installer-exited", id);
    });
    Ok("installer".into())
}

#[tauri::command]
async fn find_executables(state: State<'_, AppState>, id: String) -> CmdResult<Vec<String>> {
    let g = state.game(&id)?;
    tokio::task::spawn_blocking(move || find_executables_blocking(&g))
        .await
        .map_err(err)
}

fn find_executables_blocking(g: &Game) -> Vec<String> {
    let mut out = vec![];
    // Папка, куда установщик поставил игру (наша или найденная в реестре).
    let mut known = vec![g.target_install_dir()];
    if let Some(d) = &g.install_dir {
        if !known.contains(d) {
            known.insert(0, d.clone());
        }
    }
    out.extend(files::find_game_exes(&known, 7, None));
    if sys::IS_WINDOWS {
        // Установщик ставит игру куда выбрал пользователь — ищем exe,
        // появившиеся после запуска установщика, в типичных папках всех дисков.
        if let Some(started) = g.install_started {
            let since =
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(started.saturating_sub(120));
            let mut roots = vec![];
            for letter in b'C'..=b'Z' {
                let drive = PathBuf::from(format!("{}:\\", letter as char));
                if !drive.exists() {
                    continue;
                }
                for sub in ["Games", "Program Files", "Program Files (x86)", "Игры"] {
                    roots.push(drive.join(sub));
                }
            }
            for p in files::find_game_exes(&roots, 5, Some(since)) {
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
    } else {
        // Установщики обычно ставят в Games / Program Files внутри префикса.
        let mut roots = vec![];
        if let Ok(rd) = std::fs::read_dir(g.drive_c()) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_lowercase();
                if e.path().is_dir() && !matches!(n.as_str(), "windows" | "programdata" | "users") {
                    roots.push(e.path());
                }
            }
        }
        for p in files::find_game_exes(&roots, 7, None) {
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    // Портативные игры — прямо в раздаче.
    for p in files::find_game_exes(&[g.extracted_dir(), g.download_dir()], 7, None) {
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

#[tauri::command]
async fn set_executable(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    path: String,
) -> CmdResult<String> {
    let p = PathBuf::from(&path);
    if !p.is_file() {
        return Err("Файл не найден".into());
    }
    // Выбрали установщик из раздачи — это не игра, его надо запустить как установку.
    let g0 = state.game(&id)?;
    if is_release_installer(&g0, &p) {
        install_game_impl(&app, &id, Some(p)).await?;
        return Ok("Это установщик — запускаю установку игры".into());
    }
    state.update(&id, |g| {
        g.exe = Some(p);
        g.status = Status::Installed;
    });
    notify(&app);
    let g = state.game(&id)?;
    // installKind 2 — фикс уже вшит в раздачу: отдельный не ставим, чтобы не откатить версию.
    // Если вариантов фикса несколько — пусть выберет пользователь (кнопка «Установить фикс»).
    if g.update_from.is_some() {
        state.update(&id, |g| g.update_from = None);
        notify(&app);
    }
    let fixes = g.addons.iter().filter(|a| a.is_fix()).count();
    // Тот же вариант фикса, что стоял раньше (после обновления игры) — ставим без вопросов.
    let same = g
        .addon_applied
        .as_ref()
        .and_then(|t| g.addons.iter().position(|a| a.is_fix() && &a.title == t));
    if let (Some(i), false) = (same, g.addons_applied) {
        return match apply_addons(app.clone(), state, id, Some(i)).await {
            Ok(_) => Ok("Готово. Фикс поставлен заново".into()),
            Err(e) => Ok(format!("Игра готова, но фикс не поставился: {e}")),
        };
    }
    if g.install_kind != 2 && fixes > 1 && !g.addons_applied {
        return Ok(format!("Готово к запуску. Для игры есть {fixes} варианта фикса — выберите в меню «Установить фикс»"));
    }
    if g.install_kind != 2 && fixes == 1 && !g.addons_applied {
        return match apply_addons(app.clone(), state, id, None).await {
            Ok(n) => Ok(format!("Готово. Фикс установлен ({n} файлов)")),
            Err(e) => Ok(format!("Игра готова, но фикс не поставился: {e}")),
        };
    }
    Ok("Готово к запуску".into())
}

/// Файл — установщик из скачанной раздачи (а не exe игры).
fn is_release_installer(g: &Game, p: &Path) -> bool {
    (p.starts_with(g.download_dir()) || p.starts_with(g.extracted_dir()))
        && files::looks_like_installer(p)
}

/// Скачивает выбранный фикс (`index` в списке addons из базы; по умолчанию — первый
/// подходящий) и ставит его в папку игры. Варианты фикса не суммируются: ставится один.
#[tauri::command]
async fn apply_addons(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    index: Option<usize>,
) -> CmdResult<usize> {
    let g = state.game(&id)?;
    let exe = g.exe.clone().ok_or("Сначала выберите exe игры")?;
    let target = game_root_for_fix(&exe);
    let addon = match index {
        Some(i) => g.addons.get(i).ok_or("Нет такого фикса")?,
        None => g
            .addons
            .iter()
            .find(|a| a.is_fix())
            .ok_or("Для игры нет фикса с прямой ссылкой")?,
    };
    if matches!(addon.delivery.as_deref(), Some("magnet"))
        || addon.uris.first().is_some_and(|u| u.starts_with("magnet:"))
    {
        return Err("Это дополнение раздаётся торрентом — скачайте его со страницы игры".into());
    }
    let mut total = 0;
    {
        toast(&app, format!("Скачиваю фикс: {}", addon.title));
        let addons_dir = g.dir.join("addons");
        let file = match files::download_addon(addon, &addons_dir).await {
            Ok(f) => f,
            // freetp.org закрыт Cloudflare и таймером — прямой запрос получает 403.
            // Тогда качаем через встроенный браузер, как это сделал бы человек.
            Err(e) => match addon.getfile_url.as_deref().or(addon.referer.as_deref()) {
                Some(page) => {
                    toast(
                        &app,
                        "Сайт не отдал фикс напрямую — открываю страницу загрузки…",
                    );
                    download_in_webview(&app, page, &addons_dir).await?
                }
                None => return Err(e.to_string()),
            },
        };
        if files::is_inno_installer(&file) {
            toast(&app, "Фикс — это установщик, запускаю его с папкой игры…");
            if !run_fix_installer(&state, &g, &file, &target).await? {
                return Err(format!(
                    "Установщик фикса закрылся с ошибкой, и файлов фикса (OnlineFix / SteamFix) в {} не видно. \
                     Если в установщике была выбрана другая папка — поставьте фикс снова и укажите папку игры.",
                    target.display()
                ));
            }
            total += 1;
        } else {
            let unpack = g
                .dir
                .join("addons")
                .join(format!("unpacked-{}", files::sanitize(&addon.title)));
            let _ = std::fs::remove_dir_all(&unpack);
            let target2 = target.clone();
            total += tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
                files::extract(&file, &unpack)?;
                files::copy_over(&unpack, &target2)
            })
            .await
            .map_err(err)?
            .map_err(err)?;
        }
    }
    let title = addon.title.clone();
    state.update(&id, |g| {
        g.addons_applied = true;
        g.addon_applied = Some(title);
    });
    notify(&app);
    Ok(total)
}

/// Убирает ярлыки сайтов, которые установщик положил на рабочий стол
/// (FreeTP.Org.url и т.п.). Ярлык самой игры (.lnk) не трогаем.
fn remove_site_shortcuts(state: &AppState, id: &str, since: std::time::SystemTime) -> usize {
    let mut dirs_list: Vec<PathBuf> = dirs::desktop_dir().into_iter().collect();
    if let Some(p) = std::env::var_os("PUBLIC") {
        dirs_list.push(PathBuf::from(p).join("Desktop"));
    }
    if let Ok(g) = state.game(id) {
        if let Ok(rd) = std::fs::read_dir(g.drive_c().join("users")) {
            for e in rd.flatten() {
                dirs_list.push(e.path().join("Desktop"));
            }
        }
    }
    files::remove_site_shortcuts(&dirs_list, since)
}

/// Путь в том виде, в каком его видит Windows-программа (под Wine — C:\ или Z:\).
fn windows_path(g: &Game, p: &Path) -> String {
    if sys::IS_WINDOWS {
        return p.to_string_lossy().into_owned();
    }
    match p.strip_prefix(g.drive_c()) {
        Ok(rest) => format!("C:\\{}", rest.to_string_lossy().replace('/', "\\")),
        Err(_) => format!("Z:{}", p.to_string_lossy().replace('/', "\\")),
    }
}

/// Запускает установщик фикса (Inno Setup) с папкой игры: сначала тихо,
/// а если не вышло — обычным мастером, где папка уже подставлена.
/// Возвращает true, если фикс встал: установщик завершился успешно или файлы фикса на месте.
async fn run_fix_installer(
    state: &AppState,
    g: &Game,
    installer: &Path,
    target: &Path,
) -> CmdResult<bool> {
    let proton_dir = if sys::IS_WINDOWS {
        PathBuf::new()
    } else {
        state.resolve_proton(g)?
    };
    let use_umu = state.settings.lock().use_umu;
    let dir_arg = format!("/DIR={}", windows_path(g, target));
    let attempts: [&[&str]; 2] = [
        &[
            "/SILENT",
            "/SP-",
            "/NORESTART",
            "/NOICONS",
            "/PASSWORD=online-fix.me",
        ],
        &["/SP-", "/NORESTART", "/NOICONS"],
    ];
    for flags in attempts {
        let mut args: Vec<String> = flags.iter().map(|s| s.to_string()).collect();
        args.push(dir_arg.clone());
        let mut child = proton::spawn(proton::RunSpec {
            proton_dir: &proton_dir,
            compat_dir: &g.compat_dir(),
            exe: installer,
            args,
            use_umu,
            online_fix: false,
            dll_overrides: None,
            extra_env: proton::installer_env(),
            log_file: None,
            steam_overlay: false,
        })
        .map_err(err)?;
        let started = std::time::SystemTime::now() - std::time::Duration::from_secs(5);
        let status = tokio::task::spawn_blocking(move || child.wait())
            .await
            .map_err(err)?
            .map_err(err)?;
        remove_site_shortcuts(state, &g.id, started);
        if status.success() || files::fix_present(target) {
            return Ok(true);
        }
    }
    Ok(files::fix_present(target))
}

/// Скрипт для страницы getfile: ждёт окончания таймера и жмёт «Скачать».
const AUTOCLICK_JS: &str = r#"
(function () {
  if (window.__linfix) return; window.__linfix = true;
  var clicked = false;
  setInterval(function () {
    if (clicked) return;
    var b = document.querySelector('.dl-button, .ftp-dl-button');
    if (!b) return;
    var st = getComputedStyle(b);
    if (st.display === 'none' || st.visibility === 'hidden' || !b.offsetParent) return;
    clicked = true;
    b.removeAttribute('target');
    var href = b.getAttribute('href') || '';
    var dl = document.getElementById('dlink');
    var link = href && href !== '#' ? href : (dl && dl.value && dl.value !== '#' ? dl.value : '');
    b.click();
    // Если обработчик сайта ничего не сделал — переходим по ссылке сами.
    if (link) setTimeout(function () { location.href = link; }, 3000);
  }, 700);
})();
"#;

/// Скрипт для страницы с JSON (база Online-Fix за Cloudflare): когда вместо
/// проверки появится JSON — «скачивает» текст страницы, и лаунчер его перехватывает.
const GRAB_JSON_JS: &str = r#"
(function () {
  if (window.__linfix) return; window.__linfix = true;
  var iv = setInterval(function () {
    if (!document.body) return;
    var el = document.querySelector('pre') || document.body;
    var t = (el.textContent || '').trim();
    if (t[0] !== '{' && t[0] !== '[') return;
    clearInterval(iv);
    var a = document.createElement('a');
    a.href = URL.createObjectURL(new Blob([t], { type: 'application/json' }));
    a.download = 'linfix-data.json';
    document.body.appendChild(a);
    a.click();
  }, 500);
})();
"#;

struct WebviewDownload<'a> {
    page: &'a str,
    dest_dir: &'a Path,
    script: &'a str,
    title: &'a str,
    visible: bool,
    timeout_secs: u64,
}

/// Фикс со страницы freetp: окно видно пользователю — если Cloudflare
/// попросит подтвердить, что вы не робот, это можно сделать вручную.
async fn download_in_webview(app: &AppHandle, page: &str, dest_dir: &Path) -> CmdResult<PathBuf> {
    webview_download(
        app,
        WebviewDownload {
            page,
            dest_dir,
            script: AUTOCLICK_JS,
            title: "OnlineFix-Launcher — скачивание фикса (подождите ~30 с, кнопка нажмётся сама)",
            visible: true,
            timeout_secs: 600,
        },
    )
    .await
}

/// Скачивает JSON со страницы за Cloudflare через скрытое окно WebView.
pub(crate) async fn fetch_json_via_webview(
    app: &AppHandle,
    url: &str,
    dest_dir: &Path,
) -> CmdResult<Vec<u8>> {
    let path = webview_download(
        app,
        WebviewDownload {
            page: url,
            dest_dir,
            script: GRAB_JSON_JS,
            title: "OnlineFix-Launcher — загрузка базы",
            visible: false,
            timeout_secs: 40,
        },
    )
    .await?;
    let bytes = std::fs::read(&path).map_err(err)?;
    let _ = std::fs::remove_file(&path);
    Ok(bytes)
}

/// Открывает страницу во встроенном окне, перехватывает скачанный файл
/// и кладёт его в `dest_dir`.
async fn webview_download(app: &AppHandle, opts: WebviewDownload<'_>) -> CmdResult<PathBuf> {
    let WebviewDownload {
        page,
        dest_dir,
        script,
        title,
        visible,
        timeout_secs,
    } = opts;
    use tauri::webview::{DownloadEvent, WebviewWindowBuilder};
    use tauri::{WebviewUrl, WindowEvent};

    std::fs::create_dir_all(dest_dir).map_err(err)?;
    let url: tauri::Url = page.parse().map_err(err)?;
    let (tx, rx) = tokio::sync::oneshot::channel::<CmdResult<PathBuf>>();
    let tx = Arc::new(Mutex::new(Some(tx)));
    let send = {
        let tx = tx.clone();
        move |r: CmdResult<PathBuf>| {
            if let Some(tx) = tx.lock().take() {
                let _ = tx.send(r);
            }
        }
    };

    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let label = format!(
        "wv-{}-{}",
        library::now(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let dest = dest_dir.to_path_buf();
    let send_dl = send.clone();
    // Куда WebView пишет файл — на случай, если событие «готово» не придёт.
    let target: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
    let target_dl = target.clone();
    let win = WebviewWindowBuilder::new(app, &label, WebviewUrl::External(url))
        .title(title)
        .inner_size(900.0, 700.0)
        .visible(visible)
        .initialization_script(script)
        .on_download(move |_webview, event| {
            match event {
                DownloadEvent::Requested { destination, .. } => {
                    let name = destination
                        .file_name()
                        .map(|n| n.to_os_string())
                        .unwrap_or_else(|| "download.bin".into());
                    *destination = dest.join(name);
                    *target_dl.lock() = Some(destination.clone());
                }
                DownloadEvent::Finished { path, success, .. } => match (success, path) {
                    (true, Some(p)) => send_dl(Ok(p)),
                    (true, None) => {
                        let p = target_dl.lock().clone();
                        send_dl(p.ok_or_else(|| "Файл скачан, но путь неизвестен".to_string()))
                    }
                    _ => send_dl(Err("Скачивание прервалось".into())),
                },
                _ => {}
            }
            true
        })
        .build()
        .map_err(err)?;

    let send_close = send.clone();
    win.on_window_event(move |e| {
        if matches!(e, WindowEvent::Destroyed) {
            send_close(Err("Окно загрузки закрыто — файл не скачан".into()));
        }
    });

    // Ждём событие от WebView, а параллельно следим за самим файлом:
    // если он появился и его размер перестал меняться — считаем, что скачан.
    let watch = {
        let target = target.clone();
        async move {
            let mut last = 0u64;
            let mut stable = 0;
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                let Some(p) = target.lock().clone() else {
                    continue;
                };
                let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                if size > 0 && size == last {
                    stable += 1;
                    if stable >= 3 {
                        return Ok(p);
                    }
                } else {
                    stable = 0;
                }
                last = size;
            }
        }
    };
    let res = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
        tokio::select! {
            r = rx => r.unwrap_or_else(|_| Err("Окно загрузки закрыто — файл не скачан".into())),
            r = watch => r,
        }
    })
    .await;
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.destroy();
    }
    res.unwrap_or_else(|_| Err(format!("Файл не скачался за {timeout_secs} с")))
}

/// Для UE-игр exe лежит в <Game>/Binaries/Win64 — фикс кладём в корень игры.
fn game_root_for_fix(exe: &Path) -> PathBuf {
    let dir = exe.parent().unwrap_or(exe).to_path_buf();
    let s = dir.to_string_lossy().to_lowercase();
    if s.ends_with("binaries/win64") || s.ends_with("binaries/win32") {
        // <Root>/<Project>/Binaries/Win64 -> <Root>
        if let Some(root) = dir
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
        {
            return root.to_path_buf();
        }
    }
    dir
}

// ───────────────────────── Запуск ─────────────────────────

#[tauri::command]
fn launch_game(app: AppHandle, state: State<AppState>, id: String) -> CmdResult<()> {
    let g = state.game(&id)?;
    if state.running.lock().contains_key(&id) {
        return Err("Игра уже запущена".into());
    }
    let exe = g.exe.clone().ok_or("Не выбран exe игры")?;
    if !exe.is_file() {
        state.update(&id, |g| g.status = Status::NeedsExe);
        notify(&app);
        return Err(format!("Файл игры не найден: {}. Выберите exe заново", exe.display()));
    }
    // Раньше вместо игры можно было выбрать setup.exe — тогда запускаем установку.
    if is_release_installer(&g, &exe) {
        state.update(&id, |g| {
            g.exe = None;
            g.status = Status::Downloaded;
        });
        let app2 = app.clone();
        let id2 = id.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = install_game_impl(&app2, &id2, Some(exe)).await {
                toast(&app2, e);
            }
        });
        return Err("Выбран установщик, а не игра — запускаю установку. После неё выберите exe игры".into());
    }
    let proton_dir = state.resolve_proton(&g)?;
    let (use_umu, log, steam_overlay, wayland) = {
        let s = state.settings.lock();
        (s.use_umu, s.proton_log.then(|| g.dir.join("proton.log")), s.steam_overlay, s.wayland)
    };
    // Нативный Wayland-драйвер Wine (GE, CachyOS, EM, DW). Свои переменные игры идут после
    // и могут это переопределить.
    let mut extra_env = Vec::new();
    if wayland {
        extra_env.push(("PROTON_ENABLE_WAYLAND".to_string(), "1".to_string()));
    }
    extra_env.extend(proton::parse_env(&g.env));
    let mut child = proton::spawn(proton::RunSpec {
        proton_dir: &proton_dir,
        compat_dir: &g.compat_dir(),
        exe: &exe,
        args: g.launch_args.split_whitespace().map(String::from).collect(),
        use_umu,
        online_fix: g.spacewar,
        dll_overrides: Some(g.dll_overrides.clone()),
        extra_env,
        log_file: log,
        steam_overlay,
    })
    .map_err(err)?;
    state.running.lock().insert(id.clone(), child.id());
    let started = library::now();
    state.update(&id, |g| g.last_played = Some(started));
    notify(&app);

    let app2 = app.clone();
    let log_on = log_enabled(&state);
    std::thread::spawn(move || {
        let status = child.wait();
        let state = app2.state::<AppState>();
        state.running.lock().remove(&id);
        let played = library::now().saturating_sub(started);
        // Игра закрылась почти сразу с ошибкой — скорее всего, не запустилась.
        if played < 8 {
            if let Ok(st) = &status {
                if !st.success() {
                    let hint = if sys::IS_WINDOWS {
                        "Проверьте, что выбран правильный exe".to_string()
                    } else if log_on {
                        "Подробности — в proton.log в папке игры".to_string()
                    } else {
                        "Включите «Логи Proton» в настройках и запустите снова — появится proton.log".to_string()
                    };
                    toast(&app2, format!("Игра закрылась сразу (код {}). {hint}", st.code().unwrap_or(-1)));
                }
            }
        }
        state.update(&id, |g| g.playtime_secs += played);
        notify(&app2);
        let _ = app2.emit("game-exited", id);
    });
    Ok(())
}

fn log_enabled(state: &AppState) -> bool {
    state.settings.lock().proton_log
}

#[tauri::command]
fn stop_game(app: AppHandle, state: State<AppState>, id: String) -> CmdResult<()> {
    let g = state.game(&id)?;
    let proton_dir = state.resolve_proton(&g)?;
    let pid = state.running.lock().get(&id).copied();
    proton::kill(&proton_dir, &g.compat_dir(), pid);
    notify(&app);
    Ok(())
}

#[tauri::command]
fn open_dir(state: State<AppState>, id: String, which: String) -> CmdResult<()> {
    let g = state.game(&id)?;
    let path = match which.as_str() {
        "prefix" => g.drive_c(),
        "exe" => g
            .exe
            .as_ref()
            .and_then(|e| e.parent().map(Path::to_path_buf))
            .unwrap_or(g.dir.clone()),
        _ => g.dir.clone(),
    };
    open_external(path.as_os_str())
}

#[tauri::command]
fn open_url(url: String) -> CmdResult<()> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("Неверная ссылка".into());
    }
    open_external(std::ffi::OsStr::new(&url))
}

/// Открывает ссылку или папку системным приложением.
fn open_external(target: &std::ffi::OsStr) -> CmdResult<()> {
    #[cfg(target_os = "windows")]
    let mut cmd = std::process::Command::new("explorer");
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(target)
        .spawn()
        .map_err(|e| format!("Не удалось открыть {}: {e}", target.to_string_lossy()))?;
    Ok(())
}

// ───────────────────────── Обложки ─────────────────────────

/// Ищет игру в магазине Steam и возвращает URL горизонтальной обложки.
#[tauri::command]
async fn cover(
    state: State<'_, AppState>,
    title: String,
    article: Option<String>,
) -> CmdResult<Option<String>> {
    if let Some(c) = state.covers.lock().get(&title) {
        return Ok(c.clone());
    }
    let found = covers::lookup(&title, article.as_deref())
        .await
        .map_err(err)?;
    let snapshot = {
        let mut map = state.covers.lock();
        map.insert(title, found.clone());
        map.clone()
    };
    let _ = library::save_json(&state.data_dir.join("cache/covers-v2.json"), &snapshot);
    Ok(found)
}

// ───────────────────────── Настройки / система ─────────────────────────

#[tauri::command]
fn get_settings(state: State<AppState>) -> Settings {
    state.settings.lock().clone()
}

#[tauri::command]
fn save_settings(state: State<AppState>, mut settings: Settings) -> CmdResult<()> {
    settings.games_dir = expand_home(settings.games_dir);
    settings.prefixes_dir = settings
        .prefixes_dir
        .filter(|p| !p.as_os_str().is_empty())
        .map(expand_home);
    if let Some(p) = &settings.prefixes_dir {
        if let Some(fs) = sys::fs_type(p).filter(|f| sys::fs_unsuitable_for_prefix(f)) {
            return Err(format!("Папка префиксов на {fs}: там Proton не работает. Выберите раздел Linux (ext4, btrfs…)"));
        }
    }
    library::save_json(&state.settings_file(), &settings).map_err(err)?;
    *state.settings.lock() = settings;
    Ok(())
}

/// `~/x` → `/home/user/x`.
fn expand_home(p: PathBuf) -> PathBuf {
    match (p.strip_prefix("~"), dirs::home_dir()) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => p,
    }
}

#[tauri::command]
fn list_protons() -> Vec<proton::ProtonInfo> {
    proton::list_protons()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemStatus {
    /// "windows" | "linux" | "macos"
    platform: &'static str,
    steam_root: Option<String>,
    steam_running: bool,
    umu: bool,
    protons: usize,
    /// Есть ли GE-Proton (он работает без контейнера Steam Runtime — надёжнее всего).
    ge_proton: bool,
    /// Процессор умеет x86-64-v3 (для Proton-CachyOS v3).
    cpu_v3: bool,
    /// Найденный архиватор: путь к 7z или WinRAR.
    archiver: Option<String>,
    /// Есть ли чем распаковать RAR на Linux (unrar).
    unrar: bool,
    /// Steam из Flatpak (у него своя песочница — онлайн может не видеть клиента).
    steam_flatpak: bool,
    /// ID дистрибутива из /etc/os-release (ubuntu, arch, fedora…) — для подсказок.
    distro: Option<String>,
    /// Похожие дистрибутивы (ID_LIKE) — для подсказок.
    distro_like: Option<String>,
}

fn os_release(key: &str) -> Option<String> {
    let text = std::fs::read_to_string("/etc/os-release").ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .map(|v| v.trim_matches('"').to_lowercase())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DirInfo {
    /// Файловая система (Linux).
    fs: Option<String>,
    /// Префикс Proton на ней работать не будет (NTFS/exFAT/FAT).
    unsuitable: bool,
    /// Свободно байт.
    free_bytes: Option<u64>,
}

/// Сведения о папке для игр: файловая система и свободное место.
#[tauri::command]
fn dir_info(path: String) -> DirInfo {
    let p = PathBuf::from(path.trim());
    let mut existing = p.as_path();
    while !existing.exists() {
        match existing.parent() {
            Some(x) => existing = x,
            None => break,
        }
    }
    let fs = sys::fs_type(&p);
    DirInfo {
        unsuitable: fs.as_deref().is_some_and(sys::fs_unsuitable_for_prefix),
        fs,
        free_bytes: fs4::available_space(existing).ok(),
    }
}

#[tauri::command]
async fn system_status() -> SystemStatus {
    // Проверки запускают внешние программы — не держим главный поток.
    tokio::task::spawn_blocking(|| SystemStatus {
        platform: std::env::consts::OS,
        steam_root: proton::steam_root().map(|p| p.to_string_lossy().to_string()),
        steam_running: proton::is_steam_running(),
        umu: proton::umu_available(),
        protons: proton::list_protons().len(),
        ge_proton: proton::list_protons().iter().any(|p| p.name.to_lowercase().contains("ge-proton")),
        cpu_v3: proton::cpu_x86_64_v3(),
        archiver: sys::seven_zip()
            .or_else(sys::winrar)
            .map(|p| p.to_string_lossy().to_string()),
        unrar: sys::which("unrar").is_some(),
        steam_flatpak: proton::steam_root()
            .is_some_and(|p| p.to_string_lossy().contains("com.valvesoftware.Steam")),
        distro: os_release("ID"),
        distro_like: os_release("ID_LIKE"),
    })
    .await
    .expect("system_status task")
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GeResult {
    tag: String,
    /// Последняя версия уже была установлена.
    already: bool,
}

/// Скачивает свежий GE-Proton (только Linux). Оставлено для мастера первого запуска.
#[tauri::command]
async fn install_ge_proton(app: AppHandle) -> CmdResult<GeResult> {
    install_proton(app, proton::ProtonKind::Ge).await
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ProtonProgress {
    kind: proton::ProtonKind,
    pct: u8,
}

/// Скачивает свежую сборку Proton выбранного типа (только Linux).
/// Проценты загрузки идут событием `proton-progress`.
#[tauri::command]
async fn install_proton(app: AppHandle, kind: proton::ProtonKind) -> CmdResult<GeResult> {
    let (a1, a2) = (app.clone(), app.clone());
    proton::install_proton(
        kind,
        move |pct| {
            let _ = a1.emit("proton-progress", ProtonProgress { kind, pct });
        },
        move |msg| toast(&a2, msg),
    )
    .await
    .map(|(tag, already)| GeResult { tag, already })
    .map_err(err)
}

#[tauri::command]
fn start_steam() -> CmdResult<()> {
    proton::start_steam().map_err(err)
}

// ───────────────────────── Точка входа ─────────────────────────

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let mut settings: Settings = library::load_json(&data_dir.join("settings.json"));
            if settings.source.is_empty() {
                settings = Settings::default();
            }
            // Выбор базы Hydra убран: это урезанная копия arachnel (без фиксов),
            // она осталась только запасным источником.
            if settings.source == "hydra" {
                settings.source = "arachnel".into();
            }
            if settings.rev < library::SETTINGS_REV {
                settings.migrate();
                let _ = library::save_json(&data_dir.join("settings.json"), &settings);
            }
            let mut games: Vec<Game> = library::load_json(&data_dir.join("library.json"));
            // Прерванные установки откатываем в «скачано».
            for g in games.iter_mut() {
                if g.status == Status::Installing {
                    g.status = Status::Downloaded;
                }
            }
            let resume: Vec<Game> = games
                .iter()
                .filter(|g| g.status == Status::Downloading)
                .cloned()
                .collect();
            let covers_cache: HashMap<String, Option<String>> =
                library::load_json(&data_dir.join("cache/covers-v2.json"));
            app.manage(AppState {
                data_dir,
                settings: Mutex::new(settings),
                games: Mutex::new(games),
                session: tokio::sync::OnceCell::new(),
                torrents: Mutex::new(HashMap::new()),
                pending: Mutex::new(HashMap::new()),
                running: Mutex::new(HashMap::new()),
                covers: Mutex::new(covers_cache),
            });
            // Самопроверка загрузки базы Online-Fix: LINFIX_SELFTEST=onlinefix linfix.exe
            if std::env::var("LINFIX_SELFTEST").as_deref() == Ok("onlinefix") {
                let h = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let cache = h.state::<AppState>().data_dir.join("cache");
                    match load_onlinefix(&h, &cache, true).await {
                        Ok(r) => println!(
                            "SELFTEST ok: {} игр, {}, свежая {}",
                            r.entries.len(),
                            r.origin,
                            r.newest
                        ),
                        Err(e) => println!("SELFTEST err: {e}"),
                    }
                    h.exit(0);
                });
            }
            // Докачиваем незавершённые загрузки (librqbit проверит уже скачанные куски).
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                for g in resume {
                    if let Err(e) = start_torrent(handle.clone(), g.clone()).await {
                        handle
                            .state::<AppState>()
                            .update(&g.id, |x| x.last_error = Some(e));
                    }
                }
                notify(&handle);
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_catalog,
            popular,
            check_updates,
            start_game_update,
            app_version,
            steam_page,
            check_app_update,
            install_app_update,
            dir_info,
            add_game,
            library,
            running_games,
            downloads,
            pause_download,
            resume_download,
            install_game,
            find_executables,
            set_executable,
            apply_addons,
            launch_game,
            stop_game,
            update_game,
            remove_game,
            open_dir,
            open_url,
            get_settings,
            save_settings,
            list_protons,
            system_status,
            start_steam,
            cover,
            install_ge_proton,
            install_proton,
        ])
        .run(tauri::generate_context!())
        .expect("error while running OnlineFix-Launcher");
}
