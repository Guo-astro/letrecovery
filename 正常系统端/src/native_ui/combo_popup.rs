//! The drop-down list of every read-only ComboBox (CBS_DROPDOWNLIST), drawn by LetRecovery.
//!
//! The native ComboLBox popup is never opened. The list looks exactly like the application's own
//! list controls: the same rounded outline (radius and border width of every field), the same
//! surface and text colours, the same text inset, the highlighted row in the selected-navigation
//! colours used by every list, and the system scrollbar of the current theme (drawn with the very
//! theme parts the lists use). There is no shadow.
//!
//! Everything is composed into one 32-bit surface and handed to DWM (or GDI without composition)
//! with UpdateLayeredWindow, so a frame is always complete: no erase, no partial paint, nothing
//! to flicker, and the corners outside the rounded outline are truly transparent. Hover changes
//! re-render the small surface only. The opening animation (fade in while sliding a few pixels
//! into place, ease-out, 140 ms) only changes the window position and constant alpha of the
//! already rendered surface. Nothing here needs a GPU.
//!
//! The combo keeps the keyboard focus the whole time (the popup never activates), selection is
//! written back with CB_SETCURSEL and the parent receives the usual CBN_DROPDOWN, CBN_SELENDOK /
//! CBN_SELENDCANCEL, CBN_CLOSEUP and CBN_SELCHANGE notifications.

use std::cell::RefCell;
use std::ffi::c_void;
use std::time::Instant;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    COLORREF, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleDC, CreateDIBSection, CreateSolidBrush, DeleteDC, DeleteObject,
    DrawTextW, EndPaint, FillRect, GdiFlush, GetDC, GetMonitorInfoW, GetTextMetricsW,
    MonitorFromWindow, Polygon, ReleaseDC, SelectObject, SetBkMode, SetTextColor, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, DT_END_ELLIPSIS, DT_NOPREFIX,
    DT_SINGLELINE, DT_VCENTER, HBITMAP, HDC, HFONT, HGDIOBJ, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, TEXTMETRICW, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, IsWindowEnabled, ReleaseCapture, SetCapture,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetAncestor, GetDlgCtrlID, GetParent,
    GetWindowRect, IsWindow, KillTimer, LoadCursorW, RegisterClassExW, SendMessageW,
    ShowWindow, UpdateLayeredWindow, GA_ROOT, HMENU, IDC_ARROW, SW_SHOWNOACTIVATE, ULW_ALPHA,
    WINDOW_EX_STYLE, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

use windows::Win32::UI::Controls::{CloseThemeData, DrawThemeBackground, OpenThemeData, HTHEME};

use super::controls::{rounded_control_frame_geometry, InnoMetrics};
use super::theme::Palette;
use crate::native_ui::GetDpiForWindow;

const CLASS_NAME: PCWSTR = w!("LetRecovery.DropDownList");
const ANIMATION_TIMER_ID: usize = 0x4c52_4450;
const OPEN_DURATION_SECONDS: f32 = 0.14;
const MAX_VISIBLE_ROWS: usize = 12;

const WM_COMMAND_MESSAGE: u32 = 0x0111;
const WM_GETFONT_MESSAGE: u32 = 0x0031;
const WM_KEYDOWN_MESSAGE: u32 = 0x0100;
const WM_CHAR_MESSAGE: u32 = 0x0102;
const WM_SYSKEYDOWN_MESSAGE: u32 = 0x0104;
const WM_TIMER_MESSAGE: u32 = 0x0113;
const WM_MOUSEMOVE_MESSAGE: u32 = 0x0200;
const WM_LBUTTONDOWN_MESSAGE: u32 = 0x0201;
const WM_LBUTTONUP_MESSAGE: u32 = 0x0202;
const WM_LBUTTONDBLCLK_MESSAGE: u32 = 0x0203;
const WM_MOUSEWHEEL_MESSAGE: u32 = 0x020a;
const WM_CAPTURECHANGED_MESSAGE: u32 = 0x0215;
const WM_MOUSEACTIVATE_MESSAGE: u32 = 0x0021;
const WM_NCHITTEST_MESSAGE: u32 = 0x0084;
const WM_PAINT_MESSAGE: u32 = 0x000f;
const WM_ERASEBKGND_MESSAGE: u32 = 0x0014;
const MA_NOACTIVATE_RESULT: isize = 3;
const HTCLIENT_RESULT: isize = 1;

const CB_GETCOUNT: u32 = 0x0146;
const CB_GETCURSEL: u32 = 0x0147;
const CB_GETLBTEXT: u32 = 0x0148;
const CB_GETLBTEXTLEN: u32 = 0x0149;
const CB_SETCURSEL: u32 = 0x014e;
const CB_GETDROPPEDWIDTH: u32 = 0x015f;
const CBN_SELCHANGE: u16 = 1;
const CBN_DROPDOWN: u16 = 7;
const CBN_CLOSEUP: u16 = 8;
const CBN_SELENDOK: u16 = 9;
const CBN_SELENDCANCEL: u16 = 10;

const VK_TAB_KEY: u16 = 0x09;
const VK_RETURN_KEY: u16 = 0x0d;
const VK_ESCAPE_KEY: u16 = 0x1b;
const VK_PRIOR_KEY: u16 = 0x21;
const VK_NEXT_KEY: u16 = 0x22;
const VK_END_KEY: u16 = 0x23;
const VK_HOME_KEY: u16 = 0x24;
const VK_UP_KEY: u16 = 0x26;
const VK_DOWN_KEY: u16 = 0x28;
const VK_F4_KEY: u16 = 0x73;

fn scale(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32
}

/// A top-down 32-bit surface selected into its own memory DC.
pub(crate) struct Surface {
    pub(crate) dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u32,
    pub(crate) width: i32,
    pub(crate) height: i32,
}

impl Surface {
    pub(crate) unsafe fn new(width: i32, height: i32) -> Option<Self> {
        if width <= 0 || height <= 0 {
            return None;
        }
        let screen = GetDC(HWND::default());
        let dc = CreateCompatibleDC(screen);
        let _ = ReleaseDC(HWND::default(), screen);
        if dc.is_invalid() {
            return None;
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: (width * height * 4) as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut::<c_void>();
        let Ok(bitmap) = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0)
        else {
            let _ = DeleteDC(dc);
            return None;
        };
        if bits.is_null() {
            let _ = DeleteObject(bitmap);
            let _ = DeleteDC(dc);
            return None;
        }
        let previous = SelectObject(dc, bitmap);
        Some(Self {
            dc,
            bitmap,
            previous,
            bits: bits.cast::<u32>(),
            width,
            height,
        })
    }

    pub(crate) unsafe fn release(&mut self) {
        if !self.dc.is_invalid() {
            let _ = SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.bitmap);
            let _ = DeleteDC(self.dc);
        }
        self.dc = HDC::default();
        self.bits = std::ptr::null_mut();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrollPart {
    None,
    Up,
    Down,
    Thumb,
    Track,
}

struct Popup {
    combo: HWND,
    hwnd: HWND,
    palette: Palette,
    font: HFONT,
    dpi: u32,
    items: Vec<Vec<u16>>,
    /// The combo's selection when the list opened.
    original: Option<usize>,
    /// The highlighted row: starts on the current value, follows the pointer and the arrow keys;
    /// Enter or a click accepts it.
    hot: Option<usize>,
    top: usize,
    rows: usize,
    row_height: i32,
    /// Outline radius and width, the same geometry as every other field and list.
    radius: i32,
    border: i32,
    /// Rows area and the scrollbar (when the list scrolls), in surface coordinates.
    rows_rect: RECT,
    scrollbar: Option<RECT>,
    theme: HTHEME,
    /// The closed field's own surface colour.
    background: COLORREF,
    scroll_hot: ScrollPart,
    scroll_hover: bool,
    /// Final screen position of the window.
    origin: POINT,
    slide: i32,
    offset: i32,
    alpha: u8,
    started: Instant,
    animating: bool,
    surface: Surface,
    /// Per pixel: coverage of the outline shape and coverage inside the border.
    mask: std::rc::Rc<Vec<[u8; 2]>>,
    thumb_drag: Option<(i32, usize)>,
}

thread_local! {
    static POPUP: RefCell<Option<Popup>> = const { RefCell::new(None) };
}

enum Action {
    None,
    Close { combo: HWND, accept: bool },
}

pub(crate) fn is_open(combo: HWND) -> bool {
    POPUP.with(|cell| {
        cell.try_borrow()
            .map(|popup| popup.as_ref().is_some_and(|popup| popup.combo == combo))
            .unwrap_or(false)
    })
}

pub(crate) unsafe fn toggle(combo: HWND, palette: Palette) {
    if is_open(combo) {
        close(combo, false);
    } else {
        open(combo, palette);
    }
}

unsafe fn notify(combo: HWND, code: u16) {
    if let Ok(parent) = GetParent(combo) {
        let id = GetDlgCtrlID(combo) as u16 as usize;
        let _ = SendMessageW(
            parent,
            WM_COMMAND_MESSAGE,
            WPARAM(id | (usize::from(code) << 16)),
            LPARAM(combo.0 as isize),
        );
    }
}

unsafe fn combo_strings(combo: HWND) -> Vec<Vec<u16>> {
    let count = SendMessageW(combo, CB_GETCOUNT, WPARAM(0), LPARAM(0)).0.max(0) as usize;
    let mut items = Vec::with_capacity(count);
    for index in 0..count {
        let length = SendMessageW(combo, CB_GETLBTEXTLEN, WPARAM(index), LPARAM(0)).0;
        if length <= 0 {
            items.push(Vec::new());
            continue;
        }
        let mut text = vec![0u16; length as usize + 1];
        let copied = SendMessageW(
            combo,
            CB_GETLBTEXT,
            WPARAM(index),
            LPARAM(text.as_mut_ptr() as isize),
        )
        .0
        .clamp(0, length) as usize;
        text.truncate(copied);
        items.push(text);
    }
    items
}

pub(crate) unsafe fn text_height(font: HFONT, dpi: u32) -> i32 {
    let fallback = scale(16, dpi);
    let screen = GetDC(HWND::default());
    if screen.is_invalid() {
        return fallback;
    }
    let previous = (!font.is_invalid()).then(|| SelectObject(screen, font));
    let mut metrics = TEXTMETRICW::default();
    let measured = GetTextMetricsW(screen, &mut metrics).as_bool();
    if let Some(previous) = previous {
        let _ = SelectObject(screen, previous);
    }
    let _ = ReleaseDC(HWND::default(), screen);
    if measured {
        metrics.tmHeight.max(1)
    } else {
        fallback
    }
}

pub(crate) unsafe fn work_area(window: HWND) -> RECT {
    let monitor = MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST);
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if GetMonitorInfoW(monitor, &mut info).as_bool() {
        info.rcWork
    } else {
        RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        }
    }
}

unsafe fn register_class() -> bool {
    static REGISTERED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *REGISTERED.get_or_init(|| {
        let Ok(module) = GetModuleHandleW(None) else {
            return false;
        };
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(popup_proc),
            hInstance: HINSTANCE(module.0),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        RegisterClassExW(&class) != 0
    })
}

/// Signed distance from a pixel centre to a rounded rectangle (negative inside).
fn rounded_distance(x: f32, y: f32, rect: RECT, radius: f32) -> f32 {
    let half_width = (rect.right - rect.left) as f32 / 2.0;
    let half_height = (rect.bottom - rect.top) as f32 / 2.0;
    let centre_x = rect.left as f32 + half_width;
    let centre_y = rect.top as f32 + half_height;
    let radius = radius.min(half_width).min(half_height);
    let qx = (x - centre_x).abs() - (half_width - radius);
    let qy = (y - centre_y).abs() - (half_height - radius);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - radius
}

thread_local! {
    static MASKS: RefCell<Vec<((i32, i32, i32, i32), std::rc::Rc<Vec<[u8; 2]>>)>> =
        const { RefCell::new(Vec::new()) };
    static ROW_HEIGHTS: RefCell<Vec<((isize, u32), i32)>> = const { RefCell::new(Vec::new()) };
}

/// The outline mask depends only on the size; a combo opens with the same size every time.
pub(crate) fn cached_mask(
    width: i32,
    height: i32,
    radius: i32,
    border: i32,
) -> std::rc::Rc<Vec<[u8; 2]>> {
    let key = (width, height, radius, border);
    MASKS.with(|masks| {
        let mut masks = masks.borrow_mut();
        if let Some((_, mask)) = masks.iter().find(|(entry, _)| *entry == key) {
            return mask.clone();
        }
        let mask = std::rc::Rc::new(build_mask(width, height, radius, border));
        if masks.len() >= 8 {
            masks.remove(0);
        }
        masks.push((key, mask.clone()));
        mask
    })
}

/// Row height of a report ListView (LVS_REPORT, full-row select, double-buffered: the style of
/// every list in the application) with this font, measured once per font and DPI.
pub(crate) unsafe fn list_row_height(parent: HWND, font: HFONT, dpi: u32) -> Option<i32> {
    use windows::Win32::UI::Controls::{LVCF_WIDTH, LVCOLUMNW, LVIF_TEXT, LVITEMW};
    const LVS_REPORT_STYLE: u32 = 0x0001;
    const LVS_SHOWSELALWAYS_STYLE: u32 = 0x0008;
    const LVM_SETEXTENDEDLISTVIEWSTYLE: u32 = 0x1036;
    const LVM_INSERTCOLUMNW: u32 = 0x1061;
    const LVM_INSERTITEMW: u32 = 0x104D;
    const LVM_GETITEMRECT: u32 = 0x100E;
    const LVS_EX_FULLROWSELECT: isize = 0x0020;
    const LVS_EX_DOUBLEBUFFER: isize = 0x0001_0000;
    let key = (font.0 as isize, dpi);
    if let Some(height) = ROW_HEIGHTS.with(|cache| {
        cache.borrow().iter().find(|(entry, _)| *entry == key).map(|(_, height)| *height)
    }) {
        return Some(height);
    }
    let list = CreateWindowExW(
        WINDOW_EX_STYLE(0x0000_0004), // WS_EX_NOPARENTNOTIFY
        w!("SysListView32"),
        w!(""),
        windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
            windows::Win32::UI::WindowsAndMessaging::WS_CHILD.0
                | LVS_REPORT_STYLE
                | LVS_SHOWSELALWAYS_STYLE,
        ),
        -4000,
        -4000,
        200,
        200,
        parent,
        HMENU::default(),
        HINSTANCE::default(),
        None,
    )
    .ok()?;
    if !font.is_invalid() {
        let _ = SendMessageW(
            list,
            0x0030, // WM_SETFONT
            WPARAM(font.0 as usize),
            LPARAM(0),
        );
    }
    let _ = SendMessageW(
        list,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        WPARAM(0),
        LPARAM(LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER),
    );
    let column = LVCOLUMNW {
        mask: LVCF_WIDTH,
        cx: 120,
        ..Default::default()
    };
    let _ = SendMessageW(
        list,
        LVM_INSERTCOLUMNW,
        WPARAM(0),
        LPARAM((&column as *const LVCOLUMNW) as isize),
    );
    let mut text: Vec<u16> = "Ag".encode_utf16().chain(std::iter::once(0)).collect();
    let item = LVITEMW {
        mask: LVIF_TEXT,
        pszText: windows::core::PWSTR(text.as_mut_ptr()),
        ..Default::default()
    };
    let _ = SendMessageW(
        list,
        LVM_INSERTITEMW,
        WPARAM(0),
        LPARAM((&item as *const LVITEMW) as isize),
    );
    let mut bounds = RECT::default(); // left = LVIR_BOUNDS (0)
    let measured = SendMessageW(
        list,
        LVM_GETITEMRECT,
        WPARAM(0),
        LPARAM((&mut bounds as *mut RECT) as isize),
    )
    .0 != 0;
    let _ = DestroyWindow(list);
    let height = bounds.bottom - bounds.top;
    if !measured || height <= 0 || height > scale(80, dpi) {
        return None;
    }
    ROW_HEIGHTS.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() >= 16 {
            cache.remove(0);
        }
        cache.push((key, height));
    });
    Some(height)
}

fn build_mask(width: i32, height: i32, radius: i32, border: i32) -> Vec<[u8; 2]> {
    let shape = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };
    let radius = radius as f32;
    let border = border as f32;
    let mut mask = Vec::with_capacity((width * height).max(0) as usize);
    for y in 0..height {
        for x in 0..width {
            let distance = rounded_distance(x as f32 + 0.5, y as f32 + 0.5, shape, radius);
            let outer = (0.5 - distance).clamp(0.0, 1.0);
            let inner = (0.5 - (distance + border)).clamp(0.0, 1.0);
            mask.push([(outer * 255.0).round() as u8, (inner * 255.0).round() as u8]);
        }
    }
    mask
}

pub(crate) unsafe fn open(combo: HWND, palette: Palette) {
    close_any(false);
    if !IsWindow(combo).as_bool() || !IsWindowEnabled(combo).as_bool() || !register_class() {
        return;
    }
    notify(combo, CBN_DROPDOWN);
    let items = combo_strings(combo);
    if items.is_empty() {
        notify(combo, CBN_CLOSEUP);
        return;
    }
    let dpi = GetDpiForWindow(combo).max(96);
    let s = |value: i32| scale(value, dpi);
    let font = HFONT(SendMessageW(combo, WM_GETFONT_MESSAGE, WPARAM(0), LPARAM(0)).0 as *mut _);
    // Exactly the row height of the application's report lists (the install page's partition
    // list): measured from a real ListView with the same font, not guessed.
    let row_height = list_row_height(GetAncestor(combo, GA_ROOT), font, dpi)
        .unwrap_or_else(|| text_height(font, dpi) + s(4));
    let gap = s(2);
    let mut combo_rect = RECT::default();
    if GetWindowRect(combo, &mut combo_rect).is_err() {
        notify(combo, CBN_CLOSEUP);
        return;
    }
    let field_height = super::theme::combo_closed_height(
        combo,
        InnoMetrics::for_dpi(dpi).field_height,
    )
    .min((combo_rect.bottom - combo_rect.top).max(1));
    let field_bottom = combo_rect.top + field_height;
    let work = work_area(combo);
    let dropped_width = SendMessageW(combo, CB_GETDROPPEDWIDTH, WPARAM(0), LPARAM(0)).0.max(0) as i32;
    let width = (combo_rect.right - combo_rect.left)
        .max(dropped_width)
        .max(s(80))
        .min((work.right - work.left).max(s(80)));
    // The outline of every field: same radius and border width.
    let (radius, border) = rounded_control_frame_geometry(width, s(200), dpi)
        .map_or((s(5), s(1).max(1)), |geometry| (geometry.radius, geometry.side_band.max(1)));
    // Rows start right at the outline: a highlighted row meets the border on both sides, and the
    // first row meets the top border (the rounded corners clip it like the lists' selection).
    let inset = border;
    let wanted = items.len().min(MAX_VISIBLE_ROWS);
    let fit = |space: i32| (((space - inset * 2) / row_height).max(0)) as usize;
    let below = fit(work.bottom - field_bottom - gap);
    let above_space = fit(combo_rect.top - gap - work.top);
    let open_above = below < wanted && above_space > below;
    let rows = wanted.min(if open_above { above_space } else { below }).max(1);
    let height = rows as i32 * row_height + inset * 2;
    let x = combo_rect
        .left
        .clamp(work.left, (work.right - width).max(work.left));
    let y = if open_above {
        combo_rect.top - gap - height
    } else {
        field_bottom + gap
    };
    let origin = POINT { x, y };
    let scrolls = items.len() > rows;
    let scrollbar_width = s(17).min(width / 3);
    let content = RECT {
        left: inset,
        top: inset,
        right: width - inset,
        bottom: height - inset,
    };
    let scrollbar = scrolls.then(|| RECT {
        left: width - border - scrollbar_width,
        top: border,
        right: width - border,
        bottom: height - border,
    });
    let rows_rect = RECT {
        right: scrollbar.map_or(content.right, |bar| bar.left),
        ..content
    };
    let owner = GetAncestor(combo, GA_ROOT);
    let Ok(hwnd) = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WINDOW_EX_STYLE(0x0000_0004),
        CLASS_NAME,
        w!(""),
        WS_POPUP,
        origin.x,
        origin.y,
        width,
        height,
        owner,
        HMENU::default(),
        HINSTANCE::default(),
        None,
    ) else {
        notify(combo, CBN_CLOSEUP);
        return;
    };
    let Some(surface) = Surface::new(width, height) else {
        let _ = DestroyWindow(hwnd);
        notify(combo, CBN_CLOSEUP);
        return;
    };
    let theme = if scrolls {
        let theme = OpenThemeData(
            hwnd,
            if palette.dark {
                w!("DarkMode_Explorer::ScrollBar")
            } else {
                w!("Explorer::ScrollBar")
            },
        );
        if theme.is_invalid() {
            OpenThemeData(hwnd, w!("ScrollBar"))
        } else {
            theme
        }
    } else {
        HTHEME::default()
    };
    let selection = SendMessageW(combo, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
    let original = usize::try_from(selection).ok().filter(|index| *index < items.len());
    let slide = if open_above { s(8) } else { -s(8) };
    let mut popup = Popup {
        combo,
        hwnd,
        palette,
        font,
        dpi,
        mask: cached_mask(width, height, radius, border),
        items,
        original,
        hot: original,
        top: 0,
        rows,
        row_height,
        radius,
        border,
        rows_rect,
        scrollbar,
        theme,
        background: super::theme::drop_down_field_color(combo, palette),
        scroll_hot: ScrollPart::None,
        scroll_hover: false,
        origin,
        slide,
        offset: slide,
        alpha: 0,
        started: Instant::now(),
        animating: true,
        surface,
        thumb_drag: None,
    };
    if let Some(index) = original {
        // Open with the current value in view, a little below the top when possible.
        popup.top = index.saturating_sub(rows / 3);
    }
    clamp_top(&mut popup);
    render(&mut popup);
    present(&popup);
    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    animate_open(&mut popup);
    POPUP.with(|cell| {
        if let Ok(mut slot) = cell.try_borrow_mut() {
            *slot = Some(popup);
        }
    });
    let _ = SetCapture(hwnd);
    super::theme::repaint_drop_down_field(combo, palette);
}

/// The opening animation, paced by the compositor: one frame per display refresh (DwmFlush), so
/// it is as smooth as the monitor allows instead of following the coarse 10-16 ms message timer
/// that made it stutter. Each frame only moves the window and changes its constant alpha (the
/// surface is already rendered). It lasts 120 ms; without composition it steps every 8 ms.
unsafe fn animate_open(popup: &mut Popup) {
    const DURATION_SECONDS: f32 = 0.12;
    let composited = windows::Win32::Graphics::Dwm::DwmIsCompositionEnabled()
        .map(|enabled| enabled.as_bool())
        .unwrap_or(false);
    let started = Instant::now();
    for _ in 0..240 {
        let progress = (started.elapsed().as_secs_f32() / DURATION_SECONDS).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - progress).powi(3);
        popup.alpha = (255.0 * eased).round().clamp(0.0, 255.0) as u8;
        popup.offset = ((1.0 - eased) * popup.slide as f32).round() as i32;
        present(popup);
        if progress >= 1.0 {
            break;
        }
        if !composited || windows::Win32::Graphics::Dwm::DwmFlush().is_err() {
            std::thread::sleep(std::time::Duration::from_millis(8));
        }
    }
    popup.alpha = 255;
    popup.offset = 0;
    popup.animating = false;
    present(popup);
}

pub(crate) unsafe fn close(combo: HWND, accept: bool) {
    let popup = POPUP.with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return None;
        };
        if slot.as_ref().is_some_and(|popup| popup.combo == combo) {
            slot.take()
        } else {
            None
        }
    });
    if let Some(popup) = popup {
        finish(popup, accept);
    }
}

pub(crate) unsafe fn close_any(accept: bool) {
    let popup = POPUP.with(|cell| cell.try_borrow_mut().ok().and_then(|mut slot| slot.take()));
    if let Some(popup) = popup {
        finish(popup, accept);
    }
}

unsafe fn finish(mut popup: Popup, accept: bool) {
    let _ = KillTimer(popup.hwnd, ANIMATION_TIMER_ID);
    if GetCapture() == popup.hwnd {
        let _ = ReleaseCapture();
    }
    let _ = DestroyWindow(popup.hwnd);
    popup.surface.release();
    if !popup.theme.is_invalid() {
        let _ = CloseThemeData(popup.theme);
    }
    let combo = popup.combo;
    if !IsWindow(combo).as_bool() {
        return;
    }
    let current = SendMessageW(combo, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
    let chosen = if accept { popup.hot } else { None };
    let changed = chosen.is_some_and(|index| index as isize != current);
    if let Some(index) = chosen.filter(|_| changed) {
        let _ = SendMessageW(combo, CB_SETCURSEL, WPARAM(index), LPARAM(0));
    }
    notify(combo, if accept { CBN_SELENDOK } else { CBN_SELENDCANCEL });
    notify(combo, CBN_CLOSEUP);
    if changed {
        notify(combo, CBN_SELCHANGE);
    }
    super::theme::repaint_drop_down_field(combo, popup.palette);
}

fn clamp_top(popup: &mut Popup) {
    let last_top = popup.items.len().saturating_sub(popup.rows);
    popup.top = popup.top.min(last_top);
}

fn ensure_visible(popup: &mut Popup, index: usize) {
    if index < popup.top {
        popup.top = index;
    } else if index >= popup.top + popup.rows {
        popup.top = index + 1 - popup.rows;
    }
    clamp_top(popup);
}

fn scrollable(popup: &Popup) -> bool {
    popup.scrollbar.is_some() && popup.items.len() > popup.rows
}

/// Up arrow, track and down arrow of the scrollbar (arrows are square).
fn scrollbar_parts(popup: &Popup) -> Option<(RECT, RECT, RECT)> {
    let bar = popup.scrollbar?;
    let arrow = (bar.right - bar.left).min((bar.bottom - bar.top) / 3).max(1);
    let up = RECT {
        bottom: bar.top + arrow,
        ..bar
    };
    let down = RECT {
        top: bar.bottom - arrow,
        ..bar
    };
    let track = RECT {
        top: up.bottom,
        bottom: down.top,
        ..bar
    };
    Some((up, track, down))
}

fn thumb_rect(popup: &Popup) -> Option<RECT> {
    let (_, track, _) = scrollbar_parts(popup)?;
    let track_height = (track.bottom - track.top).max(1);
    let count = popup.items.len().max(1);
    let thumb_height = (track_height * popup.rows as i32 / count as i32)
        .max(scale(16, popup.dpi))
        .min(track_height);
    let range = count.saturating_sub(popup.rows).max(1);
    let top = track.top + (track_height - thumb_height) * popup.top as i32 / range as i32;
    Some(RECT {
        top,
        bottom: top + thumb_height,
        ..track
    })
}

fn contains(rect: RECT, x: i32, y: i32) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

fn scroll_part_at(popup: &Popup, x: i32, y: i32) -> ScrollPart {
    let Some((up, track, down)) = scrollbar_parts(popup) else {
        return ScrollPart::None;
    };
    if contains(up, x, y) {
        ScrollPart::Up
    } else if contains(down, x, y) {
        ScrollPart::Down
    } else if thumb_rect(popup).is_some_and(|thumb| contains(thumb, x, y)) {
        ScrollPart::Thumb
    } else if contains(track, x, y) {
        ScrollPart::Track
    } else {
        ScrollPart::None
    }
}

fn row_rect(popup: &Popup, slot: usize) -> RECT {
    let top = popup.rows_rect.top + slot as i32 * popup.row_height;
    RECT {
        top,
        bottom: top + popup.row_height,
        ..popup.rows_rect
    }
}

fn row_at(popup: &Popup, x: i32, y: i32) -> Option<usize> {
    let area = popup.rows_rect;
    if x < area.left || x >= area.right || y < area.top || y >= area.bottom {
        return None;
    }
    let slot = ((y - area.top) / popup.row_height.max(1)) as usize;
    if slot >= popup.rows {
        return None;
    }
    let index = popup.top + slot;
    (index < popup.items.len()).then_some(index)
}

unsafe fn fill_rect(dc: HDC, rect: RECT, color: COLORREF) {
    let brush = CreateSolidBrush(color);
    let _ = FillRect(dc, &rect, brush);
    let _ = DeleteObject(brush);
}

unsafe fn render(popup: &mut Popup) {
    let dc = popup.surface.dc;
    if dc.is_invalid() {
        return;
    }
    let palette = popup.palette;
    let whole = RECT {
        left: 0,
        top: 0,
        right: popup.surface.width,
        bottom: popup.surface.height,
    };
    fill_rect(dc, whole, popup.background);
    let previous_font = (!popup.font.is_invalid()).then(|| SelectObject(dc, popup.font));
    let _ = SetBkMode(dc, TRANSPARENT);
    let inset = scale(7, popup.dpi);
    let highlight = popup.hot.or(popup.original);
    let last = (popup.top + popup.rows).min(popup.items.len());
    for (slot, index) in (popup.top..last).enumerate() {
        let row = row_rect(popup, slot);
        let (text_color, background) = if highlight == Some(index) {
            super::theme::list_selection_colors(palette, false)
        } else {
            (palette.text, popup.background)
        };
        if background != popup.background {
            fill_rect(dc, row, background);
        }
        let mut text = popup.items[index].clone();
        let mut text_rect = RECT {
            left: row.left + inset,
            right: row.right - inset,
            ..row
        };
        if !text.is_empty() && text_rect.right > text_rect.left {
            let _ = SetTextColor(dc, text_color);
            let _ = DrawTextW(
                dc,
                &mut text,
                &mut text_rect,
                DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
        }
    }
    if let Some(previous_font) = previous_font {
        let _ = SelectObject(dc, previous_font);
    }
    render_scrollbar(popup);
    apply_mask(popup);
}

/// The scrollbar of the current Windows theme, drawn with the same theme parts USER32 uses for the
/// lists' scrollbars (DarkMode_Explorer in dark mode), including the hover and hot states.
unsafe fn render_scrollbar(popup: &Popup) {
    const SBP_ARROWBTN: i32 = 1;
    const SBP_THUMBBTNVERT: i32 = 3;
    const SBP_LOWERTRACKVERT: i32 = 6;
    const SBP_UPPERTRACKVERT: i32 = 7;
    const SBP_GRIPPERVERT: i32 = 9;
    const ABS_UPNORMAL: i32 = 1;
    const ABS_UPHOT: i32 = 2;
    const ABS_UPPRESSED: i32 = 3;
    const ABS_DOWNNORMAL: i32 = 5;
    const ABS_DOWNHOT: i32 = 6;
    const ABS_UPHOVER: i32 = 17;
    const ABS_DOWNHOVER: i32 = 18;
    const SCRBS_NORMAL: i32 = 1;
    const SCRBS_HOT: i32 = 2;
    const SCRBS_PRESSED: i32 = 3;
    const SCRBS_HOVER: i32 = 5;
    let Some((up, track, down)) = scrollbar_parts(popup) else {
        return;
    };
    let Some(thumb) = thumb_rect(popup) else {
        return;
    };
    let dc = popup.surface.dc;
    let upper = RECT {
        bottom: thumb.top,
        ..track
    };
    let lower = RECT {
        top: thumb.bottom,
        ..track
    };
    if popup.theme.is_invalid() {
        // Visual styles are off: a plain track, thumb and arrow marks in the list colours.
        let palette = popup.palette;
        fill_rect(dc, RECT { top: up.top, bottom: down.bottom, ..track }, palette.button);
        let inset = scale(4, popup.dpi);
        fill_rect(
            dc,
            RECT {
                left: thumb.left + inset,
                right: thumb.right - inset,
                ..thumb
            },
            palette.text_disabled,
        );
        for (rect, pointing_up) in [(up, true), (down, false)] {
            let cx = (rect.left + rect.right) / 2;
            let cy = (rect.top + rect.bottom) / 2;
            let half = scale(4, popup.dpi);
            let points = if pointing_up {
                [
                    POINT { x: cx - half, y: cy + half / 2 },
                    POINT { x: cx + half, y: cy + half / 2 },
                    POINT { x: cx, y: cy - half / 2 },
                ]
            } else {
                [
                    POINT { x: cx - half, y: cy - half / 2 },
                    POINT { x: cx + half, y: cy - half / 2 },
                    POINT { x: cx, y: cy + half / 2 },
                ]
            };
            let brush = CreateSolidBrush(palette.text_secondary);
            let previous = SelectObject(dc, brush);
            let _ = Polygon(dc, &points);
            let _ = SelectObject(dc, previous);
            let _ = DeleteObject(brush);
        }
        return;
    }
    let hover = popup.scroll_hover || popup.thumb_drag.is_some();
    let track_state = if hover { SCRBS_HOVER } else { SCRBS_NORMAL };
    let up_state = match popup.scroll_hot {
        ScrollPart::Up => ABS_UPHOT,
        _ if hover => ABS_UPHOVER,
        _ => ABS_UPNORMAL,
    };
    let down_state = match popup.scroll_hot {
        ScrollPart::Down => ABS_DOWNHOT,
        _ if hover => ABS_DOWNHOVER,
        _ => ABS_DOWNNORMAL,
    };
    let thumb_state = if popup.thumb_drag.is_some() {
        SCRBS_PRESSED
    } else if popup.scroll_hot == ScrollPart::Thumb {
        SCRBS_HOT
    } else {
        track_state
    };
    let _ = ABS_UPPRESSED;
    let draw = |part: i32, state: i32, rect: RECT| {
        if rect.bottom > rect.top && rect.right > rect.left {
            let _ = DrawThemeBackground(popup.theme, dc, part, state, &rect, None);
        }
    };
    draw(SBP_UPPERTRACKVERT, track_state, upper);
    draw(SBP_LOWERTRACKVERT, track_state, lower);
    draw(SBP_THUMBBTNVERT, thumb_state, thumb);
    draw(SBP_GRIPPERVERT, thumb_state, thumb);
    draw(SBP_ARROWBTN, up_state, up);
    draw(SBP_ARROWBTN, down_state, down);
}

/// Turns the opaque GDI composition into premultiplied BGRA: the rounded outline with its
/// antialiased border; everything outside the outline is fully transparent.
unsafe fn apply_mask(popup: &Popup) {
    compose_outline(&popup.surface, &popup.mask, popup.palette.border);
}

pub(crate) unsafe fn compose_outline(surface: &Surface, mask: &[[u8; 2]], border: COLORREF) {
    let _ = GdiFlush();
    let count = (surface.width * surface.height).max(0) as usize;
    if surface.bits.is_null() || mask.len() != count {
        return;
    }
    let pixels = std::slice::from_raw_parts_mut(surface.bits, count);
    let border = border.0;
    let border_red = border & 0xff;
    let border_green = (border >> 8) & 0xff;
    let border_blue = (border >> 16) & 0xff;
    for (pixel, [outer, inner]) in pixels.iter_mut().zip(mask.iter().copied()) {
        if outer == 255 && inner == 255 {
            *pixel = 0xff00_0000 | (*pixel & 0x00ff_ffff);
            continue;
        }
        if outer == 0 {
            *pixel = 0;
            continue;
        }
        let content = *pixel;
        let outer = u32::from(outer);
        let inner = u32::from(inner).min(outer);
        let edge = outer - inner;
        let blue = ((content & 0xff) * inner + border_blue * edge) / 255;
        let green = (((content >> 8) & 0xff) * inner + border_green * edge) / 255;
        let red = (((content >> 16) & 0xff) * inner + border_red * edge) / 255;
        *pixel = (outer << 24) | (red.min(255) << 16) | (green.min(255) << 8) | blue.min(255);
    }
}

unsafe fn present(popup: &Popup) {
    let position = POINT {
        x: popup.origin.x,
        y: popup.origin.y + popup.offset,
    };
    let size = SIZE {
        cx: popup.surface.width,
        cy: popup.surface.height,
    };
    let source = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: 0, // AC_SRC_OVER
        BlendFlags: 0,
        SourceConstantAlpha: popup.alpha,
        AlphaFormat: 1, // AC_SRC_ALPHA
    };
    let _ = UpdateLayeredWindow(
        popup.hwnd,
        HDC::default(),
        Some(&position),
        Some(&size),
        popup.surface.dc,
        Some(&source),
        COLORREF(0),
        Some(&blend),
        ULW_ALPHA,
    );
}

unsafe fn refresh(popup: &mut Popup) {
    render(popup);
    present(popup);
}

unsafe fn set_hot(popup: &mut Popup, index: usize) {
    if popup.items.is_empty() {
        return;
    }
    let index = index.min(popup.items.len() - 1);
    popup.hot = Some(index);
    ensure_visible(popup, index);
    refresh(popup);
}

unsafe fn move_hot(popup: &mut Popup, delta: isize) {
    let count = popup.items.len() as isize;
    if count == 0 {
        return;
    }
    let current = popup.hot.or(popup.original).map_or(-1, |index| index as isize);
    let next = if current < 0 {
        if delta > 0 {
            0
        } else {
            count - 1
        }
    } else if delta.abs() == 1 {
        // Single steps wrap around: Down on the last row goes to the first, Up on the first
        // goes to the last. Page steps stop at the ends.
        (current + delta).rem_euclid(count)
    } else {
        (current + delta).clamp(0, count - 1)
    };
    set_hot(popup, next as usize);
}

unsafe fn scroll_rows(popup: &mut Popup, delta: isize) {
    let top = (popup.top as isize + delta).max(0) as usize;
    popup.top = top;
    clamp_top(popup);
    refresh(popup);
}

fn point_from(lparam: LPARAM) -> (i32, i32) {
    let x = (lparam.0 & 0xffff) as u16 as i16 as i32;
    let y = ((lparam.0 >> 16) & 0xffff) as u16 as i16 as i32;
    (x, y)
}

unsafe fn drag_thumb(popup: &mut Popup, y: i32) {
    let Some((grab, grab_top)) = popup.thumb_drag else {
        return;
    };
    let (Some((_, track, _)), Some(thumb)) = (scrollbar_parts(popup), thumb_rect(popup)) else {
        return;
    };
    let free = ((track.bottom - track.top) - (thumb.bottom - thumb.top)).max(1);
    let range = popup.items.len().saturating_sub(popup.rows) as i32;
    let moved = (y - grab) * range / free;
    popup.top = (grab_top as i32 + moved).clamp(0, range.max(0)) as usize;
    clamp_top(popup);
    refresh(popup);
}

unsafe fn handle(popup: &mut Popup, hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> Action {
    match message {
        WM_TIMER_MESSAGE if wparam.0 == ANIMATION_TIMER_ID => {
            let progress =
                (popup.started.elapsed().as_secs_f32() / OPEN_DURATION_SECONDS).clamp(0.0, 1.0);
            let eased = 1.0 - (1.0 - progress).powi(3);
            popup.alpha = (40.0 + 215.0 * eased).round().min(255.0) as u8;
            popup.offset = ((1.0 - eased) * popup.slide as f32).round() as i32;
            present(popup);
            if progress >= 1.0 {
                popup.animating = false;
                let _ = KillTimer(hwnd, ANIMATION_TIMER_ID);
            }
            Action::None
        }
        WM_MOUSEMOVE_MESSAGE => {
            let (x, y) = point_from(lparam);
            if popup.thumb_drag.is_some() {
                drag_thumb(popup, y);
                return Action::None;
            }
            let part = scroll_part_at(popup, x, y);
            let hover = part != ScrollPart::None;
            let mut changed = part != popup.scroll_hot || hover != popup.scroll_hover;
            popup.scroll_hot = part;
            popup.scroll_hover = hover;
            if let Some(index) = row_at(popup, x, y) {
                if popup.hot != Some(index) {
                    popup.hot = Some(index);
                    changed = true;
                }
            }
            if changed {
                refresh(popup);
            }
            Action::None
        }
        WM_LBUTTONDOWN_MESSAGE | WM_LBUTTONDBLCLK_MESSAGE => {
            let (x, y) = point_from(lparam);
            if x < 0 || y < 0 || x >= popup.surface.width || y >= popup.surface.height {
                return Action::Close {
                    combo: popup.combo,
                    accept: false,
                };
            }
            match scroll_part_at(popup, x, y) {
                ScrollPart::Up => scroll_rows(popup, -1),
                ScrollPart::Down => scroll_rows(popup, 1),
                ScrollPart::Thumb => {
                    popup.thumb_drag = Some((y, popup.top));
                    refresh(popup);
                }
                ScrollPart::Track => {
                    let above = thumb_rect(popup).is_some_and(|thumb| y < thumb.top);
                    let page = popup.rows as isize;
                    scroll_rows(popup, if above { -page } else { page });
                }
                ScrollPart::None => {
                    if let Some(index) = row_at(popup, x, y) {
                        popup.hot = Some(index);
                        refresh(popup);
                    }
                }
            }
            Action::None
        }
        WM_LBUTTONUP_MESSAGE => {
            if popup.thumb_drag.take().is_some() {
                refresh(popup);
                return Action::None;
            }
            let (x, y) = point_from(lparam);
            match row_at(popup, x, y) {
                Some(index) => {
                    popup.hot = Some(index);
                    Action::Close {
                        combo: popup.combo,
                        accept: true,
                    }
                }
                // Releasing the button that opened the list (still over the combo) keeps it open.
                None => Action::None,
            }
        }
        WM_MOUSEWHEEL_MESSAGE => {
            let delta = ((wparam.0 >> 16) & 0xffff) as u16 as i16 as isize;
            scroll_rows(popup, -(delta / 120) * 3);
            Action::None
        }
        WM_CAPTURECHANGED_MESSAGE if lparam.0 != hwnd.0 as isize => Action::Close {
            combo: popup.combo,
            accept: false,
        },
        _ => Action::None,
    }
}

unsafe extern "system" fn popup_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_MOUSEACTIVATE_MESSAGE => LRESULT(MA_NOACTIVATE_RESULT),
        WM_NCHITTEST_MESSAGE => LRESULT(HTCLIENT_RESULT),
        WM_ERASEBKGND_MESSAGE => LRESULT(1),
        WM_PAINT_MESSAGE => {
            let mut paint = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut paint);
            let _ = EndPaint(hwnd, &paint);
            LRESULT(0)
        }
        WM_TIMER_MESSAGE
        | WM_MOUSEMOVE_MESSAGE
        | WM_LBUTTONDOWN_MESSAGE
        | WM_LBUTTONDBLCLK_MESSAGE
        | WM_LBUTTONUP_MESSAGE
        | WM_MOUSEWHEEL_MESSAGE
        | WM_CAPTURECHANGED_MESSAGE => {
            let action = POPUP.with(|cell| {
                let Ok(mut slot) = cell.try_borrow_mut() else {
                    return Action::None;
                };
                match slot.as_mut() {
                    Some(popup) if popup.hwnd == hwnd => handle(popup, hwnd, message, wparam, lparam),
                    _ => Action::None,
                }
            });
            if let Action::Close { combo, accept } = action {
                close(combo, accept);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

/// Keyboard and wheel input that reaches the focused combo. Returns true when it was used here:
/// F4 and Alt+Up/Down open the list; while it is open the arrows, Page Up/Down, Home/End and a
/// typed letter move the highlight, Enter accepts, Escape cancels and the wheel scrolls.
pub(crate) unsafe fn handle_combo_input(
    combo: HWND,
    palette: Palette,
    message: u32,
    wparam: WPARAM,
    _lparam: LPARAM,
) -> bool {
    let open_now = is_open(combo);
    let key = wparam.0 as u16;
    match message {
        WM_KEYDOWN_MESSAGE if !open_now => {
            if key == VK_F4_KEY {
                open(combo, palette);
                return true;
            }
            false
        }
        WM_SYSKEYDOWN_MESSAGE if key == VK_UP_KEY || key == VK_DOWN_KEY => {
            if open_now {
                close(combo, true);
            } else {
                open(combo, palette);
            }
            true
        }
        WM_KEYDOWN_MESSAGE => {
            match key {
                VK_RETURN_KEY | VK_F4_KEY | VK_TAB_KEY => close(combo, true),
                VK_ESCAPE_KEY => close(combo, false),
                _ => {
                    with_popup(combo, |popup| match key {
                        VK_UP_KEY => move_hot(popup, -1),
                        VK_DOWN_KEY => move_hot(popup, 1),
                        VK_PRIOR_KEY => move_hot(popup, -(popup.rows as isize)),
                        VK_NEXT_KEY => move_hot(popup, popup.rows as isize),
                        VK_HOME_KEY => set_hot(popup, 0),
                        VK_END_KEY => set_hot(popup, popup.items.len().saturating_sub(1)),
                        _ => {}
                    });
                }
            }
            true
        }
        WM_CHAR_MESSAGE if open_now => {
            let typed = char::from_u32(wparam.0 as u32).map(|character| character.to_lowercase().next());
            if let Some(Some(typed)) = typed {
                if !typed.is_control() {
                    with_popup(combo, |popup| {
                        let count = popup.items.len();
                        let start = popup.hot.map_or(0, |index| index + 1);
                        let found = (0..count).map(|step| (start + step) % count).find(|index| {
                            String::from_utf16_lossy(&popup.items[*index])
                                .chars()
                                .next()
                                .and_then(|first| first.to_lowercase().next())
                                == Some(typed)
                        });
                        if let Some(index) = found {
                            set_hot(popup, index);
                        }
                    });
                }
            }
            true
        }
        WM_MOUSEWHEEL_MESSAGE if open_now => {
            let delta = ((wparam.0 >> 16) & 0xffff) as u16 as i16 as isize;
            with_popup(combo, |popup| scroll_rows(popup, -(delta / 120) * 3));
            true
        }
        _ => false,
    }
}

unsafe fn with_popup(combo: HWND, action: impl FnOnce(&mut Popup)) {
    POPUP.with(|cell| {
        if let Ok(mut slot) = cell.try_borrow_mut() {
            if let Some(popup) = slot.as_mut().filter(|popup| popup.combo == combo) {
                action(popup);
            }
        }
    });
}
