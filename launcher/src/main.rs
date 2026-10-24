// "3DS": a game library in front of Azahar. It lists your games with their real icons, starts
// them in Azahar, and shows/restores the save backups made by the 3ds-setup watcher.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backups;
mod games;

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

// ---------------------------------------------------------------- where things are
// Must match 3ds-setup.py / 3ds-setup.ps1.

struct Paths {
    install_dir: PathBuf,
    config: PathBuf,
    azahar: PathBuf,
    azahar_data: PathBuf,
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
        let (install_dir, config, azahar, azahar_data);
        if cfg!(target_os = "macos") {
            install_dir = home.join("Library/Application Support/3ds-setup");
            config = install_dir.join("config.json");
            azahar = home.join("Applications/Azahar.app/Contents/MacOS/azahar");
            azahar_data = if xdg_data.join("azahar-emu").exists() || xdg_config.join("azahar-emu").exists() {
                xdg_data.join("azahar-emu")
            } else {
                home.join("Library/Application Support/Azahar")
            };
        } else {
            install_dir = xdg_data.join("3ds-setup");
            config = xdg_config.join("3ds-setup/config.json");
            azahar = install_dir.join("azahar.AppImage");
            let flatpak = home.join(".var/app/org.azahar_emu.Azahar/data/azahar-emu");
            azahar_data = if flatpak.is_dir() { flatpak } else { xdg_data.join("azahar-emu") };
        }
        Paths { install_dir, config, azahar, azahar_data, host: hostname(), home, documents }
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

fn script_command(p: &Paths, args: &[&str]) -> Command {
    #[cfg(windows)]
    {
        let root = env_path("SystemRoot").unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let mut cmd = Command::new(root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"));
        cmd.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]);
        cmd.arg(p.install_dir.join("3ds-setup.ps1"));
        // PowerShell spells options -Game / -Quiet
        cmd.args(args.iter().map(|a| match *a {
            "--game" => "-Game",
            "--quiet" => "-Quiet",
            a => a,
        }));
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd
    }
    #[cfg(not(windows))]
    {
        let bundled = p.install_dir.join("python/bin/python3");
        let mut cmd = Command::new(if bundled.exists() { bundled } else { PathBuf::from("python3") });
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
    let mut cmd = script_command(p, &["watch"]);
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe; it detaches the watcher from our process group.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
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

fn find_games(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let path = e.path();
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
    let mut found: Vec<(PathBuf, games::RomInfo, &'static str)> = files
        .into_iter()
        .map(|f| {
            let info = games::read_rom(&f);
            let kind = if info.homebrew { "homebrew" } else if info.is_cia { "cia" } else { "game" };
            (f, info, kind)
        })
        .collect();
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
    }
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
    let status = Command::new(&p.azahar)
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

        let mut cmd = Command::new(&p.azahar);
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
    Command::new(&p.azahar).spawn().map(|_| ()).map_err(|e| format!("couldn't start Azahar: {e}"))
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
    Command::new(opener).arg(&dir).spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_autosave(app: AppHandle, minutes: u64) -> Result<(), String> {
    set_config_value(&paths(&app), "autosave_state_every_minutes", json!(minutes))
}

#[tauri::command]
fn set_fullscreen(app: AppHandle, on: bool) -> Result<(), String> {
    set_config_value(&paths(&app), "launcher_fullscreen", json!(on))
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
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
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
            set_backup_dir
        ])
        .run(tauri::generate_context!())
        .expect("error while running the 3DS launcher");
}
