// OnlineFix-Launcher — перевод интерфейса. SPDX-License-Identifier: GPL-3.0-or-later
//
// Исходный язык — русский: строки интерфейса и сообщения бэкенда пишутся по-русски,
// а словари в ui/locales/<язык>.js переводят их («русский текст» → перевод).
// Строки с подстановками задаются шаблоном: «{0} скачана — можно устанавливать».
//
// Переводится всё автоматически: MutationObserver ловит новые узлы и меняет
// текст и атрибуты (title, placeholder, aria-label) по словарю. Там, где фраза
// собирается из кусков в один текст, код вызывает t("шаблон", [значения]).
// Новый язык — один файл в ui/locales + строка в LANGS.

const I18N = (() => {
  const LANGS = {
    uk: "Українська",
    ru: "Русский",
    en: "English",
    de: "Deutsch",
    pl: "Polski",
    es: "Español",
  };
  const LOCALES = { uk: "uk-UA", ru: "ru-RU", en: "en-US", de: "de-DE", pl: "pl-PL", es: "es-ES" };

  function detect() {
    const saved = localStorage.getItem("lang");
    if (saved && LANGS[saved]) return saved;
    // По умолчанию — английский; язык меняется в мастере первого запуска и в настройках.
    return "en";
  }

  const lang = detect();
  const dict = (window.LINFIX_LOCALES || {})[lang] || {};
  const norm = (s) => String(s).replace(/\s+/g, " ").trim();
  const CYR = /[Ѐ-ӿ]/;
  const esc = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

  // Шаблоны с подстановками: длинные (точные) — первыми.
  const patterns = [];
  for (const [k, v] of Object.entries(dict)) {
    if (!/\{\d+\}/.test(k)) continue;
    const parts = k.split(/(\{\d+\})/);
    const order = [];
    const src = parts.map((p) => {
      const m = /^\{(\d+)\}$/.exec(p);
      if (m) { order.push(+m[1]); return "([\\s\\S]*?)"; }
      return esc(p);
    }).join("");
    patterns.push({ re: new RegExp(`^${src}$`), order, v, fixed: k.replace(/\{\d+\}/g, "").length });
  }
  patterns.sort((a, b) => b.fixed - a.fixed);

  const fill = (s, params) => (params ? String(s).replace(/\{(\d+)\}/g, (_, i) => params[+i] ?? "") : s);

  // Перевод готового текста (без параметров). Значения внутри шаблона тоже переводятся.
  function lookup(text, depth = 0) {
    const n = norm(text);
    if (!n || !CYR.test(n)) return null;
    if (dict[n] !== undefined) return dict[n];
    if (depth > 2) return null;
    for (const p of patterns) {
      const m = p.re.exec(n);
      if (!m) continue;
      const vals = [];
      p.order.forEach((idx, i) => { vals[idx] = lookup(m[i + 1], depth + 1) ?? m[i + 1]; });
      return fill(p.v, vals);
    }
    return null;
  }

  /** t("Скачать") или t("{0} из {1}", [a, b]) */
  function t(key, params) {
    if (lang === "ru") return fill(key, params);
    const v = dict[norm(key)];
    if (v !== undefined) return fill(v, params);
    return fill(params ? key : lookup(key) ?? key, params);
  }

  // Перевод текста узла с сохранением пробелов по краям.
  function translateString(s) {
    const r = lookup(s);
    if (r == null) return null;
    const lead = /^\s*/.exec(s)[0], trail = /\s*$/.exec(s)[0];
    return lead + r + trail;
  }

  const ATTRS = ["title", "placeholder", "aria-label"];
  const SKIP = new Set(["SCRIPT", "STYLE", "CODE", "TEXTAREA", "INPUT"]);

  function walk(node) {
    if (node.nodeType === 3) {
      const r = translateString(node.data);
      if (r != null && r !== node.data) node.data = r;
      return;
    }
    if (node.nodeType !== 1 || node.tagName === "SCRIPT" || node.tagName === "STYLE") return;
    // У полей ввода переводим подсказки (placeholder/title), но не значение.
    for (const a of ATTRS) {
      const v = node.getAttribute(a);
      if (v && CYR.test(v)) { const r = translateString(v); if (r != null) node.setAttribute(a, r); }
    }
    if (SKIP.has(node.tagName)) return;
    for (let c = node.firstChild; c; c = c.nextSibling) walk(c);
  }

  function start() {
    document.documentElement.lang = lang;
    if (lang === "ru") return;
    const tt = translateString(document.title);
    if (tt) document.title = tt;
    walk(document.body);
    new MutationObserver((muts) => {
      for (const m of muts) m.addedNodes.forEach(walk);
    }).observe(document.body, { childList: true, subtree: true });
  }

  function setLang(l) {
    localStorage.setItem("lang", l);
    location.reload();
  }

  return { lang, locale: LOCALES[lang], LANGS, t, setLang, start, hasLocale: (l) => l === "ru" || !!(window.LINFIX_LOCALES || {})[l] };
})();

const t = I18N.t;
I18N.start();
