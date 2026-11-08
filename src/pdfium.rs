//! Minimal bindings to the handful of PDFium functions the reader needs.
//!
//! MinGW's linker links straight against lib/pdfium.dll (no import library), and
//! the DLL only has to sit next to the executable at runtime.

use std::cell::RefCell;
use std::ffi::{c_char, c_int, c_ulong, c_void};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::ptr::{null, null_mut};

type Handle = *mut c_void;

#[repr(C)]
struct FileAccess {
    len: c_ulong,
    get_block: unsafe extern "C" fn(*mut c_void, c_ulong, *mut u8, c_ulong) -> c_int,
    param: *mut c_void,
}

#[repr(C)]
struct SizeF {
    width: f32,
    height: f32,
}

const FPDF_ANNOT: c_int = 0x01;
const FPDF_LCD_TEXT: c_int = 0x02;
const BITMAP_BGRX: c_int = 3;
/// FPDF_PageToDevice/DeviceToPage work in integers, so map to a page this many times
/// larger than points to keep sub-point precision.
const K: f32 = 8.0;
/// Upper bounds when walking the outline, in case a damaged file has cycles.
const MAX_OUTLINE_ITEMS: usize = 10_000;
const MAX_OUTLINE_DEPTH: usize = 32;

#[link(name = "pdfium")]
unsafe extern "C" {
    fn FPDF_InitLibrary();
    fn FPDF_LoadCustomDocument(access: *mut FileAccess, password: *const c_char) -> Handle;
    fn FPDF_CloseDocument(doc: Handle);
    fn FPDF_GetLastError() -> c_ulong;
    fn FPDF_GetPageCount(doc: Handle) -> c_int;
    fn FPDF_GetPageSizeByIndexF(doc: Handle, index: c_int, size: *mut SizeF) -> c_int;
    fn FPDF_LoadPage(doc: Handle, index: c_int) -> Handle;
    fn FPDF_ClosePage(page: Handle);
    fn FPDFBitmap_CreateEx(w: c_int, h: c_int, format: c_int, first_scan: *mut c_void, stride: c_int) -> Handle;
    fn FPDFBitmap_FillRect(bitmap: Handle, left: c_int, top: c_int, w: c_int, h: c_int, color: c_ulong) -> c_int;
    fn FPDFBitmap_Destroy(bitmap: Handle);
    #[allow(clippy::too_many_arguments)]
    fn FPDF_RenderPageBitmap(
        bitmap: Handle,
        page: Handle,
        start_x: c_int,
        start_y: c_int,
        size_x: c_int,
        size_y: c_int,
        rotate: c_int,
        flags: c_int,
    );
    #[allow(clippy::too_many_arguments)]
    fn FPDF_PageToDevice(
        page: Handle,
        start_x: c_int,
        start_y: c_int,
        size_x: c_int,
        size_y: c_int,
        rotate: c_int,
        page_x: f64,
        page_y: f64,
        device_x: *mut c_int,
        device_y: *mut c_int,
    ) -> c_int;
    #[allow(clippy::too_many_arguments)]
    fn FPDF_DeviceToPage(
        page: Handle,
        start_x: c_int,
        start_y: c_int,
        size_x: c_int,
        size_y: c_int,
        rotate: c_int,
        device_x: c_int,
        device_y: c_int,
        page_x: *mut f64,
        page_y: *mut f64,
    ) -> c_int;
    fn FPDFText_LoadPage(page: Handle) -> Handle;
    fn FPDFText_ClosePage(text_page: Handle);
    fn FPDFText_CountChars(text_page: Handle) -> c_int;
    fn FPDFText_GetUnicode(text_page: Handle, index: c_int) -> u32;
    fn FPDFText_GetCharIndexAtPos(text_page: Handle, x: f64, y: f64, x_tolerance: f64, y_tolerance: f64) -> c_int;
    fn FPDFText_GetText(text_page: Handle, start_index: c_int, count: c_int, result: *mut u16) -> c_int;
    fn FPDFText_FindStart(text_page: Handle, find_what: *const u16, flags: c_ulong, start_index: c_int) -> Handle;
    fn FPDFText_FindNext(search: Handle) -> c_int;
    fn FPDFText_GetSchResultIndex(search: Handle) -> c_int;
    fn FPDFText_GetSchCount(search: Handle) -> c_int;
    fn FPDFText_FindClose(search: Handle);
    fn FPDFText_CountRects(text_page: Handle, start_index: c_int, count: c_int) -> c_int;
    fn FPDFText_GetRect(text_page: Handle, rect_index: c_int, left: *mut f64, top: *mut f64, right: *mut f64, bottom: *mut f64) -> c_int;
    fn FPDFBookmark_GetFirstChild(doc: Handle, bookmark: Handle) -> Handle;
    fn FPDFBookmark_GetNextSibling(doc: Handle, bookmark: Handle) -> Handle;
    fn FPDFBookmark_GetTitle(bookmark: Handle, buffer: *mut c_void, buflen: c_ulong) -> c_ulong;
    fn FPDFBookmark_GetCount(bookmark: Handle) -> c_int;
    fn FPDFBookmark_GetDest(doc: Handle, bookmark: Handle) -> Handle;
    fn FPDFBookmark_GetAction(bookmark: Handle) -> Handle;
    fn FPDFAction_GetDest(doc: Handle, action: Handle) -> Handle;
    fn FPDFDest_GetDestPageIndex(doc: Handle, dest: Handle) -> c_int;
    fn FPDFDest_GetLocationInPage(
        dest: Handle,
        has_x: *mut c_int,
        has_y: *mut c_int,
        has_zoom: *mut c_int,
        x: *mut f32,
        y: *mut f32,
        zoom: *mut f32,
    ) -> c_int;
}

/// A rectangle as (x, y, w, h) in points from the page's top-left corner.
pub type Rect = [f32; 4];
/// One search match: its highlight rectangles.
pub type Match = Vec<Rect>;

pub struct OutlineItem {
    pub title: String,
    pub depth: usize,
    pub page: Option<usize>,
    /// Target point in PDF page coordinates, if the destination has one.
    pub point: Option<(f32, f32)>,
    /// The PDF asks for this item's children to be shown.
    pub expanded: bool,
}

pub fn init() {
    unsafe { FPDF_InitLibrary() }
}

/// PDFium pulls bytes from the file on demand through this, so the PDF is
/// never loaded into memory as a whole.
struct Source {
    access: FileAccess,
    file: RefCell<File>,
}

unsafe extern "C" fn read_block(param: *mut c_void, pos: c_ulong, buf: *mut u8, size: c_ulong) -> c_int {
    let source = unsafe { &*(param as *const Source) };
    let out = unsafe { std::slice::from_raw_parts_mut(buf, size as usize) };
    let mut file = source.file.borrow_mut();
    (file.seek(SeekFrom::Start(pos as u64)).is_ok() && file.read_exact(out).is_ok()) as c_int
}

pub struct Document {
    handle: Handle,
    source: *mut Source,
}

impl Document {
    pub fn open(path: &Path) -> Result<Document, String> {
        let file = File::open(path).map_err(|e| format!("Could not open the file: {e}"))?;
        let len = file.metadata().map_err(|e| format!("Could not read the file: {e}"))?.len();
        let len = c_ulong::try_from(len).map_err(|_| "Files larger than 4 GB are not supported.".to_string())?;

        let source = Box::into_raw(Box::new(Source {
            access: FileAccess { len, get_block: read_block, param: null_mut() },
            file: RefCell::new(file),
        }));
        let handle = unsafe {
            (*source).access.param = source as *mut c_void;
            FPDF_LoadCustomDocument(&raw mut (*source).access, null())
        };
        if handle.is_null() {
            let msg = match unsafe { FPDF_GetLastError() } {
                2 => "The file could not be read.",
                3 => "This file is not a valid PDF, or it is damaged.",
                4 => "This PDF is password-protected (passwords are not supported yet).",
                5 => "This PDF uses an unsupported security handler.",
                _ => "PDFium could not open this file.",
            };
            unsafe { drop(Box::from_raw(source)) };
            return Err(msg.to_string());
        }
        Ok(Document { handle, source })
    }

    pub fn page_count(&self) -> usize {
        unsafe { FPDF_GetPageCount(self.handle) }.max(0) as usize
    }

    /// Page size in points (1/72 inch). Falls back to US Letter if unreadable.
    pub fn page_size(&self, index: usize) -> (f32, f32) {
        let mut size = SizeF { width: 0.0, height: 0.0 };
        let ok = unsafe { FPDF_GetPageSizeByIndexF(self.handle, index as c_int, &mut size) };
        if ok != 0 && size.width > 0.0 && size.height > 0.0 {
            (size.width, size.height)
        } else {
            (612.0, 792.0)
        }
    }

    /// Renders the part `(x, y, w, h)` of a page drawn at `full_w` x `full_h`
    /// pixels. Returns top-down BGRX pixels, `w * h` long.
    pub fn render(&self, index: usize, full_w: i32, full_h: i32, x: i32, y: i32, w: i32, h: i32) -> Option<Vec<u32>> {
        unsafe {
            let page = FPDF_LoadPage(self.handle, index as c_int);
            if page.is_null() {
                return None;
            }
            let mut pixels = vec![0u32; w as usize * h as usize];
            let bitmap = FPDFBitmap_CreateEx(w, h, BITMAP_BGRX, pixels.as_mut_ptr() as *mut c_void, w * 4);
            if bitmap.is_null() {
                FPDF_ClosePage(page);
                return None;
            }
            FPDFBitmap_FillRect(bitmap, 0, 0, w, h, 0xFFFF_FFFF);
            FPDF_RenderPageBitmap(bitmap, page, -x, -y, full_w, full_h, 0, FPDF_ANNOT | FPDF_LCD_TEXT);
            FPDFBitmap_Destroy(bitmap);
            FPDF_ClosePage(page);
            Some(pixels)
        }
    }

    /// Loads a page's text layer. The result must be dropped before this document.
    pub fn text_page(&self, index: usize) -> Option<TextPage> {
        unsafe {
            let page = FPDF_LoadPage(self.handle, index as c_int);
            if page.is_null() {
                return None;
            }
            let text = FPDFText_LoadPage(page);
            if text.is_null() {
                FPDF_ClosePage(page);
                return None;
            }
            Some(TextPage { page, text, size: self.page_size(index) })
        }
    }

    pub fn find(&self, index: usize, query: &[u16]) -> Vec<Match> {
        self.text_page(index).map(|t| t.find(query)).unwrap_or_default()
    }

    /// Converts a PDF page coordinate to points from the page's top-left corner.
    pub fn pdf_point_to_top_left(&self, index: usize, x: f32, y: f32) -> Option<(f32, f32)> {
        unsafe {
            let page = FPDF_LoadPage(self.handle, index as c_int);
            if page.is_null() {
                return None;
            }
            let point = to_top_left(page, self.page_size(index), x as f64, y as f64);
            FPDF_ClosePage(page);
            Some(point)
        }
    }

    /// The document outline (bookmarks), flattened in display order.
    pub fn outline(&self) -> Vec<OutlineItem> {
        let mut items = Vec::new();
        self.walk_outline(null_mut(), 0, &mut items);
        items
    }

    fn walk_outline(&self, parent: Handle, depth: usize, out: &mut Vec<OutlineItem>) {
        if depth > MAX_OUTLINE_DEPTH {
            return;
        }
        unsafe {
            let mut bookmark = FPDFBookmark_GetFirstChild(self.handle, parent);
            while !bookmark.is_null() && out.len() < MAX_OUTLINE_ITEMS {
                out.push(self.outline_item(bookmark, depth));
                self.walk_outline(bookmark, depth + 1, out);
                bookmark = FPDFBookmark_GetNextSibling(self.handle, bookmark);
            }
        }
    }

    unsafe fn outline_item(&self, bookmark: Handle, depth: usize) -> OutlineItem {
        unsafe {
            let bytes = FPDFBookmark_GetTitle(bookmark, null_mut(), 0) as usize;
            let mut buf = vec![0u16; bytes / 2];
            FPDFBookmark_GetTitle(bookmark, buf.as_mut_ptr() as *mut c_void, bytes as c_ulong);
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            let title = String::from_utf16_lossy(&buf[..end]).split_whitespace().collect::<Vec<_>>().join(" ");

            let mut dest = FPDFBookmark_GetDest(self.handle, bookmark);
            if dest.is_null() {
                let action = FPDFBookmark_GetAction(bookmark);
                if !action.is_null() {
                    dest = FPDFAction_GetDest(self.handle, action);
                }
            }
            let (mut page, mut point) = (None, None);
            if !dest.is_null() {
                let index = FPDFDest_GetDestPageIndex(self.handle, dest);
                page = (index >= 0).then_some(index as usize);
                let (mut has_x, mut has_y, mut has_zoom) = (0, 0, 0);
                let (mut x, mut y, mut zoom) = (0.0, 0.0, 0.0);
                if FPDFDest_GetLocationInPage(dest, &mut has_x, &mut has_y, &mut has_zoom, &mut x, &mut y, &mut zoom) != 0 && has_y != 0 {
                    point = Some((if has_x != 0 { x } else { 0.0 }, y));
                }
            }
            OutlineItem { title, depth, page, point, expanded: FPDFBookmark_GetCount(bookmark) > 0 }
        }
    }
}

impl Drop for Document {
    fn drop(&mut self) {
        unsafe {
            FPDF_CloseDocument(self.handle);
            drop(Box::from_raw(self.source));
        }
    }
}

/// Converts PDF page coordinates to points from the top-left, honoring page rotation and origin.
unsafe fn to_top_left(page: Handle, size: (f32, f32), x: f64, y: f64) -> (f32, f32) {
    let (dw, dh) = ((size.0 * K) as c_int, (size.1 * K) as c_int);
    let (mut dx, mut dy) = (0, 0);
    unsafe { FPDF_PageToDevice(page, 0, 0, dw, dh, 0, x, y, &mut dx, &mut dy) };
    (dx as f32 / K, dy as f32 / K)
}

/// A loaded page with its text layer, for selection, copying and search.
pub struct TextPage {
    page: Handle,
    text: Handle,
    size: (f32, f32),
}

/// Passed as the end of a range to mean "through the last character".
pub const TO_END: i32 = i32::MAX;

impl TextPage {
    pub fn char_count(&self) -> i32 {
        unsafe { FPDFText_CountChars(self.text) }.max(0)
    }

    /// Index of the character at or near (x, y), in points from the top-left. The search
    /// widens step by step, so a press in the margin next to a line still finds it.
    pub fn char_at(&self, x: f32, y: f32) -> Option<i32> {
        let (dw, dh) = ((self.size.0 * K) as c_int, (self.size.1 * K) as c_int);
        let (mut px, mut py) = (0.0, 0.0);
        unsafe {
            FPDF_DeviceToPage(self.page, 0, 0, dw, dh, 0, (x * K) as c_int, (y * K) as c_int, &mut px, &mut py);
            [2.0, 8.0, 24.0, 72.0]
                .into_iter()
                .map(|tol| FPDFText_GetCharIndexAtPos(self.text, px, py, tol, tol))
                .find(|&i| i >= 0)
        }
    }

    /// Clamps an inclusive range (`to` may be TO_END) to this page; None if it is empty.
    fn range(&self, from: i32, to: i32) -> Option<(i32, i32)> {
        let last = self.char_count() - 1;
        let (from, to) = (from.max(0), to.min(last));
        (to >= from).then_some((from, to - from + 1))
    }

    /// Highlight rectangles for characters `from..=to`.
    pub fn rects(&self, from: i32, to: i32) -> Vec<Rect> {
        let Some((start, count)) = self.range(from, to) else { return Vec::new() };
        self.rects_for(start, count)
    }

    fn rects_for(&self, start: i32, count: i32) -> Vec<Rect> {
        let mut rects = Vec::new();
        unsafe {
            for r in 0..FPDFText_CountRects(self.text, start, count) {
                let (mut l, mut t, mut rr, mut b) = (0.0, 0.0, 0.0, 0.0);
                if FPDFText_GetRect(self.text, r, &mut l, &mut t, &mut rr, &mut b) == 0 {
                    continue;
                }
                let (x1, y1) = to_top_left(self.page, self.size, l, t);
                let (x2, y2) = to_top_left(self.page, self.size, rr, b);
                rects.push([x1.min(x2), y1.min(y2), (x1 - x2).abs(), (y1 - y2).abs()]);
            }
        }
        rects
    }

    /// Text of characters `from..=to`, with line breaks as \r\n.
    pub fn text(&self, from: i32, to: i32) -> String {
        let Some((start, count)) = self.range(from, to) else { return String::new() };
        let mut buf = vec![0u16; count as usize + 1];
        let written = unsafe { FPDFText_GetText(self.text, start, count, buf.as_mut_ptr()) };
        // `written` includes the terminator.
        buf.truncate((written - 1).max(0) as usize);
        // PDFium marks a word hyphenated across lines with U+FFFE; drop it to rejoin the word.
        buf.retain(|&c| c != 0xFFFE);
        String::from_utf16_lossy(&buf)
    }

    /// The word around character `index`, as an inclusive range.
    pub fn word_at(&self, index: i32) -> (i32, i32) {
        let is_word = |i: i32| {
            let c = unsafe { FPDFText_GetUnicode(self.text, i) };
            char::from_u32(c).is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '\'')
        };
        if !is_word(index) {
            return (index, index);
        }
        let (mut from, mut to) = (index, index);
        while from > 0 && is_word(from - 1) {
            from -= 1;
        }
        while to + 1 < self.char_count() && is_word(to + 1) {
            to += 1;
        }
        (from, to)
    }

    /// Finds all case-insensitive occurrences of `query` (UTF-16, no terminator).
    pub fn find(&self, query: &[u16]) -> Vec<Match> {
        let query: Vec<u16> = query.iter().copied().chain(Some(0)).collect();
        let mut matches = Vec::new();
        unsafe {
            let search = FPDFText_FindStart(self.text, query.as_ptr(), 0, 0);
            if search.is_null() {
                return matches;
            }
            while FPDFText_FindNext(search) != 0 {
                let rects = self.rects_for(FPDFText_GetSchResultIndex(search), FPDFText_GetSchCount(search));
                if !rects.is_empty() {
                    matches.push(rects);
                }
            }
            FPDFText_FindClose(search);
        }
        matches
    }
}

impl Drop for TextPage {
    fn drop(&mut self) {
        unsafe {
            FPDFText_ClosePage(self.text);
            FPDF_ClosePage(self.page);
        }
    }
}
