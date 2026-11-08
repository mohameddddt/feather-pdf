//! Writes a COFF object with a .rsrc section (what a resource compiler would produce),
//! holding the app icon and the application manifest. The linker merges it into the .exe.

/// Resource id of the app icon group, loaded at runtime with LoadImageW(hinstance, 1, ...).
const ICON_GROUP_ID: u32 = 1;
const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;
const RT_MANIFEST: u32 = 24;
const LANG_EN_US: u32 = 0x0409;

/// Common Controls 6 (modern tree view and edit boxes), per-monitor DPI awareness and
/// Windows 10/11 compatibility.
pub const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="FeatherPDF" version="0.1.0.0"/>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0"
        processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
</assembly>
"#;

/// Builds the object file from icon images (size, RGBA pixels) and the manifest.
pub fn object(icons: &[(u32, Vec<u8>)], manifest: &[u8]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = icons.iter().map(|(size, rgba)| icon_dib(*size, rgba)).collect();
    let mut group = Vec::new();
    push16(&mut group, 0); // reserved
    push16(&mut group, 1); // type: icon
    push16(&mut group, icons.len() as u16);
    for (i, ((size, _), image)) in icons.iter().zip(&images).enumerate() {
        let dim = if *size >= 256 { 0 } else { *size as u8 }; // 0 means 256
        group.extend([dim, dim, 0, 0]);
        push16(&mut group, 1); // planes
        push16(&mut group, 32); // bits per pixel
        push32(&mut group, image.len() as u32);
        push16(&mut group, i as u16 + 1); // RT_ICON id
    }

    let tree = vec![
        (RT_ICON, images.into_iter().enumerate().map(|(i, img)| (i as u32 + 1, img)).collect()),
        (RT_GROUP_ICON, vec![(ICON_GROUP_ID, group)]),
        (RT_MANIFEST, vec![(1, manifest.to_vec())]),
    ];
    let (section, relocs) = rsrc_section(&tree);
    coff(&section, &relocs)
}

/// An icon image in the classic DIB layout: header with doubled height, bottom-up BGRA
/// pixels, then a 1-bit AND mask (1 = transparent).
fn icon_dib(size: u32, rgba: &[u8]) -> Vec<u8> {
    let s = size as usize;
    let mask_row = s.div_ceil(32) * 4;
    let mut out = Vec::with_capacity(40 + s * s * 4 + mask_row * s);
    push32(&mut out, 40);
    push32(&mut out, size);
    push32(&mut out, size * 2);
    push16(&mut out, 1);
    push16(&mut out, 32);
    push32(&mut out, 0); // BI_RGB
    push32(&mut out, (s * s * 4 + mask_row * s) as u32);
    out.extend([0u8; 16]);
    for y in (0..s).rev() {
        for x in 0..s {
            let p = &rgba[(y * s + x) * 4..][..4];
            out.extend([p[2], p[1], p[0], p[3]]);
        }
    }
    for y in (0..s).rev() {
        let mut row = vec![0u8; mask_row];
        for x in 0..s {
            if rgba[(y * s + x) * 4 + 3] == 0 {
                row[x / 8] |= 0x80 >> (x % 8);
            }
        }
        out.extend(row);
    }
    out
}

/// Lays out the three-level resource directory (type / id / language) followed by the data.
/// Returns the section bytes and the offsets of fields that need an RVA relocation.
fn rsrc_section(tree: &[(u32, Vec<(u32, Vec<u8>)>)]) -> (Vec<u8>, Vec<u32>) {
    const DIR: usize = 16;
    const ENTRY: usize = 8;
    const DATA_ENTRY: usize = 16;
    let leaves: usize = tree.iter().map(|(_, items)| items.len()).sum();

    // Offsets of every table, in the order they are written.
    let root = 0;
    let mut offset = DIR + ENTRY * tree.len();
    let mut type_dirs = Vec::new();
    for (_, items) in tree {
        type_dirs.push(offset);
        offset += DIR + ENTRY * items.len();
    }
    let mut lang_dirs = Vec::new();
    for _ in 0..leaves {
        lang_dirs.push(offset);
        offset += DIR + ENTRY;
    }
    let data_entries: Vec<usize> = (0..leaves).map(|i| offset + i * DATA_ENTRY).collect();
    offset += leaves * DATA_ENTRY;
    let mut data_offsets = Vec::new();
    for (_, items) in tree {
        for (_, data) in items {
            offset = offset.next_multiple_of(8);
            data_offsets.push(offset);
            offset += data.len();
        }
    }

    let mut out = vec![0u8; offset.next_multiple_of(8)];
    let mut relocs = Vec::new();
    let dir = |out: &mut Vec<u8>, at: usize, ids: usize| {
        out[at + 14..at + 16].copy_from_slice(&(ids as u16).to_le_bytes()); // NumberOfIdEntries
    };
    let entry = |out: &mut Vec<u8>, at: usize, id: u32, target: u32| {
        out[at..at + 4].copy_from_slice(&id.to_le_bytes());
        out[at + 4..at + 8].copy_from_slice(&target.to_le_bytes());
    };
    const SUBDIR: u32 = 0x8000_0000;

    dir(&mut out, root, tree.len());
    let mut leaf = 0;
    for (t, (type_id, items)) in tree.iter().enumerate() {
        entry(&mut out, root + DIR + t * ENTRY, *type_id, SUBDIR | type_dirs[t] as u32);
        dir(&mut out, type_dirs[t], items.len());
        for (i, (id, data)) in items.iter().enumerate() {
            entry(&mut out, type_dirs[t] + DIR + i * ENTRY, *id, SUBDIR | lang_dirs[leaf] as u32);
            dir(&mut out, lang_dirs[leaf], 1);
            entry(&mut out, lang_dirs[leaf] + DIR, LANG_EN_US, data_entries[leaf] as u32);
            let de = data_entries[leaf];
            // OffsetToData holds the section offset here; the relocation turns it into an RVA.
            out[de..de + 4].copy_from_slice(&(data_offsets[leaf] as u32).to_le_bytes());
            out[de + 4..de + 8].copy_from_slice(&(data.len() as u32).to_le_bytes());
            relocs.push(de as u32);
            out[data_offsets[leaf]..data_offsets[leaf] + data.len()].copy_from_slice(data);
            leaf += 1;
        }
    }
    (out, relocs)
}

/// Wraps the section in an x86-64 COFF object with one section symbol for the relocations.
fn coff(section: &[u8], relocs: &[u32]) -> Vec<u8> {
    const HEADER: u32 = 20;
    const SECTION_HEADER: u32 = 40;
    const IMAGE_REL_AMD64_ADDR32NB: u16 = 3;
    let data_at = HEADER + SECTION_HEADER;
    let relocs_at = data_at + section.len() as u32;
    let symbols_at = relocs_at + 10 * relocs.len() as u32;

    let mut out = Vec::new();
    push16(&mut out, 0x8664); // machine: x86-64
    push16(&mut out, 1); // sections
    push32(&mut out, 0); // timestamp
    push32(&mut out, symbols_at);
    push32(&mut out, 1); // symbols
    push16(&mut out, 0); // optional header size
    push16(&mut out, 0); // characteristics

    out.extend(*b".rsrc\0\0\0");
    push32(&mut out, 0); // virtual size
    push32(&mut out, 0); // virtual address
    push32(&mut out, section.len() as u32);
    push32(&mut out, data_at);
    push32(&mut out, relocs_at);
    push32(&mut out, 0); // line numbers
    push16(&mut out, relocs.len() as u16);
    push16(&mut out, 0);
    push32(&mut out, 0x4030_0040); // initialized data, 4-byte aligned, readable

    out.extend(section);
    for &at in relocs {
        push32(&mut out, at);
        push32(&mut out, 0); // symbol 0: the section itself
        push16(&mut out, IMAGE_REL_AMD64_ADDR32NB);
    }

    out.extend(*b".rsrc\0\0\0");
    push32(&mut out, 0); // value
    push16(&mut out, 1); // section number
    push16(&mut out, 0); // type
    out.push(3); // storage class: static
    out.push(0); // aux symbols
    push32(&mut out, 4); // empty string table
    out
}

fn push16(out: &mut Vec<u8>, v: u16) {
    out.extend(v.to_le_bytes());
}

fn push32(out: &mut Vec<u8>, v: u32) {
    out.extend(v.to_le_bytes());
}
