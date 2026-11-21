// LinFix — динамический цвет Material Design 3. SPDX-License-Identifier: GPL-3.0-or-later
//
// Из одного «исходного» цвета строятся тональные палитры (как в Material You),
// а из них — все цветовые роли схемы для светлой и тёмной темы.
// Тон палитры = светлота L* из CIELAB (в HCT это та же величина), поэтому
// контраст между ролями (например, тон 40 на тоне 90) получается как в MD3.

const MD3Theme = (() => {
  // ── sRGB ⇄ OKLab ──
  // Оттенок и хрому берём в OKLCH (оттенок в нём не «плывёт», особенно у синих),
  // а тон, как в HCT, — это светлота L* из CIELAB: её подбираем через яркость Y.
  const lin = (c) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
  const delin = (c) => (c <= 0.0031308 ? 12.92 * c : 1.055 * c ** (1 / 2.4) - 0.055);
  const lstarToY = (t) => { const fy = (t + 16) / 116; return t > 8 ? fy ** 3 : t / (24389 / 27); };

  function toOklab([r, g, b]) {
    const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
    const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
    const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
    return [
      0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
      1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
      0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
    ];
  }
  function fromOklch(L, C, h) {
    const a = C * Math.cos((h * Math.PI) / 180), b = C * Math.sin((h * Math.PI) / 180);
    const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
    const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
    const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
    return [
      4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
      -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
      -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
    ];
  }
  const lum = ([r, g, b]) => 0.2126 * r + 0.7152 * g + 0.0722 * b;

  function hexToOklch(hex) {
    const n = parseInt(hex.replace("#", ""), 16);
    const rgb = [(n >> 16) & 255, (n >> 8) & 255, n & 255].map((v) => lin(v / 255));
    const [L, A, B] = toOklab(rgb);
    return { l: L, c: Math.hypot(A, B), h: ((Math.atan2(B, A) * 180) / Math.PI + 360) % 360 };
  }
  const inGamut = (rgb) => rgb.every((v) => v >= -1e-4 && v <= 1 + 1e-4);
  const toHex = (rgb) =>
    "#" + rgb.map((v) => Math.round(Math.min(1, Math.max(0, delin(v))) * 255).toString(16).padStart(2, "0")).join("");

  // OKLab-светлота, при которой у цвета (C, h) нужная яркость Y.
  function solveL(C, h, Y) {
    let lo = 0, hi = 1;
    for (let i = 0; i < 22; i++) {
      const mid = (lo + hi) / 2;
      if (lum(fromOklch(mid, C, h)) < Y) lo = mid; else hi = mid;
    }
    return (lo + hi) / 2;
  }

  // Цвет заданного тона; если хрома не помещается в sRGB — уменьшаем её.
  function tone(h, c, t) {
    if (t >= 100) return "#ffffff";
    if (t <= 0) return "#000000";
    const Y = lstarToY(t);
    const at = (C) => fromOklch(solveL(C, h, Y), C, h);
    let rgb = at(c);
    if (!inGamut(rgb)) {
      let lo = 0, hi = c;
      for (let i = 0; i < 16; i++) {
        const mid = (lo + hi) / 2;
        if (inGamut(at(mid))) lo = mid; else hi = mid;
      }
      rgb = at(lo);
    }
    return toHex(rgb);
  }
  const palette = (h, c) => (t) => tone(h, c, t);

  // Схема «tonal spot» — стандартная для Material You.
  function scheme(seedHex, dark) {
    const s = hexToOklch(seedHex);
    // Как в MD3 tonal spot: хрома основной палитры фиксирована, от исходного цвета берётся оттенок.
    const P = palette(s.h, 0.125);
    const S = palette(s.h, 0.045);
    const T = palette((s.h + 60) % 360, 0.08);
    const N = palette(s.h, 0.012);
    const NV = palette(s.h, 0.026);
    const G = palette(150, 0.11); // «успех» — всегда зелёный
    const E = palette(27, 0.19);  // ошибка — всегда красная
    const d = dark;
    return {
      "primary": P(d ? 80 : 40), "on-primary": P(d ? 20 : 100),
      "primary-container": P(d ? 30 : 90), "on-primary-container": P(d ? 90 : 10),
      "secondary": S(d ? 80 : 40), "on-secondary": S(d ? 20 : 100),
      "secondary-container": S(d ? 30 : 90), "on-secondary-container": S(d ? 90 : 10),
      "tertiary": T(d ? 80 : 40), "on-tertiary": T(d ? 20 : 100),
      "tertiary-container": T(d ? 30 : 90), "on-tertiary-container": T(d ? 90 : 10),
      "error": E(d ? 80 : 40), "on-error": E(d ? 20 : 100),
      "error-container": E(d ? 30 : 90), "on-error-container": E(d ? 90 : 10),
      "success-container": G(d ? 30 : 90), "on-success-container": G(d ? 90 : 10),
      "surface": N(d ? 6 : 98), "surface-dim": N(d ? 6 : 87), "surface-bright": N(d ? 24 : 98),
      "surface-container-lowest": N(d ? 4 : 100), "surface-container-low": N(d ? 10 : 96),
      "surface-container": N(d ? 12 : 94), "surface-container-high": N(d ? 17 : 92),
      "surface-container-highest": N(d ? 22 : 90),
      "on-surface": N(d ? 90 : 10), "on-surface-variant": NV(d ? 80 : 30),
      "outline": NV(d ? 60 : 50), "outline-variant": NV(d ? 30 : 80),
      "inverse-surface": N(d ? 90 : 20), "inverse-on-surface": N(d ? 20 : 95),
      "inverse-primary": P(d ? 40 : 80),
    };
  }

  const PRESETS = [
    { name: "Фиолетовый", hex: "#6750a4" },
    { name: "Синий", hex: "#0b57d0" },
    { name: "Бирюзовый", hex: "#00696f" },
    { name: "Зелёный", hex: "#386a20" },
    { name: "Оранжевый", hex: "#8b5000" },
    { name: "Красный", hex: "#b3261e" },
    { name: "Розовый", hex: "#984061" },
  ];

  const media = window.matchMedia("(prefers-color-scheme: dark)");
  const get = () => ({
    mode: localStorage.getItem("themeMode") || localStorage.getItem("theme") || "dark", // dark | light | system
    seed: localStorage.getItem("themeSeed") || PRESETS[0].hex,
  });

  function apply() {
    const { mode, seed } = get();
    const dark = mode === "system" ? media.matches : mode !== "light";
    const root = document.documentElement;
    root.dataset.theme = dark ? "dark" : "light";
    root.style.colorScheme = dark ? "dark" : "light";
    for (const [k, v] of Object.entries(scheme(seed, dark))) root.style.setProperty(`--md-${k}`, v);
    root.style.setProperty("--md-scrim", dark ? "rgba(0,0,0,.55)" : "rgba(0,0,0,.32)");
  }
  media.addEventListener("change", () => get().mode === "system" && apply());

  return {
    PRESETS,
    get,
    apply,
    scheme,
    setMode(m) { localStorage.setItem("themeMode", m); apply(); },
    setSeed(hex) { localStorage.setItem("themeSeed", hex); apply(); },
  };
})();

MD3Theme.apply();
