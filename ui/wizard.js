// LinFix — мастер первого запуска (Material Design 3). SPDX-License-Identifier: GPL-3.0-or-later
// Проверяет Steam / Proton / архиваторы, помогает выбрать папку для игр.

const Wizard = (() => {
  let step = 0;
  let sys = null;
  let dir = "";
  let busy = false;

  const fmtGB = (b) => (b == null ? "?" : b >= 1024 ** 4 ? `${(b / 1024 ** 4).toFixed(1)} ТБ` : `${Math.round(b / 1024 ** 3)} ГБ`);

  // Команда установки пакета под дистрибутив пользователя.
  function pkgHint(what) {
    const id = `${sys?.distro || ""} ${sys?.distroLike || ""}`;
    const cmd = {
      arch: { "7z": "sudo pacman -S 7zip", unrar: "sudo pacman -S unrar" },
      fedora: { "7z": "sudo dnf install p7zip p7zip-plugins", unrar: "sudo dnf install unrar" },
      debian: { "7z": "sudo apt install p7zip-full", unrar: "sudo apt install unrar" },
      suse: { "7z": "sudo zypper install 7zip", unrar: "sudo zypper install unrar" },
    };
    if (/arch|manjaro|cachyos|endeavouros|garuda/.test(id)) return cmd.arch[what];
    if (/fedora|nobara|rhel|centos|bazzite/.test(id)) return cmd.fedora[what];
    if (/debian|ubuntu|mint|pop|zorin|elementary/.test(id)) return cmd.debian[what];
    if (/suse/.test(id)) return cmd.suse[what];
    return null;
  }

  const cmdBox = (cmd) => cmd
    ? `<div class="cmd"><code>${esc(cmd)}</code><button class="icon-btn sl" data-copy="${esc(cmd)}" title="Копировать">${icon("copy")}</button></div>`
    : "";

  function item(state, title, sub, extra = "") {
    const ic = { ok: "ok", warn: "warning", err: "error", opt: "info" }[state];
    return `<div class="wz-item ${state}">
      <span class="wz-ico">${icon(ic)}</span>
      <div class="grow"><div class="body-l">${title}</div><div class="body-m muted">${sub}</div>${extra}</div>
    </div>`;
  }

  function stepWelcome() {
    const { seed } = MD3Theme.get();
    return `
      <div class="wz-hero">${icon("games")}</div>
      <h2 class="headline-s">Добро пожаловать в LinFix</h2>
      <div class="body-m muted">Лаунчер игр с онлайн-фиксами FreeTP и Online-Fix. Без регистрации — базы открытые.</div>
      <ol class="wz-steps body-m">
        <li><b>Каталог</b> — найдите игру и нажмите «Скачать».</li>
        <li><b>Установка</b> — ${sys?.platform === "windows" ? "установщик запустится как обычно" : "установщик запустится через Proton в отдельном префиксе"}.</li>
        <li><b>Фикс</b> — онлайн-фикс поставится сам.</li>
        <li><b>Игра</b> — запуск через Steam (Spacewar 480), играйте с друзьями.</li>
      </ol>
      <div class="title-s">Цвет интерфейса</div>
      <div class="swatches" id="wz-seed">${MD3Theme.PRESETS.map((p) => {
        const s = MD3Theme.scheme(p.hex, document.documentElement.dataset.theme === "dark");
        return `<button class="swatch sl ${p.hex === seed ? "on" : ""}" data-seed="${p.hex}" title="${esc(p.name)}"
          style="--sw-a:${s.primary};--sw-b:${s["secondary-container"]};--sw-c:${s.tertiary}">${icon("check")}</button>`;
      }).join("")}</div>`;
  }

  function stepSystem() {
    if (!sys) return `<div class="empty"><div class="spinner"></div><span class="body-l">Проверяю систему…</span></div>`;
    const win = sys.platform === "windows";
    let html = `<h2 class="headline-s">Проверка системы</h2><div class="wz-list">`;
    html += sys.steamRoot
      ? item("ok", "Steam установлен", esc(sys.steamRoot))
      : item("err", "Steam не найден", "Нужен для онлайна: установите Steam и войдите в аккаунт.");
    if (sys.steamFlatpak)
      html += item("warn", "Steam из Flatpak", "Песочница Flatpak может мешать онлайну. Если игра не видит Steam — поставьте Steam из пакетов дистрибутива.");
    html += sys.steamRunning
      ? item("ok", "Steam запущен", "Онлайн через Spacewar (480) будет работать")
      : item("warn", "Steam не запущен", "Для онлайна клиент Steam должен работать во время игры.",
          `<button class="btn tonal has-icon wz-btn" id="wz-steam">${icon("steam")}Запустить Steam</button>`);
    if (!win) {
      if (sys.geProton) html += item("ok", "GE-Proton установлен", `Всего версий Proton: ${sys.protons}`);
      else html += item(sys.protons ? "warn" : "err", sys.protons ? `Proton: ${sys.protons}, но нет GE-Proton` : "Proton не найден",
        "GE-Proton работает без Steam Runtime и лучше всего подходит для игр с фиксами. Скачается ~500 МБ в compatibilitytools.d.",
        `<button class="btn filled has-icon wz-btn" id="wz-ge" ${busy ? "disabled" : ""}>${icon("download")}${busy ? "Скачиваю…" : "Скачать GE-Proton"}</button>`);
    }
    html += sys.archiver
      ? item("ok", "Архиватор", esc(sys.archiver))
      : item("err", "Архиватор не найден", win ? "Установите 7-Zip или WinRAR — нужен для фиксов и архивов." : "Нужен 7z для фиксов и архивов.", win ? "" : cmdBox(pkgHint("7z")));
    if (!win && !sys.unrar)
      html += item("opt", "unrar (по желанию)", "Некоторые фиксы и раздачи в RAR — 7z не всегда их открывает.", cmdBox(pkgHint("unrar")));
    html += `</div><button class="btn text has-icon" id="wz-recheck">${icon("refresh")}Проверить снова</button>`;
    return html;
  }

  function stepDir() {
    return `
      <h2 class="headline-s">Где хранить игры</h2>
      <div class="body-m muted">В этой папке у каждой игры будет своя папка: раздача, ${sys?.platform === "windows" ? "" : "префикс Proton, "}сама игра. Нужно много места.</div>
      <div class="field-row">
        <div class="field"><label>Папка для игр</label><input id="wz-dir" value="${esc(dir)}" spellcheck="false"/></div>
        <button class="btn outlined" id="wz-browse">Обзор</button>
      </div>
      <div id="wz-dirinfo" class="wz-dirinfo body-m"></div>`;
  }

  function stepDone() {
    return `
      <div class="wz-hero ok">${icon("check")}</div>
      <h2 class="headline-s">Всё готово</h2>
      <ul class="wz-steps body-m">
        <li>Для онлайна держите <b>Steam запущенным</b> во время игры.</li>
        <li>Вкладки <b>FreeTP</b> и <b>Online-Fix</b> в каталоге — две разные базы, выбирайте, откуда качать.</li>
        <li>Новые версии игр и фиксов появятся в <b>Библиотеке</b> — с кнопкой «Обновить».</li>
        <li>Мастер можно открыть снова: <b>Настройки → Оформление</b>.</li>
      </ul>`;
  }

  const STEPS = [stepWelcome, stepSystem, stepDir, stepDone];

  async function refreshDirInfo(d) {
    const box = $("#wz-dirinfo", d);
    if (!box) return;
    const v = $("#wz-dir", d).value.trim();
    if (!v) { box.innerHTML = ""; return; }
    try {
      const info = await invoke("dir_info", { path: v });
      box.innerHTML = `${icon(info.unsuitable ? "warning" : "ok")}<span>Свободно ${fmtGB(info.freeBytes)}${info.fs ? ` · ${esc(info.fs)}` : ""}${
        info.unsuitable ? ". Это раздел Windows: игры можно хранить здесь, но префиксы Proton лаунчер положит в домашнюю папку." : ""}</span>`;
      box.classList.toggle("warn", !!info.unsuitable);
    } catch { box.innerHTML = ""; }
  }

  function render() {
    const last = step === STEPS.length - 1;
    const dlg = openDialog(`
      <div class="wz-progress">${STEPS.map((_, i) => `<i class="${i <= step ? "on" : ""}"></i>`).join("")}</div>
      <div class="scroll wz-body">${STEPS[step]()}</div>
      <div class="dlg-actions">
        ${step === 0 ? `<button class="btn text" id="wz-skip">Пропустить</button>` : `<button class="btn text" id="wz-back">Назад</button>`}
        <span style="flex:1"></span>
        <button class="btn filled has-icon" id="wz-next">${last ? `${icon("explore")}Открыть каталог` : "Далее"}</button>
      </div>`, (d) => {
      d.classList.add("wizard");
      $$("[data-seed]", d).forEach((b) => (b.onclick = () => { MD3Theme.setSeed(b.dataset.seed); render(); }));
      $$("[data-copy]", d).forEach((b) => (b.onclick = () => navigator.clipboard?.writeText(b.dataset.copy).then(() => snack("Команда скопирована"))));
      const skip = $("#wz-skip", d); if (skip) skip.onclick = finish;
      const back = $("#wz-back", d); if (back) back.onclick = () => { step--; render(); };
      $("#wz-next", d).onclick = async () => {
        if (step === 2) {
          const v = $("#wz-dir", d).value.trim();
          if (v && v !== state.settings?.gamesDir) await saveSettings({ gamesDir: v });
          dir = v;
        }
        if (last) { finish(); go("catalog"); return; }
        step++;
        render();
        if (step === 1 && !sys) loadSys();
      };
      const st = $("#wz-steam", d);
      if (st) st.onclick = () => invoke("start_steam").then(() => { snack("Steam запускается…"); setTimeout(loadSys, 8000); }).catch(fail);
      const ge = $("#wz-ge", d);
      if (ge) ge.onclick = async () => {
        busy = true; render();
        try { const r = await invoke("install_ge_proton"); snack(r.already ? `${r.tag} — уже последняя версия` : `${r.tag} установлен`); } catch (e) { fail(e); }
        busy = false; await loadSys();
      };
      const re = $("#wz-recheck", d); if (re) re.onclick = () => { sys = null; render(); loadSys(); };
      const inp = $("#wz-dir", d);
      if (inp) {
        let t; inp.oninput = () => { clearTimeout(t); t = setTimeout(() => refreshDirInfo(d), 300); };
        refreshDirInfo(d);
        $("#wz-browse", d).onclick = async () => {
          if (!T) return;
          const p = await T.dialog.open({ directory: true, defaultPath: inp.value || undefined });
          if (p) { inp.value = p; refreshDirInfo(d); }
        };
      }
    });
    dlg.onclick = null; // не закрывать мастер случайным кликом мимо
  }

  async function loadSys() {
    try { sys = await invoke("system_status"); state.system = sys; } catch { sys = sys || { platform: "linux" }; }
    if ($("#dialog").open && $("#dialog").classList.contains("wizard")) render();
  }

  function finish() {
    localStorage.setItem("wizardDone", "1");
    $("#dialog").classList.remove("wizard");
    closeDialog();
  }

  return {
    async open() {
      step = 0;
      dir = state.settings?.gamesDir || "";
      sys = state.system || null;
      render();
      if (!sys) loadSys();
      $("#dialog").addEventListener("close", () => {
        $("#dialog").classList.remove("wizard");
        localStorage.setItem("wizardDone", "1");
      }, { once: true });
    },
    shouldShow: () => !localStorage.getItem("wizardDone"),
  };
})();
