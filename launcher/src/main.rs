// "3DS": a game library in front of Azahar. It lists your games with their real icons, starts
// them in Azahar, and shows/restores the save backups made by the 3ds-setup watcher.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backups;
mod games;
mod pad;

use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, State};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A Command for another program. Inside our AppImage the environment points at the libraries
/// bundled with us (XDG_DATA_DIRS, GTK_PATH...); Azahar, python and xdg-open must not pick those up.
fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(target_os = "linux")]
    if let Some(appdir) = std::env::var("APPDIR").ok().filter(|d| d.len() > 1) {
        for (key, value) in std::env::vars_os() {
            let value = value.to_string_lossy();
            if !value.contains(&appdir) {
                continue;
            }
            let kept: Vec<&str> = value.split(':').filter(|s| !s.is_empty() && !s.starts_with(&appdir)).collect();
            if kept.is_empty() {
                cmd.env_remove(&key);
            } else {
                cmd.env(&key, kept.join(":"));
            }
        }
        for key in ["APPDIR", "APPIMAGE", "ARGV0", "OWD", "GDK_BACKEND", "GTK_THEME", "WEBKIT_DISABLE_DMABUF_RENDERER"] {
            cmd.env_remove(key);
        }
    }
    cmd
}

// ---------------------------------------------------------------- where things are
// Must match 3ds-setup.py / 3ds-setup.ps1.

struct Paths {
    install_dir: PathBuf,
    config: PathBuf,
    azahar: PathBuf,
    azahar_data: PathBuf,
    azahar_ini: PathBuf, // Azahar's settings (qt-config.ini)
    host: String,
    home: PathBuf,
    documents: PathBuf,
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key).filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn paths(app: &AppHandle) -> Paths {
    let home = app.path().home_dir().unwrap_or_else(|_| PathBuf::from("."));
    let documents = app.path().document_dir().unwrap_or_else(|_| home.join("Documents"));
    #[cfg(windows)]
    {
        let local = env_path("LOCALAPPDATA").unwrap_or_else(|| home.join("AppData/Local"));
        let roaming = env_path("APPDATA").unwrap_or_else(|| home.join("AppData/Roaming"));
        let azahar_dir = local.join("Programs").join("Azahar");
        let portable = azahar_dir.join("user");
        Paths {
            install_dir: local.join("3ds-setup"),
            config: roaming.join("3ds-setup").join("config.json"),
            azahar: azahar_dir.join("azahar.exe"),
            azahar_ini: if portable.is_dir() { portable.join("config/qt-config.ini") } else { roaming.join("Azahar/config/qt-config.ini") },
            azahar_data: if portable.is_dir() { portable } else { roaming.join("Azahar") },
            host: std::env::var("COMPUTERNAME").unwrap_or_default(),
            home,
            documents,
        }
    }
    #[cfg(not(windows))]
    {
        let xdg_data = env_path("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share"));
        let xdg_config = env_path("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
        let (install_dir, config, azahar, azahar_data, azahar_ini);
        if cfg!(target_os = "macos") {
            install_dir = home.join("Library/Application Support/3ds-setup");
            config = install_dir.join("config.json");
            azahar = home.join("Applications/Azahar.app/Contents/MacOS/azahar");
            let xdg = xdg_data.join("azahar-emu").exists() || xdg_config.join("azahar-emu").exists();
            azahar_data = if xdg { xdg_data.join("azahar-emu") } else { home.join("Library/Application Support/Azahar") };
            azahar_ini = if xdg { xdg_config.join("azahar-emu") } else { azahar_data.join("config") }.join("qt-config.ini");
        } else {
            install_dir = xdg_data.join("3ds-setup");
            config = xdg_config.join("3ds-setup/config.json");
            azahar = install_dir.join("azahar.AppImage");
            let flatpak = home.join(".var/app/org.azahar_emu.Azahar");
            let flatpak_data = flatpak.join("data/azahar-emu");
            (azahar_data, azahar_ini) = if flatpak_data.is_dir() {
                (flatpak_data, flatpak.join("config/azahar-emu/qt-config.ini"))
            } else {
                (xdg_data.join("azahar-emu"), xdg_config.join("azahar-emu/qt-config.ini"))
            };
        }
        Paths { install_dir, config, azahar, azahar_data, azahar_ini, host: hostname(), home, documents }
    }
}

#[cfg(unix)]
fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: buf is valid for buf.len() bytes; gethostname NUL-terminates on success.
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return String::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

impl Paths {
    fn heartbeat(&self) -> PathBuf {
        self.install_dir.join("watch.heartbeat")
    }
    fn fallback(&self) -> PathBuf {
        self.install_dir.join("pending-backups").join(&self.host)
    }
    fn launcher_state(&self) -> PathBuf {
        self.install_dir.join("launcher.json")
    }
}

// ---------------------------------------------------------------- settings (config.json)

/// Setup and the watcher use these files too. On Windows a file can't be opened while another
/// program is writing it, and a reader can catch it half-written, so keep trying for a moment.
fn read_json(path: &Path) -> Result<Map<String, Value>, String> {
    if !path.exists() {
        return Ok(Map::new());
    }
    let mut last = String::new();
    for _ in 0..30 {
        match std::fs::read_to_string(path) {
            // PowerShell writes a byte order mark
            Ok(text) => match serde_json::from_str(text.trim_start_matches('\u{feff}')) {
                Ok(map) => return Ok(map),
                Err(e) => last = e.to_string(),
            },
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!("couldn't read {}: {last}", path.display()))
}

fn write_json(path: &Path, map: &Map<String, Value>) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.partial");
    let text = serde_json::to_string_pretty(map).map_err(|e| e.to_string())? + "\n";
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    let mut result = Ok(());
    for _ in 0..30 {
        result = std::fs::rename(&tmp, path);
        if result.is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    result.map_err(|e| e.to_string())
}

struct Config {
    backup_dir: PathBuf,
    games_dir: PathBuf,
    autosave_minutes: u64,
    fullscreen: bool,
}

fn config(p: &Paths) -> Config {
    let c = read_json(&p.config).unwrap_or_default();
    let path = |k: &str, default: PathBuf| c.get(k).and_then(Value::as_str).map(PathBuf::from).unwrap_or(default);
    Config {
        backup_dir: path("backup_dir", p.home.join("3DS-Backups")),
        games_dir: path("games_dir", p.documents.join("3DS Games")),
        autosave_minutes: c.get("autosave_state_every_minutes").and_then(Value::as_u64).unwrap_or(10),
        fullscreen: c.get("launcher_fullscreen").and_then(Value::as_bool).unwrap_or(false),
    }
}

fn set_config_value(p: &Paths, key: &str, value: Value) -> Result<(), String> {
    // Never write settings we couldn't read: that would drop the other settings.
    let mut c = read_json(&p.config)?;
    c.insert(key.into(), value);
    write_json(&p.config, &c)
}

fn backup_saves_dirs(p: &Paths, cfg: &Config) -> (Vec<PathBuf>, bool) {
    let on_target = cfg.backup_dir.is_dir();
    let mut dirs = vec![p.fallback().join("saves")];
    if on_target {
        dirs.insert(0, cfg.backup_dir.join(&p.host).join("saves"));
    }
    (dirs, on_target)
}

// ---------------------------------------------------------------- the 3ds-setup script

#[cfg(windows)]
fn powershell() -> Command {
    let root = env_path("SystemRoot").unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let mut cmd = command(root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"));
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Keep a child going after we quit (its own session, so it isn't in our process group).
#[cfg(unix)]
fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: setsid is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

fn script_command(p: &Paths, args: &[&str]) -> Command {
    #[cfg(windows)]
    {
        let mut cmd = powershell();
        cmd.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]);
        cmd.arg(p.install_dir.join("3ds-setup.ps1"));
        // PowerShell spells options -Game / -Quiet
        cmd.args(args.iter().map(|a| match *a {
            "--game" => "-Game",
            "--quiet" => "-Quiet",
            a => a,
        }));
        cmd
    }
    #[cfg(not(windows))]
    {
        let bundled = p.install_dir.join("python/bin/python3");
        let mut cmd = command(if bundled.exists() { bundled } else { PathBuf::from("python3") });
        cmd.arg(p.install_dir.join("3ds-setup.py"));
        cmd.args(args);
        cmd
    }
}

fn run_script(p: &Paths, args: &[&str]) -> Result<(), String> {
    let out = script_command(p, args).output().map_err(|e| format!("couldn't run 3ds-setup: {e}"))?;
    match out.status.code() {
        Some(0) => Ok(()),
        Some(3) => Err("Please close the game first, then try again.".into()),
        _ => {
            let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
            let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("unknown error");
            Err(last.trim().to_string())
        }
    }
}

fn watcher_alive(p: &Paths) -> bool {
    std::fs::metadata(p.heartbeat())
        .and_then(|m| m.modified())
        .map(|t| t.elapsed().unwrap_or_default() < Duration::from_secs(90))
        .unwrap_or(false)
}

/// The watcher normally starts at login; start it if it isn't running (it refuses to run twice).
fn ensure_watcher(p: &Paths) {
    if watcher_alive(p) {
        return;
    }
    // Linux setups run it as a systemd user service. Starting it that way (rather than as our
    // child) also keeps Steam from thinking 3DS is still running after you quit it.
    #[cfg(target_os = "linux")]
    {
        let started = command("systemctl")
            .args(["--user", "start", "3ds-setup-watch.service"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if started.map(|s| s.success()).unwrap_or(false) {
            return;
        }
    }
    let mut cmd = script_command(p, &["watch"]);
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(unix)]
    detach(&mut cmd);
    let _ = cmd.spawn();
}

// ---------------------------------------------------------------- the library

#[derive(Serialize, Clone)]
struct Game {
    path: String,
    title_id: Option<String>,
    product_code: Option<String>,
    name: String,
    publisher: Option<String>,
    icon: Option<String>,
    kind: &'static str, // game | homebrew | cia (needs installing) | installed
    encrypted: bool,
    play_seconds: u64,
    last_saved: Option<u64>, // ms since epoch
    backed_up: Option<bool>,
    last_played: Option<u64>,
}

#[derive(Serialize)]
struct BackupInfo {
    dir: String,
    on_target: bool,
    last: Option<String>, // snapshot stamp, local time: 2026-10-23_19-30-00
    count: usize,
    watcher: bool,
}

#[derive(Serialize)]
struct Library {
    games: Vec<Game>,
    backup: BackupInfo,
    games_dir: String,
    autosave_minutes: u64,
    fullscreen: bool,
    azahar_ready: bool,
    playing: Option<String>,
    handheld: bool,
    gaming_mode: bool,
}

fn ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn now_ms() -> u64 {
    ms(SystemTime::now())
}

fn load_titles(p: &Paths) -> HashMap<String, String> {
    std::fs::read_to_string(p.install_dir.join("titles.json"))
        .ok()
        .and_then(|t| serde_json::from_str(t.trim_start_matches('\u{feff}')).ok())
        .unwrap_or_default()
}

/// Seconds played per title, from Azahar's own play-time file: pairs of u64 (title ID, seconds).
fn play_times(p: &Paths) -> HashMap<u64, u64> {
    let raw = std::fs::read(p.azahar_data.join("sysdata/play_time.bin")).unwrap_or_default();
    raw.chunks_exact(16)
        .map(|c| (u64::from_le_bytes(c[..8].try_into().unwrap()), u64::from_le_bytes(c[8..].try_into().unwrap())))
        .collect()
}

/// Where else to look for games, besides the games folder: the usual download spots, EmuDeck's
/// folder, and (on Linux) memory cards and USB drives, such as a Steam Deck's microSD card.
fn scan_places(p: &Paths) -> Vec<(PathBuf, u32)> {
    let mut places = vec![
        (p.home.join("Downloads"), 2),
        (p.home.join("Desktop"), 2),
        (p.home.join("Games"), 3),
        (p.home.join("Emulation/roms"), 2),
    ];
    #[cfg(target_os = "linux")]
    {
        let user = p.home.file_name().map(|n| n.to_owned()).unwrap_or_default();
        for media in [PathBuf::from("/run/media").join(&user), PathBuf::from("/run/media"), PathBuf::from("/media").join(&user)] {
            for drive in std::fs::read_dir(&media).into_iter().flatten().flatten() {
                let path = drive.path();
                if path.is_dir() && path.file_name() != Some(user.as_os_str()) {
                    places.push((path, 3));
                }
            }
        }
    }
    places
}

/// Folders that are big and never hold 3DS games, so scanning skips them.
const SKIP_DIRS: &[&str] = &["steamapps", "SteamLibrary", "compatdata", "shadercache", "node_modules", "lost+found"];

fn find_games(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let path = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
            continue;
        }
        if path.is_dir() {
            if depth > 0 {
                find_games(&path, depth - 1, out);
            }
        } else if path
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| games::EXTENSIONS.contains(&x.to_ascii_lowercase().as_str()))
        {
            out.push(path);
        }
    }
}

/// Games installed into Azahar (from .cia files): .../title/00040000/<id>/content/*.app
fn installed_games(p: &Paths) -> Vec<(PathBuf, games::RomInfo)> {
    let mut out = Vec::new();
    let sd = p.azahar_data.join("sdmc/Nintendo 3DS");
    for id0 in std::fs::read_dir(&sd).into_iter().flatten().flatten() {
        for id1 in std::fs::read_dir(id0.path()).into_iter().flatten().flatten() {
            let apps = id1.path().join("title/00040000");
            for title in std::fs::read_dir(&apps).into_iter().flatten().flatten() {
                let mut contents: Vec<PathBuf> = std::fs::read_dir(title.path().join("content"))
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("app")))
                    .collect();
                contents.sort();
                // The playable part is the content with an icon (the others are manuals etc).
                if let Some(found) = contents.into_iter().map(|c| (games::read_rom(&c), c)).find(|(i, _)| i.icon.is_some()) {
                    out.push((found.1, found.0));
                }
            }
        }
    }
    out
}

fn launcher_state(p: &Paths) -> Map<String, Value> {
    read_json(&p.launcher_state()).unwrap_or_default()
}

fn scan(p: &Paths, playing: Option<String>) -> Library {
    let cfg = config(p);
    let titles = load_titles(p);
    let times = play_times(p);
    let last_played = launcher_state(p).get("last_played").and_then(Value::as_object).cloned().unwrap_or_default();

    let (dirs, on_target) = backup_saves_dirs(p, &cfg);
    let snaps = backups::snapshots(&dirs);
    let newest = snaps.first().map(|s| backups::snapshot_fingerprints(s)).unwrap_or_default();

    let mut files = Vec::new();
    find_games(&cfg.games_dir, 3, &mut files);
    for (dir, depth) in scan_places(p) {
        find_games(&dir, depth, &mut files);
    }
    let mut seen = std::collections::HashSet::new();
    files.retain(|f| seen.insert(std::fs::canonicalize(f).unwrap_or_else(|_| f.clone())));
    let mut found: Vec<(PathBuf, games::RomInfo, &'static str)> = files
        .into_iter()
        .map(|f| {
            let info = games::read_rom(&f);
            let kind = if info.homebrew { "homebrew" } else if info.is_cia { "cia" } else { "game" };
            (f, info, kind)
        })
        .collect();
    // The same game found in two places shows once (the games folder's copy, found first).
    let mut ids = std::collections::HashSet::new();
    found.retain(|g| g.2 != "game" || g.1.title_id.is_none_or(|t| ids.insert(t)));
    let installed = installed_games(p);
    let have: Vec<u64> = found.iter().filter(|g| g.2 == "game").filter_map(|g| g.1.title_id).collect();
    let installed_ids: Vec<u64> = installed.iter().filter_map(|g| g.1.title_id).collect();
    // A .cia that's already installed shows as the installed game; a cartridge copy wins over both.
    found.retain(|g| g.2 != "cia" || !g.1.title_id.is_some_and(|t| installed_ids.contains(&t) || have.contains(&t)));
    for (path, info) in installed {
        if !info.title_id.is_some_and(|t| have.contains(&t)) {
            found.push((path, info, "installed"));
        }
    }
    // Updates and DLC aren't games of their own.
    found.retain(|g| g.1.title_id.is_none_or(|t| t >> 32 == 0x0004_0000 || g.2 == "homebrew"));

    let games = found
        .into_iter()
        .map(|(path, info, kind)| {
            let tid = info.title_id.filter(|_| kind != "homebrew").map(games::title_hex);
            let name = info
                .name
                .clone()
                .or_else(|| info.title_id.and_then(|t| games::known_name(t, &titles)))
                .unwrap_or_else(|| games::tidy_file_name(&path.file_stem().unwrap_or_default().to_string_lossy()));
            let (mut last_saved, mut backed_up) = (None, None);
            if let Some(t) = &tid {
                let (disk, newest_file) = backups::disk_fingerprint(&p.azahar_data, t);
                last_saved = newest_file.map(ms);
                if !disk.is_empty() {
                    backed_up = Some(backups::game_in(&newest, t).is_some_and(|(_, fp)| fp == disk));
                }
            }
            let key = path.to_string_lossy().to_string();
            Game {
                title_id: tid,
                product_code: info.product_code.filter(|_| kind != "homebrew"),
                name,
                publisher: info.publisher,
                icon: info.icon,
                kind,
                encrypted: info.encrypted,
                play_seconds: info.title_id.and_then(|t| times.get(&t)).copied().unwrap_or(0),
                last_saved,
                backed_up,
                last_played: last_played.get(&key).and_then(Value::as_u64),
                path: key,
            }
        })
        .collect::<Vec<_>>();
    let mut games = games;
    games.sort_by(|a, b| {
        b.last_played.cmp(&a.last_played).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Library {
        games,
        backup: BackupInfo {
            dir: cfg.backup_dir.to_string_lossy().into(),
            on_target,
            last: snaps.first().and_then(|s| s.file_stem()).map(|s| s.to_string_lossy().chars().take(19).collect()),
            count: snaps.len(),
            watcher: watcher_alive(p),
        },
        games_dir: cfg.games_dir.to_string_lossy().into(),
        autosave_minutes: cfg.autosave_minutes,
        fullscreen: cfg.fullscreen,
        azahar_ready: p.azahar.exists(),
        playing,
        handheld: handheld(),
        gaming_mode: gaming_mode(),
    }
}

// ---------------------------------------------------------------- Steam Deck

/// A Steam Deck (Valve hardware, or SteamOS), or THREEDS_HANDHELD=1 to try the layout elsewhere.
/// It gets bigger text and a side-by-side layout that fills its wide, short screen.
fn handheld() -> bool {
    static IT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *IT.get_or_init(|| {
        if let Ok(v) = std::env::var("THREEDS_HANDHELD") {
            return v == "1";
        }
        let read = |f: &str| std::fs::read_to_string(f).unwrap_or_default();
        cfg!(target_os = "linux")
            && (read("/sys/class/dmi/id/sys_vendor").trim() == "Valve"
                || read("/etc/os-release").lines().any(|l| l == "ID=steamos"))
    })
}

/// Running inside Steam's Gaming Mode, which shows one full-screen window at a time.
fn gaming_mode() -> bool {
    std::env::var_os("GAMESCOPE_WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.eq_ignore_ascii_case("gamescope"))
}

// ---------------------------------------------------------------- commands

#[derive(Default)]
struct Playing(Mutex<Option<String>>);

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn library(app: AppHandle, playing: State<'_, Playing>) -> Result<Library, String> {
    let now = playing.0.lock().unwrap().clone();
    blocking(move || {
        let p = paths(&app);
        ensure_watcher(&p);
        scan(&p, now)
    })
    .await
}

#[derive(Serialize)]
struct Version {
    zip: String,
    stamp: String,
    game_key: String,
    current: bool,
}

#[tauri::command]
async fn save_versions(app: AppHandle, title_id: String) -> Result<Vec<Version>, String> {
    blocking(move || {
        let p = paths(&app);
        let cfg = config(&p);
        let (disk, _) = backups::disk_fingerprint(&p.azahar_data, &title_id);
        let mut out: Vec<Version> = Vec::new();
        let mut prev: Option<backups::Fingerprint> = None;
        for snap in backups::snapshots(&backup_saves_dirs(&p, &cfg).0).into_iter().take(800) {
            let Some((key, fp)) = backups::game_in(&backups::snapshot_fingerprints(&snap), &title_id) else {
                continue;
            };
            let stamp: String = snap.file_stem().unwrap_or_default().to_string_lossy().chars().take(19).collect();
            let version = Version { zip: snap.to_string_lossy().into(), stamp, game_key: key, current: fp == disk };
            if prev.as_ref() == Some(&fp) {
                // Same save as the newer snapshot: it was first backed up at this older time.
                *out.last_mut().unwrap() = version;
            } else if out.len() < 60 {
                out.push(version);
            } else {
                break;
            }
            prev = Some(fp);
        }
        out
    })
    .await
}

#[tauri::command]
async fn restore(app: AppHandle, playing: State<'_, Playing>, zip: String, game_key: String) -> Result<(), String> {
    if playing.0.lock().unwrap().is_some() {
        return Err("Please close the game first, then try again.".into());
    }
    blocking(move || run_script(&paths(&app), &["--quiet", "restore", &zip, "--game", &game_key])).await?
}

#[tauri::command]
async fn backup_now(app: AppHandle) -> Result<(), String> {
    blocking(move || run_script(&paths(&app), &["--quiet", "backup"])).await?
}

#[derive(Serialize, Clone)]
struct CopyProgress {
    name: String,
    done: u64,
    total: u64,
}

#[derive(Serialize, Default)]
struct AddResult {
    added: Vec<String>,
    already: Vec<String>,
    archives: Vec<String>,
    unsupported: Vec<String>,
}

#[tauri::command]
async fn add_games(app: AppHandle, files: Vec<String>) -> Result<AddResult, String> {
    blocking(move || {
        let p = paths(&app);
        let dir = config(&p).games_dir;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut result = AddResult::default();
        for f in files {
            let src = PathBuf::from(&f);
            let name = src.file_name().unwrap_or_default().to_string_lossy().to_string();
            let ext = src.extension().and_then(|x| x.to_str()).unwrap_or("").to_ascii_lowercase();
            if !games::EXTENSIONS.contains(&ext.as_str()) {
                if ["zip", "7z", "rar"].contains(&ext.as_str()) {
                    result.archives.push(name);
                } else {
                    result.unsupported.push(name);
                }
                continue;
            }
            let dest = dir.join(&name);
            if dest.exists() || src.parent() == Some(dir.as_path()) {
                result.already.push(name);
                continue;
            }
            copy_with_progress(&app, &src, &dest, &name).map_err(|e| format!("Couldn't copy {name}: {e}"))?;
            result.added.push(name);
        }
        Ok(result)
    })
    .await?
}

fn copy_with_progress(app: &AppHandle, src: &Path, dest: &Path, name: &str) -> std::io::Result<()> {
    use std::io::{Read, Write};
    let total = std::fs::metadata(src)?.len();
    let tmp = dest.with_extension("partial");
    let result = (|| {
        let mut input = std::fs::File::open(src)?;
        let mut output = std::fs::File::create(&tmp)?;
        let mut buf = vec![0; 4 << 20];
        let mut done = 0u64;
        loop {
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            output.write_all(&buf[..n])?;
            done += n as u64;
            let _ = app.emit("copy-progress", CopyProgress { name: name.into(), done, total });
        }
        output.sync_all()?;
        std::fs::rename(&tmp, dest)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// .cia files have to be installed into Azahar before they can be played.
fn install_cia(p: &Paths, cia: &Path) -> Result<PathBuf, String> {
    let info = games::read_rom(cia);
    if info.encrypted {
        return Err("This game file is encrypted, so Azahar can't play it. You need a decrypted copy.".into());
    }
    let status = command(&p.azahar)
        .arg("-i")
        .arg(cia)
        .status()
        .map_err(|e| format!("couldn't start Azahar: {e}"))?;
    if !status.success() {
        return Err("Azahar couldn't install this game. The file may be damaged or encrypted.".into());
    }
    installed_games(p)
        .into_iter()
        .find(|(_, i)| i.title_id.is_some() && i.title_id == info.title_id)
        .map(|(path, _)| path)
        .ok_or_else(|| "The game was installed but couldn't be found afterwards.".into())
}

#[tauri::command]
async fn play(app: AppHandle, playing: State<'_, Playing>, path: String, name: String) -> Result<(), String> {
    if playing.0.lock().unwrap().is_some() {
        return Err("A game is already running.".into());
    }
    let app2 = app.clone();
    let child = blocking(move || -> Result<std::process::Child, String> {
        let p = paths(&app2);
        if !p.azahar.exists() {
            return Err("Azahar isn't installed. Run 3DS Setup again.".into());
        }
        let mut target = PathBuf::from(&path);
        if target.extension().is_some_and(|x| x.to_string_lossy().to_ascii_lowercase().ends_with("cia")) {
            let _ = app2.emit("status", "Getting the game ready (only the first time)...");
            target = install_cia(&p, &target)?;
        }
        let mut state = launcher_state(&p);
        let played = state.entry("last_played").or_insert_with(|| json!({}));
        if let Some(m) = played.as_object_mut() {
            m.insert(path.clone(), json!(now_ms()));
        }
        let _ = write_json(&p.launcher_state(), &state);

        let mut cmd = command(&p.azahar);
        if config(&p).fullscreen {
            cmd.arg("-f");
        }
        cmd.arg(&target);
        if let Some(dir) = p.azahar.parent() {
            cmd.current_dir(dir);
        }
        cmd.spawn().map_err(|e| format!("couldn't start Azahar: {e}"))
    })
    .await??;

    *playing.0.lock().unwrap() = Some(name);
    let _ = app.emit("playing", true);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.minimize();
    }
    std::thread::spawn(move || watch_game(app, child));
    Ok(())
}

fn watch_game(app: AppHandle, mut child: std::process::Child) {
    let _ = child.wait();
    *app.state::<Playing>().0.lock().unwrap() = None;
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
    let _ = app.emit("playing", false);
    let p = paths(&app);
    // The watcher backs up a few seconds after Azahar closes; if it isn't running, do it here.
    let backed_up = if watcher_alive(&p) {
        std::thread::sleep(Duration::from_secs(8));
        true
    } else {
        run_script(&p, &["--quiet", "backup"]).is_ok()
    };
    let _ = app.emit("backed-up", backed_up);
}

/// Box art from GameTDB (the database other emulator front-ends use), cached on disk.
#[tauri::command]
async fn cover(app: AppHandle, code: String) -> Result<Option<String>, String> {
    blocking(move || {
        if code.len() != 4 || !code.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        let dir = paths(&app).install_dir.join("covers");
        let file = dir.join(format!("{code}.jpg"));
        let missing = dir.join(format!("{code}.none"));
        if !file.exists() {
            // Don't ask again for a few days about a game the site doesn't have.
            let asked = missing.metadata().and_then(|m| m.modified()).ok();
            if asked.is_some_and(|t| t.elapsed().unwrap_or_default() < Duration::from_secs(3 * 86400)) {
                return None;
            }
            let _ = std::fs::create_dir_all(&dir);
            match download_cover(&code) {
                Ok(Some(jpg)) => {
                    let tmp = file.with_extension("partial");
                    std::fs::write(&tmp, jpg).ok()?;
                    std::fs::rename(&tmp, &file).ok()?;
                }
                Ok(None) => {
                    let _ = std::fs::write(&missing, "");
                    return None;
                }
                Err(()) => return None, // offline: try again next time
            }
        }
        let jpg = std::fs::read(&file).ok()?;
        Some(format!("data:image/jpeg;base64,{}", games::base64(&jpg)))
    })
    .await
}

/// Ok(None): the site has no box art for this game. Err: the site couldn't be reached.
fn download_cover(code: &str) -> Result<Option<Vec<u8>>, ()> {
    use std::io::Read;
    // The code's last letter is the region the game was sold in.
    let home = match code.as_bytes()[3] {
        b'P' | b'V' | b'X' | b'Y' | b'Z' => "EN",
        b'J' => "JA",
        b'K' => "KO",
        b'D' => "DE",
        b'F' => "FR",
        b'S' => "ES",
        b'I' => "IT",
        b'H' => "NL",
        b'R' => "RU",
        _ => "US",
    };
    let mut reached = false;
    let mut regions = vec![home];
    regions.extend(["US", "EN", "JA"].into_iter().filter(|r| *r != home));
    for region in regions {
        let url = format!("https://art.gametdb.com/3ds/coverHQ/{region}/{code}.jpg");
        match ureq::get(&url).set("User-Agent", "3ds-setup").timeout(Duration::from_secs(20)).call() {
            Ok(resp) => {
                reached = true;
                let mut jpg = Vec::new();
                if resp.into_reader().take(8 << 20).read_to_end(&mut jpg).is_ok() && jpg.starts_with(&[0xFF, 0xD8]) {
                    return Ok(Some(jpg));
                }
            }
            Err(ureq::Error::Status(..)) => reached = true,
            Err(_) => {}
        }
    }
    if reached { Ok(None) } else { Err(()) }
}

#[tauri::command]
async fn open_azahar(app: AppHandle) -> Result<(), String> {
    let p = paths(&app);
    command(&p.azahar).spawn().map(|_| ()).map_err(|e| format!("couldn't start Azahar: {e}"))
}

#[tauri::command]
fn open_folder(app: AppHandle, which: String) -> Result<(), String> {
    let p = paths(&app);
    let cfg = config(&p);
    let dir = if which == "backups" { cfg.backup_dir } else { cfg.games_dir };
    let _ = std::fs::create_dir_all(&dir);
    let opener = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    command(opener).arg(&dir).spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_autosave(app: AppHandle, minutes: u64) -> Result<(), String> {
    set_config_value(&paths(&app), "autosave_state_every_minutes", json!(minutes))
}

#[tauri::command]
fn set_fullscreen(app: AppHandle, on: bool) -> Result<(), String> {
    set_config_value(&paths(&app), "launcher_fullscreen", json!(on))
}

// ---------------------------------------------------------------- Azahar's own settings

/// The Azahar settings the 3DS app can change: name -> (section, key, Azahar's default, allowed).
/// layout_option: 0 stacked, 1 one screen, 2 large top, 3 side by side, 5 hybrid.
const EMU_SETTINGS: &[(&str, &str, &str, &str, &[&str])] = &[
    ("resolution", "Renderer", "resolution_factor", "1", &["1", "2", "3", "4", "5", "6"]),
    ("layout", "Layout", "layout_option", "0", &["0", "1", "2", "3", "5"]),
    ("swap", "Layout", "swap_screen", "false", &["true", "false"]),
    ("turbo", "Renderer", "turbo_limit", "200", &["150", "200", "300", "500"]),
];

/// (start, end) line range of [section]'s body.
fn ini_section(lines: &[String], section: &str) -> Option<(usize, usize)> {
    let header = format!("[{section}]");
    let start = lines.iter().position(|l| l.trim() == header)? + 1;
    let end = lines[start..].iter().position(|l| l.starts_with('[')).map_or(lines.len(), |i| start + i);
    Some((start, end))
}

fn ini_value(lines: &[String], section: &str, key: &str) -> Option<String> {
    let (start, end) = ini_section(lines, section)?;
    let prefix = format!("{key}=");
    lines[start..end].iter().find_map(|l| l.strip_prefix(&prefix)).map(|v| v.trim().to_string())
}

#[tauri::command]
fn emu_settings(app: AppHandle) -> HashMap<String, String> {
    let p = paths(&app);
    let text = std::fs::read_to_string(&p.azahar_ini).unwrap_or_default();
    let lines: Vec<String> = text.lines().map(String::from).collect();
    EMU_SETTINGS
        .iter()
        .map(|(name, section, key, default, _)| {
            // Azahar marks a setting it left at its default with key\default=true
            let is_default = ini_value(&lines, section, &format!("{key}\\default")).as_deref() == Some("true");
            let value = ini_value(&lines, section, key).filter(|_| !is_default).unwrap_or_else(|| default.to_string());
            // "300.000000" style doubles from some versions
            let value = value.parse::<f64>().map(|f| if f.fract() == 0.0 { format!("{}", f as i64) } else { value.clone() }).unwrap_or(value);
            (name.to_string(), value)
        })
        .collect()
}

/// Change one of Azahar's settings. Azahar writes its settings back when it closes, so this
/// waits until no game is running.
#[tauri::command]
fn set_emu_setting(app: AppHandle, playing: State<'_, Playing>, name: String, value: String) -> Result<(), String> {
    if playing.0.lock().unwrap().is_some() {
        return Err("Close the game first, then change this.".into());
    }
    let (_, section, key, _, allowed) =
        EMU_SETTINGS.iter().find(|s| s.0 == name).ok_or_else(|| format!("unknown setting {name}"))?;
    if !allowed.contains(&value.as_str()) {
        return Err(format!("{value} isn't a choice for {name}"));
    }
    let p = paths(&app);
    let text = std::fs::read_to_string(&p.azahar_ini).unwrap_or_default();
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let (start, mut end) = ini_section(&lines, section).unwrap_or_else(|| {
        lines.extend([String::new(), format!("[{section}]")]);
        (lines.len(), lines.len())
    });
    // drop the old value and its "default" flag, then add ours at the end of the section
    let (plain, flag) = (format!("{key}="), format!("{key}\\default="));
    let mut i = start;
    while i < end {
        if lines[i].starts_with(&plain) || lines[i].starts_with(&flag) {
            lines.remove(i);
            end -= 1;
        } else {
            i += 1;
        }
    }
    while end > start && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    lines.insert(end, format!("{key}={value}"));
    if let Some(dir) = p.azahar_ini.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = p.azahar_ini.with_extension("ini.partial");
    std::fs::write(&tmp, lines.join("\n").trim_matches('\n').to_string() + "\n").map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &p.azahar_ini).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- updates
// The installed setup script knows which release it came from (build-release.py writes the tag
// into it), and a release's script installs that release's 3DS app. So updating is: fetch the
// newest script, run its "update" (an install with no questions), then restart the app.

const RELEASES: &str = "https://github.com/erickfm/3ds-setup/releases";

fn setup_script(p: &Paths) -> PathBuf {
    p.install_dir.join(if cfg!(windows) { "3ds-setup.ps1" } else { "3ds-setup.py" })
}

/// The release tag in a setup script: RELEASE_TAG = "v1.2.3" or $ReleaseTag = 'v1.2.3'.
/// None for a script that isn't from a release (a checkout or a local build).
fn script_release(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix("RELEASE_TAG = ").or_else(|| l.strip_prefix("$ReleaseTag = ")))
        .map(|v| v.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
        .filter(|v| !v.is_empty())
}

/// v1.10.0 -> [1, 10, 0]
fn version_parts(tag: &str) -> Option<Vec<u64>> {
    tag.trim_start_matches('v').split('.').map(|n| n.parse().ok()).collect()
}

fn is_newer(latest: &str, current: &str) -> bool {
    match (version_parts(latest), version_parts(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// The newest release's tag: github.com/.../releases/latest redirects to .../releases/tag/<tag>.
/// (Not the GitHub API, which allows only 60 requests an hour per address.)
fn latest_release() -> Option<String> {
    let resp = ureq::head(&format!("{RELEASES}/latest"))
        .set("User-Agent", "3ds-setup")
        .timeout(Duration::from_secs(15))
        .call()
        .ok()?;
    let tag = resp.get_url().rsplit_once("/tag/")?.1.trim_end_matches('/').to_string();
    version_parts(&tag).map(|_| tag)
}

#[derive(Serialize)]
struct UpdateInfo {
    current: Option<String>,
    latest: Option<String>,
    available: bool,
}

#[tauri::command]
async fn check_update(app: AppHandle) -> Result<UpdateInfo, String> {
    blocking(move || {
        let p = paths(&app);
        let current = std::fs::read_to_string(setup_script(&p)).ok().and_then(|t| script_release(&t));
        let latest = latest_release();
        // Without a release tag (a test build), any release counts as newer.
        let available = latest.as_deref().is_some_and(|l| current.as_deref().is_none_or(|c| is_newer(l, c)));
        UpdateInfo { current, latest, available }
    })
    .await
}

fn download(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let resp = ureq::get(url)
        .set("User-Agent", "3ds-setup")
        .timeout(Duration::from_secs(60))
        .call()
        .map_err(|e| format!("Couldn't download the update. Are you online? ({e})"))?;
    let mut body = Vec::new();
    resp.into_reader().take(limit + 1).read_to_end(&mut body).map_err(|e| e.to_string())?;
    if body.len() as u64 > limit {
        return Err("The update download was too big. Try again later.".into());
    }
    Ok(body)
}

/// The last line of every setup script, so a download that was cut short can be told apart.
const SCRIPT_END: &str = "# end of 3ds-setup";

/// A whole setup script from a release, with the update command (not an error page or a
/// download that was cut short).
fn complete_script(text: &str) -> bool {
    let last = text.trim_end().lines().last().unwrap_or("");
    last.starts_with(SCRIPT_END) && (text.contains("\"update\"") || text.contains("'update'")) && script_release(text).is_some()
}

/// Put the newest release's setup script where the installed one is.
fn fetch_setup_script(p: &Paths) -> Result<(), String> {
    let bytes = if cfg!(windows) {
        // Windows gets one .bat with the PowerShell script after a marker (see build-release.py).
        let bat = download(&format!("{RELEASES}/latest/download/3DS-Setup-Windows.bat"), 4 << 20)?;
        let text = String::from_utf8_lossy(&bat);
        let at = text.rfind("###PS1###").ok_or("The update download was damaged. Try again later.")?;
        let ps1 = text[at + "###PS1###".len()..].trim_start();
        // PowerShell 5.1 reads a script without a byte order mark as the local code page.
        [&[0xEF, 0xBB, 0xBF][..], ps1.as_bytes()].concat()
    } else {
        download(&format!("{RELEASES}/latest/download/3ds-setup.py"), 4 << 20)?
    };
    if !complete_script(&String::from_utf8_lossy(&bytes)) {
        return Err("The update download was damaged. Try again later.".into());
    }
    let dest = setup_script(p);
    let tmp = dest.with_extension("partial");
    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
    }
    std::fs::rename(&tmp, &dest).map_err(|e| e.to_string())
}

/// Download and install the newest release: the 3DS app, Azahar, and the backup watcher.
/// The app keeps running (its file is swapped underneath it); call relaunch afterwards.
#[tauri::command]
async fn update_now(app: AppHandle, playing: State<'_, Playing>) -> Result<(), String> {
    if playing.0.lock().unwrap().is_some() {
        return Err("Please close the game first, then try again.".into());
    }
    blocking(move || {
        let p = paths(&app);
        // Keep the installed script until the update works: it's what backups run, and its
        // release tag is the version check_update reports as installed.
        let script = setup_script(&p);
        let kept = script.with_extension("previous");
        std::fs::copy(&script, &kept).map_err(|e| e.to_string())?;
        let restore = || {
            let _ = std::fs::rename(&kept, &script);
        };
        if let Err(e) = fetch_setup_script(&p) {
            restore();
            return Err(e);
        }
        // Output goes to a file, not a pipe: the watcher that setup restarts would otherwise hold
        // the pipe open and this would wait for it forever.
        let log = p.install_dir.join("update.log");
        let out = std::fs::File::create(&log).map_err(|e| e.to_string())?;
        let err = out.try_clone().map_err(|e| e.to_string())?;
        let status = script_command(&p, &["--quiet", "update"])
            .stdin(std::process::Stdio::null())
            .stdout(out)
            .stderr(err)
            .status()
            .map_err(|e| format!("couldn't run 3ds-setup: {e}"))?;
        if status.success() {
            let _ = std::fs::remove_file(&kept);
            return Ok(());
        }
        restore();
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("unknown error").trim();
        Err(format!("{last}\n\nDetails are in {}", log.display()))
    })
    .await?
}

/// Close, and (outside Steam's Gaming Mode) start again once this copy has quit, so the new
/// version opens. In Gaming Mode Steam only shows what it started, so you start 3DS again there.
#[tauri::command]
fn relaunch(app: AppHandle) {
    if !gaming_mode() {
        let p = paths(&app);
        let pid = std::process::id().to_string();
        #[cfg(windows)]
        {
            let exe = p.install_dir.join("3DS.exe").to_string_lossy().replace('\'', "''");
            let _ = powershell()
                .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command"])
                .arg(format!("Wait-Process -Id {pid} -ErrorAction SilentlyContinue; Start-Process -FilePath '{exe}'"))
                .spawn();
        }
        #[cfg(unix)]
        {
            let (opener, target) = if cfg!(target_os = "macos") {
                ("open", p.home.join("Applications/3DS.app"))
            } else {
                ("", p.install_dir.join("3ds-launcher"))
            };
            let mut cmd = command("sh");
            cmd.args(["-c", r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; sleep 0.3; if [ -n "$3" ]; then exec "$3" "$2"; else exec "$2"; fi"#, "sh", &pid])
                .arg(target)
                .arg(opener)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            detach(&mut cmd);
            let _ = cmd.spawn();
        }
    }
    app.exit(0);
}

/// Point backups at a new folder (a "3DS-Backups" folder inside the one picked), bringing
/// along this computer's existing backups. Nothing is deleted from the old folder.
#[tauri::command]
async fn set_backup_dir(app: AppHandle, folder: String) -> Result<String, String> {
    blocking(move || {
        let p = paths(&app);
        let old = config(&p).backup_dir.join(&p.host);
        let picked = PathBuf::from(folder);
        let new_dir = if picked.file_name().is_some_and(|n| n == "3DS-Backups") { picked } else { picked.join("3DS-Backups") };
        std::fs::create_dir_all(&new_dir).map_err(|e| format!("Can't use that folder: {e}"))?;
        let dest = new_dir.join(&p.host);
        if old.is_dir() && !dest.exists() && old != dest {
            copy_dir(&old, &dest).map_err(|e| format!("Couldn't copy your backups: {e}"))?;
        }
        set_config_value(&p, "backup_dir", json!(new_dir.to_string_lossy()))?;
        Ok(new_dir.to_string_lossy().to_string())
    })
    .await?
}

fn copy_dir(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let target = dest.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &target)?;
        } else {
            std::fs::copy(e.path(), target)?;
        }
    }
    Ok(())
}

fn main() {
    // Our AppImage brings its own WebKitGTK, and its DMA-BUF renderer can show a blank window with
    // the system's graphics drivers (e.g. under the Steam Deck's Gaming Mode). Child programs
    // don't inherit this; see command().
    #[cfg(target_os = "linux")]
    if std::env::var_os("APPIMAGE").is_some() && std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            pad::start(app.handle().clone());
            if let Some(w) = app.get_webview_window("main") {
                // Fill small screens (the Deck's is 1280x800) instead of overflowing them, and the
                // whole screen in Gaming Mode. Sized explicitly too, in case nothing honors the request.
                let screen = w.current_monitor().ok().flatten().map(|m| m.size().to_logical::<f64>(m.scale_factor()));
                let small = screen.is_some_and(|s| s.width < 1200.0 || s.height < 900.0);
                if gaming_mode() || handheld() || small {
                    if let Some(s) = screen {
                        let _ = w.set_size(tauri::LogicalSize::new(s.width, s.height));
                        let _ = w.set_position(tauri::LogicalPosition::new(0.0, 0.0));
                    }
                    let _ = if gaming_mode() { w.set_fullscreen(true) } else { w.maximize() };
                }
            }
            Ok(())
        })
        .manage(Playing::default())
        .invoke_handler(tauri::generate_handler![
            library,
            save_versions,
            restore,
            backup_now,
            add_games,
            play,
            cover,
            open_azahar,
            open_folder,
            set_autosave,
            set_fullscreen,
            emu_settings,
            set_emu_setting,
            set_backup_dir,
            check_update,
            update_now,
            relaunch
        ])
        .run(tauri::generate_context!())
        .expect("error while running the 3DS launcher");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_tag_in_scripts() {
        assert_eq!(script_release("x = 1\nRELEASE_TAG = \"v1.6.0\"\n").as_deref(), Some("v1.6.0"));
        assert_eq!(script_release("$ReleaseTag = 'v1.6.0'\r\n").as_deref(), Some("v1.6.0"));
        assert_eq!(script_release("RELEASE_TAG = \"\"\n"), None);
        assert_eq!(script_release("$ReleaseTag = ''"), None);
    }

    #[test]
    fn whole_scripts_only() {
        let py = std::fs::read_to_string("../mac-linux/3ds-setup.py").unwrap().replace("RELEASE_TAG = \"\"", "RELEASE_TAG = \"v1.6.0\"");
        let ps = std::fs::read_to_string("../windows/3ds-setup.ps1").unwrap().replace("$ReleaseTag = ''", "$ReleaseTag = 'v1.6.0'");
        for text in [py, ps] {
            assert!(complete_script(&text));
            assert!(!complete_script(&text[..text.len() * 2 / 3]));
        }
        assert!(!complete_script("<html>Not Found</html>"));
    }

    #[test]
    fn newer_versions() {
        assert!(is_newer("v1.6.0", "v1.5.0"));
        assert!(is_newer("v1.10.0", "v1.9.2"));
        assert!(!is_newer("v1.5.0", "v1.5.0"));
        assert!(!is_newer("v1.4.0", "v1.5.0"));
        assert!(!is_newer("nightly", "v1.5.0"));
    }
}
