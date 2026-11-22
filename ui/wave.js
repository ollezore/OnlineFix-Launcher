// SPDX-License-Identifier: GPL-3.0-or-later
// Волнистый индикатор прогресса (Material 3 Expressive, как на Pixel).
// Разметка прежняя: <div class="progress [indet|paused]"><i style="width:N%"></i></div>.
// Внутрь <i> рисуется SVG-линия; её амплитуда плавно меняется:
//   • в начале загрузки волна «вырастает» из ровной линии;
//   • на паузе и у самого конца (≈100%) — плавно выпрямляется;
//   • после снятия с паузы — снова плавно становится волной.
// Строки загрузок перерисовываются целиком при каждом опросе, поэтому состояние
// (амплитуда) хранится по ключу строки (data-id / data-kind), а не по элементу.
(function () {
  const WAVELENGTH = 40; // px — длинная пологая волна
  const AMPLITUDE = 2.5; // px
  const SPEED = 22; // px/с — сдвиг волны вперёд
  const EASE = 3.5; // скорость перехода «волна ↔ ровная», 1/с
  const H = 10; // высота области, px
  const STROKE = 4;

  const amps = new Map(); // ключ строки → текущая амплитуда 0..1
  const reduce = matchMedia("(prefers-reduced-motion: reduce)");
  let phase = 0;
  let last = performance.now();

  function keyOf(bar, idx) {
    const host = bar.closest("[data-id], [data-kind]");
    return host ? host.dataset.id || host.dataset.kind : "#" + idx;
  }

  function pathFor(w, amp) {
    const r = STROKE / 2;
    const x0 = r, x1 = Math.max(r, w - r); // скруглённые концы целиком внутри <i>
    const step = 2;
    const k = (2 * Math.PI) / WAVELENGTH;
    let d = "";
    for (let x = x0; ; x += step) {
      const xx = Math.min(x, x1);
      const y = H / 2 + amp * Math.sin((xx - phase) * k);
      d += (d ? "L" : "M") + xx.toFixed(1) + " " + y.toFixed(2);
      if (xx >= x1) break;
    }
    return d;
  }

  function frame(t) {
    const dt = Math.min(0.1, (t - last) / 1000);
    last = t;
    if (!reduce.matches) phase = (phase + SPEED * dt) % WAVELENGTH;
    const seen = new Set();
    document.querySelectorAll(".progress > i").forEach((i, idx) => {
      const bar = i.parentElement;
      if (!bar.offsetParent) return; // скрыт
      const key = keyOf(bar, idx);
      seen.add(key);
      let svg = i.firstElementChild;
      if (!svg || svg.tagName.toLowerCase() !== "svg") {
        i.innerHTML = `<svg height="${H}" aria-hidden="true"><path/></svg>`;
        svg = i.firstElementChild;
      }
      const w = i.getBoundingClientRect().width;
      const pct = parseFloat(i.style.width) || 0;
      const indet = bar.classList.contains("indet");
      let target = bar.classList.contains("paused") ? 0 : 1;
      // Ровная линия у самого начала и конца, волна — посередине.
      if (!indet) target *= Math.max(0, Math.min(1, pct / 2, (100 - pct) / 2));
      let a = amps.has(key) ? amps.get(key) : 0;
      a += (target - a) * Math.min(1, dt * EASE);
      amps.set(key, a);
      svg.setAttribute("width", Math.max(0, w).toFixed(1));
      svg.firstElementChild.setAttribute("d", w > 0 ? pathFor(w, AMPLITUDE * a) : "");
    });
    for (const k of amps.keys()) if (!seen.has(k)) amps.delete(k);
    requestAnimationFrame(frame);
  }
  requestAnimationFrame(frame);
})();
