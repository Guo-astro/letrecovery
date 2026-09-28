//! LetRecovery's own right-click menu for edit fields.
//!
//! The layout is the one of the native Windows edit menu: the system menu font, the native row and
//! separator heights, the native text inset on the left and the reserved space on the right, and
//! access keys shown as "复制(R)" (pressing the letter picks the item). Colours, outline and the
//! highlighted row stay LetRecovery's own. Only the editing commands are offered - none of the
//! native reading-order, Unicode control character or IME entries.
//!
//! Editable fields offer Undo, Cut, Copy, Paste, Delete and Select All; read-only fields Copy and
//! Select All; password fields never offer Cut or Copy. Items that cannot act right now (no
//! selection, nothing to paste, nothing to undo) are shown disabled. The menu follows the current
//! light or dark palette, is composed off-screen into one layered surface (rounded outline, no
//! shadow, nothing to flicker) and fades in briefly, paced by the compositor.
//!
//! Like a native menu it is modal while open: the keyboard (arrows, Home/End, Enter, Escape) and
//! the mouse (hover, click, a click anywhere else closes it) go to the menu until it closes.

use std::cell::RefCell;
use std::time::Instant;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, CreateFontIndirectW, CreateSolidBrush, DeleteObject, DrawTextW,
    EndPaint, FillRect, SelectObject, SetBkMode, SetTextColor, BLENDFUNCTION, DT_END_ELLIPSIS,
    DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, HDC, HFONT, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetCapture, ReleaseCapture, SetCapture, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetAncestor, GetCaretPos,
    GetCursorPos, GetMessageW, GetWindowLongPtrW, GetWindowTextLengthW, LoadCursorW,
    PostQuitMessage, RegisterClassExW, SendMessageW, ShowWindow, TranslateMessage,
    UpdateLayeredWindow, GA_ROOT, GWL_STYLE, HMENU, IDC_ARROW, MSG, NONCLIENTMETRICSW,
    SW_SHOWNOACTIVATE, ULW_ALPHA,
    WINDOW_EX_STYLE, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use super::combo_popup::{cached_mask, compose_outline, work_area, Surface};
use super::controls::rounded_control_frame_geometry;
use super::theme::Palette;
use crate::native_ui::{GetDpiForSystem, GetDpiForWindow};

const CLASS_NAME: PCWSTR = w!("LetRecovery.ContextMenu");

#[link(name = "user32")]
extern "system" {
    fn IsClipboardFormatAvailable(format: u32) -> i32;
    fn SystemParametersInfoW(
        action: u32,
        parameter: u32,
        value: *mut core::ffi::c_void,
        win_ini: u32,
    ) -> i32;
}

/// SystemParametersInfoForDpi (Windows 10 1607+), resolved at run time so the executable still
/// loads on Windows 7.
type SystemParametersInfoForDpiFn =
    unsafe extern "system" fn(u32, u32, *mut core::ffi::c_void, u32, u32) -> i32;

fn scale(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32
}

/// The native Windows 11 edit menu measured at 144 DPI (150 %), scaled to `dpi`.
fn native(value_at_144_dpi: i32, dpi: u32) -> i32 {
    ((i64::from(value_at_144_dpi) * i64::from(dpi.max(1)) + 72) / 144) as i32
}

/// Native row height (text centred in it).
const NATIVE_ROW: i32 = 35;
/// Native separator row height; the 1-pixel rule sits in its middle.
const NATIVE_SEPARATOR: i32 = 11;
/// Gap between the frame and the first row, and between the last row and the frame.
const NATIVE_VERTICAL_PAD: i32 = 1;
/// From the inner edge of the frame to the start of the item text.
const NATIVE_TEXT_LEFT: i32 = 54;
/// From the end of the longest item text to the inner edge of the frame.
const NATIVE_TEXT_RIGHT: i32 = 77;
/// Horizontal inset of a separator rule from the inner edge of the frame.
const NATIVE_RULE_INSET: i32 = 11;

/// The system menu font (what native menus use) for `dpi`. None when it cannot be created.
unsafe fn system_menu_font(dpi: u32) -> Option<HFONT> {
    const SPI_GETNONCLIENTMETRICS: u32 = 0x0029;
    let mut metrics = NONCLIENTMETRICSW {
        cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    let size = metrics.cbSize;
    let mut logfont = None;
    if let Ok(user32) = GetModuleHandleW(w!("user32.dll")) {
        if let Some(procedure) = GetProcAddress(user32, windows::core::s!("SystemParametersInfoForDpi")) {
            let function: SystemParametersInfoForDpiFn = std::mem::transmute(procedure);
            let pointer = (&mut metrics as *mut NONCLIENTMETRICSW).cast::<core::ffi::c_void>();
            if function(SPI_GETNONCLIENTMETRICS, size, pointer, 0, dpi) != 0 {
                logfont = Some(metrics.lfMenuFont);
            }
        }
    }
    if logfont.is_none() {
        // Windows 7/8: the metrics are for the system DPI; rescale the height to this monitor.
        let pointer = (&mut metrics as *mut NONCLIENTMETRICSW).cast::<core::ffi::c_void>();
        if SystemParametersInfoW(SPI_GETNONCLIENTMETRICS, size, pointer, 0) != 0 {
            let mut font = metrics.lfMenuFont;
            let system = i64::from(GetDpiForSystem().max(96));
            let height = i64::from(font.lfHeight);
            let scaled = (height.abs() * i64::from(dpi.max(96)) + system / 2) / system;
            font.lfHeight = if height < 0 { -scaled as i32 } else { scaled as i32 };
            logfont = Some(font);
        }
    }
    let font = CreateFontIndirectW(&logfont?);
    (!font.is_invalid()).then_some(font)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditCommand {
    Undo,
    Cut,
    Copy,
    Paste,
    Delete,
    SelectAll,
}

struct Item {
    label: String,
    /// Access key (virtual-key code of an upper-case letter), 0 for separators.
    key: u16,
    enabled: bool,
    separator: bool,
    command: Option<EditCommand>,
}

struct Menu {
    hwnd: HWND,
    items: Vec<Item>,
    /// Top of every item in surface coordinates, plus the bottom of the last one.
    tops: Vec<i32>,
    width: i32,
    /// Width of the frame band on every side.
    padding: i32,
    text_left: i32,
    rule_inset: i32,
    rule: i32,
    hot: Option<usize>,
    done: bool,
    chosen: Option<usize>,
    palette: Palette,
    font: HFONT,
    dpi: u32,
    surface: Surface,
    mask: std::rc::Rc<Vec<[u8; 2]>>,
    origin: POINT,
    alpha: u8,
    offset: i32,
}

thread_local! {
    static MENU: RefCell<Option<Menu>> = const { RefCell::new(None) };
}

unsafe fn register_class() -> bool {
    static REGISTERED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *REGISTERED.get_or_init(|| {
        let Ok(module) = GetModuleHandleW(None) else {
            return false;
        };
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(menu_proc),
            hInstance: HINSTANCE(module.0),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        RegisterClassExW(&class) != 0
    })
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// "撤销" + 'U' -> "撤销(U)", like the native Chinese menu. A Latin label (another UI language)
/// stays as it is; its access key still works.
fn access_label(text: String, key: char) -> String {
    if text.is_ascii() {
        text
    } else {
        format!("{text}({key})")
    }
}

fn separator_item() -> Item {
    Item {
        label: String::new(),
        key: 0,
        enabled: false,
        separator: true,
        command: None,
    }
}

unsafe fn selection(edit: HWND) -> (u32, u32) {
    let mut start = 0u32;
    let mut end = 0u32;
    let _ = SendMessageW(
        edit,
        0x00b0, // EM_GETSEL
        WPARAM(&mut start as *mut u32 as usize),
        LPARAM(&mut end as *mut u32 as isize),
    );
    (start, end)
}

/// Shows the menu for an Edit (from WM_CONTEXTMENU) and performs the chosen command.
pub(crate) unsafe fn show_for_edit(edit: HWND, lparam: LPARAM, palette: Palette) {
    const ES_READONLY: isize = 0x0800;
    const ES_PASSWORD: isize = 0x0020;
    const CF_UNICODETEXT: u32 = 13;
    if !register_class() {
        return;
    }
    let _ = SetFocus(edit);
    let style = GetWindowLongPtrW(edit, GWL_STYLE);
    let read_only = style & ES_READONLY != 0;
    let password = style & ES_PASSWORD != 0;
    let (start, end) = selection(edit);
    let has_selection = start != end;
    let has_text = GetWindowTextLengthW(edit) > 0;
    let item = |label: String, key: char, enabled: bool, command: EditCommand| Item {
        label: access_label(label, key),
        key: key as u16,
        enabled,
        separator: false,
        command: Some(command),
    };
    let mut items = Vec::new();
    if !read_only {
        let can_undo = SendMessageW(edit, 0x00c6, WPARAM(0), LPARAM(0)).0 != 0; // EM_CANUNDO
        let can_paste = IsClipboardFormatAvailable(CF_UNICODETEXT) != 0;
        items.push(item(crate::tr!("撤销"), 'U', can_undo, EditCommand::Undo));
        items.push(separator_item());
        items.push(item(crate::tr!("剪切"), 'T', has_selection && !password, EditCommand::Cut));
        items.push(item(crate::tr!("复制"), 'R', has_selection && !password, EditCommand::Copy));
        items.push(item(crate::tr!("粘贴"), 'P', can_paste, EditCommand::Paste));
        items.push(item(crate::tr!("删除"), 'D', has_selection, EditCommand::Delete));
    } else {
        items.push(item(crate::tr!("复制"), 'R', has_selection && !password, EditCommand::Copy));
    }
    items.push(separator_item());
    items.push(item(crate::tr!("全选"), 'A', has_text, EditCommand::SelectAll));
    // Mouse: at the pointer. Keyboard (Shift+F10, Menu key, lParam -1): at the caret.
    let mut at = POINT::default();
    if lparam.0 == -1 {
        let _ = GetCaretPos(&mut at);
        at.y += scale(18, GetDpiForWindow(edit).max(96));
        let _ = ClientToScreen(edit, &mut at);
    } else if lparam.0 != 0 {
        at.x = (lparam.0 & 0xffff) as u16 as i16 as i32;
        at.y = ((lparam.0 >> 16) & 0xffff) as u16 as i16 as i32;
    } else {
        let _ = GetCursorPos(&mut at);
    }
    let font = HFONT(SendMessageW(edit, 0x0031, WPARAM(0), LPARAM(0)).0 as *mut _);
    let chosen = track(edit, at, items, palette, font);
    if let Some(command) = chosen {
        let message = match command {
            EditCommand::Undo => 0x0304,  // WM_UNDO
            EditCommand::Cut => 0x0300,   // WM_CUT
            EditCommand::Copy => 0x0301,  // WM_COPY
            EditCommand::Paste => 0x0302, // WM_PASTE
            EditCommand::Delete => 0x0303, // WM_CLEAR
            EditCommand::SelectAll => {
                let _ = SendMessageW(edit, 0x00b1, WPARAM(0), LPARAM(-1)); // EM_SETSEL
                return;
            }
        };
        let _ = SendMessageW(edit, message, WPARAM(0), LPARAM(0));
    }
}

unsafe fn track(
    owner: HWND,
    at: POINT,
    items: Vec<Item>,
    palette: Palette,
    fallback_font: HFONT,
) -> Option<EditCommand> {
    let dpi = GetDpiForWindow(owner).max(96);
    let s = |value: i32| scale(value, dpi);
    // The native menu font; the field's own font only if the system font cannot be created.
    let own_font = system_menu_font(dpi);
    let font = own_font.unwrap_or(fallback_font);
    let release_font = || {
        if let Some(own) = own_font {
            let _ = DeleteObject(own);
        }
    };
    // Width: the longest label between the native insets.
    let screen = windows::Win32::Graphics::Gdi::GetDC(HWND::default());
    let previous = (!font.is_invalid()).then(|| SelectObject(screen, font));
    let measure = |text: &str| -> SIZE {
        let mut size = SIZE::default();
        if !text.is_empty() {
            let _ =
                windows::Win32::Graphics::Gdi::GetTextExtentPoint32W(screen, &wide(text), &mut size);
        }
        size
    };
    let widest_label = items.iter().map(|item| measure(&item.label).cx).max().unwrap_or(0);
    let text_line = measure("Ag\u{4e2d}").cy;
    if let Some(previous) = previous {
        let _ = SelectObject(screen, previous);
    }
    let _ = windows::Win32::Graphics::Gdi::ReleaseDC(HWND::default(), screen);
    let (radius, border) = rounded_control_frame_geometry(s(170), s(200), dpi)
        .map_or((s(5), s(1).max(1)), |geometry| (geometry.radius, geometry.side_band.max(1)));
    // Native rows (never lower than the text plus the native breathing room for a larger
    // system menu font), native separators and native insets inside LetRecovery's frame.
    let row_height = native(NATIVE_ROW, dpi).max(text_line + native(11, dpi));
    let separator_height = native(NATIVE_SEPARATOR, dpi);
    let vertical_pad = native(NATIVE_VERTICAL_PAD, dpi).max(1);
    let text_left = native(NATIVE_TEXT_LEFT, dpi);
    let padding = border;
    let width = padding * 2 + text_left + widest_label + native(NATIVE_TEXT_RIGHT, dpi);
    let mut tops = Vec::with_capacity(items.len() + 1);
    let mut y = padding + vertical_pad;
    for item in &items {
        tops.push(y);
        y += if item.separator { separator_height } else { row_height };
    }
    tops.push(y);
    let height = y + vertical_pad + padding;
    // Keep the whole menu on the monitor: open left of / above the point when needed.
    let work = work_area(owner);
    let x = if at.x + width > work.right { (at.x - width).max(work.left) } else { at.x };
    let y = if at.y + height > work.bottom { (at.y - height).max(work.top) } else { at.y };
    let origin = POINT { x, y };
    let Ok(hwnd) = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
        CLASS_NAME,
        w!(""),
        WS_POPUP,
        origin.x,
        origin.y,
        width,
        height,
        GetAncestor(owner, GA_ROOT),
        HMENU::default(),
        HINSTANCE::default(),
        None,
    ) else {
        release_font();
        return None;
    };
    let Some(surface) = Surface::new(width, height) else {
        let _ = DestroyWindow(hwnd);
        release_font();
        return None;
    };
    let mut menu = Menu {
        hwnd,
        items,
        tops,
        width,
        padding,
        text_left,
        rule_inset: native(NATIVE_RULE_INSET, dpi),
        rule: native(1, dpi).max(1),
        hot: None,
        done: false,
        chosen: None,
        palette,
        font,
        dpi,
        surface,
        mask: cached_mask(width, height, radius, border),
        origin,
        alpha: 0,
        offset: -s(4),
    };
    render(&mut menu);
    present(&menu);
    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    // A short fade-in, one frame per display refresh.
    let composited = windows::Win32::Graphics::Dwm::DwmIsCompositionEnabled()
        .map(|enabled| enabled.as_bool())
        .unwrap_or(false);
    let started = Instant::now();
    for _ in 0..120 {
        let progress = (started.elapsed().as_secs_f32() / 0.09).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - progress).powi(3);
        menu.alpha = (255.0 * eased).round().clamp(0.0, 255.0) as u8;
        menu.offset = (-(scale(4, dpi) as f32) * (1.0 - eased)).round() as i32;
        present(&menu);
        if progress >= 1.0 {
            break;
        }
        if !composited || windows::Win32::Graphics::Dwm::DwmFlush().is_err() {
            std::thread::sleep(std::time::Duration::from_millis(8));
        }
    }
    menu.alpha = 255;
    menu.offset = 0;
    present(&menu);
    MENU.with(|cell| *cell.borrow_mut() = Some(menu));
    let _ = SetCapture(hwnd);
    // Modal loop, like TrackPopupMenu: keys are handled here, the mouse by the menu window.
    let mut message = MSG::default();
    loop {
        let finished = MENU.with(|cell| cell.borrow().as_ref().map_or(true, |menu| menu.done));
        if finished {
            break;
        }
        if !GetMessageW(&mut message, None, 0, 0).as_bool() {
            PostQuitMessage(message.wParam.0 as i32);
            break;
        }
        match message.message {
            0x0100 => {
                // WM_KEYDOWN
                handle_key(message.wParam.0 as u16);
                continue;
            }
            0x0102 | 0x0101 => continue, // WM_CHAR, WM_KEYUP
            0x0104 | 0x0105 => {
                // WM_SYSKEYDOWN / WM_SYSKEYUP (Alt): closes the menu like a native one.
                close_menu(None);
                continue;
            }
            _ => {}
        }
        let _ = TranslateMessage(&message);
        DispatchMessageW(&message);
    }
    let menu = MENU.with(|cell| cell.borrow_mut().take());
    let Some(mut menu) = menu else {
        release_font();
        return None;
    };
    if GetCapture() == menu.hwnd {
        let _ = ReleaseCapture();
    }
    let _ = DestroyWindow(menu.hwnd);
    menu.surface.release();
    release_font();
    let chosen = menu.chosen?;
    menu.items.get(chosen).filter(|item| item.enabled).and_then(|item| item.command)
}

unsafe fn close_menu(chosen: Option<usize>) {
    MENU.with(|cell| {
        if let Ok(mut slot) = cell.try_borrow_mut() {
            if let Some(menu) = slot.as_mut() {
                menu.done = true;
                menu.chosen = chosen;
            }
        }
    });
    // Wake the modal loop if it is waiting in GetMessage.
    let _ = windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
        windows::Win32::System::Threading::GetCurrentThreadId(),
        0,
        WPARAM(0),
        LPARAM(0),
    );
}

fn selectable(menu: &Menu, index: usize) -> bool {
    menu.items
        .get(index)
        .is_some_and(|item| !item.separator && item.enabled)
}

unsafe fn handle_key(key: u16) {
    const VK_RETURN: u16 = 0x0d;
    const VK_ESCAPE: u16 = 0x1b;
    const VK_HOME: u16 = 0x24;
    const VK_END: u16 = 0x23;
    const VK_UP: u16 = 0x26;
    const VK_DOWN: u16 = 0x28;
    let mut choose = None;
    let mut cancel = false;
    MENU.with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return;
        };
        let Some(menu) = slot.as_mut() else {
            return;
        };
        let count = menu.items.len();
        let step = |menu: &Menu, from: Option<usize>, forward: bool| -> Option<usize> {
            let mut index = from;
            for _ in 0..count {
                let next = match index {
                    None => {
                        if forward {
                            0
                        } else {
                            count - 1
                        }
                    }
                    Some(current) => {
                        if forward {
                            (current + 1) % count
                        } else {
                            (current + count - 1) % count
                        }
                    }
                };
                if selectable(menu, next) {
                    return Some(next);
                }
                index = Some(next);
            }
            None
        };
        match key {
            VK_DOWN => menu.hot = step(menu, menu.hot, true),
            VK_UP => menu.hot = step(menu, menu.hot, false),
            VK_HOME => menu.hot = step(menu, None, true),
            VK_END => menu.hot = step(menu, None, false),
            VK_RETURN => {
                if let Some(hot) = menu.hot.filter(|hot| selectable(menu, *hot)) {
                    choose = Some(hot);
                }
            }
            VK_ESCAPE => cancel = true,
            key @ 0x41..=0x5a => {
                // Access key, as in a native menu: the letter picks its item at once.
                if let Some(index) = menu
                    .items
                    .iter()
                    .position(|item| item.key == key && !item.separator && item.enabled)
                {
                    choose = Some(index);
                }
            }
            _ => {}
        }
        render(menu);
        present(menu);
    });
    if let Some(index) = choose {
        close_menu(Some(index));
    } else if cancel {
        close_menu(None);
    }
}

fn item_at(menu: &Menu, x: i32, y: i32) -> Option<usize> {
    if x < 0 || x >= menu.width {
        return None;
    }
    (0..menu.items.len()).find(|index| y >= menu.tops[*index] && y < menu.tops[index + 1])
}

unsafe extern "system" fn menu_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    const WM_MOUSEMOVE: u32 = 0x0200;
    const WM_LBUTTONDOWN: u32 = 0x0201;
    const WM_LBUTTONUP: u32 = 0x0202;
    const WM_RBUTTONDOWN: u32 = 0x0204;
    const WM_RBUTTONUP: u32 = 0x0205;
    const WM_CAPTURECHANGED: u32 = 0x0215;
    match message {
        0x0021 => LRESULT(3), // WM_MOUSEACTIVATE -> MA_NOACTIVATE
        0x0014 => LRESULT(1), // WM_ERASEBKGND
        0x000f => {
            let mut paint = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut paint);
            let _ = EndPaint(hwnd, &paint);
            LRESULT(0)
        }
        WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP => {
            let x = (lparam.0 & 0xffff) as u16 as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xffff) as u16 as i16 as i32;
            let mut result: Option<Option<usize>> = None;
            MENU.with(|cell| {
                let Ok(mut slot) = cell.try_borrow_mut() else {
                    return;
                };
                let Some(menu) = slot.as_mut().filter(|menu| menu.hwnd == hwnd) else {
                    return;
                };
                let inside = x >= 0 && y >= 0 && x < menu.surface.width && y < menu.surface.height;
                let item = item_at(menu, x, y);
                match message {
                    WM_MOUSEMOVE => {
                        let hot = item.filter(|index| selectable(menu, *index));
                        if hot != menu.hot {
                            menu.hot = hot;
                            render(menu);
                            present(menu);
                        }
                    }
                    WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
                        if !inside {
                            result = Some(None);
                        }
                    }
                    _ => {
                        if let Some(index) = item.filter(|index| selectable(menu, *index)) {
                            result = Some(Some(index));
                        }
                    }
                }
            });
            if let Some(chosen) = result {
                close_menu(chosen);
            }
            LRESULT(0)
        }
        WM_CAPTURECHANGED if lparam.0 != hwnd.0 as isize => {
            close_menu(None);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

unsafe fn fill(dc: HDC, rect: RECT, color: COLORREF) {
    let brush = CreateSolidBrush(color);
    let _ = FillRect(dc, &rect, brush);
    let _ = DeleteObject(brush);
}

unsafe fn render(menu: &mut Menu) {
    let dc = menu.surface.dc;
    let palette = menu.palette;
    fill(
        dc,
        RECT {
            left: 0,
            top: 0,
            right: menu.surface.width,
            bottom: menu.surface.height,
        },
        palette.edit,
    );
    let previous = (!menu.font.is_invalid()).then(|| SelectObject(dc, menu.font));
    let _ = SetBkMode(dc, TRANSPARENT);
    for (index, item) in menu.items.iter().enumerate() {
        let top = menu.tops[index];
        let bottom = menu.tops[index + 1];
        if item.separator {
            let middle = top + (bottom - top - menu.rule) / 2;
            fill(
                dc,
                RECT {
                    left: menu.padding + menu.rule_inset,
                    top: middle,
                    right: menu.width - menu.padding - menu.rule_inset,
                    bottom: middle + menu.rule,
                },
                palette.separator,
            );
            continue;
        }
        let row = RECT {
            left: menu.padding,
            top,
            right: menu.width - menu.padding,
            bottom,
        };
        let highlighted = menu.hot == Some(index) && item.enabled;
        let (text_color, background) = if highlighted {
            super::theme::list_selection_colors(palette, false)
        } else if item.enabled {
            (palette.text, palette.edit)
        } else {
            (palette.text_disabled, palette.edit)
        };
        if highlighted {
            fill(dc, row, background);
        }
        let _ = SetTextColor(dc, text_color);
        let mut label = wide(&item.label);
        let mut label_rect = RECT {
            left: row.left + menu.text_left,
            right: row.right,
            ..row
        };
        let _ = DrawTextW(
            dc,
            &mut label,
            &mut label_rect,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
    }
    if let Some(previous) = previous {
        let _ = SelectObject(dc, previous);
    }
    compose_outline(&menu.surface, &menu.mask, palette.border);
}

unsafe fn present(menu: &Menu) {
    let position = POINT {
        x: menu.origin.x,
        y: menu.origin.y + menu.offset,
    };
    let size = SIZE {
        cx: menu.surface.width,
        cy: menu.surface.height,
    };
    let source = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: 0,
        BlendFlags: 0,
        SourceConstantAlpha: menu.alpha,
        AlphaFormat: 1,
    };
    let _ = UpdateLayeredWindow(
        menu.hwnd,
        HDC::default(),
        Some(&position),
        Some(&size),
        menu.surface.dc,
        Some(&source),
        COLORREF(0),
        Some(&blend),
        ULW_ALPHA,
    );
}

#[allow(dead_code)]
fn window_ex_style_is_menu(style: WINDOW_EX_STYLE) -> bool {
    style.0 & WS_EX_TOOLWINDOW.0 != 0
}
