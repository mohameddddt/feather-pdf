//! Look and feel: theme colors, fonts, icon glyphs, small GDI drawing helpers, and the
//! Windows calls that make native parts (title bar, menus, controls) match the theme.

use std::mem::{size_of, transmute};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows_sys::Win32::UI::Controls::{ICC_TREEVIEW_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, SetWindowTheme};
use windows_sys::Win32::UI::WindowsAndMessaging::{HICON, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW};

pub const APP_NAME: &str = "Feather PDF";
pub const TAGLINE: &str = "Light on memory. Easy on the eyes.";
/// Resource id of the app icon that build.rs embeds.
pub const APP_ICON_ID: usize = 1;

/// The app icon at `size` pixels. The caller owns it (DestroyIcon).
pub fn app_icon(size: i32) -> HICON {
    unsafe { LoadImageW(GetModuleHandleW(null()), APP_ICON_ID as *const u16, IMAGE_ICON, size, size, LR_DEFAULTCOLOR) as HICON }
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// A COLORREF (0x00BBGGRR).
pub const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

/// Mixes `t` of color `b` into color `a`.
pub fn blend(a: u32, b: u32, t: f32) -> u32 {
    let mix = |shift: u32| {
        let (x, y) = (((a >> shift) & 0xFF) as f32, ((b >> shift) & 0xFF) as f32);
        ((x + (y - x) * t).round() as u32) << shift
    };
    mix(0) | mix(8) | mix(16)
}

#[derive(Clone, Copy)]
pub struct Theme {
    pub dark: bool,
    /// Behind the pages.
    pub view_bg: u32,
    /// Toolbar and sidebar.
    pub chrome_bg: u32,
    pub border: u32,
    pub text: u32,
    pub text_dim: u32,
    pub text_disabled: u32,
    pub hover: u32,
    pub pressed: u32,
    pub accent: u32,
    pub accent_text: u32,
    pub field_bg: u32,
    pub field_border: u32,
    pub toast_bg: u32,
    pub toast_text: u32,
    pub thumb: u32,
    pub thumb_hot: u32,
    /// Opacity of the page shadow, from the page edge outward.
    pub shadow: [f32; 4],
}

/// Brand colors, matching the logo's teal-to-blue gradient (build/logo.rs).
pub mod brand {
    use super::rgb;
    /// Deep teal: accent on light backgrounds (5:1 contrast with white text).
    pub const TEAL_DEEP: u32 = rgb(0x0F, 0x76, 0x6E);
    /// Bright teal: accent on dark backgrounds.
    pub const TEAL_BRIGHT: u32 = rgb(0x2D, 0xD4, 0xBF);
    /// Mint: selected text on the page.
    pub const MINT: u32 = rgb(0x99, 0xF6, 0xE4);
}

impl Theme {
    /// Neutrals carry a slight teal tint so the chrome feels part of the brand.
    pub fn light() -> Theme {
        Theme {
            dark: false,
            view_bg: rgb(0xE2, 0xE8, 0xE7),
            chrome_bg: rgb(0xF7, 0xFA, 0xF9),
            border: rgb(0xDB, 0xE3, 0xE1),
            text: rgb(0x16, 0x1D, 0x1C),
            text_dim: rgb(0x5A, 0x66, 0x64),
            text_disabled: rgb(0xB0, 0xBA, 0xB8),
            hover: rgb(0xE7, 0xEF, 0xED),
            pressed: rgb(0xD9, 0xE5, 0xE2),
            accent: brand::TEAL_DEEP,
            accent_text: rgb(0xFF, 0xFF, 0xFF),
            field_bg: rgb(0xFF, 0xFF, 0xFF),
            field_border: rgb(0xCF, 0xD9, 0xD7),
            toast_bg: rgb(0x1C, 0x2A, 0x28),
            toast_text: rgb(0xFF, 0xFF, 0xFF),
            thumb: rgb(0x98, 0xA4, 0xA2),
            thumb_hot: rgb(0x6A, 0x78, 0x76),
            shadow: [0.20, 0.11, 0.06, 0.03],
        }
    }

    pub fn dark() -> Theme {
        Theme {
            dark: true,
            view_bg: rgb(0x14, 0x19, 0x18),
            chrome_bg: rgb(0x1D, 0x24, 0x23),
            border: rgb(0x2E, 0x38, 0x36),
            text: rgb(0xEE, 0xF4, 0xF3),
            text_dim: rgb(0xA0, 0xAE, 0xAC),
            text_disabled: rgb(0x55, 0x61, 0x5F),
            hover: rgb(0x2A, 0x34, 0x32),
            pressed: rgb(0x34, 0x40, 0x3E),
            accent: brand::TEAL_BRIGHT,
            accent_text: rgb(0x06, 0x24, 0x20),
            field_bg: rgb(0x14, 0x1A, 0x19),
            field_border: rgb(0x3A, 0x46, 0x44),
            toast_bg: rgb(0x2A, 0x36, 0x34),
            toast_text: rgb(0xFF, 0xFF, 0xFF),
            thumb: rgb(0x5E, 0x6C, 0x6A),
            thumb_hot: rgb(0x96, 0xA6, 0xA4),
            shadow: [0.55, 0.32, 0.16, 0.07],
        }
    }
}

/// Icons from Segoe Fluent Icons (Windows 11) / Segoe MDL2 Assets (Windows 10).
pub mod glyph {
    pub const MENU: char = '\u{E700}';
    pub const FOLDER: char = '\u{E838}';
    pub const UP: char = '\u{E70E}';
    pub const DOWN: char = '\u{E70D}';
    pub const ZOOM_OUT: char = '\u{E71F}';
    pub const ZOOM_IN: char = '\u{E8A3}';
    pub const FIT_WIDTH: char = '\u{E799}';
    pub const FIT_PAGE: char = '\u{E9A6}';
    pub const SEARCH: char = '\u{E721}';
    pub const MORE: char = '\u{E712}';
    pub const CLOSE: char = '\u{E711}';
    pub const FULL_SCREEN: char = '\u{E740}';
}

pub struct Fonts {
    pub ui: HFONT,
    pub ui_bold: HFONT,
    pub small: HFONT,
    /// The "Feather PDF" wordmark on the welcome screen.
    pub brand: HFONT,
    pub icon: HFONT,
}

impl Fonts {
    pub fn new(dpi: u32) -> Fonts {
        let px = |n: i32| n * dpi as i32 / 96;
        let icon_face = if font_exists("Segoe Fluent Icons") { "Segoe Fluent Icons" } else { "Segoe MDL2 Assets" };
        // Segoe UI Variable (Windows 11) has a display cut that suits the wordmark.
        let brand_face = if font_exists("Segoe UI Variable Display") { "Segoe UI Variable Display" } else { "Segoe UI" };
        Fonts {
            ui: font("Segoe UI", px(14), 400),
            ui_bold: font("Segoe UI", px(14), 600),
            small: font("Segoe UI", px(12), 400),
            brand: font(brand_face, px(30), 600),
            icon: font(icon_face, px(16), 400),
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        for f in [self.ui, self.ui_bold, self.small, self.brand, self.icon] {
            unsafe { DeleteObject(f) };
        }
    }
}

fn font(face: &str, px: i32, weight: i32) -> HFONT {
    let face = wide(face);
    unsafe {
        CreateFontW(
            -px,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET as _,
            OUT_DEFAULT_PRECIS as _,
            CLIP_DEFAULT_PRECIS as _,
            CLEARTYPE_QUALITY as _,
            (DEFAULT_PITCH | FF_DONTCARE) as _,
            face.as_ptr(),
        )
    }
}

/// GDI silently substitutes missing fonts, so check what it actually picked.
fn font_exists(face: &str) -> bool {
    unsafe {
        let probe = font(face, 16, 400);
        let dc = CreateCompatibleDC(null_mut());
        let old = SelectObject(dc, probe);
        let mut buf = [0u16; 64];
        let len = GetTextFaceW(dc, buf.len() as i32, buf.as_mut_ptr());
        SelectObject(dc, old);
        DeleteDC(dc);
        DeleteObject(probe);
        String::from_utf16_lossy(&buf[..(len.max(1) - 1) as usize]).eq_ignore_ascii_case(face)
    }
}

// ---- Drawing helpers ----

pub fn rect(x: i32, y: i32, w: i32, h: i32) -> RECT {
    RECT { left: x, top: y, right: x + w, bottom: y + h }
}

pub fn contains(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

pub unsafe fn fill(dc: HDC, r: &RECT, color: u32) {
    unsafe {
        let brush = CreateSolidBrush(color);
        FillRect(dc, r, brush);
        DeleteObject(brush);
    }
}

pub unsafe fn round_rect(dc: HDC, r: &RECT, radius: i32, color: u32, border: Option<u32>) {
    unsafe {
        let brush = CreateSolidBrush(color);
        let pen = CreatePen(PS_SOLID, 1, border.unwrap_or(color));
        let (old_brush, old_pen) = (SelectObject(dc, brush), SelectObject(dc, pen));
        RoundRect(dc, r.left, r.top, r.right, r.bottom, radius * 2, radius * 2);
        SelectObject(dc, old_brush);
        SelectObject(dc, old_pen);
        DeleteObject(brush);
        DeleteObject(pen);
    }
}

pub unsafe fn text(dc: HDC, font: HFONT, s: &str, r: &RECT, color: u32, flags: u32) {
    unsafe {
        let old = SelectObject(dc, font);
        SetBkMode(dc, TRANSPARENT as _);
        SetTextColor(dc, color);
        let w = wide(s);
        let mut r = *r;
        DrawTextW(dc, w.as_ptr(), -1, &mut r, flags | DT_SINGLELINE | DT_NOPREFIX);
        SelectObject(dc, old);
    }
}

pub unsafe fn text_width(dc: HDC, font: HFONT, s: &str) -> i32 {
    unsafe {
        let old = SelectObject(dc, font);
        let w: Vec<u16> = s.encode_utf16().collect();
        let mut size = SIZE { cx: 0, cy: 0 };
        GetTextExtentPoint32W(dc, w.as_ptr(), w.len() as i32, &mut size);
        SelectObject(dc, old);
        size.cx
    }
}

/// Draws into an off-screen bitmap and copies it over in one go, so redrawn chrome doesn't flicker.
pub unsafe fn buffered(dc: HDC, r: &RECT, draw: impl FnOnce(HDC)) {
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    if w <= 0 || h <= 0 {
        return;
    }
    unsafe {
        let mem = CreateCompatibleDC(dc);
        let bmp = CreateCompatibleBitmap(dc, w, h);
        let old = SelectObject(mem, bmp);
        SetViewportOrgEx(mem, -r.left, -r.top, null_mut());
        draw(mem);
        BitBlt(dc, r.left, r.top, w, h, mem, r.left, r.top, SRCCOPY);
        SelectObject(mem, old);
        DeleteObject(bmp);
        DeleteDC(mem);
    }
}

// ---- Platform integration ----

/// True when Windows is set to dark mode for apps.
pub fn system_prefers_dark() -> bool {
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let name = wide("AppsUseLightTheme");
    let (mut value, mut size) = (1u32, 4u32);
    let status = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), RRF_RT_REG_DWORD, null_mut(), &mut value as *mut _ as _, &mut size)
    };
    status == 0 && value == 0
}

/// Dark or light title bar; on Windows 11 the caption also takes the toolbar color.
pub fn style_title_bar(hwnd: HWND, theme: &Theme) {
    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const DWMWA_BORDER_COLOR: u32 = 34;
    const DWMWA_CAPTION_COLOR: u32 = 35;
    const DWMWA_TEXT_COLOR: u32 = 36;
    let dark: BOOL = theme.dark as BOOL;
    unsafe {
        DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
        for (attr, color) in [(DWMWA_CAPTION_COLOR, theme.chrome_bg), (DWMWA_BORDER_COLOR, theme.border), (DWMWA_TEXT_COLOR, theme.text)] {
            DwmSetWindowAttribute(hwnd, attr, &color as *const _ as _, 4);
        }
    }
}

/// Uxtheme exports these only by ordinal. They are undocumented but used widely
/// (Notepad++, File Explorer itself) to get dark popup menus and scrollbars.
unsafe fn uxtheme_ordinal(ordinal: usize) -> Option<unsafe extern "system" fn() -> isize> {
    unsafe {
        let lib = LoadLibraryW(wide("uxtheme.dll").as_ptr());
        if lib.is_null() { None } else { GetProcAddress(lib, ordinal as *const u8) }
    }
}

/// Makes popup menus follow the theme.
pub fn style_menus(dark: bool) {
    unsafe {
        if let Some(f) = uxtheme_ordinal(135) {
            let set_preferred_app_mode: unsafe extern "system" fn(i32) -> i32 = transmute(f);
            set_preferred_app_mode(if dark { 2 } else { 3 }); // ForceDark / ForceLight
        }
        if let Some(f) = uxtheme_ordinal(136) {
            let flush_menu_themes: unsafe extern "system" fn() = transmute(f);
            flush_menu_themes();
        }
    }
}

/// Gives a common control the Explorer look, dark or light (including its scrollbars).
pub fn style_control(hwnd: HWND, dark: bool) {
    unsafe {
        if let Some(f) = uxtheme_ordinal(133) {
            let allow_dark_mode_for_window: unsafe extern "system" fn(HWND, BOOL) -> BOOL = transmute(f);
            allow_dark_mode_for_window(hwnd, dark as BOOL);
        }
        let name = wide(if dark { "DarkMode_Explorer" } else { "Explorer" });
        SetWindowTheme(hwnd, name.as_ptr(), null());
    }
}

/// Registers the common control classes. The manifest that build.rs embeds selects
/// Common Controls 6, which gives the tree view and edit boxes their modern look.
pub fn init_common_controls() {
    let icc = INITCOMMONCONTROLSEX { dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_TREEVIEW_CLASSES };
    unsafe { InitCommonControlsEx(&icc) };
}
