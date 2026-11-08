//! Remembers where each PDF was left off, in %APPDATA%\FeatherPDF\positions.tsv.
//!
//! One line per file, most recent first: page, offset into the page in points, zoom,
//! fit mode, and the lowercased absolute path (last, since it is the only free-form field).

use std::path::{Path, PathBuf};

const MAX_ENTRIES: usize = 200;

#[derive(Clone, Copy)]
pub struct Position {
    pub page: usize,
    pub offset_pt: f32,
    pub zoom: f32,
    /// 0 = none, 1 = fit width, 2 = fit page.
    pub fit: u8,
}

fn store_path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("FeatherPDF").join("positions.tsv"))
}

fn key(path: &Path) -> String {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()).to_string_lossy().to_lowercase()
}

fn read_lines() -> Vec<String> {
    store_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

fn parse(line: &str) -> Option<(Position, &str)> {
    let mut parts = line.splitn(5, '\t');
    let pos = Position {
        page: parts.next()?.parse().ok()?,
        offset_pt: parts.next()?.parse().ok()?,
        zoom: parts.next()?.parse().ok()?,
        fit: parts.next()?.parse().ok()?,
    };
    Some((pos, parts.next()?))
}

pub fn load(path: &Path) -> Option<Position> {
    let key = key(path);
    read_lines().iter().filter_map(|l| parse(l)).find(|(_, k)| *k == key).map(|(pos, _)| pos)
}

/// Best effort: a failure to save only means the position is not remembered.
pub fn save(path: &Path, pos: Position) {
    let Some(store) = store_path() else { return };
    let key = key(path);
    let mut lines = vec![format!("{}\t{:.1}\t{:.4}\t{}\t{key}", pos.page, pos.offset_pt, pos.zoom, pos.fit)];
    lines.extend(read_lines().into_iter().filter(|l| parse(l).is_some_and(|(_, k)| k != key)).take(MAX_ENTRIES - 1));
    if let Some(dir) = store.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(store, lines.join("\n") + "\n");
}
