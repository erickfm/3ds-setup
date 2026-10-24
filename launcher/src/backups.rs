// Reads the save snapshots the 3ds-setup watcher writes (<backup dir>/<computer>/saves/*.zip)
// to show which versions of a game's save exist. Restoring is left to the 3ds-setup script.

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// (path inside the zip, crc32, size) of every file belonging to one game.
pub type Fingerprint = BTreeSet<(String, u32, u64)>;

/// Title ID (16 hex, upper case) a save file belongs to. Mirrors title_of() in 3ds-setup.py.
fn title_of(rel: &str) -> Option<(String, &str)> {
    let p: Vec<&str> = rel.split('/').collect();
    if p.len() > 6 && p[0] == "sdmc" && (p[4] == "title" || p[4] == "extdata") {
        return Some(((p[5].to_string() + p[6]).to_uppercase(), p[4]));
    }
    None
}

/// Mirrors game_keys() in 3ds-setup.py: extdata is filed under the game with the same
/// unique ID when that game has a save in the same snapshot.
pub fn game_keys(names: &[String]) -> HashMap<String, String> {
    let mut by_unique = HashMap::new();
    for n in names {
        if let Some((t, "title")) = title_of(n) {
            if let Ok(lo) = u32::from_str_radix(&t[8..], 16) {
                by_unique.insert((lo >> 8) & 0xFFFFF, t);
            }
        }
    }
    let mut keys = HashMap::new();
    for n in names {
        if let Some((t, kind)) = title_of(n) {
            let key = match (kind, u32::from_str_radix(&t[8..], 16)) {
                ("extdata", Ok(lo)) => by_unique.get(&lo).cloned().unwrap_or(t),
                _ => t,
            };
            keys.insert(n.clone(), key);
        }
    }
    keys
}

/// Key a game's extdata has when the snapshot holds no title save for it.
pub fn extdata_key(title_id: &str) -> String {
    let lo = u32::from_str_radix(&title_id[8..], 16).unwrap_or(0);
    format!("00000000{:08X}", (lo >> 8) & 0xFFFFF)
}

pub fn snapshots(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut snaps: Vec<PathBuf> = dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "zip"))
        .collect();
    snaps.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    snaps
}

/// Every game's fingerprint in one snapshot, keyed by title ID.
pub fn snapshot_fingerprints(zip_path: &Path) -> HashMap<String, Fingerprint> {
    let mut out: HashMap<String, Fingerprint> = HashMap::new();
    let Ok(file) = File::open(zip_path) else { return out };
    let Ok(mut zip) = zip::ZipArchive::new(file) else { return out };
    let mut entries = Vec::new();
    for i in 0..zip.len() {
        if let Ok(f) = zip.by_index_raw(i) {
            if !f.is_dir() && !f.name().starts_with("config/") {
                entries.push((f.name().to_string(), f.crc32(), f.size()));
            }
        }
    }
    let names: Vec<String> = entries.iter().map(|e| e.0.clone()).collect();
    let keys = game_keys(&names);
    for e in entries {
        if let Some(k) = keys.get(&e.0) {
            out.entry(k.clone()).or_default().insert(e);
        }
    }
    out
}

/// One game's save in a snapshot: its own key, or its extdata on its own.
pub fn game_in(fps: &HashMap<String, Fingerprint>, title_id: &str) -> Option<(String, Fingerprint)> {
    if let Some(fp) = fps.get(title_id) {
        return Some((title_id.to_string(), fp.clone()));
    }
    let ext = extdata_key(title_id);
    fps.get(&ext).map(|fp| (ext, fp.clone()))
}

/// The same fingerprint computed from the save files on disk right now.
pub fn disk_fingerprint(azahar_data: &Path, title_id: &str) -> (Fingerprint, Option<std::time::SystemTime>) {
    let mut fp = Fingerprint::new();
    let mut newest = None;
    let (hi, lo) = (title_id[..8].to_lowercase(), title_id[8..].to_lowercase());
    let ext = extdata_key(title_id)[8..].to_lowercase();
    let sd = azahar_data.join("sdmc").join("Nintendo 3DS");
    for id0 in read_dirs(&sd) {
        for id1 in read_dirs(&id0) {
            let save = id1.join("title").join(&hi).join(&lo).join("data");
            let extdata = id1.join("extdata").join("00000000").join(&ext);
            for dir in [save, extdata] {
                walk(&dir, &mut |path| {
                    let Ok(rel) = path.strip_prefix(azahar_data) else { return };
                    let rel = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
                    let Ok(meta) = path.metadata() else { return };
                    if let Ok(m) = meta.modified() {
                        newest = newest.max(Some(m));
                    }
                    if let Some(crc) = crc_file(path) {
                        fp.insert((rel, crc, meta.len()));
                    }
                });
            }
        }
    }
    (fp, newest)
}

fn read_dirs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
        .unwrap_or_default()
}

fn walk(dir: &Path, f: &mut dyn FnMut(&Path)) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, f);
        } else {
            f(&p);
        }
    }
}

fn crc_file(path: &Path) -> Option<u32> {
    let mut f = File::open(path).ok()?;
    let mut h = crc32fast::Hasher::new();
    let mut buf = vec![0; 1 << 16];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            return Some(h.finalize());
        }
        h.update(&buf[..n]);
    }
}
