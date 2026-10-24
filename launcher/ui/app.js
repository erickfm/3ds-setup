"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const dialog = window.__TAURI__.dialog;
const $ = (id) => document.getElementById(id);

const EXTENSIONS = ["3ds", "cci", "cxi", "3dsx", "cia", "zcci", "zcxi", "z3dsx", "zcia"];
const COLUMNS = 5;

let lib = null;        // what the backend found: games, backups, settings
let selected = null;   // path of the selected game
let playing = null;    // name of the game running in Azahar
let modalOpen = false;
let onModalClose = null;

// ---------------------------------------------------------------- little helpers

const pref = {
  get(key, fallback) {
    try { const v = localStorage.getItem(key); return v === null ? fallback : JSON.parse(v); } catch { return fallback; }
  },
  set(key, value) {
    try { localStorage.setItem(key, JSON.stringify(value)); } catch { /* private mode etc. */ }
  },
};

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
  if (!lib.games.some((g) => g.path === selected)) selected = lib.games[0]?.path ?? null;
  render();
}

const current = () => lib?.games.find((g) => g.path === selected) ?? null;

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

function renderStage() {
  const stage = $("stage");
  const g = current();
  stage.style.animation = "none";
  void stage.offsetWidth; // restart the little entrance animation
  stage.style.animation = "";

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

  stage.innerHTML = `
    <div class="banner-icon">${art(g)}</div>
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
  if (g.title_id) $("btn-history").onclick = () => showVersions(g);
}

function renderGrid() {
  const grid = $("grid");
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
}

function renderHints() {
  const pad = [...(navigator.getGamepads?.() ?? [])].some(Boolean);
  $("hints").innerHTML = !lib.games.length ? `<span>Your games show up here</span>`
    : pad ? `<span><span class="key">A</span>Play</span><span><span class="key">B</span>Back</span><span><span class="key">✚</span>Choose</span>`
    : `<span>Double-click a game to play</span><span><span class="key">⏎</span>Play</span><span><span class="key">←→</span>Choose</span>`;
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

function move(delta) {
  if (!lib?.games.length || modalOpen || playing) return;
  const i = Math.max(0, lib.games.findIndex((g) => g.path === selected));
  const next = Math.min(lib.games.length - 1, Math.max(0, i + delta));
  select(lib.games[next].path);
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

async function showVersions(g) {
  sound("open");
  const head = `<div class="modal-head"><span class="mini">${art(g)}</span><h2>Older saves</h2>
    <button class="x" data-close title="Close">${icon("close")}</button></div>`;
  openModal(`${head}<div class="modal-body"><div class="spinner"></div></div>`);
  let versions;
  try {
    versions = await invoke("save_versions", { titleId: g.title_id });
  } catch (e) {
    return showMessage("Couldn't read your backups", String(e));
  }
  if (!modalOpen) return;
  if (!versions.length) {
    $("modal-body").innerHTML = `${head}<div class="modal-body"><div class="empty">No backups of ${esc(g.name)} yet.<br>They're made automatically while you play.</div></div>`;
    return;
  }
  const rows = versions.map((v, i) => {
    const d = stampDate(v.stamp);
    return `<div class="row"><span class="when">${friendly(d)}<span class="sub">${ago(d)}</span></span>
      ${v.current ? `<span class="tag">Your save now</span>` : `<button class="btn" data-v="${i}">Go back to this</button>`}</div>`;
  });
  $("modal-body").innerHTML = `${head}<div class="modal-body">
    <p>Each line is your ${esc(g.name)} save as it was at that time. Pick one to go back to it.</p>
    <div class="rows">${rows.join("")}</div></div>`;
  $("modal-body").querySelectorAll("[data-v]").forEach((b) => (b.onclick = () => confirmRestore(g, versions[+b.dataset.v])));
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

function showSettings() {
  sound("open");
  const b = lib.backup;
  const last = b.last ? `Last backup ${ago(stampDate(b.last))}` : "No backups yet";
  const auto = lib.autosave_minutes;
  const seg = [0, 5, 10, 15].map((m) => `<button data-auto="${m}" class="${m === auto ? "on" : ""}">${m ? `${m} min` : "Off"}</button>`).join("");
  openModal(`<div class="modal-head"><h2>Settings</h2><button class="x" data-close title="Close">${icon("close")}</button></div>
    <div class="modal-body">
      <div class="setting"><div class="label"><b>Save backups</b><span class="path">${esc(b.dir)}</span>
        <span>${b.on_target ? last : "This folder can't be found (is the drive plugged in?). Backups are kept on this computer until it's back."}</span></div>
        <div class="btns"><button class="btn" id="s-backup-now">Back up now</button><button class="btn" id="s-backup-dir">Change...</button><button class="btn" id="s-backup-open">Open</button></div></div>
      <div class="setting"><div class="label"><b>Automatic save states</b><span>Saves your exact spot in the game every few minutes, as a safety net. Press Ctrl+V in a game to jump back to the newest one.</span></div>
        <div class="seg">${seg}</div></div>
      <div class="setting"><div class="label"><b>Play in full screen</b><span>Press F11 in the game to switch.</span></div>
        <button class="switch${lib.fullscreen ? " on" : ""}" id="s-full" aria-label="Play in full screen"></button></div>
      <div class="setting"><div class="label"><b>Sounds</b><span>Little clicks in this menu.</span></div>
        <button class="switch${pref.get("sounds", true) ? " on" : ""}" id="s-sound" aria-label="Sounds"></button></div>
      <div class="setting"><div class="label"><b>Games folder</b><span class="path">${esc(lib.games_dir)}</span></div>
        <div class="btns"><button class="btn" id="s-games-open">Open</button></div></div>
      <div class="setting"><div class="label"><b>Controls and graphics</b><span>Change buttons, screen layout and more in Azahar's own settings (Emulation &gt; Configure).</span></div>
        <div class="btns"><button class="btn" id="s-azahar">Open Azahar</button></div></div>
    </div>`);
  $("s-backup-now").onclick = async (ev) => {
    ev.currentTarget.disabled = true;
    try { await invoke("backup_now"); sound("done"); toast("Backed up! Your saves are safe."); }
    catch (e) { toast(`Backup failed: ${e}`, 5000); }
    await refresh();
    if (modalOpen) showSettings();
  };
  $("s-backup-dir").onclick = async () => {
    const folder = await dialog.open({ directory: true, title: "Where should your save backups go?" });
    if (!folder) return;
    try {
      const dir = await invoke("set_backup_dir", { folder });
      toast(`Backups now go to ${dir}`, 4500);
    } catch (e) { return showMessage("Couldn't use that folder", String(e)); }
    await refresh();
    showSettings();
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

function showMessage(title, html, isHtml) {
  openModal(`<div class="modal-head"><h2>${esc(title)}</h2></div>
    <div class="modal-body">${isHtml ? html : `<p>${esc(html)}</p>`}</div>
    <div class="modal-foot"><button class="btn primary" data-close>OK</button></div>`);
}

// ---------------------------------------------------------------- modal

function openModal(html, { locked = false } = {}) {
  const wrap = $("modal");
  $("modal-body").innerHTML = html;
  wrap.hidden = false;
  wrap.dataset.locked = locked ? "1" : "";
  modalOpen = true;
  $("modal-body").querySelectorAll("[data-close]").forEach((b) => (b.onclick = closeModal));
  ($("modal-body").querySelector(".btn.primary:not([disabled])") || $("modal-body").querySelector("button"))?.focus();
}

function closeModal() {
  if (!modalOpen) return;
  $("modal").hidden = true;
  modalOpen = false;
  sound("back");
  onModalClose?.();
  onModalClose = null;
}

$("modal").addEventListener("mousedown", (e) => {
  if (e.target === $("modal") && !$("modal").dataset.locked) closeModal();
});

// ---------------------------------------------------------------- input

document.addEventListener("keydown", (e) => {
  if (modalOpen) {
    if (e.key === "Escape" && !$("modal").dataset.locked) closeModal();
    return;
  }
  const keys = { ArrowLeft: -1, ArrowRight: 1, ArrowUp: -COLUMNS, ArrowDown: COLUMNS };
  if (e.key in keys) { e.preventDefault(); move(keys[e.key]); }
  else if (e.key === "Enter" && document.activeElement?.tagName !== "BUTTON") play(current());
});

// Controllers: D-pad / left stick to choose, A to play, B to close.
const padHeld = {};
let padRepeat = 0;
function pollPads() {
  const pad = [...(navigator.getGamepads?.() ?? [])].find(Boolean);
  if (pad) {
    const ax = pad.axes[0] ?? 0, ay = pad.axes[1] ?? 0;
    const dir = pad.buttons[14]?.pressed || ax < -0.5 ? -1 : pad.buttons[15]?.pressed || ax > 0.5 ? 1
      : pad.buttons[12]?.pressed || ay < -0.5 ? -COLUMNS : pad.buttons[13]?.pressed || ay > 0.5 ? COLUMNS : 0;
    const now = performance.now();
    if (dir && (padHeld.dir !== dir || now > padRepeat)) {
      move(dir);
      padRepeat = now + (padHeld.dir === dir ? 140 : 380);
    }
    padHeld.dir = dir;
    for (const [i, action] of [[0, "a"], [1, "b"], [9, "a"]]) {
      const down = pad.buttons[i]?.pressed;
      if (down && !padHeld[i]) {
        if (action === "b" && modalOpen && !$("modal").dataset.locked) closeModal();
        if (action === "a") modalOpen ? document.activeElement?.click() : play(current());
      }
      padHeld[i] = down;
    }
  }
  requestAnimationFrame(pollPads);
}
window.addEventListener("gamepadconnected", () => lib && renderHints());
window.addEventListener("gamepaddisconnected", () => lib && renderHints());

// ---------------------------------------------------------------- wiring

$("btn-add").onclick = pickGames;
$("btn-folder").onclick = () => invoke("open_folder", { which: "games" });
$("btn-settings").onclick = () => lib && showSettings();
$("backup-pill").onclick = () => lib && showSettings();

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
requestAnimationFrame(pollPads);
