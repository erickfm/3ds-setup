// Reads a game's name, icon and title ID straight from its file, the way Azahar's game list
// does. Formats: https://www.3dbrew.org/wiki/NCSD, /NCCH, /ExeFS, /CIA, /SMDH, /3DSX, and
// Azahar's compressed variants (Z3DS header + zstd) from src/common/file_derived.h.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Files Azahar can play, plus .cia, which Azahar has to install first.
pub const EXTENSIONS: &[&str] =
    &["3ds", "cci", "cxi", "3dsx", "cia", "zcci", "zcxi", "z3dsx", "zcia"];

/// How much of a compressed game we decompress looking for its icon. The icon sits right
/// after the game's code, which is at most a few tens of MB.
const MAX_DECOMPRESSED: usize = 96 << 20;

#[derive(Default, Debug, Clone)]
pub struct RomInfo {
    pub title_id: Option<u64>,
    pub name: Option<String>,
    pub publisher: Option<String>,
    pub icon: Option<String>, // data: URL
    pub encrypted: bool,
    pub is_cia: bool,
    pub homebrew: bool,
}

trait Source {
    fn read_at(&mut self, off: u64, len: usize) -> Option<Vec<u8>>;
}

struct Plain(File);

impl Source for Plain {
    fn read_at(&mut self, off: u64, len: usize) -> Option<Vec<u8>> {
        let mut buf = vec![0; len];
        self.0.seek(SeekFrom::Start(off)).ok()?;
        self.0.read_exact(&mut buf).ok()?;
        Some(buf)
    }
}

/// The start of a compressed game, decompressed into memory.
struct Unpacked(Vec<u8>);

impl Source for Unpacked {
    fn read_at(&mut self, off: u64, len: usize) -> Option<Vec<u8>> {
        let start = usize::try_from(off).ok()?;
        self.0.get(start..start.checked_add(len)?).map(<[u8]>::to_vec)
    }
}

fn u16le(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn u64le(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}
fn u32be(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}
fn u64be(b: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(b[at..at + 8].try_into().unwrap())
}
fn align64(n: u64) -> u64 {
    (n + 63) & !63
}

pub fn read_rom(path: &Path) -> RomInfo {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let mut info = RomInfo { is_cia: ext.ends_with("cia"), ..Default::default() };
    let Ok(mut file) = File::open(path) else { return info };
    let mut magic = [0u8; 0x104];
    let n = file.read(&mut magic).unwrap_or(0);
    if n >= 4 && &magic[..4] == b"Z3DS" {
        let Some(unpacked) = unpack_z3ds(&mut file) else { return info };
        let mut src = Unpacked(unpacked);
        parse(&mut src, &ext, &mut info);
    } else {
        parse(&mut Plain(file), &ext, &mut info);
    }
    info
}

fn parse(src: &mut dyn Source, ext: &str, info: &mut RomInfo) {
    let Some(head) = src.read_at(0, 0x200) else { return };
    if &head[0x100..0x104] == b"NCSD" {
        // Cartridge image: the game is the NCCH in partition 0.
        let offset = u64::from(u32le(&head, 0x120)) * 0x200;
        parse_ncch(src, offset, info);
    } else if &head[0x100..0x104] == b"NCCH" {
        parse_ncch(src, 0, info);
    } else if &head[..4] == b"3DSX" {
        info.homebrew = true;
        if u16le(&head, 4) > 0x20 {
            let off = u64::from(u32le(&head, 0x20));
            if off != 0 {
                if let Some(smdh) = src.read_at(off, 0x36C0) {
                    read_smdh(&smdh, info);
                }
            }
        }
    } else if ext.ends_with("cia") {
        parse_cia(src, &head, info);
    }
}

fn parse_ncch(src: &mut dyn Source, base: u64, info: &mut RomInfo) {
    let Some(h) = src.read_at(base, 0x200) else { return };
    if &h[0x100..0x104] != b"NCCH" {
        return;
    }
    info.title_id = Some(u64le(&h, 0x118));
    let no_crypto = h[0x18F] & 0x4 != 0;
    info.encrypted = !no_crypto;
    let exefs = u64::from(u32le(&h, 0x1A0)) * 0x200;
    if !no_crypto || exefs == 0 {
        return;
    }
    let Some(table) = src.read_at(base + exefs, 0xA0) else { return };
    for entry in table.chunks(16) {
        if &entry[..5] == b"icon\0" {
            let off = u64::from(u32le(entry, 8));
            if let Some(smdh) = src.read_at(base + exefs + 0x200 + off, 0x36C0) {
                read_smdh(&smdh, info);
            }
        }
    }
}

fn parse_cia(src: &mut dyn Source, head: &[u8], info: &mut RomInfo) {
    let cert_off = align64(u64::from(u32le(head, 0)));
    let tik_off = cert_off + align64(u64::from(u32le(head, 8)));
    let tmd_off = tik_off + align64(u64::from(u32le(head, 0xC)));
    let content_off = tmd_off + align64(u64::from(u32le(head, 0x10)));
    let meta_size = u32le(head, 0x14);
    let meta_off = content_off + align64(u64le(head, 0x18));

    if let Some(sig) = src.read_at(tmd_off, 4) {
        let sig_len = match u32be(&sig, 0) {
            0x10000 | 0x10003 => 0x200 + 0x3C,
            0x10001 | 0x10004 => 0x100 + 0x3C,
            0x10002 | 0x10005 => 0x3C + 0x40,
            _ => 0,
        };
        if sig_len != 0 {
            let tmd = tmd_off + 4 + sig_len;
            if let Some(h) = src.read_at(tmd, 0x9C4 + 0x30) {
                info.title_id = Some(u64be(&h, 0x4C));
                // Content type bit 0 = encrypted with the title key.
                info.encrypted = u16::from_be_bytes([h[0x9C4 + 6], h[0x9C4 + 7]]) & 1 != 0;
            }
        }
    }
    if meta_size >= 0x400 + 0x36C0 {
        if let Some(smdh) = src.read_at(meta_off + 0x400, 0x36C0) {
            read_smdh(&smdh, info);
        }
    }
    if info.icon.is_none() && !info.encrypted {
        let tid = info.title_id;
        parse_ncch(src, content_off, info);
        info.title_id = tid.or(info.title_id);
    }
}

fn utf16(b: &[u8]) -> String {
    let units: Vec<u16> = b.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
    String::from_utf16_lossy(&units).trim().to_string()
}

fn read_smdh(smdh: &[u8], info: &mut RomInfo) {
    if &smdh[..4] != b"SMDH" {
        return;
    }
    // English first, then whatever language the game does have.
    for lang in std::iter::once(1).chain(0..16) {
        let t = 8 + lang * 0x200;
        let short = utf16(&smdh[t..t + 0x80]);
        let long = utf16(&smdh[t + 0x80..t + 0x180]);
        if !short.is_empty() || !long.is_empty() {
            let name = if long.is_empty() { short } else { long };
            info.name = Some(name.split_whitespace().collect::<Vec<_>>().join(" "));
            let publisher = utf16(&smdh[t + 0x180..t + 0x200]);
            info.publisher = (!publisher.is_empty()).then_some(publisher);
            break;
        }
    }
    info.icon = Some(icon_data_url(&smdh[0x24C0..0x24C0 + 0x1200]));
}

/// 48x48 RGB565 icon stored in 8x8 Morton-ordered tiles -> a BMP data: URL.
fn icon_data_url(raw: &[u8]) -> String {
    const SIZE: usize = 48;
    const XLUT: [usize; 8] = [0x00, 0x01, 0x04, 0x05, 0x10, 0x11, 0x14, 0x15];
    const YLUT: [usize; 8] = [0x00, 0x02, 0x08, 0x0A, 0x20, 0x22, 0x28, 0x2A];
    let mut bmp = Vec::with_capacity(54 + SIZE * SIZE * 3);
    let pixels = (SIZE * SIZE * 3) as u32;
    bmp.extend_from_slice(b"BM");
    for v in [54 + pixels, 0, 54, 40] {
        bmp.extend_from_slice(&v.to_le_bytes());
    }
    for v in [SIZE as i32, SIZE as i32] {
        bmp.extend_from_slice(&v.to_le_bytes());
    }
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&24u16.to_le_bytes());
    for v in [0u32, pixels, 2835, 2835, 0, 0] {
        bmp.extend_from_slice(&v.to_le_bytes());
    }
    for y in (0..SIZE).rev() {
        // BMP rows go bottom-up
        for x in 0..SIZE {
            let i = XLUT[x % 8] + YLUT[y % 8] + (x & !7) * 8 + (y & !7) * SIZE;
            let v = u16::from(raw[i * 2]) | u16::from(raw[i * 2 + 1]) << 8;
            let (r, g, b) = ((v >> 11) & 31, (v >> 5) & 63, v & 31);
            bmp.extend_from_slice(&[(b * 255 / 31) as u8, (g * 255 / 63) as u8, (r * 255 / 31) as u8]);
        }
    }
    format!("data:image/bmp;base64,{}", base64(&bmp))
}

fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Z3DS = 0x20-byte header, metadata, then the original file as a run of zstd frames
/// (plus a skippable seek-table frame at the end).
fn unpack_z3ds(file: &mut File) -> Option<Vec<u8>> {
    let mut header = [0u8; 0x20];
    file.seek(SeekFrom::Start(0)).ok()?;
    file.read_exact(&mut header).ok()?;
    let data_start = u64::from(u16le(&header, 0xA)) + u64::from(u32le(&header, 0xC));
    file.seek(SeekFrom::Start(data_start)).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let mut out = Vec::new();
    while out.len() < MAX_DECOMPRESSED {
        let mut magic = [0u8; 4];
        if reader.read_exact(&mut magic).is_err() {
            break;
        }
        let m = u32::from_le_bytes(magic);
        if m & 0xFFFF_FFF0 == 0x184D_2A50 {
            let mut len = [0u8; 4];
            reader.read_exact(&mut len).ok()?;
            std::io::copy(&mut (&mut reader).take(u64::from(u32::from_le_bytes(len))), &mut std::io::sink()).ok()?;
            continue;
        }
        let mut frame = ruzstd::decoding::StreamingDecoder::new(std::io::Read::chain(&magic[..], &mut reader)).ok()?;
        let limit = (MAX_DECOMPRESSED - out.len()) as u64;
        if (&mut frame).take(limit).read_to_end(&mut out).is_err() {
            break;
        }
    }
    (!out.is_empty()).then_some(out)
}

/// "Pokemon X (USA) (En,Fr) [b]" -> "Pokemon X"
pub fn tidy_file_name(stem: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in stem.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ if depth == 0 => out.push(if c == '_' { ' ' } else { c }),
            _ => {}
        }
    }
    let tidy = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if tidy.is_empty() { stem.to_string() } else { tidy }
}

pub fn title_hex(tid: u64) -> String {
    format!("{tid:016X}")
}

pub fn known_name(tid: u64, titles: &HashMap<String, String>) -> Option<String> {
    titles.get(&title_hex(tid)).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ROM_SAMPLES=dir cargo test -- --nocapture : prints what we read from each file there.
    #[test]
    fn read_samples() {
        let Some(dir) = std::env::var_os("ROM_SAMPLES") else { return };
        let mut paths: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            let info = read_rom(&path);
            println!(
                "{}: tid={:?} name={:?} publisher={:?} encrypted={} cia={} homebrew={} icon={}",
                path.file_name().unwrap().to_string_lossy(),
                info.title_id.map(title_hex),
                info.name,
                info.publisher,
                info.encrypted,
                info.is_cia,
                info.homebrew,
                info.icon.as_ref().map_or(0, String::len)
            );
            if let Some(icon) = info.icon {
                std::fs::write(path.with_extension("icon.b64"), &icon["data:image/bmp;base64,".len()..]).unwrap();
            }
        }
    }

    #[test]
    fn tidies_names() {
        assert_eq!(tidy_file_name("Pokemon X (USA) (En,Fr) [b]"), "Pokemon X");
        assert_eq!(tidy_file_name("Mario_Kart_7"), "Mario Kart 7");
    }

    #[test]
    fn base64_matches() {
        assert_eq!(base64(b"hello!?"), "aGVsbG8hPw==");
    }
}
