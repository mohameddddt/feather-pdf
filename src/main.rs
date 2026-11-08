#![windows_subsystem = "windows"]

mod pdfium;
mod positions;
mod ui;

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsString;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::Win32::UI::Controls::Dialogs::*;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use pdfium::{Document, Match, OutlineItem, Rect, TO_END, TextPage};
use positions::Position;
use ui::{APP_NAME, Fonts, Theme, contains, fill, glyph, rect, round_rect, text, wide};

/// Upper bound for all cached page bitmaps together.
const CACHE_BUDGET: usize = 32 * 1024 * 1024;
/// Pages whose full bitmap would be larger than this are rendered only around the visible area.
const FULL_PAGE_LIMIT: usize = 24 * 1024 * 1024;
/// PDFium keeps fonts and parsed data for every page it touches until the document closes.
/// When memory outside the page cache grows this much past its level after opening, the
/// document is reopened to release it.
const PDFIUM_GROWTH_LIMIT: usize = 32 * 1024 * 1024;
/// Search scans pages in slices of this length so the window stays responsive.
const SEARCH_SLICE: Duration = Duration::from_millis(40);
/// Text layers kept loaded while dragging a selection.
const TEXT_PAGE_CACHE: usize = 2;
const WM_APP_SEARCH: u32 = WM_APP + 1;
/// Enter or Esc pressed in the page number box (wParam is the key).
const WM_APP_PAGE_KEY: u32 = WM_APP + 2;
const MIN_ZOOM: f32 = 0.1;
const MAX_ZOOM: f32 = 8.0;
const ZOOM_STEP: f32 = 1.2;
const ZOOM_PRESETS: [u32; 8] = [50, 75, 100, 125, 150, 200, 300, 400];
// Sizes at 96 DPI.
const TOOLBAR_H: i32 = 48;
const SIDEBAR_W: i32 = 280;
const SIDEBAR_HEADER_H: i32 = 40;
const PAGE_GAP: i32 = 16;
/// Logo on the welcome screen.
const LOGO_SIZE: i32 = 104;
const HIT_COLOR: u32 = ui::rgb(0xFF, 0xEB, 0x3B);
const CURRENT_HIT_COLOR: u32 = ui::rgb(0xFF, 0x96, 0x00);
const SELECTION_COLOR: u32 = ui::brand::MINT;
/// Raster op "dest AND brush": tints white paper like a highlighter but leaves black text black.
const PATAND: u32 = 0x00A0_00C9;
const NORMAL_STYLE: u32 = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;
const TOAST_TIMER: usize = 1;
const TOAST_MS: u32 = 1800;
/// Scrolls the view while a selection is dragged past its edge.
const DRAG_TIMER: usize = 2;
const DRAG_MS: u32 = 30;
const TOOLTIP_TIMER: usize = 3;
const TOOLTIP_MS: u32 = 500;
const CF_UNICODETEXT: u32 = 13;
// Modifier flags in WM_MOUSEWHEEL wParam.
const MK_SHIFT: u32 = 0x4;
const MK_CONTROL: u32 = 0x8;

#[derive(Clone, Copy, PartialEq)]
enum Command {
    Open,
    Copy,
    SelectAll,
    Find,
    FindNext,
    FindPrev,
    ZoomIn,
    ZoomOut,
    ActualSize,
    FitWidth,
    FitPage,
    ToggleOutline,
    ToggleTheme,
    FullScreen,
    About,
    Escape,
}

/// Right-click and "more" menu; `None` is a separator. Item ids are index + 1.
const MENU: &[Option<(Command, &str)>] = &[
    Some((Command::Open, "Open...\tCtrl+O")),
    None,
    Some((Command::Copy, "Copy\tCtrl+C")),
    Some((Command::SelectAll, "Select page text\tCtrl+A")),
    None,
    Some((Command::Find, "Find...\tCtrl+F")),
    Some((Command::FindNext, "Find next\tF3")),
    Some((Command::FindPrev, "Find previous\tShift+F3")),
    None,
    Some((Command::ZoomIn, "Zoom in\tCtrl++")),
    Some((Command::ZoomOut, "Zoom out\tCtrl+-")),
    Some((Command::ActualSize, "Actual size\tCtrl+1")),
    Some((Command::FitWidth, "Fit width\tCtrl+0")),
    Some((Command::FitPage, "Fit page\tCtrl+2")),
    None,
    Some((Command::ToggleOutline, "Contents\tCtrl+B")),
    Some((Command::ToggleTheme, "Dark theme")),
    Some((Command::FullScreen, "Full screen\tF11")),
    None,
    Some((Command::About, "About Feather PDF")),
];

/// Toolbar buttons.
#[derive(Clone, Copy, PartialEq)]
enum Tool {
    Sidebar,
    Open,
    PrevPage,
    NextPage,
    ZoomOut,
    ZoomMenu,
    ZoomIn,
    FitWidth,
    FitPage,
    Search,
    FindPrev,
    FindNext,
    FindClose,
    FullScreen,
    More,
}

impl Tool {
    fn glyph(self) -> char {
        match self {
            Tool::Sidebar => glyph::MENU,
            Tool::Open => glyph::FOLDER,
            Tool::PrevPage | Tool::FindPrev => glyph::UP,
            Tool::NextPage | Tool::FindNext => glyph::DOWN,
            Tool::ZoomOut => glyph::ZOOM_OUT,
            Tool::ZoomIn => glyph::ZOOM_IN,
            Tool::FitWidth => glyph::FIT_WIDTH,
            Tool::FitPage => glyph::FIT_PAGE,
            Tool::Search => glyph::SEARCH,
            Tool::FindClose => glyph::CLOSE,
            Tool::FullScreen => glyph::FULL_SCREEN,
            Tool::More => glyph::MORE,
            Tool::ZoomMenu => ' ',
        }
    }

    fn tip(self) -> &'static str {
        match self {
            Tool::Sidebar => "Contents (Ctrl+B)",
            Tool::Open => "Open (Ctrl+O)",
            Tool::PrevPage => "Previous page",
            Tool::NextPage => "Next page",
            Tool::ZoomOut => "Zoom out (Ctrl+-)",
            Tool::ZoomMenu => "Zoom level",
            Tool::ZoomIn => "Zoom in (Ctrl++)",
            Tool::FitWidth => "Fit width (Ctrl+0)",
            Tool::FitPage => "Fit page (Ctrl+2)",
            Tool::Search => "Find (Ctrl+F)",
            Tool::FindPrev => "Previous match (Shift+F3)",
            Tool::FindNext => "Next match (F3)",
            Tool::FindClose => "Close (Esc)",
            Tool::FullScreen => "Full screen (F11)",
            Tool::More => "More options",
        }
    }
}

/// Work that must run outside the app borrow because it opens a modal loop or resizes
/// the window (both send messages back to wndproc synchronously).
#[derive(Clone, Copy)]
enum Deferred {
    Command(Command),
    /// Screen position for the popup.
    ZoomMenu(i32, i32),
    MoreMenu(i32, i32),
}

/// What a held mouse button is doing.
#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    Tool(Tool),
    Select,
    /// Dragging a scrollbar thumb; the value is where on the thumb it was grabbed.
    VThumb(i32),
    HThumb(i32),
}

#[derive(Clone, Copy, PartialEq)]
enum Fit {
    None,
    Width,
    Page,
}

impl Fit {
    fn to_u8(self) -> u8 {
        match self {
            Fit::None => 0,
            Fit::Width => 1,
            Fit::Page => 2,
        }
    }

    fn from_u8(v: u8) -> Fit {
        match v {
            0 => Fit::None,
            2 => Fit::Page,
            _ => Fit::Width,
        }
    }
}

#[derive(Clone, Copy)]
struct Area {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Area {
    fn contains(&self, o: &Area) -> bool {
        o.x >= self.x && o.y >= self.y && o.x + o.w <= self.x + self.w && o.y + o.h <= self.y + self.h
    }

    fn intersect(&self, o: &Area) -> Option<Area> {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = (self.x + self.w).min(o.x + o.w);
        let b = (self.y + self.h).min(o.y + o.h);
        (r > x && b > y).then_some(Area { x, y, w: r - x, h: b - y })
    }

    fn has(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    fn bytes(&self) -> usize {
        self.w as usize * self.h as usize * 4
    }

    fn rect(&self) -> RECT {
        rect(self.x, self.y, self.w, self.h)
    }
}

struct CachedPage {
    page: usize,
    /// Pixels per point this was rendered at; entries for another zoom are dropped.
    scale: f32,
    /// Rendered region in page pixel coordinates.
    area: Area,
    pixels: Vec<u32>,
    last_used: u64,
}

struct Search {
    query: Vec<u16>,
    /// Matches per page, filled in lazily as pages are searched or shown.
    hits: HashMap<usize, Vec<Match>>,
    /// (page, match index) of the selected match.
    current: Option<(usize, usize)>,
    /// A scan for the next match that is still running.
    pending: Option<PendingSearch>,
}

#[derive(Clone, Copy)]
struct PendingSearch {
    forward: bool,
    start_page: usize,
    /// Pages already scanned past `start_page`.
    scanned: usize,
}

/// A text selection between two (page, character index) positions, in either order.
struct Selection {
    anchor: (usize, i32),
    focus: (usize, i32),
    /// Highlight rectangles per page, with the character range they were computed for.
    rects: HashMap<usize, ((i32, i32), Vec<Rect>)>,
    moved: bool,
}

impl Selection {
    fn ordered(&self) -> ((usize, i32), (usize, i32)) {
        if self.anchor <= self.focus { (self.anchor, self.focus) } else { (self.focus, self.anchor) }
    }

    /// Inclusive character range selected on `page` (the end may be TO_END).
    fn range_on(&self, page: usize) -> (i32, i32) {
        let (a, b) = self.ordered();
        (if page == a.0 { a.1 } else { 0 }, if page == b.0 { b.1 } else { TO_END })
    }
}

struct App {
    hwnd: HWND,
    find_edit: HWND,
    page_edit: HWND,
    tree: HWND,
    find_visible: bool,
    fonts: Fonts,
    /// The app icon at welcome-screen size.
    logo: HICON,
    theme: Theme,
    /// Set when the user picked a theme; otherwise the Windows setting is followed.
    theme_override: Option<bool>,
    /// Background brush for the edit boxes, matching the theme.
    field_brush: HBRUSH,
    doc: Option<Document>,
    path: Option<PathBuf>,
    search: Option<Search>,
    search_status: String,
    selection: Option<Selection>,
    /// Recently used text layers (page, layer); must be cleared before `doc` changes.
    text_pages: Vec<(usize, TextPage)>,
    outline: Vec<OutlineItem>,
    /// The user wants the contents sidebar (it only shows when the PDF has an outline).
    sidebar_wanted: bool,
    /// Page sizes in points.
    page_sizes: Vec<(f32, f32)>,
    /// Top edge of each page in document pixels at the current zoom.
    page_tops: Vec<i32>,
    content_w: i32,
    content_h: i32,
    zoom: f32,
    fit: Fit,
    dpi: u32,
    scroll_x: i32,
    scroll_y: i32,
    /// Full client size; the document view is what the toolbar and sidebar leave.
    window_w: i32,
    window_h: i32,
    /// Top-left of the document view in the client area.
    view_x: i32,
    view_y: i32,
    /// Size of the document view.
    client_w: i32,
    client_h: i32,
    // Toolbar layout, in client coordinates.
    tools: Vec<(Tool, RECT)>,
    separators: Vec<i32>,
    page_box: RECT,
    page_label: RECT,
    search_field: RECT,
    /// "Open file" button on the empty screen, in view coordinates.
    empty_button: RECT,
    hover_tool: Option<Tool>,
    hover_empty_button: bool,
    /// Pointer is near a scrollbar, which then widens.
    scroll_hot: bool,
    tooltip: Option<Tool>,
    tracking_leave: bool,
    drag: Drag,
    deferred: Option<Deferred>,
    cache: Vec<CachedPage>,
    cache_bytes: usize,
    /// Private memory outside the page cache right after the document was (re)opened.
    mem_baseline: usize,
    frame: u64,
    title: String,
    shown_page: usize,
    toast: Option<String>,
    /// Window placement to restore when leaving full screen.
    fullscreen: Option<WINDOWPLACEMENT>,
}

impl App {
    fn new(hwnd: HWND, find_edit: HWND, page_edit: HWND, tree: HWND, dpi: u32) -> App {
        let mut app = App {
            hwnd,
            find_edit,
            page_edit,
            tree,
            find_visible: false,
            fonts: Fonts::new(dpi),
            logo: null_mut(),
            theme: Theme::light(),
            theme_override: None,
            field_brush: null_mut(),
            doc: None,
            path: None,
            search: None,
            search_status: String::new(),
            selection: None,
            text_pages: Vec::new(),
            outline: Vec::new(),
            sidebar_wanted: true,
            page_sizes: Vec::new(),
            page_tops: Vec::new(),
            content_w: 0,
            content_h: 0,
            zoom: 1.0,
            fit: Fit::Width,
            dpi,
            scroll_x: 0,
            scroll_y: 0,
            window_w: 0,
            window_h: 0,
            view_x: 0,
            view_y: 0,
            client_w: 0,
            client_h: 0,
            tools: Vec::new(),
            separators: Vec::new(),
            page_box: rect(0, 0, 0, 0),
            page_label: rect(0, 0, 0, 0),
            search_field: rect(0, 0, 0, 0),
            empty_button: rect(0, 0, 0, 0),
            hover_tool: None,
            hover_empty_button: false,
            scroll_hot: false,
            tooltip: None,
            tracking_leave: false,
            drag: Drag::None,
            deferred: None,
            cache: Vec::new(),
            cache_bytes: 0,
            mem_baseline: 0,
            frame: 0,
            title: String::new(),
            shown_page: usize::MAX,
            toast: None,
            fullscreen: None,
        };
        app.apply_fonts();
        app.apply_theme();
        app
    }

    fn open(&mut self, path: &Path) -> Result<(), String> {
        let doc = Document::open(path)?;
        self.save_position();
        // Text layers point into the old document, so they go before it does.
        self.text_pages.clear();
        self.selection = None;
        self.search = None;
        self.search_status.clear();
        self.page_sizes = (0..doc.page_count()).map(|i| doc.page_size(i)).collect();
        self.outline = doc.outline();
        self.doc = Some(doc);
        self.path = Some(path.to_path_buf());
        self.clear_cache();
        self.fill_outline_tree();

        let saved = positions::load(path).filter(|p| p.page < self.page_sizes.len());
        self.fit = saved.map_or(Fit::Width, |p| Fit::from_u8(p.fit));
        self.zoom = saved.map_or(1.0, |p| p.zoom.clamp(MIN_ZOOM, MAX_ZOOM));
        self.scroll_x = 0;
        self.scroll_y = 0;
        self.mem_baseline = private_bytes();
        self.shown_page = usize::MAX;
        self.layout();
        if let Some(pos) = saved {
            self.scroll_y = self.page_tops[pos.page] + (pos.offset_pt * self.px_per_pt()).round() as i32;
        }
        self.refresh();
        if let Some(pos) = saved.filter(|p| p.page > 0) {
            self.show_toast(format!("Resumed at page {}", pos.page + 1));
        }
        Ok(())
    }

    /// Remembers the reading position of the open document for next time.
    fn save_position(&self) {
        let Some(path) = &self.path else { return };
        if self.page_tops.is_empty() {
            return;
        }
        let page = self.top_page();
        let offset_pt = (self.scroll_y - self.page_tops[page]) as f32 / self.px_per_pt();
        positions::save(path, Position { page, offset_pt, zoom: self.zoom, fit: self.fit.to_u8() });
    }

    /// Reopens the document if PDFium's per-page data has grown past PDFIUM_GROWTH_LIMIT.
    /// Rendered pages, search results and the selection live in our own structures and survive this.
    fn trim_memory(&mut self) {
        let Some(path) = self.path.clone() else { return };
        let outside_cache = private_bytes().saturating_sub(self.cache_bytes);
        if outside_cache <= self.mem_baseline + PDFIUM_GROWTH_LIMIT {
            return;
        }
        if let Ok(doc) = Document::open(&path) {
            // Guard against the file having been replaced on disk.
            if doc.page_count() == self.page_sizes.len() {
                self.text_pages.clear();
                self.doc = Some(doc);
                self.mem_baseline = private_bytes().saturating_sub(self.cache_bytes);
            }
        }
    }

    fn s(&self, px: i32) -> i32 {
        px * self.dpi as i32 / 96
    }

    fn px_per_pt(&self) -> f32 {
        self.zoom * self.dpi as f32 / 72.0
    }

    fn gap(&self) -> i32 {
        self.s(PAGE_GAP)
    }

    fn line(&self) -> i32 {
        self.s(40)
    }

    fn page_px(&self, i: usize) -> (i32, i32) {
        let s = self.px_per_pt();
        let (w, h) = self.page_sizes[i];
        (((w * s).round() as i32).max(1), ((h * s).round() as i32).max(1))
    }

    fn page_x(&self, i: usize) -> i32 {
        (self.content_w.max(self.client_w) - self.page_px(i).0) / 2
    }

    /// Index of the page at document y coordinate `y`.
    fn page_at(&self, y: i32) -> usize {
        self.page_tops.partition_point(|&t| t <= y).saturating_sub(1)
    }

    /// The page whose top is at (or just above) the top of the view.
    fn top_page(&self) -> usize {
        self.page_at(self.scroll_y + self.gap())
    }

    fn current_page(&self) -> usize {
        self.page_at(self.scroll_y + self.client_h / 2)
    }

    fn toolbar_h(&self) -> i32 {
        if self.fullscreen.is_some() { 0 } else { self.s(TOOLBAR_H) }
    }

    fn sidebar_visible(&self) -> bool {
        self.sidebar_wanted && !self.outline.is_empty() && self.fullscreen.is_none()
    }

    // ---- Theme and fonts ----

    fn apply_fonts(&mut self) {
        self.fonts = Fonts::new(self.dpi);
        unsafe {
            if !self.logo.is_null() {
                DestroyIcon(self.logo);
            }
            self.logo = ui::app_icon(self.s(LOGO_SIZE));
            // Title bar and taskbar icons at this monitor's DPI.
            for (kind, size) in [(ICON_SMALL, 16), (ICON_BIG, 32)] {
                let old = SendMessageW(self.hwnd, WM_SETICON, kind as WPARAM, ui::app_icon(self.s(size)) as LPARAM);
                if old != 0 {
                    DestroyIcon(old as HICON);
                }
            }
            for edit in [self.find_edit, self.page_edit] {
                SendMessageW(edit, WM_SETFONT, self.fonts.ui as WPARAM, 1);
            }
            SendMessageW(self.tree, WM_SETFONT, self.fonts.ui as WPARAM, 1);
            SendMessageW(self.tree, TVM_SETITEMHEIGHT, self.s(30) as WPARAM, 0);
            SendMessageW(self.tree, TVM_SETINDENT, self.s(16) as WPARAM, 0);
        }
    }

    fn apply_theme(&mut self) {
        let dark = self.theme_override.unwrap_or_else(ui::system_prefers_dark);
        self.theme = if dark { Theme::dark() } else { Theme::light() };
        let t = self.theme;
        ui::style_title_bar(self.hwnd, &t);
        ui::style_menus(dark);
        ui::style_control(self.tree, dark);
        unsafe {
            SendMessageW(self.tree, TVM_SETBKCOLOR, 0, t.chrome_bg as LPARAM);
            SendMessageW(self.tree, TVM_SETTEXTCOLOR, 0, t.text as LPARAM);
            if !self.field_brush.is_null() {
                DeleteObject(self.field_brush);
            }
            self.field_brush = CreateSolidBrush(t.field_bg);
            RedrawWindow(self.hwnd, null(), null_mut(), RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN);
        }
    }

    // ---- Layout ----

    fn layout(&mut self) {
        let sidebar = self.sidebar_visible();
        self.view_x = if sidebar { self.s(SIDEBAR_W).min(self.window_w / 2) } else { 0 };
        self.view_y = self.toolbar_h();
        self.client_w = (self.window_w - self.view_x).max(0);
        self.client_h = (self.window_h - self.view_y).max(0);
        unsafe {
            let top = self.view_y + self.s(SIDEBAR_HEADER_H);
            MoveWindow(self.tree, 0, top, (self.view_x - 1).max(0), (self.window_h - top).max(0), 1);
            ShowWindow(self.tree, if sidebar { SW_SHOWNA } else { SW_HIDE });
        }
        self.layout_toolbar();

        let gap = self.gap();
        let max_w_pt = self.page_sizes.iter().map(|s| s.0).fold(0.0f32, f32::max);
        let max_h_pt = self.page_sizes.iter().map(|s| s.1).fold(0.0f32, f32::max);
        let pt_to_px = self.dpi as f32 / 72.0;
        if max_w_pt > 0.0 && self.client_w > 0 {
            let fit_w = (self.client_w - 2 * gap).max(50) as f32 / (max_w_pt * pt_to_px);
            let fit_h = (self.client_h - 2 * gap).max(50) as f32 / (max_h_pt * pt_to_px);
            match self.fit {
                Fit::Width => self.zoom = fit_w.clamp(MIN_ZOOM, MAX_ZOOM),
                Fit::Page => self.zoom = fit_w.min(fit_h).clamp(MIN_ZOOM, MAX_ZOOM),
                Fit::None => {}
            }
        }
        let s = self.px_per_pt();
        let mut y = gap;
        self.page_tops.clear();
        for i in 0..self.page_sizes.len() {
            self.page_tops.push(y);
            y += self.page_px(i).1 + gap;
        }
        self.content_h = if self.page_sizes.is_empty() { 0 } else { y };
        self.content_w = (max_w_pt * s).round() as i32 + 2 * gap;

        let before = self.cache.len();
        self.cache.retain(|c| c.scale == s);
        if self.cache.len() != before {
            self.cache_bytes = self.cache.iter().map(|c| c.area.bytes()).sum();
        }
    }

    fn layout_toolbar(&mut self) {
        self.tools.clear();
        self.separators.clear();
        let tb = self.toolbar_h();
        unsafe {
            ShowWindow(self.page_edit, if tb > 0 { SW_SHOWNA } else { SW_HIDE });
            ShowWindow(self.find_edit, if tb > 0 && self.find_visible { SW_SHOWNA } else { SW_HIDE });
            EnableWindow(self.page_edit, self.doc.is_some() as BOOL);
        }
        if tb == 0 {
            return;
        }
        let (bw, bh, gap, sep) = (self.s(36), self.s(32), self.s(2), self.s(9));
        let top = (tb - bh) / 2;
        let mut x = self.s(8);
        let add = |tools: &mut Vec<(Tool, RECT)>, x: &mut i32, tool: Tool, w: i32| {
            tools.push((tool, rect(*x, top, w, bh)));
            *x += w + gap;
        };

        add(&mut self.tools, &mut x, Tool::Sidebar, bw);
        add(&mut self.tools, &mut x, Tool::Open, bw);
        self.separators.push(x + sep - gap);
        x += 2 * sep;
        add(&mut self.tools, &mut x, Tool::PrevPage, bw);
        self.page_box = rect(x, top, self.s(52), bh);
        x += self.s(52) + self.s(6);
        let label = format!("of {}", self.page_sizes.len());
        let label_w = unsafe {
            let dc = GetDC(self.hwnd);
            let w = ui::text_width(dc, self.fonts.ui, &label);
            ReleaseDC(self.hwnd, dc);
            w
        };
        self.page_label = rect(x, top, label_w, bh);
        x += label_w + self.s(4);
        add(&mut self.tools, &mut x, Tool::NextPage, bw);
        self.separators.push(x + sep - gap);
        x += 2 * sep;
        add(&mut self.tools, &mut x, Tool::ZoomOut, bw);
        let zoom_w = self.s(64);
        add(&mut self.tools, &mut x, Tool::ZoomMenu, zoom_w);
        add(&mut self.tools, &mut x, Tool::ZoomIn, bw);

        // Right side, laid out from the right edge.
        let mut r = self.window_w - self.s(8);
        let add_right = |tools: &mut Vec<(Tool, RECT)>, r: &mut i32, tool: Tool| {
            *r -= bw;
            tools.push((tool, rect(*r, top, bw, bh)));
            *r -= gap;
        };
        add_right(&mut self.tools, &mut r, Tool::More);
        add_right(&mut self.tools, &mut r, Tool::FullScreen);
        self.separators.push(r - sep + gap);
        r -= 2 * sep;
        if self.find_visible {
            add_right(&mut self.tools, &mut r, Tool::FindClose);
            add_right(&mut self.tools, &mut r, Tool::FindNext);
            add_right(&mut self.tools, &mut r, Tool::FindPrev);
        } else {
            add_right(&mut self.tools, &mut r, Tool::Search);
        }

        // The fit buttons go first when the search field needs the room (they are also in the zoom menu).
        let fit_end = x + 2 * sep + 2 * (bw + gap);
        let min_field = if self.find_visible { self.s(340) } else { 0 };
        if r - fit_end - sep >= min_field {
            self.separators.push(x + sep - gap);
            x += 2 * sep;
            add(&mut self.tools, &mut x, Tool::FitWidth, bw);
            add(&mut self.tools, &mut x, Tool::FitPage, bw);
        }
        self.search_field = if self.find_visible {
            let w = self.s(420).min(r - x - sep).max(self.s(160));
            rect(r - w, top, w, bh)
        } else {
            rect(0, 0, 0, 0)
        };

        // The edit boxes sit inside their drawn fields.
        let eh = self.s(20);
        unsafe {
            let b = self.page_box;
            MoveWindow(self.page_edit, b.left + self.s(4), b.top + (bh - eh) / 2, b.right - b.left - self.s(8), eh, 1);
            let f = self.search_field;
            let (left_pad, right_pad) = (self.s(32), self.s(92));
            MoveWindow(self.find_edit, f.left + left_pad, f.top + (bh - eh) / 2, (f.right - f.left - left_pad - right_pad).max(0), eh, 1);
        }
    }

    /// Keeps the document point under view position (ax, ay) in place across a layout change.
    fn with_anchor(&mut self, ax: i32, ay: i32, change: impl FnOnce(&mut App)) {
        let anchor = (!self.page_tops.is_empty()).then(|| {
            let doc_y = self.scroll_y + ay;
            let page = self.page_at(doc_y);
            let off_pt = (doc_y - self.page_tops[page]) as f32 / self.px_per_pt();
            let frac_x = (self.scroll_x + ax) as f32 / self.content_w.max(1) as f32;
            (page, off_pt, frac_x)
        });
        change(self);
        self.layout();
        if let Some((page, off_pt, frac_x)) = anchor {
            self.scroll_y = self.page_tops[page] + (off_pt * self.px_per_pt()).round() as i32 - ay;
            self.scroll_x = (frac_x * self.content_w as f32).round() as i32 - ax;
        }
        self.refresh();
    }

    fn set_zoom(&mut self, zoom: f32, ax: i32, ay: i32) {
        if self.doc.is_none() {
            return;
        }
        self.with_anchor(ax, ay, |a| {
            a.fit = Fit::None;
            a.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        });
    }

    fn set_fit(&mut self, fit: Fit) {
        if self.doc.is_none() {
            return;
        }
        if fit == Fit::Page {
            let page = self.current_page();
            self.fit = fit;
            self.layout();
            self.scroll_x = 0;
            self.go_to_page(page);
        } else {
            self.with_anchor(self.client_w / 2, self.client_h / 2, |a| a.fit = fit);
        }
    }

    fn toggle_outline(&mut self) {
        if self.outline.is_empty() {
            let msg = if self.doc.is_some() { "This PDF has no table of contents" } else { "Open a PDF first" };
            self.show_toast(msg.to_string());
            return;
        }
        self.with_anchor(0, 0, |a| a.sidebar_wanted = !a.sidebar_wanted);
    }

    fn go_to_page(&mut self, page: usize) {
        if let Some(&top) = self.page_tops.get(page) {
            self.scroll_to(self.scroll_x, top - self.gap());
        }
    }

    fn next_page(&mut self) {
        let p = self.top_page();
        self.go_to_page((p + 1).min(self.page_tops.len().saturating_sub(1)));
    }

    fn prev_page(&mut self) {
        let p = self.top_page();
        let Some(&top) = self.page_tops.get(p) else { return };
        // Partway into a page, "previous" first goes back to its top.
        if self.scroll_y > top - self.gap() + self.s(4) {
            self.go_to_page(p);
        } else {
            self.go_to_page(p.saturating_sub(1));
        }
    }

    fn scroll_to(&mut self, x: i32, y: i32) {
        self.scroll_x = x;
        self.scroll_y = y;
        self.refresh();
    }

    /// Clamps scrolling, syncs the title and page box, and schedules a repaint.
    fn refresh(&mut self) {
        self.scroll_x = self.scroll_x.clamp(0, (self.content_w - self.client_w).max(0));
        self.scroll_y = self.scroll_y.clamp(0, (self.content_h - self.client_h).max(0));
        unsafe { InvalidateRect(self.hwnd, null(), 0) };
        self.update_title();
    }

    fn update_title(&mut self) {
        let mut page = 0;
        let title = match &self.path {
            Some(path) if !self.page_sizes.is_empty() => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                page = self.current_page() + 1;
                format!("{name} - {APP_NAME}")
            }
            _ => APP_NAME.to_string(),
        };
        if title != self.title {
            unsafe { SetWindowTextW(self.hwnd, wide(&title).as_ptr()) };
            self.title = title;
        }
        if page != self.shown_page {
            // The toolbar is hidden in full screen, so announce page changes there instead.
            if self.fullscreen.is_some() && page > 0 {
                self.show_toast(format!("Page {page} of {}", self.page_sizes.len()));
            }
            self.shown_page = page;
            self.sync_page_box();
            self.trim_memory();
        }
    }

    /// Shows the current page in the page box, unless the user is typing there.
    fn sync_page_box(&self) {
        unsafe {
            if GetFocus() != self.page_edit {
                let text = if self.page_sizes.is_empty() { String::new() } else { (self.current_page() + 1).to_string() };
                SetWindowTextW(self.page_edit, wide(&text).as_ptr());
            }
        }
    }

    fn on_page_key(&mut self, vk: u16) {
        if vk == VK_RETURN {
            let text = unsafe {
                let mut buf = [0u16; 16];
                let len = GetWindowTextW(self.page_edit, buf.as_mut_ptr(), buf.len() as i32);
                String::from_utf16_lossy(&buf[..len.max(0) as usize])
            };
            if let Ok(n) = text.trim().parse::<usize>() {
                self.go_to_page(n.clamp(1, self.page_sizes.len().max(1)) - 1);
            }
        }
        unsafe { SetFocus(self.hwnd) };
        self.sync_page_box();
    }

    fn show_toast(&mut self, text: String) {
        self.toast = Some(text);
        unsafe {
            SetTimer(self.hwnd, TOAST_TIMER, TOAST_MS, None);
            InvalidateRect(self.hwnd, null(), 0);
        }
    }

    fn clear_cache(&mut self) {
        self.cache = Vec::new();
        self.cache_bytes = 0;
    }

    /// Returns the cache index of a bitmap for `page` covering `needed` (page pixel coordinates).
    fn ensure_rendered(&mut self, page: usize, needed: Area) -> Option<usize> {
        if let Some(idx) = self.cache.iter().position(|c| c.page == page && c.area.contains(&needed)) {
            self.cache[idx].last_used = self.frame;
            return Some(idx);
        }
        let (pw, ph) = self.page_px(page);
        let full = Area { x: 0, y: 0, w: pw, h: ph };
        let target = if full.bytes() <= FULL_PAGE_LIMIT {
            full
        } else {
            // Zoomed far in: render the visible part plus a margin to scroll into.
            let margin = Area {
                x: needed.x - self.client_w / 2,
                y: needed.y - self.client_h,
                w: needed.w + self.client_w,
                h: needed.h + 2 * self.client_h,
            };
            margin.intersect(&full)?
        };

        self.cache.retain(|c| c.page != page);
        let pixels = self.doc.as_ref()?.render(page, pw, ph, target.x, target.y, target.w, target.h)?;
        self.cache.push(CachedPage { page, scale: self.px_per_pt(), area: target, pixels, last_used: self.frame });
        self.cache_bytes = self.cache.iter().map(|c| c.area.bytes()).sum();
        self.evict();
        self.cache.iter().position(|c| c.page == page)
    }

    /// Drops least recently used bitmaps until under budget; pages on screen right now are kept.
    fn evict(&mut self) {
        while self.cache_bytes > CACHE_BUDGET {
            let oldest = self
                .cache
                .iter()
                .enumerate()
                .filter(|(_, c)| c.last_used < self.frame)
                .min_by_key(|(_, c)| c.last_used)
                .map(|(i, _)| i);
            let Some(i) = oldest else { break };
            self.cache_bytes -= self.cache.swap_remove(i).area.bytes();
        }
    }

    /// The text layer of `page`, from a small most-recently-used cache.
    fn text_page(&mut self, page: usize) -> Option<&TextPage> {
        if let Some(i) = self.text_pages.iter().position(|(p, _)| *p == page) {
            let entry = self.text_pages.remove(i);
            self.text_pages.push(entry);
        } else {
            let tp = self.doc.as_ref()?.text_page(page)?;
            if self.text_pages.len() >= TEXT_PAGE_CACHE {
                self.text_pages.remove(0);
            }
            self.text_pages.push((page, tp));
        }
        self.text_pages.last().map(|(_, tp)| tp)
    }

    // ---- Contents sidebar ----

    fn fill_outline_tree(&mut self) {
        unsafe {
            SendMessageW(self.tree, TVM_DELETEITEM, 0, TVI_ROOT as LPARAM);
            // parents[d] is the tree item new items at depth d go under.
            let mut parents: Vec<HTREEITEM> = vec![TVI_ROOT];
            let mut expand = Vec::new();
            for (i, item) in self.outline.iter().enumerate() {
                parents.truncate(item.depth + 1);
                let Some(&parent) = parents.last() else { continue };
                let mut text = wide(if item.title.is_empty() { "(untitled)" } else { &item.title });
                let mut insert: TVINSERTSTRUCTW = zeroed();
                insert.hParent = parent;
                insert.hInsertAfter = TVI_LAST;
                insert.Anonymous.item.mask = TVIF_TEXT | TVIF_PARAM;
                insert.Anonymous.item.pszText = text.as_mut_ptr();
                insert.Anonymous.item.lParam = i as LPARAM;
                let handle = SendMessageW(self.tree, TVM_INSERTITEMW, 0, &insert as *const _ as LPARAM) as HTREEITEM;
                if item.expanded {
                    expand.push(handle);
                }
                parents.push(handle);
            }
            for handle in expand {
                SendMessageW(self.tree, TVM_EXPAND, TVE_EXPAND as WPARAM, handle as LPARAM);
            }
        }
    }

    fn go_to_outline_item(&mut self, index: usize) {
        let Some(item) = self.outline.get(index) else { return };
        let (Some(page), point) = (item.page, item.point) else { return };
        if page >= self.page_tops.len() {
            return;
        }
        let y_pt = point.and_then(|(x, y)| self.doc.as_ref()?.pdf_point_to_top_left(page, x, y)).map(|p| p.1);
        let y = self.page_tops[page] + y_pt.map_or(0, |y| (y * self.px_per_pt()) as i32) - self.gap();
        self.scroll_to(self.scroll_x, y);
    }

    fn tree_item_param(&self, item: HTREEITEM) -> Option<usize> {
        unsafe {
            let mut tv: TVITEMW = zeroed();
            tv.mask = TVIF_PARAM;
            tv.hItem = item;
            (SendMessageW(self.tree, TVM_GETITEMW, 0, &mut tv as *mut _ as LPARAM) != 0).then_some(tv.lParam as usize)
        }
    }

    fn on_tree_notify(&mut self, lparam: LPARAM) {
        unsafe {
            let header = &*(lparam as *const NMHDR);
            match header.code {
                // Clicking an item navigates even if it is already selected.
                NM_CLICK => {
                    let mut hit: TVHITTESTINFO = zeroed();
                    let pos = GetMessagePos();
                    hit.pt = POINT { x: loword(pos as usize) as i16 as i32, y: hiword(pos as usize) as i16 as i32 };
                    ScreenToClient(self.tree, &mut hit.pt);
                    let item = SendMessageW(self.tree, TVM_HITTEST, 0, &mut hit as *mut _ as LPARAM) as HTREEITEM;
                    if item != 0 as HTREEITEM && hit.flags & TVHT_ONITEM != 0 {
                        if let Some(index) = self.tree_item_param(item) {
                            self.go_to_outline_item(index);
                        }
                    }
                }
                TVN_SELCHANGEDW => {
                    let nm = &*(lparam as *const NMTREEVIEWW);
                    if nm.action == TVC_BYKEYBOARD {
                        self.go_to_outline_item(nm.itemNew.lParam as usize);
                    }
                }
                _ => {}
            }
        }
    }

    // ---- Text selection ----

    /// The page under view position (x, y) and the point on it in points from its
    /// top-left, clamped to the page.
    fn view_point(&self, x: i32, y: i32) -> Option<(usize, f32, f32)> {
        if self.page_tops.is_empty() {
            return None;
        }
        let doc_y = self.scroll_y + y;
        let page = self.page_at(doc_y.max(0));
        let (pw, ph) = self.page_px(page);
        let s = self.px_per_pt();
        let px = (x + self.scroll_x - self.page_x(page)).clamp(0, pw);
        let py = (doc_y - self.page_tops[page]).clamp(0, ph);
        Some((page, px as f32 / s, py as f32 / s))
    }

    fn over_page(&self, x: i32, y: i32) -> bool {
        if self.page_tops.is_empty() || x < 0 || y < 0 || x >= self.client_w || y >= self.client_h {
            return false;
        }
        let doc_y = self.scroll_y + y;
        let page = self.page_at(doc_y);
        let (pw, ph) = self.page_px(page);
        let px = x + self.scroll_x - self.page_x(page);
        let py = doc_y - self.page_tops[page];
        (0..pw).contains(&px) && (0..ph).contains(&py)
    }

    fn char_at_view(&mut self, x: i32, y: i32) -> Option<(usize, i32)> {
        let (page, px, py) = self.view_point(x, y)?;
        let index = self.text_page(page)?.char_at(px, py)?;
        Some((page, index))
    }

    fn start_selection(&mut self, x: i32, y: i32, double: bool) {
        if self.selection.take().is_some() {
            unsafe { InvalidateRect(self.hwnd, null(), 0) };
        }
        let Some((page, index)) = self.char_at_view(x, y) else { return };
        if double {
            let Some((from, to)) = self.text_page(page).map(|tp| tp.word_at(index)) else { return };
            self.selection = Some(Selection { anchor: (page, from), focus: (page, to), rects: HashMap::new(), moved: true });
            self.update_selection();
            return;
        }
        self.selection = Some(Selection { anchor: (page, index), focus: (page, index), rects: HashMap::new(), moved: false });
        self.drag = Drag::Select;
        unsafe {
            SetCapture(self.hwnd);
            SetTimer(self.hwnd, DRAG_TIMER, DRAG_MS, None);
        }
    }

    fn extend_selection(&mut self, x: i32, y: i32) {
        let Some(focus) = self.char_at_view(x, y) else { return };
        if let Some(sel) = &mut self.selection {
            if sel.focus != focus {
                sel.focus = focus;
                sel.moved = true;
                self.update_selection();
            }
        }
    }

    fn finish_selection(&mut self) {
        if self.selection.as_ref().is_some_and(|s| !s.moved) {
            // A plain click: nothing selected.
            self.selection = None;
        }
        self.text_pages.clear();
        unsafe {
            KillTimer(self.hwnd, DRAG_TIMER);
            InvalidateRect(self.hwnd, null(), 0);
        }
    }

    /// While dragging past the top or bottom edge, scrolls toward the pointer and extends the selection.
    fn on_drag_timer(&mut self) {
        if self.drag != Drag::Select {
            return;
        }
        let (x, y) = self.cursor_in_view();
        let dy = if y < 0 { y } else if y > self.client_h { y - self.client_h } else { 0 };
        if dy != 0 {
            self.scroll_to(self.scroll_x, self.scroll_y + dy);
            self.extend_selection(x, y.clamp(0, self.client_h - 1));
        }
    }

    /// Recomputes highlight rectangles for pages whose selected range changed.
    fn update_selection(&mut self) {
        let Some(sel) = &mut self.selection else { return };
        let ((first, _), (last, _)) = sel.ordered();
        let ranges: Vec<(usize, (i32, i32))> = (first..=last).map(|p| (p, sel.range_on(p))).collect();
        let mut rects = std::mem::take(&mut sel.rects);
        rects.retain(|p, _| (first..=last).contains(p));
        for (page, range) in ranges {
            if rects.get(&page).is_some_and(|(r, _)| *r == range) {
                continue;
            }
            let page_rects = self.text_page(page).map(|tp| tp.rects(range.0, range.1)).unwrap_or_default();
            rects.insert(page, (range, page_rects));
        }
        if let Some(sel) = &mut self.selection {
            sel.rects = rects;
        }
        unsafe { InvalidateRect(self.hwnd, null(), 0) };
    }

    fn select_page_text(&mut self) {
        if self.page_tops.is_empty() {
            return;
        }
        let page = self.current_page();
        let count = self.text_page(page).map_or(0, |tp| tp.char_count());
        if count == 0 {
            self.show_toast("No text on this page".to_string());
            return;
        }
        self.selection = Some(Selection { anchor: (page, 0), focus: (page, count - 1), rects: HashMap::new(), moved: true });
        self.update_selection();
        self.show_toast(format!("Selected the text of page {}", page + 1));
    }

    fn copy_selection(&mut self) {
        let Some(sel) = &self.selection else {
            self.show_toast("Nothing selected".to_string());
            return;
        };
        let ((first, _), (last, _)) = sel.ordered();
        let ranges: Vec<(usize, (i32, i32))> = (first..=last).map(|p| (p, sel.range_on(p))).collect();
        let mut text = String::new();
        for (page, (from, to)) in ranges {
            // Load each page on its own so a long selection doesn't keep many text layers alive.
            let Some(tp) = self.doc.as_ref().and_then(|d| d.text_page(page)) else { continue };
            if !text.is_empty() && !text.ends_with('\n') {
                text.push_str("\r\n");
            }
            text.push_str(&tp.text(from, to));
        }
        if set_clipboard(self.hwnd, &text) {
            self.show_toast(format!("Copied {} characters", text.chars().count()));
        } else {
            self.show_toast("Could not open the clipboard".to_string());
        }
    }

    // ---- Search ----

    fn page_hits(&mut self, page: usize) -> &[Match] {
        let (Some(doc), Some(Search { query, hits, .. })) = (&self.doc, &mut self.search) else {
            return &[];
        };
        hits.entry(page).or_insert_with(|| doc.find(page, query))
    }

    fn open_find(&mut self) {
        if self.doc.is_none() {
            return;
        }
        if !self.find_visible {
            self.find_visible = true;
            self.layout_toolbar();
        }
        unsafe {
            SetFocus(self.find_edit);
            SendMessageW(self.find_edit, EM_SETSEL, 0, -1);
            InvalidateRect(self.hwnd, null(), 0);
        }
    }

    fn close_find(&mut self) -> bool {
        if !self.find_visible {
            return false;
        }
        self.find_visible = false;
        self.search = None;
        self.search_status.clear();
        self.layout_toolbar();
        unsafe {
            SetFocus(self.hwnd);
            InvalidateRect(self.hwnd, null(), 0);
        }
        true
    }

    /// Closes the find box, else clears the selection. Returns false if there was neither.
    fn escape(&mut self) -> bool {
        if self.close_find() {
            return true;
        }
        if self.selection.take().is_some() {
            unsafe { InvalidateRect(self.hwnd, null(), 0) };
            return true;
        }
        false
    }

    fn find(&mut self, forward: bool) {
        let query = unsafe {
            let mut buf = vec![0u16; GetWindowTextLengthW(self.find_edit) as usize + 1];
            let len = GetWindowTextW(self.find_edit, buf.as_mut_ptr(), buf.len() as i32);
            buf.truncate(len.max(0) as usize);
            buf
        };
        if query.is_empty() || self.page_sizes.is_empty() {
            self.open_find();
            return;
        }
        if self.search.as_ref().is_none_or(|s| s.query != query) {
            self.search = Some(Search { query, hits: HashMap::new(), current: None, pending: None });
        }

        let (start_page, start_idx) = match self.search.as_ref().and_then(|s| s.current) {
            Some((p, i)) => (p, Some(i)),
            None => (self.top_page(), None),
        };
        let len = self.page_hits(start_page).len();
        let on_start_page = match (forward, start_idx) {
            (true, Some(i)) if i + 1 < len => Some(i + 1),
            (true, None) if len > 0 => Some(0),
            (false, Some(i)) if i > 0 => Some(i - 1),
            (false, None) if len > 0 => Some(len - 1),
            _ => None,
        };
        if let Some(idx) = on_start_page {
            self.select_match(start_page, idx, false);
            return;
        }
        if let Some(s) = &mut self.search {
            s.pending = Some(PendingSearch { forward, start_page, scanned: 0 });
        }
        self.continue_search();
    }

    /// Scans further pages for the pending search. Works in short slices and posts
    /// WM_APP_SEARCH to itself to continue, so long documents don't freeze the window.
    fn continue_search(&mut self) {
        let Some(PendingSearch { forward, start_page, mut scanned }) = self.search.as_ref().and_then(|s| s.pending) else {
            return;
        };
        let n = self.page_sizes.len();
        let deadline = Instant::now() + SEARCH_SLICE;
        while scanned < n {
            scanned += 1;
            let p = if forward { (start_page + scanned) % n } else { (start_page + n - scanned) % n };
            let len = self.page_hits(p).len();
            if len > 0 {
                let wrapped = if forward { p <= start_page } else { p >= start_page };
                self.end_search();
                self.select_match(p, if forward { 0 } else { len - 1 }, wrapped);
                return;
            }
            if Instant::now() > deadline && scanned < n {
                if let Some(s) = &mut self.search {
                    s.pending = Some(PendingSearch { forward, start_page, scanned });
                }
                self.trim_memory();
                self.search_status = format!("Searching {}%", scanned * 100 / n);
                unsafe {
                    InvalidateRect(self.hwnd, null(), 0);
                    PostMessageW(self.hwnd, WM_APP_SEARCH, 0, 0);
                }
                return;
            }
        }
        self.end_search();
        if let Some(s) = &mut self.search {
            s.current = None;
        }
        self.search_status = "No results".to_string();
        unsafe { InvalidateRect(self.hwnd, null(), 0) };
    }

    fn end_search(&mut self) {
        if let Some(s) = &mut self.search {
            s.pending = None;
        }
        self.trim_memory();
    }

    fn select_match(&mut self, page: usize, idx: usize, wrapped: bool) {
        if let Some(s) = &mut self.search {
            s.current = Some((page, idx));
        }
        let count = self.page_hits(page).len();
        self.search_status = format!("{}/{count} on p. {}", idx + 1, page + 1);
        if wrapped {
            self.show_toast("Search continued from the other end".to_string());
        }
        self.scroll_to_match(page, idx);
    }

    fn scroll_to_match(&mut self, page: usize, idx: usize) {
        let s = self.px_per_pt();
        let Some(rects) = self.page_hits(page).get(idx).cloned() else { return };
        let left = rects.iter().map(|r| r[0]).fold(f32::MAX, f32::min);
        let top = rects.iter().map(|r| r[1]).fold(f32::MAX, f32::min);
        let right = rects.iter().map(|r| r[0] + r[2]).fold(0.0, f32::max);
        let bottom = rects.iter().map(|r| r[1] + r[3]).fold(0.0, f32::max);
        let (x, y) = (self.page_x(page), self.page_tops[page]);
        let (l, t) = (x + (left * s) as i32, y + (top * s) as i32);
        let (r, b) = (x + (right * s) as i32, y + (bottom * s) as i32);
        let (mut sx, mut sy) = (self.scroll_x, self.scroll_y);
        if t < sy || b > sy + self.client_h {
            sy = t - self.client_h / 3;
        }
        if l < sx || r > sx + self.client_w {
            sx = l - self.client_w / 3;
        }
        self.scroll_to(sx, sy);
    }

    // ---- Scrollbars (drawn over the view, Windows 11 style) ----

    /// Vertical scrollbar track and thumb in view coordinates, if the document is taller than the view.
    fn v_bar(&self) -> Option<(Area, Area)> {
        let max = self.content_h - self.client_h;
        if max <= 0 {
            return None;
        }
        let (m, w) = (self.s(2), self.s(14));
        let bottom = if self.content_w > self.client_w { w } else { 0 };
        let track = Area { x: self.client_w - w, y: m, w, h: self.client_h - 2 * m - bottom };
        let len = ((track.h as i64 * self.client_h as i64 / self.content_h as i64) as i32).clamp(self.s(32).min(track.h), track.h);
        let pos = track.y + ((track.h - len) as i64 * self.scroll_y as i64 / max as i64) as i32;
        Some((track, Area { x: track.x, y: pos, w, h: len }))
    }

    fn h_bar(&self) -> Option<(Area, Area)> {
        let max = self.content_w - self.client_w;
        if max <= 0 {
            return None;
        }
        let (m, h) = (self.s(2), self.s(14));
        let right = if self.content_h > self.client_h { h } else { 0 };
        let track = Area { x: m, y: self.client_h - h, w: self.client_w - 2 * m - right, h };
        let len = ((track.w as i64 * self.client_w as i64 / self.content_w as i64) as i32).clamp(self.s(32).min(track.w), track.w);
        let pos = track.x + ((track.w - len) as i64 * self.scroll_x as i64 / max as i64) as i32;
        Some((track, Area { x: pos, y: track.y, w: len, h }))
    }

    /// Starts dragging a scrollbar if (x, y) is on one. Clicking the track jumps there.
    fn grab_scrollbar(&mut self, x: i32, y: i32) -> bool {
        if let Some((track, thumb)) = self.v_bar().filter(|(t, _)| t.has(x, y)) {
            let grab = if thumb.has(x, y) { y - thumb.y } else { thumb.h / 2 };
            self.drag = Drag::VThumb(grab);
            self.drag_scrollbar(x, y);
            let _ = track;
        } else if let Some((_, thumb)) = self.h_bar().filter(|(t, _)| t.has(x, y)) {
            let grab = if thumb.has(x, y) { x - thumb.x } else { thumb.w / 2 };
            self.drag = Drag::HThumb(grab);
            self.drag_scrollbar(x, y);
        } else {
            return false;
        }
        unsafe { SetCapture(self.hwnd) };
        true
    }

    fn drag_scrollbar(&mut self, x: i32, y: i32) {
        match self.drag {
            Drag::VThumb(grab) => {
                let Some((track, thumb)) = self.v_bar() else { return };
                let room = (track.h - thumb.h).max(1) as i64;
                let max = (self.content_h - self.client_h) as i64;
                let y = ((y - grab - track.y) as i64 * max / room) as i32;
                self.scroll_to(self.scroll_x, y);
            }
            Drag::HThumb(grab) => {
                let Some((track, thumb)) = self.h_bar() else { return };
                let room = (track.w - thumb.w).max(1) as i64;
                let max = (self.content_w - self.client_w) as i64;
                let x = ((x - grab - track.x) as i64 * max / room) as i32;
                self.scroll_to(x, self.scroll_y);
            }
            _ => {}
        }
    }

    fn near_scrollbar(&self, x: i32, y: i32) -> bool {
        self.v_bar().is_some_and(|(t, _)| t.has(x, y)) || self.h_bar().is_some_and(|(t, _)| t.has(x, y))
    }

    // ---- Painting ----

    fn paint(&mut self, hdc: HDC) {
        self.frame += 1;
        let t = self.theme;
        unsafe {
            // Chrome first (double-buffered, so hover changes don't flicker), then clip it away.
            if self.view_y > 0 {
                ui::buffered(hdc, &rect(0, 0, self.window_w, self.view_y), |dc| self.paint_toolbar(dc));
            }
            if self.view_x > 0 {
                let header = rect(0, self.view_y, self.view_x, self.s(SIDEBAR_HEADER_H));
                ui::buffered(hdc, &header, |dc| {
                    fill(dc, &header, t.chrome_bg);
                    let mut label = header;
                    label.left += self.s(16);
                    text(dc, self.fonts.ui_bold, "Contents", &label, t.text, DT_VCENTER);
                });
                fill(hdc, &rect(self.view_x - 1, self.view_y, 1, self.window_h - self.view_y), t.border);
            }
            ExcludeClipRect(hdc, 0, 0, self.window_w, self.view_y);
            ExcludeClipRect(hdc, 0, self.view_y, self.view_x, self.window_h);

            // The document view, in its own coordinates.
            SetViewportOrgEx(hdc, self.view_x, self.view_y, null_mut());
            if self.doc.is_none() {
                // Drawn off-screen and copied in one go: painting the text over a freshly
                // cleared background would make it blink on every repaint.
                let block = self.welcome_rect();
                ui::buffered(hdc, &block, |dc| {
                    fill(dc, &block, t.view_bg);
                    self.paint_empty_state(dc);
                });
                ExcludeClipRect(hdc, block.left, block.top, block.right, block.bottom);
            }
            self.paint_pages(hdc);
            fill(hdc, &rect(0, 0, self.client_w, self.client_h), t.view_bg);

            // Overlays, clipped to the view.
            SelectClipRgn(hdc, null_mut());
            IntersectClipRect(hdc, 0, 0, self.client_w, self.client_h);
            self.paint_scrollbars(hdc);
            self.paint_toast(hdc);

            SetViewportOrgEx(hdc, 0, 0, null_mut());
            SelectClipRgn(hdc, null_mut());
            self.paint_tooltip(hdc);
        }
    }

    unsafe fn paint_pages(&mut self, hdc: HDC) {
        if self.doc.is_none() || self.page_tops.is_empty() {
            return;
        }
        let client = Area { x: 0, y: 0, w: self.client_w, h: self.client_h };
        let mut i = self.page_at(self.scroll_y);
        while i < self.page_tops.len() && self.page_tops[i] < self.scroll_y + self.client_h {
            let (pw, ph) = self.page_px(i);
            let sx = self.page_x(i) - self.scroll_x;
            let sy = self.page_tops[i] - self.scroll_y;
            let page = Area { x: sx, y: sy, w: pw, h: ph };
            if let Some(vis) = page.intersect(&client) {
                let needed = Area { x: vis.x - sx, y: vis.y - sy, w: vis.w, h: vis.h };
                if let Some(idx) = self.ensure_rendered(i, needed) {
                    let c = &self.cache[idx];
                    unsafe {
                        blit(hdc, sx + c.area.x, sy + c.area.y, c.area.w, c.area.h, &c.pixels);
                        self.paint_highlights(hdc, i, sx, sy);
                        ExcludeClipRect(hdc, vis.x, vis.y, vis.x + vis.w, vis.y + vis.h);
                    }
                }
            }
            unsafe { self.paint_shadow(hdc, page) };
            i += 1;
        }
    }

    /// A soft shadow: rings around the page that fade into the background. Each ring is
    /// drawn as four strips and then clipped away, so nothing is painted twice.
    unsafe fn paint_shadow(&self, hdc: HDC, page: Area) {
        let t = self.theme;
        let ring = |k: i32| {
            let k = self.s(k);
            rect(page.x - k, page.y - k + self.s(1), page.w + 2 * k, page.h + 2 * k)
        };
        let mut inner = page.rect();
        for (k, alpha) in t.shadow.iter().enumerate() {
            let outer = ring(k as i32 + 1);
            let color = ui::blend(t.view_bg, 0, *alpha);
            unsafe {
                for strip in [
                    RECT { left: outer.left, top: outer.top, right: outer.right, bottom: inner.top },
                    RECT { left: outer.left, top: inner.bottom, right: outer.right, bottom: outer.bottom },
                    RECT { left: outer.left, top: inner.top, right: inner.left, bottom: inner.bottom },
                    RECT { left: inner.right, top: inner.top, right: outer.right, bottom: inner.bottom },
                ] {
                    if strip.right > strip.left && strip.bottom > strip.top {
                        fill(hdc, &strip, color);
                    }
                }
            }
            inner = outer;
        }
        unsafe { ExcludeClipRect(hdc, inner.left, inner.top, inner.right, inner.bottom) };
    }

    /// Search matches and the selection on `page`, drawn with its top-left at view (sx, sy).
    unsafe fn paint_highlights(&mut self, hdc: HDC, page: usize, sx: i32, sy: i32) {
        let mut groups: Vec<(u32, Vec<Rect>)> = Vec::new();
        if let Some(current) = self.search.as_ref().map(|s| s.current) {
            let (mut others, mut cur) = (Vec::new(), Vec::new());
            for (m, rects) in self.page_hits(page).iter().enumerate() {
                if current == Some((page, m)) { &mut cur } else { &mut others }.extend_from_slice(rects);
            }
            groups.push((HIT_COLOR, others));
            groups.push((CURRENT_HIT_COLOR, cur));
        }
        if let Some((_, rects)) = self.selection.as_ref().and_then(|s| s.rects.get(&page)) {
            groups.push((SELECTION_COLOR, rects.clone()));
        }
        let s = self.px_per_pt();
        let pad = self.s(1);
        unsafe {
            for (color, rects) in groups.into_iter().filter(|(_, r)| !r.is_empty()) {
                let brush = CreateSolidBrush(color);
                let old = SelectObject(hdc, brush);
                for r in rects {
                    let (l, t) = (sx + (r[0] * s) as i32 - pad, sy + (r[1] * s) as i32 - pad);
                    let (rr, b) = (sx + ((r[0] + r[2]) * s).ceil() as i32 + pad, sy + ((r[1] + r[3]) * s).ceil() as i32 + pad);
                    PatBlt(hdc, l, t, rr - l, b - t, PATAND);
                }
                SelectObject(hdc, old);
                DeleteObject(brush);
            }
        }
    }

    fn tool_enabled(&self, tool: Tool) -> bool {
        match tool {
            Tool::Open | Tool::More | Tool::FullScreen => true,
            Tool::Sidebar => !self.outline.is_empty(),
            _ => self.doc.is_some(),
        }
    }

    fn tool_active(&self, tool: Tool) -> bool {
        match tool {
            Tool::Sidebar => self.sidebar_visible(),
            Tool::FitWidth => self.doc.is_some() && self.fit == Fit::Width,
            Tool::FitPage => self.doc.is_some() && self.fit == Fit::Page,
            _ => false,
        }
    }

    unsafe fn paint_toolbar(&self, dc: HDC) {
        let t = self.theme;
        let radius = self.s(5);
        unsafe {
            fill(dc, &rect(0, 0, self.window_w, self.view_y), t.chrome_bg);
            fill(dc, &rect(0, self.view_y - 1, self.window_w, 1), t.border);
            for &x in &self.separators {
                let h = self.s(20);
                fill(dc, &rect(x, (self.view_y - h) / 2, 1, h), t.border);
            }
            for &(tool, r) in &self.tools {
                let enabled = self.tool_enabled(tool);
                let hovered = enabled && self.hover_tool == Some(tool);
                let pressed = hovered && self.drag == Drag::Tool(tool);
                let active = self.tool_active(tool);
                if pressed {
                    round_rect(dc, &r, radius, t.pressed, None);
                } else if hovered || active {
                    round_rect(dc, &r, radius, t.hover, None);
                }
                let color = if !enabled { t.text_disabled } else if active { t.accent } else { t.text };
                if tool == Tool::ZoomMenu {
                    let label = if self.doc.is_some() { format!("{}%", (self.zoom * 100.0).round()) } else { "-".to_string() };
                    text(dc, self.fonts.ui, &label, &r, color, DT_CENTER | DT_VCENTER);
                } else {
                    text(dc, self.fonts.icon, &tool.glyph().to_string(), &r, color, DT_CENTER | DT_VCENTER);
                }
            }

            // Page number box and "of N".
            let enabled = self.doc.is_some();
            round_rect(dc, &self.page_box, radius, t.field_bg, Some(t.field_border));
            let label = if enabled { format!("of {}", self.page_sizes.len()) } else { String::new() };
            text(dc, self.fonts.ui, &label, &self.page_label, t.text_dim, DT_LEFT | DT_VCENTER);

            // Search field: icon, the edit box (a child window), and the status on the right.
            if self.find_visible {
                let f = self.search_field;
                round_rect(dc, &f, radius, t.field_bg, Some(t.accent));
                let icon = rect(f.left, f.top, self.s(32), f.bottom - f.top);
                text(dc, self.fonts.icon, &glyph::SEARCH.to_string(), &icon, t.text_dim, DT_CENTER | DT_VCENTER);
                let mut status = f;
                status.right -= self.s(10);
                status.left = status.right - self.s(86);
                text(dc, self.fonts.ui, &self.search_status, &status, t.text_dim, DT_RIGHT | DT_VCENTER);
            }
        }
    }

    /// The area of the view the welcome screen's content occupies, in view coordinates.
    fn welcome_rect(&self) -> RECT {
        let (cx, cy) = (self.client_w / 2, self.client_h / 2);
        rect(cx - self.s(270), cy - self.s(180), self.s(540), self.s(300))
    }

    /// The welcome screen: logo, wordmark, tagline and an "Open a PDF" button.
    unsafe fn paint_empty_state(&mut self, hdc: HDC) {
        let t = self.theme;
        let (cx, cy) = (self.client_w / 2, self.client_h / 2);
        let wide_rect = |y: i32, h: i32| rect(0, y, self.client_w, h);
        let center = DT_CENTER | DT_VCENTER;
        unsafe {
            let logo = self.s(LOGO_SIZE);
            DrawIconEx(hdc, cx - logo / 2, cy - self.s(170), self.logo, logo, logo, 0, null_mut(), DI_NORMAL);
            text(hdc, self.fonts.brand, APP_NAME, &wide_rect(cy - self.s(62), self.s(44)), t.text, center);
            text(hdc, self.fonts.ui, ui::TAGLINE, &wide_rect(cy - self.s(16), self.s(24)), t.text_dim, center);
            let (bw, bh) = (self.s(156), self.s(40));
            self.empty_button = rect(cx - bw / 2, cy + self.s(28), bw, bh);
            let color = if self.hover_empty_button { ui::blend(t.accent, t.view_bg, 0.15) } else { t.accent };
            round_rect(hdc, &self.empty_button, self.s(6), color, None);
            text(hdc, self.fonts.ui_bold, "Open a PDF", &self.empty_button, t.accent_text, center);
            let hint = "or drop a file anywhere  \u{00B7}  Ctrl+O";
            text(hdc, self.fonts.small, hint, &wide_rect(cy + self.s(80), self.s(20)), t.text_dim, center);
        }
    }

    unsafe fn paint_scrollbars(&self, hdc: HDC) {
        let t = self.theme;
        let hot = self.scroll_hot || matches!(self.drag, Drag::VThumb(_) | Drag::HThumb(_));
        let (thick, m) = (if hot { self.s(8) } else { self.s(4) }, self.s(3));
        let color = if hot { t.thumb_hot } else { t.thumb };
        unsafe {
            if let Some((_, thumb)) = self.v_bar() {
                let r = rect(self.client_w - m - thick, thumb.y, thick, thumb.h);
                round_rect(hdc, &r, thick / 2, color, None);
            }
            if let Some((_, thumb)) = self.h_bar() {
                let r = rect(thumb.x, self.client_h - m - thick, thumb.w, thick);
                round_rect(hdc, &r, thick / 2, color, None);
            }
        }
    }

    unsafe fn paint_toast(&self, hdc: HDC) {
        let Some(toast) = &self.toast else { return };
        let t = self.theme;
        unsafe {
            let w = ui::text_width(hdc, self.fonts.ui, toast) + self.s(32);
            let h = self.s(38);
            let r = rect((self.client_w - w) / 2, self.client_h - h - self.s(32), w, h);
            round_rect(hdc, &r, self.s(8), t.toast_bg, Some(ui::blend(t.toast_bg, t.toast_text, 0.12)));
            text(hdc, self.fonts.ui, toast, &r, t.toast_text, DT_CENTER | DT_VCENTER);
        }
    }

    /// Where the tooltip for `tool` goes, in client coordinates: centered under its button.
    fn tooltip_rect(&self, tool: Tool) -> Option<RECT> {
        let &(_, button) = self.tools.iter().find(|(t, _)| *t == tool)?;
        let w = unsafe {
            let dc = GetDC(self.hwnd);
            let w = ui::text_width(dc, self.fonts.ui, tool.tip());
            ReleaseDC(self.hwnd, dc);
            w
        } + self.s(20);
        let x = ((button.left + button.right - w) / 2).clamp(self.s(4), self.window_w - w - self.s(4));
        Some(rect(x, button.bottom + self.s(6), w, self.s(30)))
    }

    unsafe fn paint_tooltip(&self, hdc: HDC) {
        let Some(tool) = self.tooltip else { return };
        let Some(r) = self.tooltip_rect(tool) else { return };
        let t = self.theme;
        unsafe {
            round_rect(hdc, &r, self.s(5), t.toast_bg, Some(ui::blend(t.toast_bg, t.toast_text, 0.12)));
            text(hdc, self.fonts.ui, tool.tip(), &r, t.toast_text, DT_CENTER | DT_VCENTER);
        }
    }

    // ---- Partial repaints: hover effects only redraw what changed ----

    fn invalidate(&self, r: &RECT) {
        unsafe { InvalidateRect(self.hwnd, r, 0) };
    }

    /// Invalidates a rectangle given in view coordinates.
    fn invalidate_view(&self, r: &RECT) {
        let mut r = *r;
        unsafe { OffsetRect(&mut r, self.view_x, self.view_y) };
        self.invalidate(&r);
    }

    fn invalidate_toolbar(&self) {
        self.invalidate(&rect(0, 0, self.window_w, self.view_y));
    }

    fn hide_tooltip(&mut self) {
        if let Some(r) = self.tooltip.take().and_then(|t| self.tooltip_rect(t)) {
            self.invalidate(&r);
        }
    }

    // ---- Input ----

    fn cursor_in_view(&self) -> (i32, i32) {
        let mut pt = POINT { x: 0, y: 0 };
        unsafe {
            GetCursorPos(&mut pt);
            ScreenToClient(self.hwnd, &mut pt);
        }
        (pt.x - self.view_x, pt.y - self.view_y)
    }

    fn tool_at(&self, x: i32, y: i32) -> Option<Tool> {
        self.tools.iter().find(|(_, r)| contains(r, x, y)).map(|(t, _)| *t)
    }

    fn set_hover(&mut self, tool: Option<Tool>, empty_button: bool, scroll_hot: bool) {
        if tool != self.hover_tool {
            self.hover_tool = tool;
            self.hide_tooltip();
            unsafe {
                KillTimer(self.hwnd, TOOLTIP_TIMER);
                if tool.is_some() {
                    SetTimer(self.hwnd, TOOLTIP_TIMER, TOOLTIP_MS, None);
                }
            }
            self.invalidate_toolbar();
        }
        if empty_button != self.hover_empty_button {
            self.hover_empty_button = empty_button;
            self.invalidate_view(&self.empty_button);
        }
        if scroll_hot != self.scroll_hot {
            self.scroll_hot = scroll_hot;
            // The scrollbars sit along the right and bottom edges of the view.
            let w = self.s(16);
            self.invalidate_view(&rect(self.client_w - w, 0, w, self.client_h));
            self.invalidate_view(&rect(0, self.client_h - w, self.client_w, w));
        }
    }

    fn on_mouse_move(&mut self, x: i32, y: i32) {
        if !self.tracking_leave {
            let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.hwnd, dwHoverTime: 0 };
            unsafe { TrackMouseEvent(&mut tme) };
            self.tracking_leave = true;
        }
        let (vx, vy) = (x - self.view_x, y - self.view_y);
        match self.drag {
            Drag::Tool(_) => {
                let over = self.tool_at(x, y);
                if over != self.hover_tool {
                    self.hover_tool = over;
                    self.invalidate_toolbar();
                }
            }
            Drag::Select => self.extend_selection(vx, vy),
            Drag::VThumb(_) | Drag::HThumb(_) => self.drag_scrollbar(vx, vy),
            Drag::None => {
                let in_view = vx >= 0 && vy >= 0;
                let tool = if y < self.view_y { self.tool_at(x, y) } else { None };
                let empty = self.doc.is_none() && in_view && contains(&self.empty_button, vx, vy);
                let scroll = in_view && self.near_scrollbar(vx, vy);
                self.set_hover(tool, empty, scroll);
            }
        }
    }

    fn on_mouse_down(&mut self, x: i32, y: i32, double: bool) {
        unsafe { SetFocus(self.hwnd) };
        let (vx, vy) = (x - self.view_x, y - self.view_y);
        if y < self.view_y {
            if let Some(tool) = self.tool_at(x, y).filter(|t| self.tool_enabled(*t)) {
                self.drag = Drag::Tool(tool);
                self.hide_tooltip();
                unsafe { SetCapture(self.hwnd) };
                self.invalidate_toolbar();
            }
            return;
        }
        if vx < 0 {
            return;
        }
        if self.doc.is_none() {
            if contains(&self.empty_button, vx, vy) {
                self.deferred = Some(Deferred::Command(Command::Open));
            }
            return;
        }
        if !self.grab_scrollbar(vx, vy) {
            self.start_selection(vx, vy, double);
        }
    }

    fn on_mouse_up(&mut self, x: i32, y: i32) {
        let drag = std::mem::replace(&mut self.drag, Drag::None);
        if drag == Drag::None {
            return;
        }
        unsafe { ReleaseCapture() };
        match drag {
            Drag::Tool(tool) => {
                // Whatever the tool changes repaints itself; the button only needs its pressed look cleared.
                self.invalidate_toolbar();
                if self.tool_at(x, y) == Some(tool) {
                    self.activate_tool(tool);
                }
            }
            Drag::Select => self.finish_selection(),
            _ => unsafe {
                InvalidateRect(self.hwnd, null(), 0);
            },
        }
    }

    /// Capture was taken away (e.g. Alt+Tab mid-drag).
    fn cancel_drag(&mut self) {
        if std::mem::replace(&mut self.drag, Drag::None) == Drag::Select {
            self.finish_selection();
        }
        unsafe { InvalidateRect(self.hwnd, null(), 0) };
    }

    fn activate_tool(&mut self, tool: Tool) {
        let (cx, cy) = (self.client_w / 2, self.client_h / 2);
        let button = self.tools.iter().find(|(t, _)| *t == tool).map(|(_, r)| *r).unwrap_or(rect(0, 0, 0, 0));
        let screen = |x: i32, y: i32| {
            let mut pt = POINT { x, y };
            unsafe { ClientToScreen(self.hwnd, &mut pt) };
            (pt.x, pt.y)
        };
        match tool {
            Tool::Sidebar => self.toggle_outline(),
            Tool::Open => self.deferred = Some(Deferred::Command(Command::Open)),
            Tool::PrevPage => self.prev_page(),
            Tool::NextPage => self.next_page(),
            Tool::ZoomOut => self.set_zoom(self.zoom / ZOOM_STEP, cx, cy),
            Tool::ZoomIn => self.set_zoom(self.zoom * ZOOM_STEP, cx, cy),
            Tool::ZoomMenu => {
                let (x, y) = screen(button.left, button.bottom + self.s(4));
                self.deferred = Some(Deferred::ZoomMenu(x, y));
            }
            Tool::FitWidth => self.set_fit(Fit::Width),
            Tool::FitPage => self.set_fit(Fit::Page),
            Tool::Search => self.open_find(),
            Tool::FindPrev => self.find(false),
            Tool::FindNext => self.find(true),
            Tool::FindClose => {
                self.close_find();
            }
            Tool::FullScreen => self.deferred = Some(Deferred::Command(Command::FullScreen)),
            Tool::More => {
                let (x, y) = screen(button.right, button.bottom + self.s(4));
                self.deferred = Some(Deferred::MoreMenu(x, y));
            }
        }
    }

    /// Navigation keys; commands with shortcuts go through `run_command`.
    fn on_key(&mut self, vk: u16) -> bool {
        if self.page_tops.is_empty() {
            return false;
        }
        let shift = key_down(VK_SHIFT);
        let (x, y) = (self.scroll_x, self.scroll_y);
        // In fit-page mode, paging moves exactly one page.
        if self.fit == Fit::Page {
            match vk {
                VK_NEXT | VK_SPACE if !(vk == VK_SPACE && shift) => {
                    self.next_page();
                    return true;
                }
                VK_PRIOR | VK_SPACE => {
                    self.prev_page();
                    return true;
                }
                _ => {}
            }
        }
        let page = (self.client_h - self.line()).max(self.line());
        match vk {
            VK_UP => self.scroll_to(x, y - self.line()),
            VK_DOWN => self.scroll_to(x, y + self.line()),
            VK_LEFT => self.scroll_to(x - self.line(), y),
            VK_RIGHT => self.scroll_to(x + self.line(), y),
            VK_PRIOR => self.scroll_to(x, y - page),
            VK_NEXT => self.scroll_to(x, y + page),
            VK_SPACE if shift => self.scroll_to(x, y - page),
            VK_SPACE => self.scroll_to(x, y + page),
            VK_HOME => self.scroll_to(x, 0),
            VK_END => self.scroll_to(x, i32::MAX),
            _ => return false,
        }
        true
    }

    fn on_wheel(&mut self, delta: i32, flags: u32, screen_x: i32, screen_y: i32, horizontal: bool) {
        let step = 3 * self.line();
        if flags & MK_CONTROL != 0 && !horizontal {
            let mut pt = POINT { x: screen_x, y: screen_y };
            unsafe { ScreenToClient(self.hwnd, &mut pt) };
            self.set_zoom(self.zoom * ZOOM_STEP.powf(delta as f32 / 120.0), pt.x - self.view_x, pt.y - self.view_y);
        } else if horizontal {
            self.scroll_to(self.scroll_x + delta * step / 120, self.scroll_y);
        } else if flags & MK_SHIFT != 0 {
            self.scroll_to(self.scroll_x - delta * step / 120, self.scroll_y);
        } else {
            self.scroll_to(self.scroll_x, self.scroll_y - delta * step / 120);
        }
    }

    fn command(&mut self, cmd: Command) {
        let (cx, cy) = (self.client_w / 2, self.client_h / 2);
        match cmd {
            Command::Copy => self.copy_selection(),
            Command::SelectAll => self.select_page_text(),
            Command::Find => self.open_find(),
            Command::FindNext => self.find(true),
            Command::FindPrev => self.find(false),
            Command::ZoomIn => self.set_zoom(self.zoom * ZOOM_STEP, cx, cy),
            Command::ZoomOut => self.set_zoom(self.zoom / ZOOM_STEP, cx, cy),
            Command::ActualSize => self.set_zoom(1.0, cx, cy),
            Command::FitWidth => self.set_fit(Fit::Width),
            Command::FitPage => self.set_fit(Fit::Page),
            Command::ToggleOutline => self.toggle_outline(),
            Command::ToggleTheme => {
                self.theme_override = Some(!self.theme.dark);
                self.apply_theme();
            }
            // Need to run outside the app borrow; see run_command.
            Command::Open | Command::FullScreen | Command::About | Command::Escape => {}
        }
    }

    /// Handles a message, or returns None to fall through to DefWindowProc.
    fn handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        let (mx, my) = (loword(lparam as usize) as i16 as i32, hiword(lparam as usize) as i16 as i32);
        match msg {
            WM_PAINT => unsafe {
                let mut ps: PAINTSTRUCT = zeroed();
                let hdc = BeginPaint(self.hwnd, &mut ps);
                self.paint(hdc);
                EndPaint(self.hwnd, &ps);
            },
            WM_ERASEBKGND => return Some(1),
            WM_SIZE => {
                if wparam as u32 == SIZE_MINIMIZED {
                    // Nothing is visible, so give the page bitmaps back to the system.
                    self.clear_cache();
                } else {
                    let (w, h) = (mx & 0xFFFF, my & 0xFFFF);
                    self.with_anchor(0, 0, |a| {
                        a.window_w = w;
                        a.window_h = h;
                    });
                }
            }
            WM_GETMINMAXINFO => unsafe {
                let info = &mut *(lparam as *mut MINMAXINFO);
                info.ptMinTrackSize = POINT { x: self.s(820), y: self.s(420) };
            },
            WM_SETTINGCHANGE => {
                // Windows switched between light and dark mode.
                let area = lparam as *const u16;
                if self.theme_override.is_none() && !area.is_null() && unsafe { wide_eq(area, "ImmersiveColorSet") } {
                    self.apply_theme();
                }
                return None;
            }
            WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC => unsafe {
                let dc = wparam as HDC;
                SetTextColor(dc, if msg == WM_CTLCOLORSTATIC { self.theme.text_disabled } else { self.theme.text });
                SetBkColor(dc, self.theme.field_bg);
                return Some(self.field_brush as LRESULT);
            },
            WM_TIMER => match wparam {
                TOAST_TIMER => unsafe {
                    KillTimer(self.hwnd, TOAST_TIMER);
                    self.toast = None;
                    InvalidateRect(self.hwnd, null(), 0);
                },
                DRAG_TIMER => self.on_drag_timer(),
                TOOLTIP_TIMER => {
                    unsafe { KillTimer(self.hwnd, TOOLTIP_TIMER) };
                    self.tooltip = self.hover_tool.filter(|_| self.drag == Drag::None);
                    if let Some(r) = self.tooltip.and_then(|t| self.tooltip_rect(t)) {
                        self.invalidate(&r);
                    }
                }
                _ => return None,
            },
            WM_COMMAND if lparam as HWND == self.find_edit && hiword(wparam) as u32 == EN_CHANGE => {
                // The query changed: drop old highlights until the next search.
                self.search = None;
                self.search_status.clear();
                unsafe { InvalidateRect(self.hwnd, null(), 0) };
            }
            WM_COMMAND if lparam as HWND == self.page_edit => match hiword(wparam) as u32 {
                EN_SETFOCUS => unsafe {
                    PostMessageW(self.page_edit, EM_SETSEL, 0, -1);
                },
                EN_KILLFOCUS => self.sync_page_box(),
                _ => {}
            },
            WM_APP_PAGE_KEY => self.on_page_key(wparam as u16),
            WM_NOTIFY if unsafe { (*(lparam as *const NMHDR)).hwndFrom } == self.tree => self.on_tree_notify(lparam),
            WM_APP_SEARCH => self.continue_search(),
            WM_LBUTTONDOWN => self.on_mouse_down(mx, my, false),
            WM_LBUTTONDBLCLK => self.on_mouse_down(mx, my, true),
            WM_MOUSEMOVE => self.on_mouse_move(mx, my),
            WM_LBUTTONUP => self.on_mouse_up(mx, my),
            WM_CAPTURECHANGED => self.cancel_drag(),
            WM_MOUSELEAVE => {
                self.tracking_leave = false;
                if self.drag == Drag::None {
                    self.set_hover(None, false, false);
                }
            }
            WM_SETCURSOR if wparam as HWND == self.hwnd && loword(lparam as usize) as u32 == HTCLIENT => unsafe {
                let (x, y) = self.cursor_in_view();
                let cursor = if self.doc.is_none() && contains(&self.empty_button, x, y) {
                    IDC_HAND
                } else if self.over_page(x, y) && !self.near_scrollbar(x, y) {
                    IDC_IBEAM
                } else {
                    IDC_ARROW
                };
                SetCursor(LoadCursorW(null_mut(), cursor));
                return Some(1);
            },
            WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
                let delta = hiword(wparam) as i16 as i32;
                self.on_wheel(delta, loword(wparam) as u32, mx, my, msg == WM_MOUSEHWHEEL);
            }
            WM_KEYDOWN => {
                if !self.on_key(wparam as u16) {
                    return None;
                }
            }
            _ => return None,
        }
        Some(0)
    }
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` on the app state, or returns None if it is missing or already borrowed
/// (re-entrant messages sent while a handler runs go to DefWindowProc).
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| cell.try_borrow_mut().ok().and_then(|mut app| app.as_mut().map(f)))
}

fn loword(v: usize) -> u16 {
    (v & 0xFFFF) as u16
}

fn hiword(v: usize) -> u16 {
    ((v >> 16) & 0xFFFF) as u16
}

/// Compares a NUL-terminated UTF-16 string with `s`.
unsafe fn wide_eq(p: *const u16, s: &str) -> bool {
    let mut i = 0;
    for c in s.encode_utf16() {
        if unsafe { *p.add(i) } != c {
            return false;
        }
        i += 1;
    }
    unsafe { *p.add(i) == 0 }
}

fn private_bytes() -> usize {
    unsafe {
        let mut counters: PROCESS_MEMORY_COUNTERS_EX = zeroed();
        counters.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters as *mut _ as *mut PROCESS_MEMORY_COUNTERS, counters.cb);
        counters.PrivateUsage
    }
}

fn set_clipboard(hwnd: HWND, text: &str) -> bool {
    let data = wide(text);
    unsafe {
        if OpenClipboard(hwnd) == 0 {
            return false;
        }
        EmptyClipboard();
        let mem = GlobalAlloc(GMEM_MOVEABLE, data.len() * 2);
        let ok = !mem.is_null() && {
            let dst = GlobalLock(mem) as *mut u16;
            if !dst.is_null() {
                std::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
                GlobalUnlock(mem);
            }
            // On success the clipboard owns the memory.
            !dst.is_null() && !SetClipboardData(CF_UNICODETEXT, mem as HANDLE).is_null()
        };
        CloseClipboard();
        ok
    }
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetKeyState(vk as i32) < 0 }
}

fn command_for_key(vk: u16) -> Option<Command> {
    let ctrl = key_down(VK_CONTROL);
    let shift = key_down(VK_SHIFT);
    Some(match vk {
        VK_O if ctrl => Command::Open,
        VK_C if ctrl => Command::Copy,
        VK_A if ctrl => Command::SelectAll,
        VK_B if ctrl => Command::ToggleOutline,
        VK_F if ctrl => Command::Find,
        VK_F3 | VK_RETURN if shift => Command::FindPrev,
        VK_F3 | VK_RETURN => Command::FindNext,
        VK_OEM_PLUS | VK_ADD if ctrl => Command::ZoomIn,
        VK_OEM_MINUS | VK_SUBTRACT if ctrl => Command::ZoomOut,
        VK_1 | VK_NUMPAD1 if ctrl => Command::ActualSize,
        VK_0 | VK_NUMPAD0 if ctrl => Command::FitWidth,
        VK_2 | VK_NUMPAD2 if ctrl => Command::FitPage,
        VK_F11 => Command::FullScreen,
        VK_ESCAPE => Command::Escape,
        _ => return None,
    })
}

/// Runs a command. Ones that open modal windows or resize the window run outside the
/// app borrow, because they send messages back to wndproc synchronously.
fn run_command(hwnd: HWND, cmd: Command) {
    match cmd {
        Command::Open => {
            if let Some(path) = pick_file(hwnd) {
                open_path(hwnd, &path);
            }
        }
        Command::FullScreen => toggle_fullscreen(hwnd),
        Command::About => show_about(hwnd),
        Command::Find => {
            // The search field lives in the toolbar, which full screen hides.
            if with_app(|a| a.fullscreen.is_some()) == Some(true) {
                toggle_fullscreen(hwnd);
            }
            with_app(|a| a.open_find());
        }
        Command::Escape => {
            if with_app(|a| !a.escape() && a.fullscreen.is_some()) == Some(true) {
                toggle_fullscreen(hwnd);
            }
        }
        _ => {
            with_app(|a| a.command(cmd));
        }
    }
}

fn run_deferred(hwnd: HWND, deferred: Deferred) {
    match deferred {
        Deferred::Command(cmd) => run_command(hwnd, cmd),
        Deferred::ZoomMenu(x, y) => zoom_menu(hwnd, x, y),
        Deferred::MoreMenu(x, y) => {
            if let Some(cmd) = context_menu(hwnd, x, y, true) {
                run_command(hwnd, cmd);
            }
        }
    }
}

fn show_about(hwnd: HWND) {
    let content = format!(
        "Version {}\n\n{}\nA fast PDF reader built to use as little memory as possible.\n\nPDF rendering by PDFium (BSD license).",
        env!("CARGO_PKG_VERSION"),
        ui::TAGLINE
    );
    unsafe {
        TaskDialog(
            hwnd,
            GetModuleHandleW(null()),
            wide(&format!("About {APP_NAME}")).as_ptr(),
            wide(APP_NAME).as_ptr(),
            wide(&content).as_ptr(),
            TDCBF_OK_BUTTON,
            ui::APP_ICON_ID as *const u16,
            null_mut(),
        );
    }
}

fn toggle_fullscreen(hwnd: HWND) {
    unsafe {
        if let Some(Some(placement)) = with_app(|a| a.fullscreen.take()) {
            SetWindowLongPtrW(hwnd, GWL_STYLE, (NORMAL_STYLE | WS_VISIBLE) as isize);
            SetWindowPlacement(hwnd, &placement);
            SetWindowPos(hwnd, null_mut(), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_FRAMECHANGED);
            with_app(|a| a.with_anchor(0, 0, |_| {}));
        } else {
            let mut placement: WINDOWPLACEMENT = zeroed();
            placement.length = size_of::<WINDOWPLACEMENT>() as u32;
            GetWindowPlacement(hwnd, &mut placement);
            let mut mi: MONITORINFO = zeroed();
            mi.cbSize = size_of::<MONITORINFO>() as u32;
            GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut mi);
            with_app(|a| a.fullscreen = Some(placement));
            let r = mi.rcMonitor;
            SetWindowLongPtrW(hwnd, GWL_STYLE, (WS_POPUP | WS_CLIPCHILDREN | WS_VISIBLE) as isize);
            SetWindowPos(hwnd, HWND_TOP, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOOWNERZORDER | SWP_FRAMECHANGED);
            with_app(|a| a.show_toast("Full screen - press Esc or F11 to exit".to_string()));
        }
    }
}

fn zoom_menu(hwnd: HWND, x: i32, y: i32) {
    let Some((fit, zoom)) = with_app(|a| (a.fit, a.zoom)) else { return };
    unsafe {
        let menu = CreatePopupMenu();
        let check = |on: bool| if on { MF_STRING | MF_CHECKED } else { MF_STRING };
        AppendMenuW(menu, check(fit == Fit::Width), 1, wide("Fit width\tCtrl+0").as_ptr());
        AppendMenuW(menu, check(fit == Fit::Page), 2, wide("Fit page\tCtrl+2").as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        for (i, p) in ZOOM_PRESETS.iter().enumerate() {
            let on = fit == Fit::None && (zoom * 100.0).round() as u32 == *p;
            AppendMenuW(menu, check(on), 10 + i, wide(&format!("{p}%")).as_ptr());
        }
        let id = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN, x, y, 0, hwnd, null());
        DestroyMenu(menu);
        with_app(|a| match id {
            1 => a.set_fit(Fit::Width),
            2 => a.set_fit(Fit::Page),
            n if n >= 10 => {
                if let Some(p) = ZOOM_PRESETS.get(n as usize - 10) {
                    a.set_zoom(*p as f32 / 100.0, a.client_w / 2, a.client_h / 2);
                }
            }
            _ => {}
        });
    }
}

fn context_menu(hwnd: HWND, x: i32, y: i32, right_align: bool) -> Option<Command> {
    let (has_doc, has_sel, fit, sidebar, dark, fullscreen) =
        with_app(|a| (a.doc.is_some(), a.selection.is_some(), a.fit, a.sidebar_visible(), a.theme.dark, a.fullscreen.is_some()))?;
    unsafe {
        let menu = CreatePopupMenu();
        for (i, item) in MENU.iter().enumerate() {
            match item {
                None => AppendMenuW(menu, MF_SEPARATOR, 0, null()),
                Some((cmd, label)) => {
                    let mut flags = MF_STRING;
                    let checked = match cmd {
                        Command::FitWidth => has_doc && fit == Fit::Width,
                        Command::FitPage => has_doc && fit == Fit::Page,
                        Command::ToggleOutline => sidebar,
                        Command::ToggleTheme => dark,
                        Command::FullScreen => fullscreen,
                        _ => false,
                    };
                    if checked {
                        flags |= MF_CHECKED;
                    }
                    let enabled = match cmd {
                        Command::Open | Command::FullScreen | Command::ToggleTheme | Command::About => true,
                        Command::Copy => has_sel,
                        _ => has_doc,
                    };
                    if !enabled {
                        flags |= MF_GRAYED;
                    }
                    AppendMenuW(menu, flags, i + 1, wide(label).as_ptr())
                }
            };
        }
        let align = if right_align { TPM_RIGHTALIGN } else { TPM_LEFTALIGN };
        let id = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | align, x, y, 0, hwnd, null());
        DestroyMenu(menu);
        let id = usize::try_from(id).ok()?.checked_sub(1)?;
        MENU.get(id).copied().flatten().map(|(cmd, _)| cmd)
    }
}

unsafe fn blit(hdc: HDC, x: i32, y: i32, w: i32, h: i32, pixels: &[u32]) {
    unsafe {
        let mut bmi: BITMAPINFO = zeroed();
        bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w;
        bmi.bmiHeader.biHeight = -h; // top-down
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB as _;
        SetDIBitsToDevice(hdc, x, y, w as u32, h as u32, 0, 0, 0, h as u32, pixels.as_ptr() as _, &bmi, DIB_RGB_COLORS);
    }
}

fn open_path(hwnd: HWND, path: &Path) {
    let result = with_app(|app| app.open(path));
    // The message box runs its own message loop, so show it only after the app borrow ends.
    if let Some(Err(err)) = result {
        let text = wide(&format!("{}\n\n{err}", path.display()));
        unsafe { MessageBoxW(hwnd, text.as_ptr(), wide(APP_NAME).as_ptr(), MB_OK | MB_ICONWARNING) };
    }
}

fn pick_file(hwnd: HWND) -> Option<PathBuf> {
    let filter: Vec<u16> = "PDF files\0*.pdf\0All files\0*.*\0\0".encode_utf16().collect();
    let mut buf = vec![0u16; 4096];
    unsafe {
        let mut ofn: OPENFILENAMEW = zeroed();
        ofn.lStructSize = size_of::<OPENFILENAMEW>() as u32;
        ofn.hwndOwner = hwnd;
        ofn.lpstrFilter = filter.as_ptr();
        ofn.lpstrFile = buf.as_mut_ptr();
        ofn.nMaxFile = buf.len() as u32;
        ofn.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST;
        if GetOpenFileNameW(&mut ofn) == 0 {
            return None;
        }
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(PathBuf::from(OsString::from_wide(&buf[..len])))
}

fn dropped_file(hdrop: HDROP) -> Option<PathBuf> {
    unsafe {
        let len = DragQueryFileW(hdrop, 0, null_mut(), 0) as usize;
        let mut buf = vec![0u16; len + 1];
        let got = DragQueryFileW(hdrop, 0, buf.as_mut_ptr(), buf.len() as u32) as usize;
        DragFinish(hdrop);
        (got > 0).then(|| PathBuf::from(OsString::from_wide(&buf[..got])))
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_DESTROY => {
                with_app(|a| a.save_position());
                PostQuitMessage(0);
                0
            }
            WM_ENDSESSION => {
                with_app(|a| a.save_position());
                0
            }
            WM_KEYDOWN if command_for_key(wparam as u16).is_some() => {
                run_command(hwnd, command_for_key(wparam as u16).unwrap());
                0
            }
            // Menu item ids (index into MENU + 1); control notifications have lparam set.
            WM_COMMAND if lparam == 0 => {
                if let Some(Some((cmd, _))) = MENU.get((loword(wparam) as usize).wrapping_sub(1)) {
                    run_command(hwnd, *cmd);
                }
                0
            }
            WM_CONTEXTMENU => {
                let mut pt = POINT { x: loword(lparam as usize) as i16 as i32, y: hiword(lparam as usize) as i16 as i32 };
                if lparam == -1 {
                    GetCursorPos(&mut pt); // opened from the keyboard
                }
                if let Some(cmd) = context_menu(hwnd, pt.x, pt.y, false) {
                    run_command(hwnd, cmd);
                }
                0
            }
            WM_DROPFILES => {
                if let Some(path) = dropped_file(wparam as HDROP) {
                    open_path(hwnd, &path);
                }
                0
            }
            WM_DPICHANGED => {
                let dpi = hiword(wparam) as u32;
                with_app(|app| {
                    app.dpi = dpi;
                    app.apply_fonts();
                    app.with_anchor(0, 0, |_| {});
                });
                let r = &*(lparam as *const RECT);
                SetWindowPos(hwnd, null_mut(), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                0
            }
            _ => {
                let result = with_app(|app| app.handle(msg, wparam, lparam)).flatten();
                if let Some(Some(deferred)) = with_app(|a| a.deferred.take()) {
                    run_deferred(hwnd, deferred);
                }
                result.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wparam, lparam))
            }
        }
    }
}

fn main() {
    pdfium::init();
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        ui::init_common_controls();

        let class = wide("FeatherPdfWindow");
        let hinstance = GetModuleHandleW(null());
        let mut wc: WNDCLASSW = zeroed();
        wc.style = CS_DBLCLKS;
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinstance;
        wc.hCursor = LoadCursorW(null_mut(), IDC_ARROW);
        wc.hIcon = ui::app_icon(32);
        wc.lpszClassName = class.as_ptr();
        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide(APP_NAME).as_ptr(),
            NORMAL_STYLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            null_mut(),
            null_mut(),
            hinstance,
            null(),
        );
        if hwnd.is_null() {
            return;
        }
        DragAcceptFiles(hwnd, 1);

        let child = |class: &str, style: u32| {
            CreateWindowExW(0, wide(class).as_ptr(), null(), WS_CHILD | style, 0, 0, 0, 0, hwnd, null_mut(), hinstance, null())
        };
        let find_edit = child("EDIT", ES_AUTOHSCROLL as u32);
        let page_edit = child("EDIT", (ES_CENTER | ES_NUMBER | ES_AUTOHSCROLL) as u32);
        let tree = child(
            "SysTreeView32",
            (TVS_HASBUTTONS | TVS_LINESATROOT | TVS_SHOWSELALWAYS | TVS_FULLROWSELECT | TVS_TRACKSELECT | TVS_NOHSCROLL) as u32,
        );

        let mut app = App::new(hwnd, find_edit, page_edit, tree, GetDpiForWindow(hwnd));
        let mut rc: RECT = zeroed();
        GetClientRect(hwnd, &mut rc);
        app.window_w = rc.right;
        app.window_h = rc.bottom;
        app.layout();
        app.refresh();
        APP.with(|cell| *cell.borrow_mut() = Some(app));

        if let Some(arg) = std::env::args_os().nth(1) {
            open_path(hwnd, Path::new(&arg));
        }

        ShowWindow(hwnd, SW_SHOWDEFAULT);

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            if msg.message == WM_KEYDOWN && [find_edit, page_edit, tree].contains(&msg.hwnd) {
                let vk = msg.wParam as u16;
                // Enter/Esc in the page box confirm or cancel the page number.
                if msg.hwnd == page_edit && (vk == VK_RETURN || vk == VK_ESCAPE) {
                    SendMessageW(hwnd, WM_APP_PAGE_KEY, msg.wParam, 0);
                    continue;
                }
                // Other shortcuts go to the main window, except the ones the controls use themselves.
                if let Some(cmd) = command_for_key(vk) {
                    let edit_owns = msg.hwnd != tree && matches!(cmd, Command::Copy | Command::SelectAll);
                    let tree_owns = msg.hwnd == tree && vk == VK_RETURN;
                    if !edit_owns && !tree_owns {
                        SendMessageW(hwnd, WM_KEYDOWN, msg.wParam, msg.lParam);
                        continue;
                    }
                }
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
