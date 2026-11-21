// LinFix UI — SPDX-License-Identifier: GPL-3.0-or-later
"use strict";

const T = window.__TAURI__;
const invoke = T ? T.core.invoke : mockInvoke;
const listen = T ? T.event.listen : () => {};
// Репозиторий на GitHub (страница релизов и ошибок). Тот же адрес — в tauri.conf.json → plugins.updater.
const REPO_URL = "https://github.com/ollezore/LinFix";

const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => [...r.querySelectorAll(s)];
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

const state = {
  view: "catalog",
  catalog: [],      // сгруппированные по названию игры
  filtered: [],
  shown: 0,
  filter: "all",
  sort: localStorage.getItem("catalogSort") || "popular",
  cat: localStorage.getItem("catalogTab") || "freetp",  // вкладка каталога: freetp | onlinefix
  catalogs: {},     // сгруппированные каталоги по вкладкам
  catMeta: {},      // { origin, newest } по вкладкам
  updates: new Map(), // id игры → { kind: "game" | "fix", entry, fixTitle }
  popular: null,    // Map: id статьи FreeTP → место в рейтинге посещаемости
  query: "",
  library: [],
  downloads: {},
  running: new Set(),
  settings: null,
  system: null,
};

// ───────────── Утилиты ─────────────

function hue(str) {
  let h = 0;
  for (const c of str) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return h % 360;
}
function coverHTML(title, tag = "", article = "") {
  const h = hue(title);
  const initials = title.replace(/[^\p{L}\p{N} ]/gu, "").split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]).join("").toUpperCase() || "?";
  const cached = coverCache.get(title);
  const img = cached ? `<img src="${esc(cached)}" alt="" onerror="this.remove()">` : "";
  return `<div class="cover" data-title="${esc(title)}" data-article="${esc(article || "")}" style="background: linear-gradient(135deg, hsl(${h} 45% 38%), hsl(${(h + 50) % 360} 55% 22%))">${esc(initials)}${img}${tag}</div>`;
}

// ───────────── Обложки (Steam) — грузятся лениво, по мере прокрутки ─────────────

const coverCache = new Map();   // title -> url | null
const coverQueue = [];
let coverActive = 0;
const coverObserver = new IntersectionObserver((entries) => {
  for (const e of entries) {
    if (!e.isIntersecting) continue;
    coverObserver.unobserve(e.target);
    requestCover(e.target);
  }
}, { rootMargin: "300px" });

function observeCovers(root = document) {
  $$(".cover[data-title]:not([data-cover-done])", root).forEach((el) => {
    el.dataset.coverDone = "1";
    if (!coverCache.has(el.dataset.title)) coverObserver.observe(el);
  });
}
function requestCover(el) {
  coverQueue.push(el);
  pumpCovers();
}
function pumpCovers() {
  while (coverActive < 4 && coverQueue.length) {
    const el = coverQueue.shift();
    const title = el.dataset.title;
    if (coverCache.has(title)) { applyCover(title); continue; }
    coverActive++;
    invoke("cover", { title, article: el.dataset.article || null })
      .then((url) => { coverCache.set(title, url || null); applyCover(title); })
      // Сетевая ошибка или лимит Steam — пробуем ещё раз позже (до 3 раз).
      .catch(() => {
        const n = +(el.dataset.coverTries || 0) + 1;
        el.dataset.coverTries = n;
        if (n < 3) setTimeout(() => requestCover(el), 4000 * n);
      })
      .finally(() => { coverActive--; pumpCovers(); });
  }
}
function applyCover(title) {
  const url = coverCache.get(title);
  if (!url) return;
  $$(".cover[data-title]").forEach((el) => {
    if (el.dataset.title !== title || el.querySelector("img")) return;
    const img = new Image();
    img.alt = "";
    img.onload = () => el.insertBefore(img, el.querySelector(".tag"));
    img.src = url;
  });
}
function fmtBytes(b) {
  if (!b) return `0 ${t("Б")}`;
  const u = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
  const i = Math.min(u.length - 1, Math.floor(Math.log(b) / Math.log(1024)));
  return `${(b / 1024 ** i).toLocaleString(I18N.locale, { maximumFractionDigits: i > 1 ? 1 : 0 })} ${t(u[i])}`;
}
function fmtTime(s) {
  if (s == null) return "—";
  if (s < 60) return t("{0} с", [s]);
  if (s < 3600) return t("{0} мин", [Math.floor(s / 60)]);
  return t("{0} ч {1} мин", [Math.floor(s / 3600), Math.floor((s % 3600) / 60)]);
}
function fmtDate(iso) {
  const d = new Date(iso);
  return isNaN(d) ? "" : d.toLocaleDateString(I18N.locale, { day: "numeric", month: "short", year: "numeric" });
}
// «5.72 GB» → байты (для сортировки по размеру).
function sizeBytes(s) {
  const m = /([\d.,]+)\s*([KMGTКМГТ]?)[BБ]/i.exec(s || "");
  if (!m) return 0;
  const pow = { "": 0, K: 1, "К": 1, M: 2, "М": 2, G: 3, "Г": 3, T: 4, "Т": 4 }[m[2].toUpperCase()] ?? 0;
  return parseFloat(m[1].replace(",", ".")) * 1024 ** pow;
}

const articleId = (url) => { const m = /\/po-seti\/(\d+)-/.exec(url || ""); return m ? +m[1] : null; };

// Рейтинг FreeTP по посещаемости → место каждой игры каталога (1, 2, 3…).
async function loadPopular(refresh = false) {
  try {
    const ids = await invoke("popular", { refresh });
    state.popular = new Map(ids.map((id, i) => [id, i]));
  } catch (e) {
    state.popular = new Map();
    if (state.sort === "popular" || state.filter === "top") snack(`Не удалось загрузить рейтинг популярности: ${e}`);
  }
  rankCatalog();
  applyFilter();
}

const normTitle = (t) => (t || "").toLowerCase().replace(/[^\p{L}\p{N}]/gu, "");

function rankCatalog() {
  const pos = state.popular || new Map();
  // Место по названию — из каталога FreeTP (у Online-Fix нет ссылок на статьи).
  const byTitle = new Map();
  if (state.cat !== "freetp" && state.catalogs.freetp) {
    for (const g of state.catalogs.freetp)
      for (const v of g.versions) {
        const p = pos.get(articleId(v.articleUrl));
        const k = normTitle(g.title);
        if (p !== undefined && !(byTitle.get(k) <= p)) byTitle.set(k, p);
      }
  }
  const ranked = [];
  for (const g of state.catalog) {
    g.rank = null;
    let best = Infinity;
    for (const v of g.versions) {
      const p = pos.get(articleId(v.articleUrl));
      if (p !== undefined && p < best) best = p;
    }
    const pt = byTitle.get(normTitle(g.title));
    if (pt !== undefined && pt < best) best = pt;
    g.pop = best;
    if (best !== Infinity) ranked.push(g);
  }
  ranked.sort((a, b) => a.pop - b.pop).forEach((g, i) => (g.rank = i + 1));
}

function kindTag(kind) {
  if (kind === 2) return `<span class="tag ok">Фикс в раздаче</span>`;
  if (kind === 1) return `<span class="tag">Фикс отдельно</span>`;
  return "";
}

// ───────────── Snackbar / диалоги / меню ─────────────

let snackTimer;
function snack(msg, action, onAction) {
  const el = $("#snackbar");
  $(".msg", el).textContent = msg;
  const btn = $("button", el);
  btn.classList.toggle("hidden", !action);
  btn.textContent = action || "";
  btn.onclick = () => { el.classList.remove("show"); onAction && onAction(); };
  el.classList.add("show");
  clearTimeout(snackTimer);
  snackTimer = setTimeout(() => el.classList.remove("show"), action ? 8000 : 4000);
}
const fail = (e) => snack(String(e));

function openDialog(html, onMount) {
  const dlg = $("#dialog");
  dlg.innerHTML = `<div class="dlg">${html}</div>`;
  hydrateIcons(dlg);
  dlg.onclick = (e) => { if (e.target === dlg) dlg.close(); };
  $$("[data-close]", dlg).forEach((b) => (b.onclick = () => dlg.close()));
  onMount && onMount(dlg);
  if (!dlg.open) dlg.showModal();
  return dlg;
}
const closeDialog = () => $("#dialog").close();

function confirmDialog({ icon: ic = "warning", title, text, ok = "OK", cancel = "Отмена", danger = false }) {
  return new Promise((resolve) => {
    openDialog(`
      <span class="dlg-icon">${icon(ic)}</span>
      <h2 class="headline-s" style="text-align:center">${esc(title)}</h2>
      <div class="body-m muted">${text}</div>
      <div class="dlg-actions">
        <button class="btn text" data-r="0">${esc(cancel)}</button>
        <button class="btn ${danger ? "error" : "filled"}" data-r="1">${esc(ok)}</button>
      </div>`, (dlg) => {
      $$("[data-r]", dlg).forEach((b) => (b.onclick = () => { dlg.close(); resolve(b.dataset.r === "1"); }));
      dlg.onclose = () => resolve(false);
    });
  });
}

let menuOnClose = null;
function showMenu(anchor, items, onClose, opts = {}) {
  closeMenu();
  menuOnClose = onClose || null;
  const m = document.createElement("div");
  m.className = "menu";
  m.innerHTML = items.map((it, i) => it === "-" ? "<hr>" : `<button class="sl ${it.danger ? "danger" : ""} ${it.selected ? "selected" : ""}" data-i="${i}">${icon(it.icon)}${esc(it.label)}</button>`).join("");
  document.body.appendChild(m);
  const r = anchor.getBoundingClientRect();
  if (opts.matchWidth) m.style.minWidth = `${r.width}px`;
  const mh = m.offsetHeight, mw = m.offsetWidth;
  m.style.top = `${Math.max(8, Math.min(r.bottom + 4, innerHeight - mh - 8))}px`;
  m.style.left = opts.matchWidth
    ? `${Math.max(8, Math.min(r.left, innerWidth - mw - 8))}px`
    : `${Math.min(r.right - mw, innerWidth - mw - 8)}px`;
  // Внутри открытого <dialog> меню должно быть поверх него (top layer).
  const host = anchor.closest("dialog[open]");
  if (host) host.appendChild(m);
  m.onclick = (e) => {
    const b = e.target.closest("button[data-i]");
    if (b) { closeMenu(); items[+b.dataset.i].run(); }
  };
  setTimeout(() => document.addEventListener("click", closeMenu, { once: true }), 0);
}
// Выпадающий список в стиле MD3 вместо нативного <select> (его попап рисует
// система своими цветами — на Linux GTK он не в теме приложения).
function mdSelect(sel) {
  sel.classList.add("md-native");
  let btn = sel.nextElementSibling;
  if (!btn || !btn.classList.contains("md-select")) {
    btn = document.createElement("button");
    btn.type = "button";
    btn.className = "md-select";
    btn.innerHTML = `<span class="v"></span>${icon("drop")}`;
    sel.after(btn);
    btn.onclick = (e) => {
      e.stopPropagation();
      btn.classList.add("open");
      showMenu(btn, [...sel.options].map((o, i) => ({
        icon: i === sel.selectedIndex ? "check" : "blank",
        label: o.text,
        selected: i === sel.selectedIndex,
        run: () => { sel.selectedIndex = i; sync(); sel.dispatchEvent(new Event("change")); },
      })), () => btn.classList.remove("open"), { matchWidth: true });
    };
  }
  const sync = () => { $(".v", btn).textContent = sel.options[sel.selectedIndex]?.text || ""; };
  sync();
  btn.disabled = sel.disabled || !sel.options.length;
}

function closeMenu() {
  $$(".menu").forEach((m) => m.remove());
  if (menuOnClose) { const f = menuOnClose; menuOnClose = null; f(); }
}

// ───────────── Навигация ─────────────

function go(view) {
  state.view = view;
  $$(".rail-item").forEach((b) => b.classList.toggle("active", b.dataset.view === view));
  $$(".view").forEach((v) => v.classList.toggle("active", v.id === `view-${view}`));
  if (view === "library") renderLibrary();
  if (view === "downloads") renderDownloads();
  if (view === "settings") renderSettings();
}

// ───────────── Каталог ─────────────

const CATALOGS = {
  freetp: { name: "FreeTP", search: "Поиск по играм FreeTP", loading: "Загружаю базу FreeTP…" },
  onlinefix: { name: "Online-Fix", search: "Поиск по играм Online-Fix", loading: "Загружаю базу Online-Fix… (первый раз до 30 секунд)" },
};

function groupCatalog(entries) {
  const map = new Map();
  for (const e of entries) {
    const key = e.title.trim().toLowerCase();
    if (!map.has(key)) map.set(key, { title: e.title, versions: [] });
    map.get(key).versions.push(e);
  }
  return [...map.values()].map((g) => {
    g.versions.sort((a, b) => (b.uploadDate || "").localeCompare(a.uploadDate || ""));
    g.latest = g.versions[0];
    g.search = (g.title + " " + g.versions.map((v) => v.release).join(" ")).toLowerCase();
    return g;
  });
}

async function fetchCatalog(cat, refresh) {
  const resp = await invoke("get_catalog", { refresh, catalog: cat });
  state.catalogs[cat] = groupCatalog(resp.entries);
  state.catMeta[cat] = { origin: resp.origin, newest: resp.newest };
  const badge = $(`#tab-count-${cat}`);
  if (badge) badge.textContent = state.catalogs[cat].length;
  return state.catalogs[cat];
}

async function loadCatalog(refresh = false) {
  const cat = state.cat;
  renderTabs();
  $("#catalog-grid").innerHTML = "";
  if (refresh || !state.catalogs[cat]) {
    $("#catalog-loading").classList.remove("hidden");
    $("#catalog-loading .body-l").textContent = CATALOGS[cat].loading;
  }
  try {
    const list = refresh || !state.catalogs[cat] ? await fetchCatalog(cat, refresh) : state.catalogs[cat];
    if (cat !== state.cat) return; // пока грузили, переключили вкладку
    state.catalog = list;
    renderTabs();
    rankCatalog();
    applyFilter();
    if (!state.popular) loadPopular();
    // Для «популярных» в Online-Fix нужен рейтинг FreeTP (сопоставляем по названию).
    if (cat === "onlinefix" && !state.catalogs.freetp) fetchCatalog("freetp", false).then(() => { rankCatalog(); applyFilter(); }).catch(() => {});
  } catch (e) {
    if (cat === state.cat) $("#catalog-grid").innerHTML = `<div class="empty" style="grid-column:1/-1">${icon("error")}<span class="body-l">${esc(e)}</span></div>`;
  } finally {
    if (cat === state.cat) $("#catalog-loading").classList.add("hidden");
  }
}

function renderTabs() {
  const cat = state.cat;
  $$("#catalog-tabs .tab").forEach((t) => {
    const on = t.dataset.catalog === cat;
    t.classList.toggle("active", on);
    t.setAttribute("aria-selected", on);
  });
  $$(".freetp-only").forEach((c) => c.classList.toggle("hidden", cat !== "freetp"));
  $("#search").placeholder = t(CATALOGS[cat].search);
  const m = state.catMeta[cat];
  $("#catalog-meta").innerHTML = m
    ? `${icon("info")}<span>База ${esc(CATALOGS[cat].name)}: ${esc(m.origin)} · последняя раздача ${fmtDate(m.newest)}</span>`
    : "";
}

function switchCatalog(cat) {
  if (cat === state.cat) return;
  state.cat = cat;
  localStorage.setItem("catalogTab", cat);
  if (cat !== "freetp" && (state.filter === "ready" || state.filter === "addon")) {
    state.filter = "all";
    $$("#catalog-chips .chip").forEach((x) => x.classList.toggle("selected", x.dataset.filter === "all"));
  }
  $("#catalog-content").scrollTop = 0;
  loadCatalog(false);
}

function applyFilter() {
  const q = state.query.trim().toLowerCase();
  const monthAgo = new Date(Date.now() - 45 * 864e5).toISOString();
  let list = state.catalog.filter((g) => {
    if (q && !q.split(/\s+/).every((w) => g.search.includes(w))) return false;
    if (state.filter === "ready") return g.versions.some((v) => v.installKind === 2);
    if (state.filter === "addon") return g.versions.some((v) => v.installKind === 1);
    if (state.filter === "new") return (g.latest.uploadDate || "") > monthAgo;
    if (state.filter === "top") return g.rank && g.rank <= 100;
    return true;
  });
  const byDate = (a, b) => (b.latest.uploadDate || "").localeCompare(a.latest.uploadDate || "");
  const sorters = {
    popular: (a, b) => (a.rank || Infinity) - (b.rank || Infinity) || byDate(a, b),
    new: byDate,
    "size-desc": (a, b) => sizeBytes(b.latest.fileSize) - sizeBytes(a.latest.fileSize),
    "size-asc": (a, b) => sizeBytes(a.latest.fileSize) - sizeBytes(b.latest.fileSize),
    az: (a, b) => a.title.localeCompare(b.title, I18N.locale),
  };
  // Топ-100 всегда по месту в рейтинге.
  list = list.sort(state.filter === "top" ? sorters.popular : sorters[state.sort] || sorters.popular);
  state.filtered = list;
  state.shown = 0;
  $("#catalog-grid").innerHTML = "";
  $("#catalog-count").textContent = t("{0} игр", [list.length]);
  if (!list.length && state.catalog.length) {
    $("#catalog-grid").innerHTML = `<div class="empty" style="grid-column:1/-1">${icon("search")}<span class="body-l">Ничего не найдено</span></div>`;
  }
  renderMore();
}

function renderMore() {
  const grid = $("#catalog-grid");
  const next = state.filtered.slice(state.shown, state.shown + 60);
  const owned = new Set(state.library.map((g) => g.infoHash));
  grid.insertAdjacentHTML("beforeend", next.map((g, i) => {
    const idx = state.shown + i;
    const v = g.latest;
    const have = g.versions.some((x) => owned.has(x.infoHash));
    return `<button class="card sl" data-idx="${idx}">
      ${coverHTML(g.title, kindTag(v.installKind) + (g.rank && g.rank <= 100 && (state.sort === "popular" || state.filter === "top") ? `<span class="rank">#${g.rank}</span>` : ""), v.articleUrl)}
      <div class="body">
        <div class="title-m" title="${esc(g.title)}">${esc(g.title)}</div>
        <div class="meta body-s muted">
          <span>${esc(v.fileSize)}</span><span>·</span><span>${fmtDate(v.uploadDate)}</span>
          ${g.versions.length > 1 ? `<span>·</span><span>${g.versions.length} версии</span>` : ""}
        </div>
        ${have ? `<div class="label-m" style="color:var(--md-primary)">В библиотеке</div>` : ""}
      </div>
    </button>`;
  }).join(""));
  state.shown += next.length;
  observeCovers(grid);
}

function openCatalogGame(g) {
  let sel = 0;
  const html = `
    <div class="hero">${coverHTML(g.title, kindTag(g.latest.installKind), g.latest.articleUrl)}</div>
    <h2 class="headline-s">${esc(g.title)}</h2>
    <div class="scroll">
      <div class="body-m muted">${g.latest.source === "onlinefix"
        ? "Раздача Online-Fix: фикс для онлайна уже внутри."
        : g.latest.installKind === 1
        ? "Фикс для онлайна скачается отдельно и установится автоматически после выбора exe."
        : g.latest.installKind === 2 ? "Онлайн-фикс уже вшит в раздачу." : "Обычная раздача FreeTP."}
        Игра ставится в отдельный префикс Proton и запускается через Spacewar (480) в Steam.</div>
      <div class="title-s">Раздача</div>
      <div class="radio-list">
        ${g.versions.map((v, i) => `
          <label class="radio-item sl">
            <input type="radio" name="ver" value="${i}" ${i === 0 ? "checked" : ""}/>
            <div class="grow">
              <div class="body-l">${esc(v.release)}</div>
              <div class="body-s muted">${esc(v.fileSize)} · ${fmtDate(v.uploadDate)}${(() => { const n = (v.addons || []).filter(isFix).length; return n > 1 ? ` · ${n} варианта фикса` : n ? ` · фикс: ${esc(v.addons.find(isFix).title)}` : ""; })()}</div>
            </div>
          </label>`).join("")}
      </div>
      <div class="title-s">Куда установить</div>
      <div class="field-row">
        <div class="field">
          <input id="dlg-dir" value="${esc(state.settings?.gamesDir || "")}" spellcheck="false"/>
        </div>
        <button class="btn outlined" id="dlg-browse">Обзор</button>
      </div>
      <div class="body-s muted">Внутри будет создана папка игры: раздача, префикс Proton и сама игра.</div>
    </div>
    <div class="dlg-actions">
      ${g.latest.articleUrl ? `<button class="btn text has-icon" id="dlg-article">${icon("open")}Страница игры</button>` : ""}
      <span style="flex:1"></span>
      <button class="btn text" data-close>Отмена</button>
      <button class="btn filled has-icon" id="dlg-download">${icon("download")}Скачать</button>
    </div>`;
  openDialog(html, (dlg) => {
    observeCovers(dlg);
    $$("input[name=ver]", dlg).forEach((r) => (r.onchange = () => (sel = +r.value)));
    const art = $("#dlg-article", dlg);
    if (art) art.onclick = () => invoke("open_url", { url: g.versions[sel].articleUrl || g.latest.articleUrl }).catch(fail);
    $("#dlg-browse", dlg).onclick = async () => {
      if (!T) return;
      const d = await T.dialog.open({ directory: true, defaultPath: $("#dlg-dir", dlg).value || state.settings?.gamesDir });
      if (d) $("#dlg-dir", dlg).value = d;
    };
    $("#dlg-download", dlg).onclick = async () => {
      const entry = g.versions[sel];
      try {
        const dir = $("#dlg-dir", dlg).value.trim() || null;
        await invoke("add_game", { entry, dir });
        closeDialog();
        snack(`«${g.title}» добавлена — ищу раздачу`, "Загрузки", () => go("downloads"));
        await refreshLibrary();
      } catch (e) { fail(e); }
    };
  });
}

// ───────────── Библиотека ─────────────

async function refreshLibrary() {
  try {
    state.library = await invoke("library");
    state.running = new Set(await invoke("running_games"));
  } catch (e) { fail(e); }
  if (state.view === "library") renderLibrary();
  if (state.view === "downloads") renderDownloads();
}

const STATUS = {
  downloading: ["Скачивается", ""],
  downloaded: ["Скачано — нужна установка", "warn"],
  installing: ["Установка…", "warn"],
  needsExe: ["Выберите exe игры", "warn"],
  installed: ["Готово", ""],
};

// «Bodycam-Multiplayer-Fix-Online-v6.1.exe» → «v6.1»; «…Fix-Online-Lan.exe» → «Online-Lan».
function shortFix(t) {
  const s = t.replace(/\.exe$/i, "").trim();
  const v = /v\d+(\.\d+)*\w*$/i.exec(s);
  if (v) return v[0];
  const m = /fix[\s_-]+(.+)$/i.exec(s);
  return m ? m[1].replace(/[_]/g, " ") : "установлен";
}

// Три шага: скачана → установлена → онлайн-фикс. ok / no / wait / none.
function gameChecks(g) {
  const done = g.status !== "downloading";
  const inst = g.status === "installed";
  let fix;
  if (g.fixDetected || g.addonsApplied) fix = ["ok", g.addonApplied ? `Онлайн-фикс · ${shortFix(g.addonApplied)}` : "Онлайн-фикс"];
  else if (g.installKind === 2) fix = ["ok", "Фикс вшит в раздачу"];
  else if (g.addons?.length) fix = ["no", "Онлайн-фикс не установлен"];
  else fix = ["none", "Фикса нет в базе", "info"];
  return [
    done ? ["ok", "Скачана"] : ["wait", "Скачивается"],
    inst ? ["ok", "Установлена"] : g.status === "installing" ? ["wait", "Устанавливается"] : ["none", "Не установлена"],
    fix,
  ];
}

function checksHTML(g, upd) {
  const ic = { ok: "check", no: "close", wait: "download", none: "close" };
  const items = gameChecks(g);
  if (upd?.kind === "fix") items[2] = ["new", `Новый фикс · ${shortFix(upd.fixTitle)}`, "refresh"];
  return `<div class="checks">${items.map(([st, text, ico], i) => {
    const clickable = i === 2 && (st === "no" || st === "new") && g.exe;
    return `<span class="check ${st}${clickable ? " sl" : ""}" ${clickable ? `data-act="fix" title="Установить фикс"` : ""}>${icon(ico || ic[st])}${esc(text)}</span>`;
  }).join("")}</div>`;
}

function renderLibrary() {
  const list = $("#library-list");
  $("#steam-banner").classList.toggle("hidden", !state.system || state.system.steamRunning || !state.library.length);
  if (!state.library.length) {
    list.innerHTML = `<div class="empty">${icon("games")}<span class="title-l">Библиотека пуста</span>
      <span class="body-m">Найдите игру в каталоге и нажмите «Скачать»</span>
      <button class="btn tonal" onclick="go('catalog')">Открыть каталог</button></div>`;
    return;
  }
  const games = [...state.library].sort((a, b) => (b.lastPlayed || b.addedAt) - (a.lastPlayed || a.addedAt));
  list.innerHTML = games.map((g) => {
    const running = state.running.has(g.id);
    let [label, cls] = STATUS[g.status] || [g.status, ""];
    if (running) [label, cls] = [g.status === "installed" ? "Запущена" : "Установщик открыт", "run"];
    if (g.updateFrom && g.status === "downloading") label = "Обновляется";
    if (g.updateFrom && g.status === "downloaded") label = "Обновление скачано — нажмите «Установить»";
    if (g.lastError) [label, cls] = ["Ошибка", "err"];
    const upd = state.updates.get(g.id);
    const dl = state.downloads[g.id];
    const pct = dl && dl.totalBytes ? (dl.progressBytes / dl.totalBytes) * 100 : 0;

    let primary = "";
    if (running) primary = `<button class="btn tonal has-icon" data-act="stop">${icon("stop")}Остановить</button>`;
    else if (g.status === "installed") primary = `<button class="btn filled big has-icon" data-act="play">${icon("play")}Играть</button>`;
    else if (g.status === "downloaded") primary = `<button class="btn filled has-icon" data-act="install">${icon("install")}Установить</button>`;
    else if (g.status === "needsExe") primary = `<button class="btn filled has-icon" data-act="exe">${icon("exe")}Выбрать exe</button>`;
    else if (g.status === "downloading") primary = `<button class="btn tonal" data-act="downloads">${pct.toFixed(0)}%</button>`;

    return `<div class="lib-item" data-id="${esc(g.id)}">
      ${coverHTML(g.title, "", g.articleUrl)}
      <div class="info">
        <div class="title-l" title="${esc(g.title)}">${esc(g.title)}</div>
        <div class="body-s muted">${g.source === "onlinefix" ? "Online-Fix" : "FreeTP"} · ${g.updateFrom ? `${esc(g.updateFrom)} → ` : ""}${esc(g.release)}</div>
        <span class="status ${cls}"><span class="dot"></span>${esc(label)}</span>
        ${checksHTML(g, upd)}
        ${upd?.kind === "game" && !running ? `<div class="update-banner">
          <span class="ico">${icon("download")}</span>
          <div class="grow"><div class="label-l">Доступно обновление</div>
            <div class="body-s">${esc(upd.entry.release)} · ${esc(upd.entry.fileSize)} · ${fmtDate(upd.entry.uploadDate)}</div></div>
          <button class="btn tonal has-icon" data-act="update">${icon("refresh")}Обновить</button>
        </div>` : ""}
        ${g.status === "downloading" ? `<div class="progress"><i style="width:${pct}%"></i></div>` : ""}
        ${g.lastError ? `<div class="body-s" style="color:var(--md-error)">${esc(g.lastError)}</div>` : ""}
        ${g.playtimeSecs ? `<div class="body-s muted">Наиграно: ${fmtTime(g.playtimeSecs)}</div>` : ""}
      </div>
      <div class="actions">${primary}<button class="icon-btn sl" data-act="menu">${icon("more")}</button></div>
    </div>`;
  }).join("");
  observeCovers(list);
}

// ───────────── Обновление самого лаунчера ─────────────

let appUpdate = null;

async function checkAppUpdate(manual = false) {
  const btn = $("#app-update-check");
  if (manual && btn) btn.disabled = true;
  try {
    appUpdate = await invoke("check_app_update");
    renderAppUpdate();
    if (appUpdate) snack(`Доступна новая версия LinFix ${appUpdate.version}`, "Подробнее", showAppUpdate);
    else if (manual) snack("У вас последняя версия LinFix");
  } catch (e) {
    if (manual) snack(`Не удалось проверить обновления: ${e}`, "Релизы", () => invoke("open_url", { url: `${REPO_URL}/releases` }));
  }
  if (btn) btn.disabled = false;
}

function renderAppUpdate() {
  const d = $("#app-update-desc"), b = $("#app-update-check");
  if (!d || !b) return;
  if (appUpdate) {
    d.textContent = t("Доступна версия {0}", [appUpdate.version + (appUpdate.date ? ` · ${fmtDate(appUpdate.date)}` : "")]);
    b.lastChild.textContent = t("Обновить");
    b.onclick = showAppUpdate;
  } else {
    d.textContent = t("Обновления приходят с GitHub Releases");
    b.lastChild.textContent = t("Проверить обновления");
    b.onclick = () => checkAppUpdate(true);
  }
}

async function showAppUpdate() {
  if (!appUpdate) return;
  const ok = await confirmDialog({
    icon: "download",
    title: `LinFix ${appUpdate.version}`,
    text: `${esc(t("Сейчас у вас {0}.", [appUpdate.current]))}${appUpdate.notes
      ? `<div class="release-notes">${esc(appUpdate.notes)}</div>` : ""}
      После установки лаунчер перезапустится. Загрузки игр продолжатся сами.`,
    ok: "Обновить и перезапустить",
  });
  if (!ok) return;
  snack("Скачиваю обновление LinFix…");
  try { await invoke("install_app_update"); } catch (e) { snack(String(e), "Релизы", () => invoke("open_url", { url: `${REPO_URL}/releases` })); }
}

// ───────────── Обновления ─────────────

async function checkUpdates() {
  try {
    const list = await invoke("check_updates");
    state.updates = new Map(list.map((u) => [u.id, u]));
  } catch (e) {
    console.warn("check_updates:", e);
  }
  const n = state.updates.size;
  $("#lib-badge").textContent = n;
  $("#lib-badge").classList.toggle("hidden", !n);
  if (state.view === "library") renderLibrary();
  // Свежий список фиксов мог подтянуться в библиотеку.
  try { state.library = await invoke("library"); } catch {}
  if (state.view === "library") renderLibrary();
}

async function updateGame(g) {
  const u = state.updates.get(g.id);
  if (!u?.entry) return;
  const portable = g.exe && g.exe.replaceAll("\\", "/").includes("/download/");
  const ok = await confirmDialog({
    icon: "download",
    title: `Обновить «${g.title}»?`,
    text: `${esc(g.release)} → <b>${esc(u.entry.release)}</b> (${esc(u.entry.fileSize)}, ${fmtDate(u.entry.uploadDate)}).<br><br>
      Новая раздача скачается в ту же папку: совпадающие файлы не качаются заново, сохранения и настройки в префиксе останутся.<br><br>
      ${portable ? "Фикс поставится заново автоматически." : "После загрузки нажмите «Установить» — установщик обновит игру, затем фикс поставится сам."}`,
    ok: "Обновить",
  });
  if (!ok) return;
  try {
    await invoke("start_game_update", { id: g.id, entry: u.entry });
    state.updates.delete(g.id);
    $("#lib-badge").textContent = state.updates.size;
    $("#lib-badge").classList.toggle("hidden", !state.updates.size);
    snack(`Обновляю «${g.title}»`, "Загрузки", () => go("downloads"));
  } catch (e) { fail(e); }
  refreshLibrary();
}

async function onLibraryAction(id, act, anchor) {
  const g = state.library.find((x) => x.id === id);
  if (!g) return;
  try {
    switch (act) {
      case "play": return await playGame(g);
      case "stop": await invoke("stop_game", { id }); break;
      case "install": return await installGame(g);
      case "exe": return await pickExe(g);
      case "downloads": return go("downloads");
      case "fix": return await applyFix(g);
      case "update": return await updateGame(g);
      case "menu": return showMenu(anchor, gameMenu(g));
    }
  } catch (e) { fail(e); }
  refreshLibrary();
}

function gameMenu(g) {
  const items = [
    { icon: "tune", label: "Параметры запуска", run: () => gameSettings(g) },
    { icon: "exe", label: "Сменить exe", run: () => pickExe(g) },
  ];
  if (["downloaded", "needsExe", "installed"].includes(g.status))
    items.push({ icon: "install", label: "Запустить установщик снова", run: () => installGame(g) });
  if (g.addons?.length && g.exe)
    items.push({ icon: "wrench", label: g.addonsApplied ? "Сменить или переустановить фикс…" : "Установить фикс…", run: () => applyFix(g) });
  items.push("-",
    { icon: "folder", label: "Папка игры", run: () => invoke("open_dir", { id: g.id, which: g.exe ? "exe" : "root" }).catch(fail) },
    { icon: "folder", label: "Префикс (drive_c)", run: () => invoke("open_dir", { id: g.id, which: "prefix" }).catch(fail) });
  if (g.articleUrl) items.push({ icon: "open", label: "Страница на FreeTP", run: () => invoke("open_url", { url: g.articleUrl }).catch(fail) });
  items.push("-", { icon: "delete", label: "Удалить", danger: true, run: () => removeGame(g) });
  return items;
}

async function playGame(g) {
  if (g.spacewar) {
    const sys = await invoke("system_status").catch(() => null);
    state.system = sys;
    if (sys && !sys.steamRunning) {
      const start = await confirmDialog({
        icon: "steam", title: "Steam не запущен",
        text: "Онлайн-фикс подключается к Steam через Spacewar (AppID 480). Без запущенного Steam мультиплеер работать не будет.",
        ok: "Запустить Steam", cancel: "Играть без Steam",
      });
      if (start) {
        await invoke("start_steam").catch(fail);
        snack("Steam запускается — игра стартует через 10 секунд");
        await new Promise((r) => setTimeout(r, 10000));
      }
    }
  }
  await invoke("launch_game", { id: g.id });
  snack(`Запускаю «${g.title}»…`);
  refreshLibrary();
}

async function installGame(g) {
  snack("Готовлю установку…");
  const kind = await invoke("install_game", { id: g.id });
  if (kind === "installer") {
    snack("Установщик открыт — пройдите его как обычно. Путь установки можно оставить по умолчанию.");
  } else {
    snack("Установщик не найден — игра портативная, выберите exe");
    await pickExe(g);
  }
  refreshLibrary();
}

async function pickExe(g) {
  const list = await invoke("find_executables", { id: g.id });
  openDialog(`
    <span class="dlg-icon">${icon("exe")}</span>
    <h2 class="headline-s" style="text-align:center">Какой файл запускает игру?</h2>
    <div class="body-m muted">Найденные exe — самые вероятные сверху. Обычно это exe с названием игры (у UE-игр — *-Win64-Shipping.exe).</div>
    ${list.length ? `<div class="radio-list">${list.map((p, i) => {
      const name = p.split("/").pop();
      return `<label class="radio-item sl"><input type="radio" name="exe" value="${i}" ${i === 0 ? "checked" : ""}/>
        <div class="grow"><div class="body-l">${esc(name)}</div><div class="body-s muted">${esc(p)}</div></div></label>`;
    }).join("")}</div>` : `<div class="body-m">Exe не найдены автоматически — укажите вручную.</div>`}
    <div class="dlg-actions">
      <button class="btn text" id="exe-manual">Выбрать вручную…</button>
      <span style="flex:1"></span>
      <button class="btn text" data-close>Отмена</button>
      <button class="btn filled" id="exe-ok" ${list.length ? "" : "disabled"}>Готово</button>
    </div>`, (dlg) => {
    const done = async (path) => {
      closeDialog();
      snack(g.addons?.length && !g.addonsApplied ? "Сохраняю и ставлю фикс…" : "Сохраняю…");
      try { snack(await invoke("set_executable", { id: g.id, path })); } catch (e) { fail(e); }
      refreshLibrary();
    };
    $("#exe-ok", dlg).onclick = () => {
      const r = $("input[name=exe]:checked", dlg);
      if (r) done(list[+r.value]);
    };
    $("#exe-manual", dlg).onclick = async () => {
      if (!T) return;
      const path = await T.dialog.open({ multiple: false, directory: false, defaultPath: g.dir, filters: [{ name: "Windows exe", extensions: ["exe", "EXE"] }] });
      if (path) done(path);
    };
  });
}

// Такой же отбор, как Addon::is_fix в Rust: прямая ссылка, не мод-пак, не optional.
const isFix = (a) => !a.optional && a.delivery !== "magnet" && /^https?:/.test(a.uris?.[0] || "") && !/mod pack/i.test(a.title || "");

async function applyFix(g) {
  const fixes = (g.addons || []).map((a, i) => ({ ...a, i })).filter(isFix);
  const extras = (g.addons || []).filter((a) => !isFix(a));
  if (!fixes.length) {
    snack("Для игры нет фикса с прямой ссылкой", g.articleUrl ? "Открыть страницу" : null, () => invoke("open_url", { url: g.articleUrl }));
    return;
  }
  // Варианты фикса не суммируются (новая версия / старая, Steam / LAN) — ставится один.
  const index = fixes.length === 1 && g.installKind !== 2 ? fixes[0].i : await chooseFix(g, fixes, extras);
  if (index == null) return;
  snack("Скачиваю фикс… Если откроется окно сайта — подождите ~30 секунд, кнопка нажмётся сама.");
  try {
    const n = await invoke("apply_addons", { id: g.id, index });
    snack(`Фикс установлен (${n} файлов)`);
  } catch (e) {
    snack(String(e), g.articleUrl ? "Открыть страницу" : null, () => invoke("open_url", { url: g.articleUrl }));
  }
  refreshLibrary();
}

function chooseFix(g, fixes, extras) {
  return new Promise((resolve) => {
    let sel = fixes.findIndex((f) => f.title === g.addonApplied);
    if (sel < 0) sel = 0;
    let done = false;
    const finish = (v) => { if (!done) { done = true; resolve(v); } };
    const dlg = openDialog(`
      <span class="dlg-icon">${icon("wrench")}</span>
      <h2 class="headline-s">Онлайн-фикс</h2>
      <div class="body-m muted">${g.installKind === 2
        ? "Фикс уже вшит в раздачу. Ставьте отдельный, только если онлайн не работает или нужна другая версия."
        : "Ставится один вариант: новый заменит файлы предыдущего. Обычно сверху самая новая версия."}</div>
      <div class="radio-list">
        ${fixes.map((f, k) => `
          <label class="radio-item sl">
            <input type="radio" name="fix" value="${k}" ${k === sel ? "checked" : ""}/>
            <div class="grow">
              <div class="body-l">${esc(f.title)}</div>
              <div class="body-s muted">${f.title === g.addonApplied ? "Установлен сейчас" : f.fileSize ? esc(f.fileSize) : "freetp.org"}</div>
            </div>
          </label>`).join("")}
      </div>
      ${extras.length ? `<div class="body-s muted">Ещё есть дополнения, которые ставятся вручную со страницы игры: ${extras.map((a) => esc(a.title)).join(", ")}.</div>` : ""}
      <div class="dlg-actions">
        <button class="btn text" data-close>Отмена</button>
        <button class="btn filled has-icon" id="fix-ok">${icon("download")}Установить</button>
      </div>`, (d) => {
      $$("input[name=fix]", d).forEach((r) => (r.onchange = () => (sel = +r.value)));
      $("#fix-ok", d).onclick = () => { finish(fixes[sel].i); closeDialog(); };
    });
    dlg.addEventListener("close", () => finish(null), { once: true });
  });
}

async function removeGame(g) {
  openDialog(`
    <span class="dlg-icon">${icon("delete")}</span>
    <h2 class="headline-s" style="text-align:center">Удалить «${esc(g.title)}»?</h2>
    <label class="radio-item sl" style="margin:0 -24px"><input type="checkbox" id="rm-files" checked style="accent-color:var(--md-primary);width:18px;height:18px"/>
      <div class="grow"><div class="body-l">Удалить файлы с диска</div><div class="body-s muted">${esc(g.dir)} — раздача, префикс и сохранения</div></div></label>
    <div class="dlg-actions"><button class="btn text" data-close>Отмена</button><button class="btn error" id="rm-ok">Удалить</button></div>`, (dlg) => {
    $("#rm-ok", dlg).onclick = async () => {
      const deleteFiles = $("#rm-files", dlg).checked;
      closeDialog();
      try { await invoke("remove_game", { id: g.id, deleteFiles }); snack("Удалено"); } catch (e) { fail(e); }
      refreshLibrary();
    };
  });
}

async function gameSettings(g) {
  const protons = await invoke("list_protons").catch(() => []);
  openDialog(`
    <h2 class="headline-s">Параметры: ${esc(g.title)}</h2>
    <div class="scroll">
      <div class="field"><label>Название</label><input id="gs-title" value="${esc(g.title)}"/></div>
      <div class="row" style="padding:0">
        <div class="grow"><div class="body-l">Онлайн через Steam (Spacewar 480)</div>
          <div class="body-s muted">SteamAppId=480 + DLL overrides Online-Fix</div></div>
        <label class="switch"><input type="checkbox" id="gs-spacewar" ${g.spacewar ? "checked" : ""}/><span></span></label>
      </div>
      <div class="field"><label>Proton</label><select id="gs-proton" class="md-native">
        <option value="">Как в настройках</option>
        ${protons.map((p) => `<option ${g.proton === p.name ? "selected" : ""}>${esc(p.name)}</option>`).join("")}
      </select></div>
      <div class="field"><label>Аргументы запуска</label><input id="gs-args" value="${esc(g.launchArgs)}" placeholder="-dx11 -windowed"/></div>
      <div class="field"><label>Переменные окружения</label><input id="gs-env" value="${esc(g.env)}" placeholder="DXVK_HUD=fps PROTON_USE_WINED3D=1"/></div>
      <div class="field"><label>WINEDLLOVERRIDES</label><textarea id="gs-dll" placeholder="OnlineFix64=n;SteamOverlay64=n;winmm=n,b;dnet=n;steam_api64=n,b;winhttp=n,b">${esc(g.dllOverrides)}</textarea>
        <span class="hint">Пусто — стандартный набор для Online-Fix</span></div>
      <dl class="kv body-s">
        <dt>exe</dt><dd style="word-break:break-all">${esc(g.exe || "—")}</dd>
        <dt>Префикс</dt><dd style="word-break:break-all">${esc(g.prefixDir || `${g.dir}/prefix`)}</dd>
      </dl>
    </div>
    <div class="dlg-actions"><button class="btn text" data-close>Отмена</button><button class="btn filled" id="gs-save">Сохранить</button></div>`, (dlg) => {
    mdSelect($("#gs-proton", dlg));
    $("#gs-save", dlg).onclick = async () => {
      const patch = {
        title: $("#gs-title", dlg).value,
        spacewar: $("#gs-spacewar", dlg).checked,
        proton: $("#gs-proton", dlg).value,
        launchArgs: $("#gs-args", dlg).value,
        env: $("#gs-env", dlg).value,
        dllOverrides: $("#gs-dll", dlg).value,
      };
      try { await invoke("update_game", { id: g.id, patch }); closeDialog(); snack("Сохранено"); } catch (e) { fail(e); }
      refreshLibrary();
    };
  });
}

// ───────────── Загрузки ─────────────

async function pollDownloads() {
  try {
    const list = await invoke("downloads");
    // Сглаживаем скорость (EMA), чтобы цифры не прыгали каждую секунду.
    const prev = state.downloads;
    for (const d of list) {
      const old = prev[d.id]?.smooth;
      d.smooth = old == null || d.state === "paused" ? d.speedBps : old * 0.8 + d.speedBps * 0.2;
      const left = Math.max(0, d.totalBytes - d.progressBytes);
      d.etaSmooth = d.smooth > 1024 ? Math.round(left / d.smooth) : null;
    }
    state.downloads = Object.fromEntries(list.map((d) => [d.id, d]));
    const active = list.length;
    $("#dl-badge").textContent = active;
    $("#dl-badge").classList.toggle("hidden", !active);
    if (state.view === "downloads") renderDownloads();
    if (state.view === "library" && active) {
      // Обновляем только прогресс, не перерисовывая всё.
      for (const d of list) {
        const row = $(`.lib-item[data-id="${CSS.escape(d.id)}"]`);
        if (!row) continue;
        const pct = d.totalBytes ? (d.progressBytes / d.totalBytes) * 100 : 0;
        const bar = $(".progress > i", row); if (bar) bar.style.width = `${pct}%`;
        const btn = $('[data-act="downloads"]', row); if (btn) btn.textContent = `${pct.toFixed(0)}%`;
      }
    }
  } catch { /* сессия ещё стартует */ }
}

function renderDownloads() {
  const box = $("#downloads-list");
  const games = state.library.filter((g) => g.status === "downloading");
  if (!games.length) {
    box.innerHTML = `<div class="empty">${icon("download")}<span class="title-l">Нет активных загрузок</span></div>`;
    return;
  }
  box.innerHTML = `<div class="list">${games.map((g) => {
    const d = state.downloads[g.id];
    const pct = d && d.totalBytes ? (d.progressBytes / d.totalBytes) * 100 : 0;
    const paused = d?.state === "paused";
    const init = !d || d.state === "initializing" || d.state === "metadata" || !d.totalBytes;
    const slow = d?.state === "metadata" && d.waitingSecs >= 60;
    const line = !d ? "Запуск торрент-клиента…"
      : d.state === "metadata" ? `Ищу пиров и получаю данные раздачи… ${fmtTime(d.waitingSecs)}${slow
          ? ` · <span style="color:var(--md-tertiary)">похоже, у раздачи нет сидов. Подождите или выберите другую версию в каталоге</span>` : ""}`
      : init ? "Проверяю файлы…"
      : `${t("{0} из {1}", [fmtBytes(d.progressBytes), fmtBytes(d.totalBytes)])} · ${paused ? t("пауза") : t("{0}/с · осталось {1}", [fmtBytes(d.smooth), fmtTime(d.etaSmooth)])}`;
    return `<div class="list-item" data-id="${esc(g.id)}">
      <div class="lead">${icon("download")}</div>
      <div class="grow">
        <div class="title-m">${esc(g.title)}</div>
        <div class="progress ${init ? "indet" : ""}"><i style="width:${pct}%"></i></div>
        <div class="body-s muted">${line}${d?.error ? ` · <span style="color:var(--md-error)">${esc(d.error)}</span>` : ""}</div>
      </div>
      <span class="label-l">${init ? "" : pct.toFixed(1) + "%"}</span>
      <button class="icon-btn sl" data-dl="${paused ? "resume" : "pause"}" title="${paused ? "Продолжить" : "Пауза"}">${icon(paused ? "play" : "pause")}</button>
      <button class="icon-btn sl" data-dl="remove" title="Отменить">${icon("close")}</button>
    </div>`;
  }).join("")}</div>`;
}

// ───────────── Настройки ─────────────

async function renderSettings() {
  try {
    const [settings, protons, sys] = await Promise.all([invoke("get_settings"), invoke("list_protons"), invoke("system_status")]);
    state.settings = settings; state.system = sys;
    $("#set-games-dir").value = settings.gamesDir;
    $("#set-limit").value = settings.downloadLimitMbps || 0;
    const win = sys.platform === "windows";
    $("#proton-section").classList.toggle("hidden", win);
    $("#set-umu").checked = settings.useUmu && sys.umu;
    $("#set-umu").disabled = !sys.umu;
    $("#umu-desc").textContent = sys.umu
      ? "umu-run найден: игры запускаются в контейнере Steam Runtime, с исправлениями protonfixes. Необязательно — без него игры идут через Proton напрямую"
      : "Необязательно. umu-run не установлен — чтобы включить, поставьте пакет umu-launcher из репозитория дистрибутива. Без него игры идут через Proton напрямую";
    $("#set-overlay").checked = settings.steamOverlay !== false;
    $("#set-log").checked = settings.protonLog;
    $("#set-proton").innerHTML = protons.length
      ? `<option value="">Автоматически (${esc(protons[0].name)})</option>` + protons.map((p) => `<option ${settings.proton === p.name ? "selected" : ""}>${esc(p.name)}</option>`).join("")
      : `<option value="">Proton не найден</option>`;
    mdSelect($("#set-proton"));
    const ges = protons.filter((p) => /ge-proton/i.test(p.name)).map((p) => p.name);
    $("#ge-desc").textContent = (ges.length ? `${t("Установлено: {0}", [ges.slice(0, 3).join(", ") + (ges.length > 3 ? "…" : "")])}. ` : `${t("Не установлен.")} `)
      + `${t("Лучше всего подходит для игр с онлайн-фиксом: работает без Steam Runtime и содержит больше исправлений для игр.")} `
      + t("При «Автоматически» берётся самая новая версия GE.");
    renderThemeSettings();
    renderLangSelect($("#set-lang"));

    const row = (ok, title, sub, action = "") => `<div class="row">
      <span style="color:${ok ? "var(--md-primary)" : "var(--md-error)"}">${icon(ok ? "ok" : "error")}</span>
      <div class="grow"><div class="body-l">${title}</div><div class="body-m muted">${sub}</div></div>${action}</div>`;
    $("#system-status").innerHTML =
      row(!!sys.steamRoot, "Steam", sys.steamRoot ? esc(sys.steamRoot) : "Не найден — установите Steam (нужен для онлайна)") +
      row(sys.steamRunning, sys.steamRunning ? "Steam запущен" : "Steam не запущен", "Для Spacewar (480) клиент Steam должен работать",
        sys.steamRunning ? "" : `<button class="btn tonal" id="sys-start-steam">Запустить</button>`) +
      (win
        ? row(true, "Proton не нужен", "На Windows игры и установщики запускаются напрямую")
        : row(sys.protons > 0, `Proton: ${sys.protons}`,
            sys.protons ? "Версии из Steam и compatibilitytools.d" : "Не найден — можно скачать GE-Proton автоматически",
            sys.protons ? "" : `<button class="btn tonal" id="sys-ge-proton">Скачать GE-Proton</button>`)) +
      row(!!sys.archiver, "Архиватор", sys.archiver ? esc(sys.archiver)
        : win ? "Не найден — установите 7-Zip или WinRAR (нужен для фиксов)" : "Установите p7zip (7z) — нужен для фиксов") +
      "";
    const ge = $("#sys-ge-proton");
    if (ge) ge.onclick = async () => {
      ge.disabled = true;
      ge.textContent = t("Скачивание…");
      try {
        const r = await invoke("install_ge_proton");
        snack(r.already ? `${r.tag} — уже последняя версия` : `${r.tag} установлен`);
      } catch (e) { fail(e); }
      renderSettings();
    };
    const b = $("#sys-start-steam");
    if (b) b.onclick = () => invoke("start_steam").then(() => { snack("Steam запускается…"); setTimeout(renderSettings, 8000); }).catch(fail);
  } catch (e) { fail(e); }
}

async function saveSettings(patch) {
  const s = { ...state.settings, ...patch };
  try {
    await invoke("save_settings", { settings: s });
    const sourceChanged = patch.source && patch.source !== state.settings.source;
    state.settings = s;
    if (sourceChanged) loadCatalog(false);
  } catch (e) { fail(e); }
}

// Список языков: названия на своём языке, выбранный — сверху как есть.
function renderLangSelect(sel) {
  if (!sel) return;
  sel.innerHTML = Object.entries(I18N.LANGS)
    .filter(([code]) => I18N.hasLocale(code))
    .map(([code, name]) => `<option value="${code}" ${code === I18N.lang ? "selected" : ""}>${esc(name)}</option>`)
    .join("");
  sel.onchange = () => { if (sel.value !== I18N.lang) I18N.setLang(sel.value); };
  mdSelect(sel);
}

function renderThemeSettings() {
  const { mode, seed } = MD3Theme.get();
  $$("#set-theme button").forEach((b) => b.classList.toggle("on", b.dataset.v === mode));
  const box = $("#set-seed");
  const custom = !MD3Theme.PRESETS.some((p) => p.hex === seed);
  box.innerHTML = MD3Theme.PRESETS.map((p) => {
    const sch = MD3Theme.scheme(p.hex, document.documentElement.dataset.theme === "dark");
    return `<button class="swatch sl ${p.hex === seed ? "on" : ""}" data-seed="${p.hex}" title="${esc(p.name)}"
      style="--sw-a:${sch.primary};--sw-b:${sch["secondary-container"]};--sw-c:${sch["tertiary"]}">${icon("check")}</button>`;
  }).join("") + `<label class="swatch custom sl ${custom ? "on" : ""}" title="Свой цвет" style="${custom ? `--sw-a:${seed};--sw-b:${seed};--sw-c:${seed}` : ""}">
      ${icon(custom ? "check" : "tune")}<input type="color" value="${esc(seed)}"></label>`;
  $$("button[data-seed]", box).forEach((b) => (b.onclick = () => { MD3Theme.setSeed(b.dataset.seed); renderThemeSettings(); }));
  const inp = $("input[type=color]", box);
  inp.oninput = () => MD3Theme.setSeed(inp.value);
  inp.onchange = () => renderThemeSettings();
}

// ───────────── Инициализация ─────────────

function bind() {
  $$(".rail-item").forEach((b) => (b.onclick = () => go(b.dataset.view)));

  let timer;
  $("#search").oninput = (e) => { clearTimeout(timer); timer = setTimeout(() => { state.query = e.target.value; applyFilter(); }, 150); };
  $("#refresh-catalog").onclick = (e) => { e.preventDefault(); loadCatalog(true).then(() => { loadPopular(true); checkUpdates(); }); };
  $$("#catalog-tabs .tab").forEach((t) => (t.onclick = () => switchCatalog(t.dataset.catalog)));
  const SORTS = { popular: "Популярные", new: "Новые", "size-desc": "Сначала большие", "size-asc": "Сначала маленькие", az: "По алфавиту" };
  const sortLabel = () => ($("#catalog-sort-label").textContent = SORTS[state.sort] || SORTS.popular);
  sortLabel();
  $("#catalog-sort").onclick = (e) => {
    e.stopPropagation();
    const btn = e.currentTarget;
    btn.classList.add("open");
    showMenu(btn, Object.entries(SORTS).map(([k, label]) => ({
      icon: k === state.sort ? "check" : "blank",
      label,
      selected: k === state.sort,
      run: () => { state.sort = k; localStorage.setItem("catalogSort", k); sortLabel(); applyFilter(); },
    })), () => btn.classList.remove("open"));
  };
  $$("#catalog-chips .chip").forEach((c) => (c.onclick = () => {
    $$("#catalog-chips .chip").forEach((x) => x.classList.toggle("selected", x === c));
    state.filter = c.dataset.filter; applyFilter();
    if (state.filter === "top" && !state.popular) snack("Загружаю рейтинг популярности…");
  }));
  $("#catalog-grid").onclick = (e) => {
    const card = e.target.closest(".card");
    if (card) openCatalogGame(state.filtered[+card.dataset.idx]);
  };
  new IntersectionObserver((en) => {
    if (en[0].isIntersecting && state.shown < state.filtered.length) renderMore();
  }, { root: $("#catalog-content"), rootMargin: "600px" }).observe($("#catalog-sentinel"));

  $("#library-list").onclick = (e) => {
    const b = e.target.closest("[data-act]");
    const item = e.target.closest(".lib-item");
    if (b && item) { e.stopPropagation(); onLibraryAction(item.dataset.id, b.dataset.act, b); }
  };
  $("#banner-start-steam").onclick = () => invoke("start_steam").then(() => {
    snack("Steam запускается…");
    setTimeout(async () => { state.system = await invoke("system_status"); renderLibrary(); }, 8000);
  }).catch(fail);

  $("#downloads-list").onclick = async (e) => {
    const b = e.target.closest("[data-dl]");
    const item = e.target.closest(".list-item");
    if (!b || !item) return;
    const id = item.dataset.id;
    try {
      if (b.dataset.dl === "pause") await invoke("pause_download", { id });
      if (b.dataset.dl === "resume") await invoke("resume_download", { id });
      if (b.dataset.dl === "remove") {
        const g = state.library.find((x) => x.id === id);
        return g && removeGame(g);
      }
      await pollDownloads();
    } catch (err) { fail(err); }
  };

  $("#set-games-dir").onchange = (e) => saveSettings({ gamesDir: e.target.value });
  $("#browse-games-dir").onclick = async () => {
    if (!T) return;
    const dir = await T.dialog.open({ directory: true, defaultPath: state.settings?.gamesDir });
    if (dir) { $("#set-games-dir").value = dir; saveSettings({ gamesDir: dir }); }
  };
  $("#set-limit").onchange = (e) => saveSettings({ downloadLimitMbps: Math.max(0, parseInt(e.target.value) || 0) });
  $("#set-umu").onchange = (e) => saveSettings({ useUmu: e.target.checked });
  $("#set-overlay").onchange = (e) => saveSettings({ steamOverlay: e.target.checked });
  $("#ge-download").onclick = async (e) => {
    const b = e.currentTarget;
    b.disabled = true;
    b.lastChild.textContent = t("Скачиваю…");
    try {
      const r = await invoke("install_ge_proton");
      snack(r.already ? `${r.tag} — у вас уже последняя версия` : `${r.tag} установлен и будет использоваться автоматически`);
    } catch (err) { fail(err); }
    b.disabled = false;
    b.lastChild.textContent = t("Скачать последнюю");
    renderSettings();
  };
  $("#set-log").onchange = (e) => saveSettings({ protonLog: e.target.checked });
  $("#set-proton").onchange = (e) => saveSettings({ proton: e.target.value || null });
  $$("#set-theme button").forEach((b) => (b.onclick = () => { MD3Theme.setMode(b.dataset.v); renderThemeSettings(); }));
  $("#open-wizard").onclick = () => Wizard.open();
  $$("[data-url]").forEach((a) => (a.onclick = (e) => { e.preventDefault(); invoke("open_url", { url: a.dataset.url }).catch(fail); }));

  listen("library-changed", () => refreshLibrary());
  listen("app-update-progress", (e) => snack(`Обновление LinFix: ${e.payload}%`));
  $("#open-repo").onclick = () => invoke("open_url", { url: REPO_URL }).catch(fail);
  renderAppUpdate();
  listen("toast", (e) => snack(e.payload));
  listen("installer-exited", async (e) => {
    await refreshLibrary();
    const g = state.library.find((x) => x.id === e.payload);
    if (g) { snack("Установщик закрыт — выберите exe игры"); pickExe(g); }
  });
  listen("game-exited", () => refreshLibrary());
}

async function init() {
  hydrateIcons();
  bind();
  state.settings = await invoke("get_settings").catch(() => null);
  state.system = await invoke("system_status").catch(() => null);
  await refreshLibrary();
  loadCatalog(false);
  if (Wizard.shouldShow()) Wizard.open();
  invoke("app_version").then((v) => ($("#app-version").textContent = v)).catch(() => {});
  setTimeout(() => checkAppUpdate(false), 8000);
  // Обновления: через пару секунд после старта и затем каждые 6 часов.
  setTimeout(checkUpdates, 3000);
  setInterval(checkUpdates, 6 * 3600 * 1000);
  pollDownloads();
  setInterval(pollDownloads, 1000);
  if (location.hash.length > 1) go(location.hash.slice(1));
}

// ───────────── Мок для просмотра UI в обычном браузере (без Tauri) ─────────────

async function mockInvoke(cmd, args) {
  const demo = [
    { title: "Lethal Company", release: "Lethal Company v72 by Pioneer", magnet: "", infoHash: "a1", fileSize: "1.1 GB", uploadDate: new Date().toISOString(), installKind: 2, addons: [], articleUrl: "https://freetp.org/po-seti/5458-x.html" },
    { title: "7 Days To Die", release: "7 Days To Die v3.3.0 (b18) by Pioneer", magnet: "", infoHash: "b2", fileSize: "16.4 GB", uploadDate: "2026-10-01T10:00:00", installKind: 1, addons: [{ title: "7_Days_To_Die_Multiplayer-Fix-Online-v1.4" }], articleUrl: "https://freetp.org/po-seti/749-x.html" },
    { title: "Abiotic Factor", release: "Abiotic Factor v1.4.0 by Pioneer", magnet: "", infoHash: "c3", fileSize: "3.6 GB", uploadDate: "2026-09-12T10:00:00", installKind: 1, addons: [], articleUrl: null },
    { title: "Phasmophobia", release: "Phasmophobia v0.13", magnet: "", infoHash: "d4", fileSize: "18.2 GB", uploadDate: "2026-08-02T10:00:00", installKind: 0, addons: [], articleUrl: null },
  ];
  const lib = [{ id: "lethal", title: "Lethal Company", release: "v72 by Pioneer", infoHash: "a1", status: "installed", dir: "~/Games/LinFix/lethal", exe: "/x/Lethal Company.exe", spacewar: true, playtimeSecs: 7300, addedAt: 1, launchArgs: "", env: "", dllOverrides: "", addons: [], installKind: 2 },
    { id: "sched", title: "Schedule I", release: "v0.4.1 by Pioneer", infoHash: "c3", status: "installed", dir: "~/Games/LinFix/sched", exe: "/x/Schedule I.exe", spacewar: true, addedAt: 3, launchArgs: "", env: "", dllOverrides: "", installKind: 1, addons: [{ title: "Schedule I Multiplayer Fix" }], addonsApplied: true, addonApplied: "Schedule-I-Multiplayer-Fix-Online-v1.2.exe" },
    { id: "7dtd", title: "7 Days To Die", release: "v3.3.0 (b18) by Pioneer", infoHash: "b2", status: "downloading", dir: "~/Games/LinFix/7dtd", spacewar: true, addedAt: 2, addons: [] }];
  switch (cmd) {
    case "get_catalog":
      if (args?.catalog === "onlinefix") return { origin: "hydralinks.cloud (оригинал)", newest: "2026-11-17T01:34:44Z", entries: [
        { title: "PEAK", release: "PEAK 1.10.a", magnet: "", infoHash: "e5", fileSize: "1.2 GB", uploadDate: "2026-11-10T10:00:00", installKind: 2, addons: [], source: "onlinefix" },
        { title: "Lethal Company", release: "Lethal Company 72", magnet: "", infoHash: "e6", fileSize: "1.1 GB", uploadDate: "2026-10-10T10:00:00", installKind: 2, addons: [], source: "onlinefix" },
        { title: "ELDEN RING NIGHTREIGN", release: "ELDEN RING NIGHTREIGN 1.01.4", magnet: "", infoHash: "e7", fileSize: "21.9 GB", uploadDate: "2026-09-10T10:00:00", installKind: 2, addons: [], source: "onlinefix" }] };
      return { origin: "GitLab · BadKiko/freetp-hydra-link", newest: demo[0].uploadDate, entries: demo };
    case "library": return lib;
    case "running_games": return [];
    case "downloads": return [{ id: "7dtd", state: "live", progressBytes: 6.2e9, totalBytes: 17.6e9, speedBps: 12.4e6, etaSecs: 920 }];
    case "get_settings": return { gamesDir: "~/Games/LinFix", source: "arachnel", proton: null, useUmu: false, downloadLimitMbps: 0, protonLog: true };
    case "list_protons": return [{ name: "GE-Proton10-25", path: "" }, { name: "GE-Proton10-9", path: "" }, { name: "Proton - Experimental", path: "" }];
    case "system_status": return { platform: "linux", steamRoot: "~/.local/share/Steam", steamRunning: false, umu: false, protons: 1, geProton: false, archiver: "/usr/bin/7z", unrar: false, steamFlatpak: false, distro: "ubuntu", distroLike: "debian" };
    case "dir_info": return { fs: /mnt/.test(args.path) ? "fuseblk" : "ext4", unsuitable: /mnt/.test(args.path), freeBytes: 412e9 };
    case "cover": return null;
    case "popular": return [2266, 6618, 749, 5458];
    case "check_updates": return [
      { id: "lethal", kind: "game", entry: { title: "Lethal Company", release: "Lethal Company v73 by Pioneer", magnet: "", infoHash: "a9", fileSize: "1.2 GB", uploadDate: "2026-11-20T10:00:00", installKind: 2, addons: [], source: "freetp" } },
      { id: "sched", kind: "fix", fixTitle: "Schedule-I-Multiplayer-Fix-Online-v1.3.exe" }];
    case "find_executables": return ["/home/user/Games/LinFix/7dtd/download/7DaysToDie.exe"];
    default: return null;
  }
}

init();
