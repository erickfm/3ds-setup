"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const dialog = window.__TAURI__.dialog;
const $ = (id) => document.getElementById(id);

const EXTENSIONS = ["3ds", "cci", "cxi", "3dsx", "cia", "zcci", "zcxi", "z3dsx", "zcia"];
let COLUMNS = 5; // 4 on a Steam Deck, where tiles are bigger

let lib = null;        // what the backend found: games, backups, settings
let selected = null;   // path of the selected game
let playing = null;    // name of the game running in Azahar
let modalOpen = false;
let onModalClose = null;
let update = null;     // { current, latest, available } once checked
const covers = new Map(); // product code -> box art data: URL, or null if there's none

// ---------------------------------------------------------------- little helpers

const pref = {
  get(key, fallback) {
    try { const v = localStorage.getItem(key); return v === null ? fallback : JSON.parse(v); } catch { return fallback; }
  },
  set(key, value) {
    try { localStorage.setItem(key, JSON.stringify(value)); } catch { /* private mode etc. */ }
  },
};

function applyTheme(theme) {
  if (theme === "light" || theme === "dark") document.documentElement.dataset.theme = theme;
  else delete document.documentElement.dataset.theme;
}
applyTheme(pref.get("theme", "auto"));

function esc(s) {
  return String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

const icon = (name) => `<svg><use href="#i-${name}"/></svg>`;

function art(game) {
  if (game.icon) return `<img class="art" src="${game.icon}" alt="" draggable="false">`;
  let hue = 0;
  for (const c of game.name) hue = (hue * 31 + c.charCodeAt(0)) % 360;
  const letters = game.name.replace(/[^\p{L}\p{N} ]/gu, "").split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]).join("").toUpperCase();
  return `<div class="art letter" style="--h:${hue}">${esc(letters || "?")}</div>`;
}

// Snapshot names are local times: 2026-10-23_19-30-00
function stampDate(stamp) {
  const m = /^(\d{4})-(\d\d)-(\d\d)_(\d\d)-(\d\d)-(\d\d)/.exec(stamp || "");
  return m ? new Date(+m[1], m[2] - 1, +m[3], +m[4], +m[5], +m[6]) : null;
}
const clock = (d) => d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
const dayStart = (d) => new Date(d.getFullYear(), d.getMonth(), d.getDate());

function friendly(d) {
  const now = new Date();
  const days = Math.round((dayStart(now) - dayStart(d)) / 864e5);
  if (days === 0) return `Today ${clock(d)}`;
  if (days === 1) return `Yesterday ${clock(d)}`;
  const opts = { weekday: "short", month: "short", day: "numeric" };
  if (d.getFullYear() !== now.getFullYear()) opts.year = "numeric";
  return `${d.toLocaleDateString([], opts)}, ${clock(d)}`;
}

function ago(d) {
  const s = (Date.now() - d) / 1000;
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  const f = friendly(d);
  return /^(Today|Yesterday)/.test(f) ? f[0].toLowerCase() + f.slice(1) : `on ${f}`;
}

function playTime(seconds) {
  if (seconds < 60) return null;
  const h = Math.floor(seconds / 3600), m = Math.floor((seconds % 3600) / 60);
  return h ? `${h} h ${m} min played` : `${m} min played`;
}

let toastTimer;
function toast(text, ms = 3200) {
  const t = $("toast");
  t.textContent = text;
  t.classList.add("on");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.remove("on"), ms);
}

// ---------------------------------------------------------------- sounds (made on the fly, no files)

let audio;
function sound(kind) {
  if (!pref.get("sounds", true)) return;
  try {
    audio ||= new AudioContext();
    const notes = { move: [[1320, 0.03]], open: [[660, 0.05], [990, 0.07]], back: [[740, 0.05], [520, 0.07]],
      play: [[523, 0.07], [659, 0.07], [784, 0.07], [1046, 0.16]], done: [[880, 0.06], [1175, 0.12]] }[kind];
    let at = audio.currentTime;
    for (const [freq, len] of notes) {
      const osc = audio.createOscillator(), gain = audio.createGain();
      osc.type = "triangle";
      osc.frequency.value = freq;
      gain.gain.setValueAtTime(0.0001, at);
      gain.gain.exponentialRampToValueAtTime(kind === "move" ? 0.05 : 0.09, at + 0.008);
      gain.gain.exponentialRampToValueAtTime(0.0001, at + len);
      osc.connect(gain).connect(audio.destination);
      osc.start(at);
      osc.stop(at + len + 0.02);
      at += len * 0.8;
    }
  } catch { /* no audio device */ }
}

// ---------------------------------------------------------------- data

async function refresh() {
  try {
    lib = await invoke("library");
  } catch (e) {
    toast(`Couldn't load your games: ${e}`);
    return;
  }
  playing = lib.playing;
  document.documentElement.classList.toggle("handheld", !!lib.handheld);
  COLUMNS = lib.handheld ? 4 : 5;
  if (!lib.games.some((g) => g.path === selected)) selected = lib.games[0]?.path ?? null;
  render();
  loadAllCovers();
}

const current = () => lib?.games.find((g) => g.path === selected) ?? null;

// Box art comes from the internet the first time, then from a cache on disk.
async function loadCover(code) {
  if (!code || covers.has(code)) return covers.get(code) ?? null;
  covers.set(code, null);
  let url = null;
  try { url = await invoke("cover", { code }); } catch { /* offline etc. */ }
  covers.set(code, url);
  if (url && current()?.product_code === code && !playing) renderStage(true);
  return url;
}

async function loadAllCovers() {
  const g = current();
  if (g) await loadCover(g.product_code);
  for (const game of lib.games) await loadCover(game.product_code);
}

// ---------------------------------------------------------------- rendering

function render() {
  renderStatus();
  renderStage();
  renderGrid();
  renderHints();
}

function renderStatus() {
  const pill = $("backup-pill"), b = lib.backup;
  pill.className = "pill";
  let text = "Backups on";
  if (!b.on_target) { pill.classList.add("bad"); text = "Backup folder not found"; }
  else if (!b.watcher) { pill.classList.add("warn"); text = "Backups starting..."; }
  else if (b.last) { text = `Backed up ${ago(stampDate(b.last))}`; }
  $("backup-text").textContent = text;
  pill.setAttribute("role", "button");
  pill.title = "Backup settings";
}

function setBackdrop(url) {
  const b = $("backdrop");
  if (url) b.style.backgroundImage = `url("${url}")`;
  b.classList.toggle("on", !!url);
}

function renderStage(quiet) {
  const stage = $("stage");
  const g = current();
  const had = stage.contains(document.activeElement) ? document.activeElement.id : null;
  if (had) queueMicrotask(() => [$(had), $("btn-play"), $("welcome-add"), selectedTile()]
    .find((el) => el && !el.disabled)?.focus({ preventScroll: true }));
  if (!quiet) {
    stage.style.animation = "none";
    void stage.offsetWidth; // restart the little entrance animation
    stage.style.animation = "";
  }
  const box = g && covers.get(g.product_code);
  setBackdrop(!playing && box);

  if (!lib.azahar_ready) {
    stage.innerHTML = `<div class="center-card"><h1>Almost there</h1>
      <p>Azahar, the program that runs the games, isn't installed yet. Run <b>3DS Setup</b> again to finish.</p></div>`;
    return;
  }
  if (playing) {
    const pg = lib.games.find((x) => x.name === playing);
    stage.innerHTML = `<div class="center-card">
      <div class="playing-art">${pg ? art(pg) : ""}</div>
      <h1>Playing ${esc(playing)}</h1>
      <p>Have fun! Your saves are backed up while you play.<br>Close the game window to come back here.</p></div>`;
    return;
  }
  if (!g) {
    stage.innerHTML = `<div class="center-card"><h1>Let's add your first game</h1>
      <p>Click <b>Add games</b> below, or drag game files onto this window.</p>
      <button class="btn primary" id="welcome-add">${icon("plus")} Add games</button>
      <div class="formats">Works with .3ds, .cci, .cia, .cxi and .3dsx files (and Azahar's compressed .zcci/.zcia).</div></div>`;
    $("welcome-add").onclick = pickGames;
    return;
  }

  const chips = [];
  const played = playTime(g.play_seconds);
  if (played) chips.push(`<span class="chip">${icon("clock")}${played}</span>`);
  if (g.kind === "homebrew") chips.push(`<span class="chip">${icon("box")}Homebrew</span>`);
  if (g.title_id) {
    if (g.last_saved) chips.push(`<span class="chip">${icon("save")}Saved ${ago(new Date(g.last_saved))}</span>`);
    if (g.backed_up === true) chips.push(`<span class="chip ok">${icon("check")}Backed up</span>`);
    else if (g.backed_up === false) chips.push(`<span class="chip wait">${icon("history")}Backing up soon</span>`);
    else if (!g.encrypted) chips.push(`<span class="chip">${icon("save")}No save yet</span>`);
  }

  let note = "";
  if (g.encrypted) {
    note = `<div class="note bad">${icon("warn")}<span>This copy of the game is encrypted, so it can't be played. You need a decrypted copy of it.</span></div>`;
  } else if (g.kind === "cia") {
    note = `<div class="note info">${icon("box")}<span>The first time you play, this game gets set up inside Azahar. It takes a moment.</span></div>`;
  }

  const picture = box
    ? `<div class="boxart" style="background-image:url('${box}')"><div class="badge-icon">${art(g)}</div></div>`
    : `<div class="banner-icon">${art(g)}</div>`;
  stage.innerHTML = `
    ${picture}
    <div class="banner-text">
      <h1>${esc(g.name)}</h1>
      ${g.publisher ? `<p class="publisher">${esc(g.publisher)}</p>` : ""}
      <div class="chips">${chips.join("")}</div>
      <div class="actions">
        <button class="btn primary" id="btn-play" ${g.encrypted ? "disabled" : ""}>${icon("play")} Play</button>
        ${g.title_id ? `<button class="btn" id="btn-history">${icon("history")} Older saves</button>` : ""}
      </div>
      ${note}
    </div>`;
  $("btn-play").onclick = () => play(g);
  if (g.title_id) $("btn-history").onclick = () => showVersions(g, false);
}

function renderGrid() {
  const grid = $("grid");
  const had = grid.contains(document.activeElement) ? (document.activeElement.id === "slot-add" ? "add" : "tile") : null;
  const tiles = lib.games.map((g, i) => {
    let badge = "";
    if (g.encrypted) badge = `<span class="badge bad">${icon("warn")}</span>`;
    else if (g.kind === "cia") badge = `<span class="badge new">${icon("box")}</span>`;
    return `<button class="tile${g.path === selected ? " selected" : ""}" data-i="${i}" role="option"
      aria-selected="${g.path === selected}" title="${esc(g.name)}">${art(g)}${badge}</button>`;
  });
  const slots = Math.max(COLUMNS * 3, Math.ceil((tiles.length + 1) / COLUMNS) * COLUMNS);
  tiles.push(`<button class="tile ghost add-slot" id="slot-add" title="Add games">${icon("plus")}</button>`);
  while (tiles.length < slots) tiles.push(`<div class="tile ghost"></div>`);
  grid.innerHTML = tiles.join("");

  grid.querySelectorAll(".tile[data-i]").forEach((el) => {
    const g = lib.games[+el.dataset.i];
    el.onclick = () => select(g.path);
    el.ondblclick = () => play(g);
  });
  $("slot-add").onclick = pickGames;
  grid.querySelector(".selected")?.scrollIntoView({ block: "nearest" });
  if (had) (had === "add" ? $("slot-add") : selectedTile() ?? $("slot-add")).focus({ preventScroll: true });
}

function renderHints() {
  const key = (k, what) => `<span><span class="key">${k}</span>${what}</span>`;
  const off = document.activeElement && !document.activeElement.dataset?.i && mainItems().includes(document.activeElement);
  $("hints").innerHTML = padCount && off ? key("A", "Select") + key("B", "Back to games") + key("☰", "Settings")
    : !lib.games.length ? `<span>Your games show up here</span>`
    : padCount ? key("A", "Play") + key("Y", "Older saves") + key("X", "All saves") + key("☰", "Settings")
    : `<span>Double-click a game to play</span>${key("⏎", "Play")}${key("←→", "Choose")}`;
}

function select(path, quiet) {
  if (path === selected) return;
  selected = path;
  if (!quiet) sound("move");
  renderStage();
  document.querySelectorAll(".tile[data-i]").forEach((el) => {
    const on = lib.games[+el.dataset.i].path === path;
    el.classList.toggle("selected", on);
    el.setAttribute("aria-selected", on);
    if (on) el.scrollIntoView({ block: "nearest" });
  });
}

// ---------------------------------------------------------------- actions

async function play(g) {
  if (!g || g.encrypted || playing) return;
  sound("play");
  toast(g.kind === "cia" ? "Getting the game ready..." : `Starting ${g.name}...`, 2500);
  try {
    await invoke("play", { path: g.path, name: g.name });
  } catch (e) {
    showMessage("Couldn't start the game", String(e));
  }
}

async function pickGames() {
  const files = await dialog.open({
    multiple: true,
    title: "Choose 3DS games to add",
    filters: [{ name: "3DS games", extensions: EXTENSIONS }],
  });
  if (files?.length) addGames(files);
}

async function addGames(files) {
  openModal(`<div class="modal-head"><h2>Adding games</h2></div>
    <div class="modal-body"><p id="copy-name">Copying...</p><div class="progress"><div id="copy-bar"></div></div></div>`, { locked: true });
  let result;
  try {
    result = await invoke("add_games", { files });
  } catch (e) {
    closeModal();
    return showMessage("Couldn't add that game", String(e));
  }
  closeModal();
  await refresh();
  const lines = [];
  if (result.added.length) {
    sound("done");
    const newest = lib.games.find((g) => result.added.some((n) => g.path.endsWith(n)));
    if (newest) select(newest.path, true);
    toast(result.added.length === 1 ? "Game added!" : `${result.added.length} games added!`);
  }
  if (result.archives.length) {
    lines.push(`<b>${result.archives.map(esc).join(", ")}</b> ${result.archives.length === 1 ? "is a compressed folder" : "are compressed folders"}. Open ${result.archives.length === 1 ? "it" : "them"} first (double-click), then add the game file inside.`);
  }
  if (result.unsupported.length) {
    lines.push(`<b>${result.unsupported.map(esc).join(", ")}</b> ${result.unsupported.length === 1 ? "isn't a 3DS game file" : "aren't 3DS game files"}. Game files end in .3ds, .cci, .cia, .cxi or .3dsx.`);
  }
  if (result.already.length && !result.added.length) toast("That game is already in your library.");
  if (lines.length) showMessage("Some files weren't added", lines.map((l) => `<p>${l}</p>`).join(""), true);
}

function saveStatus(g) {
  if (g.backed_up === true) return { cls: "ok", text: "Backed up" };
  if (g.backed_up === false) return { cls: "wait", text: "Backing up soon" };
  return { cls: "", text: "No save yet" };
}

function showSavesList(refocus) {
  if (!refocus) sound("open");
  const games = lib.games.filter((g) => g.title_id);
  games.sort((a, b) => (b.last_saved ?? 0) - (a.last_saved ?? 0));
  const b = lib.backup;
  const rows = games.map((g, i) => {
    const st = saveStatus(g);
    const saved = g.last_saved ? `Saved ${ago(new Date(g.last_saved))}` : "Not played yet";
    return `<div class="row game-row" data-g="${i}" role="button" tabindex="0">
      <span class="mini">${art(g)}</span>
      <span class="when">${esc(g.name)}<span class="sub"><span class="status-dot ${st.cls}"></span>${st.text} · ${saved}</span></span>
      <span class="chev">${icon("chev")}</span></div>`;
  });
  openModal(`<div class="modal-head"><h2>Your saves</h2><button class="x" data-close title="Close">${icon("close")}</button></div>
    <div class="modal-body">
      <div class="summary">
        <span class="chip ${b.on_target ? "ok" : "bad"}">${icon(b.on_target ? "check" : "warn")}${b.last ? `Last backup ${ago(stampDate(b.last))}` : "No backups yet"}</span>
        <span class="chip">${icon("save")}${b.count} backup${b.count === 1 ? "" : "s"} kept</span>
      </div>
      ${rows.length ? `<p>Pick a game to see every version of its save, and go back to any of them.</p><div class="rows">${rows.join("")}</div>`
        : `<div class="empty">No games with saves yet.<br>Play something and it shows up here.</div>`}
    </div>
    <div class="modal-foot"><button class="btn" id="saves-open">${icon("folder")} Open backups folder</button><button class="btn" id="saves-now">Back up now</button></div>`, { focus: refocus || "[data-g]" });
  $("modal-body").querySelectorAll("[data-g]").forEach((el) => {
    const open = () => showVersions(games[+el.dataset.g], true);
    el.onclick = open;
    el.onkeydown = (e) => e.key === "Enter" && open();
  });
  $("saves-open").onclick = () => invoke("open_folder", { which: "backups" });
  $("saves-now").onclick = async (ev) => {
    ev.currentTarget.disabled = true;
    try { await invoke("backup_now"); sound("done"); toast("Backed up! Your saves are safe."); }
    catch (e) { toast(`Backup failed: ${e}`, 5000); }
    await refresh();
    if (modalOpen) showSavesList("#saves-now");
  };
}

function dayLabel(d) {
  const days = Math.round((dayStart(new Date()) - dayStart(d)) / 864e5);
  if (days === 0) return "Today";
  if (days === 1) return "Yesterday";
  const opts = { weekday: "long", month: "long", day: "numeric" };
  if (d.getFullYear() !== new Date().getFullYear()) opts.year = "numeric";
  return d.toLocaleDateString([], opts);
}

async function showVersions(g, fromList) {
  sound("open");
  const lead = fromList ? `<button class="back" id="v-back" title="All saves">${icon("back")}</button>` : "";
  const head = `<div class="modal-head">${lead}<span class="mini">${art(g)}</span><h2>${esc(g.name)}</h2>
    <button class="x" data-close title="Close">${icon("close")}</button></div>`;
  const wireBack = () => { if (fromList) $("v-back").onclick = () => showSavesList(); };
  openModal(`${head}<div class="modal-body"><div class="spinner"></div></div>`);
  wireBack();
  let versions;
  try {
    versions = await invoke("save_versions", { titleId: g.title_id });
  } catch (e) {
    return showMessage("Couldn't read your backups", String(e));
  }
  if (!modalOpen) return;
  if (!versions.length) {
    $("modal-body").innerHTML = `${head}<div class="modal-body"><div class="empty">No backups of ${esc(g.name)} yet.<br>They're made automatically while you play.</div></div>`;
    $("modal-body").querySelector("[data-close]").onclick = closeModal;
    wireBack();
    return ($("v-back") || $("modal-body").querySelector("[data-close]")).focus();
  }
  let html = "", lastDay = "";
  versions.forEach((v, i) => {
    const d = stampDate(v.stamp);
    const day = dayLabel(d);
    if (day !== lastDay) {
      html += `${lastDay ? "</div>" : ""}<div class="day">${esc(day)}</div><div class="rows">`;
      lastDay = day;
    }
    html += `<div class="row"><span class="when">${clock(d)}<span class="sub">${ago(d)}</span></span>
      ${v.current ? `<span class="tag">Your save now</span>` : `<button class="btn" data-v="${i}">${icon("history")} Go back to this</button>`}</div>`;
  });
  html += "</div>";
  const n = versions.length;
  $("modal-body").innerHTML = `${head}<div class="modal-body">
    <p>${n} saved version${n === 1 ? "" : "s"}, newest first. Going back keeps a copy of your save now, so nothing is ever lost.</p>
    ${html}</div>`;
  $("modal-body").querySelector("[data-close]").onclick = closeModal;
  wireBack();
  $("modal-body").querySelectorAll("[data-v]").forEach((b) => (b.onclick = () => confirmRestore(g, versions[+b.dataset.v])));
  ($("modal-body").querySelector("[data-v]") || $("modal-body").querySelector("[data-close]")).focus();
}

function confirmRestore(g, v) {
  const when = ago(stampDate(v.stamp));
  openModal(`<div class="modal-head"><span class="mini">${art(g)}</span><h2>Go back?</h2></div>
    <div class="modal-body"><p>Put <b>${esc(g.name)}</b> back to how it was <b>${esc(when)}</b>?</p>
    <p>Don't worry: the save you have now is backed up first, so you can always come back to it.</p></div>
    <div class="modal-foot"><button class="btn" data-close>Cancel</button>
    <button class="btn primary" id="do-restore">${icon("history")} Go back</button></div>`);
  $("do-restore").onclick = async () => {
    $("do-restore").disabled = true;
    try {
      await invoke("restore", { zip: v.zip, gameKey: v.game_key });
    } catch (e) {
      return showMessage("Couldn't go back", String(e));
    }
    closeModal();
    sound("done");
    toast(`Done! ${g.name} is back to how it was ${when}.`, 4500);
    refresh();
  };
}

// Azahar's own settings that the app can change: [name, title, help, [[value, label], ...]]
const EMU = [
  ["resolution", "Render resolution", "Draws games at this many times the 3DS's own resolution (1× is 400×240), for cleaner edges and text on a big screen. Higher needs a faster computer; 3× suits most, a Steam Deck included.",
    [["1", "1×"], ["2", "2×"], ["3", "3×"], ["4", "4×"], ["5", "5×"]]],
  ["layout", "Screen layout", "How the two 3DS screens are arranged. F10 in a game switches between them.",
    [["0", "Stacked"], ["3", "Side by side"], ["2", "Big top"], ["1", "One screen"], ["5", "Hybrid"]]],
  ["swap", "Swap screens", "Show the bottom screen where the top one goes. F9 in a game does it too.", null],
  ["turbo", "Speed-up", "How fast the game runs while sped up. Press Tab in a game to speed up and again to stop.",
    [["150", "1.5×"], ["200", "2×"], ["300", "3×"], ["500", "5×"]]],
];

async function showSettings(refocus) {
  if (!refocus) sound("open");
  const emu = await invoke("emu_settings").catch(() => null);
  const b = lib.backup;
  const last = b.last ? `Last backup ${ago(stampDate(b.last))}` : "No backups yet";
  const auto = lib.autosave_minutes;
  const seg = [0, 5, 10, 15].map((m) => `<button data-auto="${m}" class="${m === auto ? "on" : ""}">${m ? `${m} min` : "Off"}</button>`).join("");
  openModal(`<div class="modal-head"><h2>Settings</h2><button class="x" data-close title="Close">${icon("close")}</button></div>
    <div class="modal-body">
      <div class="setting"><div class="label"><b>Save backups</b><span class="path">${esc(b.dir)}</span>
        <span>${b.on_target ? last : "This folder can't be found (is the drive plugged in?). Backups are kept on this computer until it's back."}</span></div>
        <div class="btns"><button class="btn" id="s-backup-now">Back up now</button><button class="btn" id="s-backup-dir">Change...</button><button class="btn" id="s-backup-open">Open</button></div></div>
      <div class="setting"><div class="label"><b>Look</b><span>Dark mode follows your computer unless you pick one.</span></div>
        <div class="seg">${["auto", "light", "dark"].map((t) => `<button data-look="${t}" class="${pref.get("theme", "auto") === t ? "on" : ""}">${t[0].toUpperCase() + t.slice(1)}</button>`).join("")}</div></div>
      <div class="setting"><div class="label"><b>Automatic save states</b><span>Saves your exact spot in the game every few minutes, as a safety net. Press Ctrl+V in a game to jump back to the newest one.</span></div>
        <div class="seg">${seg}</div></div>
      <div class="setting"><div class="label"><b>Play in full screen</b><span>Press F11 in the game to switch.</span></div>
        <button class="switch${lib.fullscreen ? " on" : ""}" id="s-full" aria-label="Play in full screen"></button></div>
      <div class="setting"><div class="label"><b>Sounds</b><span>Little clicks in this menu.</span></div>
        <button class="switch${pref.get("sounds", true) ? " on" : ""}" id="s-sound" aria-label="Sounds"></button></div>
      <div class="setting"><div class="label"><b>Games folder</b><span class="path">${esc(lib.games_dir)}</span></div>
        <div class="btns"><button class="btn" id="s-games-open">Open</button></div></div>
      ${emu ? `<h3 class="settings-group">Game settings</h3>${playing ? `<p class="settings-note">Close the game to change these.</p>` : ""}
      ${EMU.map(([name, title, help, choices]) => `<div class="setting"><div class="label"><b>${title}</b><span>${help}</span></div>
        ${choices
          ? `<div class="seg">${choices.map(([v, label]) => `<button data-emu="${name}" data-value="${v}" class="${emu[name] === v ? "on" : ""}"${playing ? " disabled" : ""}>${label}</button>`).join("")}</div>`
          : `<button class="switch${emu[name] === "true" ? " on" : ""}" data-emu-switch="${name}" aria-label="${title}"${playing ? " disabled" : ""}></button>`}</div>`).join("")}` : ""}
      <div class="setting"><div class="label"><b>Keyboard keys in a game</b><span>Handy for a Steam Deck's back buttons: set one to a key in Steam's controller settings.</span>
        <span class="keys"><b>Tab</b> speed up · <b>F1</b> quick save · <b>F2</b> quick load · <b>F9</b> swap screens · <b>F10</b> screen layout · <b>F11</b> full screen · <b>F4</b> pause</span></div></div>
      <div class="setting"><div class="label"><b>Controls and everything else</b><span>Change buttons and more in Azahar's own settings (Emulation &gt; Configure).</span></div>
        <div class="btns"><button class="btn" id="s-azahar">Open Azahar</button></div></div>
      <div class="setting"><div class="label"><b>Updates</b><span>${esc(updateText())}</span></div>
        <div class="btns">${update?.available
          ? `<button class="btn primary" id="s-update">${icon("download")} Update</button>`
          : `<button class="btn" id="s-check">Check now</button>`}</div></div>
    </div>`, { focus: refocus || "#s-backup-now" });
  $("modal-body").dataset.kind = "settings";
  if ($("s-update")) $("s-update").onclick = runUpdate;
  if ($("s-check")) $("s-check").onclick = async (ev) => {
    ev.currentTarget.disabled = true;
    ev.currentTarget.textContent = "Checking...";
    await checkUpdate();
    if (!update?.latest) toast("Couldn't check for updates. Are you online?");
    if (modalOpen) showSettings(update?.available ? "#s-update" : "#s-check");
  };
  document.querySelectorAll("[data-emu]").forEach((btn) => (btn.onclick = async () => {
    try { await invoke("set_emu_setting", { name: btn.dataset.emu, value: btn.dataset.value }); }
    catch (e) { return toast(String(e), 4000); }
    document.querySelectorAll(`[data-emu="${btn.dataset.emu}"]`).forEach((x) => x.classList.toggle("on", x === btn));
    sound("move");
  }));
  document.querySelectorAll("[data-emu-switch]").forEach((sw) => (sw.onclick = async () => {
    const on = !sw.classList.contains("on");
    try { await invoke("set_emu_setting", { name: sw.dataset.emuSwitch, value: String(on) }); }
    catch (e) { return toast(String(e), 4000); }
    sw.classList.toggle("on", on);
    sound("move");
  }));
  $("s-backup-now").onclick = async (ev) => {
    ev.currentTarget.disabled = true;
    try { await invoke("backup_now"); sound("done"); toast("Backed up! Your saves are safe."); }
    catch (e) { toast(`Backup failed: ${e}`, 5000); }
    await refresh();
    if (modalOpen) showSettings("#s-backup-now");
  };
  $("s-backup-dir").onclick = async () => {
    const folder = await dialog.open({ directory: true, title: "Where should your save backups go?" });
    if (!folder) return;
    try {
      const dir = await invoke("set_backup_dir", { folder });
      toast(`Backups now go to ${dir}`, 4500);
    } catch (e) { return showMessage("Couldn't use that folder", String(e)); }
    await refresh();
    showSettings("#s-backup-dir");
  };
  $("s-backup-open").onclick = () => invoke("open_folder", { which: "backups" });
  $("s-games-open").onclick = () => invoke("open_folder", { which: "games" });
  $("s-azahar").onclick = () => invoke("open_azahar").catch((e) => toast(String(e)));
  document.querySelectorAll("[data-auto]").forEach((btn) => (btn.onclick = async () => {
    await invoke("set_autosave", { minutes: +btn.dataset.auto });
    lib.autosave_minutes = +btn.dataset.auto;
    document.querySelectorAll("[data-auto]").forEach((x) => x.classList.toggle("on", x === btn));
    sound("move");
  }));
  document.querySelectorAll("[data-look]").forEach((btn) => (btn.onclick = () => {
    pref.set("theme", btn.dataset.look);
    applyTheme(btn.dataset.look);
    document.querySelectorAll("[data-look]").forEach((x) => x.classList.toggle("on", x === btn));
    sound("move");
  }));
  $("s-full").onclick = async (ev) => {
    lib.fullscreen = !lib.fullscreen;
    ev.currentTarget.classList.toggle("on", lib.fullscreen);
    await invoke("set_fullscreen", { on: lib.fullscreen });
  };
  $("s-sound").onclick = (ev) => {
    const on = !pref.get("sounds", true);
    pref.set("sounds", on);
    ev.currentTarget.classList.toggle("on", on);
    sound("move");
  };
}

// ---------------------------------------------------------------- updates

async function checkUpdate() {
  let found;
  try { found = await invoke("check_update"); } catch { return; }
  if (!found.latest && update?.available) return; // offline now: keep the update found earlier
  update = found;
  $("update-pill").hidden = !update.available;
  $("update-pill").title = update.latest ? `Update to ${update.latest}` : "Update 3DS";
}

function updateText() {
  if (!update?.latest) return "3DS checks for a newer version each time it starts.";
  if (update.available) return `${update.latest} is out${update.current ? ` (you have ${update.current})` : ""}.`;
  return `You have the newest version (${update.current}).`;
}

function confirmUpdate() {
  if (!update?.available) return;
  sound("open");
  openModal(`<div class="modal-head"><h2>Update 3DS?</h2></div>
    <div class="modal-body"><p>${esc(updateText())}</p>
      <p>Updating gets the newest 3DS app and Azahar. It takes a minute or two, then 3DS starts again by itself. Your games, saves and settings stay as they are.</p></div>
    <div class="modal-foot"><button class="btn" data-close>Not now</button>
      <button class="btn primary" id="do-update">${icon("download")} Update</button></div>`);
  $("do-update").onclick = runUpdate;
}

async function runUpdate() {
  if (playing) return showMessage("Close the game first", "Updating has to wait until you've finished playing.");
  openModal(`<div class="modal-head"><h2>Updating...</h2></div>
    <div class="modal-body"><div class="spinner"></div>
      <p class="wait-text">Getting the newest version. This takes a minute or two; 3DS starts again by itself when it's done.</p></div>`, { locked: true });
  try {
    await invoke("update_now");
  } catch (e) {
    closeModal();
    return showMessage("Couldn't update", String(e));
  }
  sound("done");
  if (lib?.gaming_mode) {
    // Steam only shows what it started, so 3DS can't reopen itself here.
    openModal(`<div class="modal-head"><h2>Updated!</h2></div>
      <div class="modal-body"><p>3DS closes now. Start it again from your Steam library to use the new version.</p></div>
      <div class="modal-foot"><button class="btn primary" id="do-relaunch">OK</button></div>`, { locked: true });
    $("do-relaunch").onclick = () => invoke("relaunch");
  } else {
    $("modal-body").querySelector(".wait-text").textContent = "Updated! Starting the new version...";
    setTimeout(() => invoke("relaunch"), 900);
  }
}

function showMessage(title, html, isHtml) {
  openModal(`<div class="modal-head"><h2>${esc(title)}</h2></div>
    <div class="modal-body">${isHtml ? html : `<p>${esc(html)}</p>`}</div>
    <div class="modal-foot"><button class="btn primary" data-close>OK</button></div>`);
}

// ---------------------------------------------------------------- modal

let returnFocus = null; // what had the focus before a dialog opened

/** Show a dialog. focus: a selector for what to start on (else its main button, else the first). */
function openModal(html, { locked = false, focus = null } = {}) {
  const wrap = $("modal"), body = $("modal-body");
  if (!modalOpen) returnFocus = document.activeElement;
  body.innerHTML = html;
  delete body.dataset.kind;
  wrap.hidden = false;
  wrap.dataset.locked = locked ? "1" : "";
  modalOpen = true;
  body.querySelectorAll("[data-close]").forEach((b) => (b.onclick = closeModal));
  ((focus && body.querySelector(focus)) || body.querySelector(".btn.primary:not([disabled])") || body.querySelector("button:not([disabled])"))?.focus();
}

function closeModal() {
  if (!modalOpen) return;
  $("modal").hidden = true;
  modalOpen = false;
  sound("back");
  if (returnFocus?.isConnected && returnFocus !== document.body) returnFocus.focus({ preventScroll: true });
  else if (padUsed()) selectedTile()?.focus({ preventScroll: true });
  returnFocus = null;
  onModalClose?.();
  onModalClose = null;
}

$("modal").addEventListener("mousedown", (e) => {
  if (e.target === $("modal") && !$("modal").dataset.locked) closeModal();
});

// ---------------------------------------------------------------- moving around
// Everything works without a mouse, from the arrow keys or a controller. On the main screen the
// D-pad moves through the games; past the grid's edge it goes on to the buttons around it (the
// toolbar, the selected game's Play and Older saves, the status pills), and B comes back to the
// games. In a dialog it moves between the dialog's buttons by where they are on screen.

const MAIN_ITEMS = '#top .pill:not([hidden]), #stage button:not([disabled]), .toolbar button, .grid button.tile';
const MODAL_ITEMS = 'button:not([disabled]), [tabindex="0"]';
const shown = (el) => el.getClientRects().length > 0;
const mainItems = () => [...document.querySelectorAll(MAIN_ITEMS)].filter(shown);
const selectedTile = () => $("grid").querySelector(".tile.selected") ?? $("grid").querySelector(".tile[data-i]");
const padUsed = () => document.documentElement.classList.contains("pad-nav");

/** The nearest of items from `from` going `dir`. sameRow keeps left/right on the same line. */
function nearest(items, from, dir, sameRow) {
  const box = (el) => { const r = el.getBoundingClientRect(); return [r.left + r.width / 2, r.top + r.height / 2, r.height]; };
  const [cx, cy, ch] = box(from);
  const vertical = dir === "up" || dir === "down";
  const sign = dir === "up" || dir === "left" ? -1 : 1;
  const found = []; // [element, distance that way, distance sideways]
  for (const el of items) {
    if (el === from) continue;
    const [x, y] = box(el);
    const along = (vertical ? y - cy : x - cx) * sign, across = Math.abs(vertical ? x - cx : y - cy);
    if (along < 4 || (sameRow && !vertical && across > ch)) continue;
    found.push([el, along, across]);
  }
  if (!found.length) return null;
  const least = (list, score) => list.reduce((a, b) => (score(b) < score(a) ? b : a))[0];
  if (!vertical) return least(found, ([, along, across]) => along + across * 3);
  // Up/down: the very next row (so no setting gets skipped), then what's closest in it.
  const row = Math.min(...found.map((f) => f[1]));
  return least(found.filter((f) => f[1] <= row + Math.max(ch / 2, 12)), ([, , across]) => across);
}

/** From a tile: the next tile in the grid, or past the grid's edge, the nearest button that way. */
function gridStep(tile, dir) {
  const tiles = [...$("grid").querySelectorAll("button.tile")];
  const i = tiles.indexOf(tile), col = i % COLUMNS, last = tiles.length - 1;
  let j = -1;
  if (dir === "left" && col > 0) j = i - 1;
  else if (dir === "right" && col < COLUMNS - 1 && i < last) j = i + 1;
  else if (dir === "up") j = i - COLUMNS;
  else if (dir === "down" && Math.floor(i / COLUMNS) < Math.floor(last / COLUMNS)) j = Math.min(i + COLUMNS, last);
  if (j >= 0) return tiles[j];
  const g = $("grid").getBoundingClientRect();
  const beyond = (el) => { const r = el.getBoundingClientRect(); return { up: r.bottom <= g.top, down: r.top >= g.bottom, left: r.right <= g.left, right: r.left >= g.right }[dir]; };
  const out = leave(tile, dir, mainItems().filter((el) => !el.classList.contains("tile") && beyond(el)));
  if (out) return out;
  // nothing beside the grid: left/right carry on to the row above/below
  if (dir === "left" && i > 0) return tiles[i - 1];
  if (dir === "right" && i < last) return tiles[i + 1];
  return null;
}

/** From a button outside the grid. */
function mainStep(cur, dir) {
  const next = (dir === "left" || dir === "right") && nearest(mainItems(), cur, dir, true);
  return next || leave(cur, dir, mainItems());
}

/** The best of `items` going `dir` from cur: up/down stay on the same screen while they can;
 *  left/right go across to the other screen when the screens sit side by side (Steam Deck),
 *  landing on Play there, or on the selected game. */
function leave(cur, dir, items) {
  const screen = (el) => el.closest(".screen");
  let next;
  if (dir === "up" || dir === "down") {
    next = nearest(items.filter((el) => screen(el) === screen(cur)), cur, dir) ?? nearest(items, cur, dir);
  } else {
    const s = screen(cur).getBoundingClientRect();
    const across = items.filter((el) => {
      const r = screen(el).getBoundingClientRect();
      return dir === "left" ? r.right <= s.left : r.left >= s.right;
    });
    const play = across.find((el) => el.id === "btn-play" || el.id === "welcome-add");
    next = play ?? nearest(across, cur, dir);
  }
  // Into the grid: land on the selected game, not whichever tile happens to be closest.
  if (next?.dataset.i !== undefined && cur.dataset.i === undefined) next = selectedTile() ?? next;
  return next;
}

function nav(dir) {
  document.documentElement.classList.add("pad-nav");
  if (modalOpen) return focusStep(dir);
  if (playing || !lib) return;
  let cur = document.activeElement;
  if (!mainItems().includes(cur)) {
    cur = selectedTile() ?? $("slot-add") ?? $("welcome-add");
    if (!cur) return;
    cur.focus({ preventScroll: true });
  }
  const next = cur.classList.contains("tile") ? gridStep(cur, dir) : mainStep(cur, dir);
  if (!next || next === cur) return;
  next.focus({ preventScroll: true });
  next.scrollIntoView({ block: "nearest" });
  sound("move");
}

/** Move the focus inside a dialog to the nearest thing in that direction. */
function focusStep(dir) {
  const body = $("modal-body");
  const items = [...body.querySelectorAll(MODAL_ITEMS)].filter(shown);
  const cur = document.activeElement;
  if (!items.includes(cur)) { items[0]?.focus(); return; }
  // The middle scrolls under a fixed top and bottom, so on screen a button scrolled out of view
  // can be "above" the close button. Up/down stay in their part, then go on to the next part.
  const PARTS = [".modal-head", ".modal-body", ".modal-foot"];
  const part = (el) => PARTS.findIndex((p) => el.closest(p));
  let best = nearest(items.filter((el) => part(el) === part(cur)), cur, dir, true);
  if (!best && (dir === "up" || dir === "down")) {
    for (let i = part(cur) + (dir === "up" ? -1 : 1); i >= 0 && i < PARTS.length && !best; i += dir === "up" ? -1 : 1) {
      best = nearest(items.filter((el) => part(el) === i), cur, dir, false);
    }
  }
  if (best) { best.focus(); best.scrollIntoView({ block: "nearest" }); sound("move"); }
}

/** A / Enter: play the focused game, or press the focused button. */
function activate() {
  const cur = document.activeElement;
  if (modalOpen) {
    if ($("modal-body").contains(cur)) cur.click();
    else focusStep("down");
    return;
  }
  if (cur?.dataset.i !== undefined && cur.classList.contains("tile")) play(lib.games[+cur.dataset.i]);
  else if (mainItems().includes(cur)) cur.click();
  else play(current());
}

/** B / Escape: close the dialog, or go back to the games. */
function back() {
  if (modalOpen) {
    if (!$("modal").dataset.locked) closeModal();
    return;
  }
  const cur = document.activeElement;
  if (mainItems().includes(cur) && cur.dataset.i === undefined) {
    selectedTile()?.focus({ preventScroll: true });
    sound("back");
  }
}

// Focusing a game (by controller, arrow keys or Tab) selects it.
$("grid").addEventListener("focusin", (e) => {
  const i = e.target.dataset?.i;
  if (i !== undefined && lib?.games[+i]) select(lib.games[+i].path, padUsed()); // nav() makes the sound
});
document.addEventListener("focusin", () => lib && renderHints());

const ARROWS = { ArrowLeft: "left", ArrowRight: "right", ArrowUp: "up", ArrowDown: "down" };
document.addEventListener("keydown", (e) => {
  if (e.key in ARROWS) { e.preventDefault(); nav(ARROWS[e.key]); }
  else if (e.key === "Escape") back();
  else if (e.key === "Enter") {
    // Enter presses a focused button by itself; games (play) and the status pill need a hand.
    const t = document.activeElement;
    if (!modalOpen && (t?.tagName !== "BUTTON" || t.dataset.i !== undefined)) { e.preventDefault(); activate(); }
  }
});

// Controllers come from the app (native, see pad.rs) as "pad" events, by button position:
// D-pad / left stick to move, A to play or press, B to go back, Y for the selected game's older
// saves, X for all saves, Start (☰) for settings. Held directions repeat.
let padCount = 0;
let padTimer = null;
listen("pads", ({ payload }) => { padCount = payload; if (lib) renderHints(); });
listen("pad", ({ payload: { button, down } }) => {
  const dir = ["up", "down", "left", "right"].includes(button);
  if (dir && !down) { clearTimeout(padTimer); padTimer = null; return; }
  if (!down || playing || document.hidden) return;
  document.documentElement.classList.add("pad-nav");
  if (dir) {
    clearTimeout(padTimer);
    nav(button);
    const again = (ms) => (padTimer = setTimeout(() => { nav(button); again(130); }, ms));
    again(380);
    return;
  }
  if (button === "a") return activate();
  if (button === "b") return back();
  if (modalOpen) {
    // Start again closes Settings
    if (button === "start" && $("modal-body").dataset.kind === "settings") closeModal();
    return;
  }
  const g = current();
  if (button === "y" && g?.title_id) showVersions(g, false);
  else if (button === "x" && lib) showSavesList();
  else if (button === "start" && lib) showSettings();
});
// the focus ring is for controller and keyboard use; the mouse doesn't need it
document.addEventListener("pointerdown", () => document.documentElement.classList.remove("pad-nav"));

// ---------------------------------------------------------------- wiring

$("btn-add").onclick = pickGames;
$("btn-folder").onclick = () => invoke("open_folder", { which: "games" });
$("btn-settings").onclick = () => lib && showSettings();
$("btn-saves").onclick = () => lib && showSavesList();
$("backup-pill").onclick = () => lib && showSettings();
$("update-pill").onclick = confirmUpdate;

listen("copy-progress", ({ payload }) => {
  const name = $("copy-name"), bar = $("copy-bar");
  if (!name) return;
  name.textContent = `Copying ${payload.name}...`;
  bar.style.width = `${(100 * payload.done) / Math.max(1, payload.total)}%`;
});
listen("status", ({ payload }) => toast(payload, 6000));
listen("playing", ({ payload }) => {
  if (!payload) { playing = null; refresh(); }
  else { playing = current()?.name ?? "your game"; if (lib) renderStage(); }
});
listen("backed-up", ({ payload }) => {
  if (payload) toast("Your save is backed up.");
  refresh();
});

window.__TAURI__.webviewWindow.getCurrentWebviewWindow().onDragDropEvent(({ payload }) => {
  const drop = $("drop");
  if (payload.type === "enter" || payload.type === "over") drop.classList.add("on");
  else if (payload.type === "leave") drop.classList.remove("on");
  else if (payload.type === "drop") {
    drop.classList.remove("on");
    if (payload.paths?.length && !playing) addGames(payload.paths);
  }
});

function tick() {
  const now = new Date();
  $("clock").textContent = clock(now);
  $("date").textContent = now.toLocaleDateString([], { weekday: "short", month: "numeric", day: "numeric" });
}
tick();
setInterval(tick, 10_000);

let lastRefresh = 0;
window.addEventListener("focus", () => {
  if (Date.now() - lastRefresh > 4000 && !modalOpen) { lastRefresh = Date.now(); refresh(); }
});
setInterval(() => !modalOpen && !document.hidden && refresh(), 60_000);

$("stage").innerHTML = `<div class="center-card"><div class="spinner"></div></div>`;
refresh();
checkUpdate();
