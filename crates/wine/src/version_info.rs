//! The strings in a Windows program's version resource (`CompanyName`, `ProductName`, ...), which
//! is what Explorer shows under Properties → Details. Installers carry their vendor's name there
//! even when the file has been renamed, so it tells a Native Access installer from a plugin's own.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// `RT_VERSION`, the resource type of the version information.
const RT_VERSION: u32 = 16;
/// More than any real version resource; a bound for a damaged or hostile file.
const MAX_RESOURCE: u32 = 64 * 1024;

/// The `StringFileInfo` pairs of the program at `path`, in file order (`("CompanyName", "Native
/// Instruments GmbH")`, ...). Empty for a file that is not a PE program or has no version resource.
pub fn version_strings(path: &Path) -> Vec<(String, String)> {
    let Ok(mut f) = File::open(path) else {
        return Vec::new();
    };
    read_resource(&mut f)
        .map(|data| parse_strings(&data))
        .unwrap_or_default()
}

/// Little-endian reads that fail instead of panicking on a short buffer.
fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn read_at(f: &mut File, offset: u64, len: usize) -> Option<Vec<u8>> {
    f.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// A section's place in memory and in the file, to turn addresses into file offsets.
struct Section {
    address: u32,
    size: u32,
    offset: u32,
}

fn file_offset(sections: &[Section], rva: u32) -> Option<u64> {
    sections
        .iter()
        .find(|s| rva >= s.address && rva - s.address < s.size)
        .map(|s| u64::from(s.offset) + u64::from(rva - s.address))
}

/// The bytes of the first version resource, found by walking the resource directory (type,
/// then name, then language). Only the headers and that one resource are read: an installer's
/// payload can be gigabytes.
fn read_resource(f: &mut File) -> Option<Vec<u8>> {
    let dos = read_at(f, 0, 64)?;
    if &dos[..2] != b"MZ" {
        return None;
    }
    let pe_at = u64::from(u32_at(&dos, 60)?);
    let coff = read_at(f, pe_at, 24)?;
    if &coff[..4] != b"PE\0\0" {
        return None;
    }
    let sections_count = usize::from(u16_at(&coff, 6)?);
    let optional_size = usize::from(u16_at(&coff, 20)?);
    let optional = read_at(f, pe_at + 24, optional_size)?;
    // The data directories start after the fixed fields, which are longer in a 64-bit file.
    let directories = match u16_at(&optional, 0)? {
        0x10b => 96,
        0x20b => 112,
        _ => return None,
    };
    // Directory 2 is the resource table.
    let resources_rva = u32_at(&optional, directories + 2 * 8)?;
    if resources_rva == 0 {
        return None;
    }
    let table = read_at(
        f,
        pe_at + 24 + optional_size as u64,
        sections_count.min(96) * 40,
    )?;
    let sections: Vec<Section> = table
        .as_chunks::<40>()
        .0
        .iter()
        .filter_map(|s| {
            Some(Section {
                address: u32_at(s, 12)?,
                size: u32_at(s, 8)?.max(u32_at(s, 16)?),
                offset: u32_at(s, 20)?,
            })
        })
        .collect();
    let root = file_offset(&sections, resources_rva)?;
    // Offsets inside the resource directory count from its start.
    let entries = |f: &mut File, dir: u32| -> Option<Vec<(u32, u32)>> {
        let head = read_at(f, root + u64::from(dir), 16)?;
        let count = usize::from(u16_at(&head, 12)?) + usize::from(u16_at(&head, 14)?);
        let list = read_at(f, root + u64::from(dir) + 16, count.min(4096) * 8)?;
        Some(
            list.as_chunks::<8>()
                .0
                .iter()
                .filter_map(|e| Some((u32_at(e, 0)?, u32_at(e, 4)?)))
                .collect(),
        )
    };
    let subdirectory = |target: u32| (target & 0x8000_0000 != 0).then_some(target & 0x7fff_ffff);
    let (_, by_type) = entries(f, 0)?
        .into_iter()
        .find(|&(id, _)| id == RT_VERSION)?;
    let (_, by_name) = *entries(f, subdirectory(by_type)?)?.first()?;
    let (_, by_language) = *entries(f, subdirectory(by_name)?)?.first()?;
    if subdirectory(by_language).is_some() {
        return None;
    }
    let data = read_at(f, root + u64::from(by_language), 8)?;
    let (rva, size) = (u32_at(&data, 0)?, u32_at(&data, 4)?);
    read_at(
        f,
        file_offset(&sections, rva)?,
        size.min(MAX_RESOURCE) as usize,
    )
}

/// One block of a `VS_VERSIONINFO` tree: its key, where its value starts and where it ends.
struct Block {
    key: String,
    value_len: usize,
    value: usize,
    end: usize,
}

fn align4(at: usize) -> usize {
    (at + 3) & !3
}

/// The blocks between `from` and `to`, one level deep.
fn blocks(data: &[u8], from: usize, to: usize) -> Vec<Block> {
    let mut out = Vec::new();
    let mut at = align4(from);
    while at + 6 <= to {
        let Some(len) = u16_at(data, at).map(usize::from).filter(|&l| l >= 6) else {
            break;
        };
        let end = (at + len).min(to);
        let value_len = usize::from(u16_at(data, at + 2).unwrap_or(0));
        let (key, after) = utf16z(data, at + 6, end);
        out.push(Block {
            key,
            value_len,
            value: align4(after),
            end,
        });
        at = align4(end);
    }
    out
}

/// A NUL-terminated UTF-16 string starting at `from`, and the offset after its NUL.
fn utf16z(data: &[u8], from: usize, to: usize) -> (String, usize) {
    let mut units = Vec::new();
    let mut at = from;
    while at + 2 <= to {
        let unit = u16_at(data, at).unwrap_or(0);
        at += 2;
        if unit == 0 {
            break;
        }
        units.push(unit);
    }
    (String::from_utf16_lossy(&units), at)
}

/// `VS_VERSIONINFO` → `StringFileInfo` → string tables (one per language) → key/value strings.
fn parse_strings(data: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for root in blocks(data, 0, data.len()) {
        // The root's value is the binary VS_FIXEDFILEINFO; its children follow it.
        let children = root.value + root.value_len;
        for info in blocks(data, children, root.end) {
            if info.key != "StringFileInfo" {
                continue;
            }
            for table in blocks(data, info.value, info.end) {
                for s in blocks(data, table.value, table.end) {
                    let (value, _) = utf16z(data, s.value, s.end);
                    let value = value.trim().to_owned();
                    if !value.is_empty() {
                        out.push((s.key, value));
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    }

    fn pad4(v: &mut Vec<u8>) {
        while !v.len().is_multiple_of(4) {
            v.push(0);
        }
    }

    /// A version-info block as Windows' resource compiler writes it.
    fn block(key: &str, kind: u16, value: &[u8], value_len: u16, children: &[Vec<u8>]) -> Vec<u8> {
        let mut v = vec![0, 0];
        v.extend(value_len.to_le_bytes());
        v.extend(kind.to_le_bytes());
        v.extend(utf16(key));
        pad4(&mut v);
        v.extend(value);
        for c in children {
            pad4(&mut v);
            v.extend(c);
        }
        let len = v.len() as u16;
        v[..2].copy_from_slice(&len.to_le_bytes());
        v
    }

    fn version_resource(strings: &[(&str, &str)]) -> Vec<u8> {
        let pairs: Vec<Vec<u8>> = strings
            .iter()
            .map(|(k, val)| {
                let text = utf16(val);
                block(k, 1, &text, (text.len() / 2) as u16, &[])
            })
            .collect();
        let table = block("040904b0", 1, &[], 0, &pairs);
        let info = block("StringFileInfo", 1, &[], 0, &[table]);
        let var = block("VarFileInfo", 1, &[], 0, &[]);
        let fixed = [0u8; 52];
        block("VS_VERSION_INFO", 0, &fixed, 52, &[info, var])
    }

    /// A 64-bit PE file with one `.rsrc` section holding `resource` as its version information.
    fn pe_with_version(resource: &[u8]) -> Vec<u8> {
        const SECTION_VA: u32 = 0x1000;
        const SECTION_AT: usize = 0x200;
        let mut file = vec![0u8; SECTION_AT];
        file[..2].copy_from_slice(b"MZ");
        file[60..64].copy_from_slice(&0x40u32.to_le_bytes());
        file[0x40..0x44].copy_from_slice(b"PE\0\0");
        file[0x44..0x46].copy_from_slice(&0x8664u16.to_le_bytes());
        file[0x46..0x48].copy_from_slice(&1u16.to_le_bytes());
        let optional_size: u16 = 112 + 16 * 8;
        file[0x54..0x56].copy_from_slice(&optional_size.to_le_bytes());
        let optional = 0x40 + 24;
        file[optional..optional + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        // Data directory 2: the resources, at the start of the section.
        file[optional + 112 + 16..optional + 112 + 20].copy_from_slice(&SECTION_VA.to_le_bytes());
        // The resource tree: type 16 → name 1 → language 0x409 → data.
        let mut rsrc = Vec::new();
        let dir = |rsrc: &mut Vec<u8>, id: u32, target: u32| {
            rsrc.extend([0u8; 12]);
            rsrc.extend(0u16.to_le_bytes());
            rsrc.extend(1u16.to_le_bytes());
            rsrc.extend(id.to_le_bytes());
            rsrc.extend(target.to_le_bytes());
        };
        dir(&mut rsrc, RT_VERSION, 0x8000_0000 | 24);
        dir(&mut rsrc, 1, 0x8000_0000 | 48);
        dir(&mut rsrc, 0x409, 72);
        let data_rva = SECTION_VA + 88;
        rsrc.extend(data_rva.to_le_bytes());
        rsrc.extend((resource.len() as u32).to_le_bytes());
        rsrc.extend([0u8; 8]);
        rsrc.extend(resource);
        let section = optional + usize::from(optional_size);
        file[section..section + 8].copy_from_slice(b".rsrc\0\0\0");
        file[section + 8..section + 12].copy_from_slice(&(rsrc.len() as u32).to_le_bytes());
        file[section + 12..section + 16].copy_from_slice(&SECTION_VA.to_le_bytes());
        file[section + 16..section + 20].copy_from_slice(&(rsrc.len() as u32).to_le_bytes());
        file[section + 20..section + 24].copy_from_slice(&(SECTION_AT as u32).to_le_bytes());
        file.extend(rsrc);
        file
    }

    fn tempfile(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("merlin-version-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn reads_the_strings_of_a_version_resource() {
        let resource = version_resource(&[
            ("CompanyName", "Native Instruments GmbH"),
            ("ProductName", "Native Access"),
            ("Comments", ""),
            ("FileDescription", "Native Access Setup "),
        ]);
        assert_eq!(
            parse_strings(&resource),
            [
                (
                    "CompanyName".to_owned(),
                    "Native Instruments GmbH".to_owned()
                ),
                ("ProductName".to_owned(), "Native Access".to_owned()),
                (
                    "FileDescription".to_owned(),
                    "Native Access Setup".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn finds_the_version_resource_in_a_program() {
        let resource = version_resource(&[("CompanyName", "Arturia"), ("ProductName", "ASC")]);
        let p = tempfile("setup.exe", &pe_with_version(&resource));
        assert_eq!(
            version_strings(&p),
            [
                ("CompanyName".to_owned(), "Arturia".to_owned()),
                ("ProductName".to_owned(), "ASC".to_owned()),
            ]
        );
    }

    #[test]
    fn anything_else_has_no_strings() {
        assert!(version_strings(&tempfile("elf", b"\x7fELF and more bytes")).is_empty());
        assert!(version_strings(&tempfile("short", b"MZ")).is_empty());
        assert!(version_strings(Path::new("/nonexistent/setup.exe")).is_empty());
        // A program with a resource section but a damaged version block.
        let p = tempfile("broken.exe", &pe_with_version(&[0xff; 40]));
        assert!(version_strings(&p).is_empty());
    }
}
