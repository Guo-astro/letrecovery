//! Text fit audit for every language.
//!
//! With the environment variable `LETRECOVERY_UI_AUDIT=1` the main window walks through all of
//! its pages after start-up, and every tool window audits itself when it is shown. For each
//! visible label, button, check box, combo box, list header and list cell the text is measured
//! with the control's own font and compared with the room the control actually has. Everything
//! that would be cut off, end in "..." or lose a wrapped line is written to the log with the
//! prefix "[UI 文本检查]" (surface, control class, text, needed and available pixels), so a
//! translation can be checked on a real machine by opening the program once per language.

use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    DrawTextW, GetDC, GetTextExtentPoint32W, ReleaseDC, SelectObject, DT_CALCRECT, DT_NOPREFIX,
    DT_WORDBREAK, HDC, HGDIOBJ,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetClassNameW, GetClientRect, GetWindowLongPtrW, GetWindowTextLengthW,
    GetWindowTextW, IsWindowVisible, SendMessageW, GWL_STYLE,
};

use crate::native_ui::GetDpiForWindow;

pub(crate) fn enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        let on = std::env::var("LETRECOVERY_UI_AUDIT")
            .map(|value| {
                let value = value.trim().to_ascii_lowercase();
                !value.is_empty() && value != "0" && value != "false" && value != "off"
            })
            .unwrap_or(false);
        if on {
            log::info!(
                "[UI 文本检查] 已开启：语言 {}",
                crate::utils::i18n::current_language()
            );
        }
        on
    })
}

fn scale(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32
}

unsafe fn class_name(hwnd: HWND) -> String {
    let mut buffer = [0u16; 64];
    let length = GetClassNameW(hwnd, &mut buffer).max(0) as usize;
    String::from_utf16_lossy(&buffer[..length])
}

unsafe fn window_text(hwnd: HWND) -> Vec<u16> {
    let length = GetWindowTextLengthW(hwnd).max(0) as usize;
    if length == 0 {
        return Vec::new();
    }
    let mut buffer = vec![0u16; length + 1];
    let copied = GetWindowTextW(hwnd, &mut buffer).max(0) as usize;
    buffer.truncate(copied);
    buffer
}

/// A DC for measuring with the control's font.
struct Measure {
    hwnd: HWND,
    dc: HDC,
    previous: Option<HGDIOBJ>,
}

impl Measure {
    unsafe fn new(hwnd: HWND, font_owner: HWND) -> Option<Self> {
        let dc = GetDC(hwnd);
        if dc.is_invalid() {
            return None;
        }
        let font = SendMessageW(font_owner, 0x0031, WPARAM(0), LPARAM(0)); // WM_GETFONT
        let previous = (font.0 != 0).then(|| SelectObject(dc, HGDIOBJ(font.0 as *mut _)));
        Some(Self { hwnd, dc, previous })
    }

    unsafe fn width(&self, text: &[u16]) -> i32 {
        let mut size = SIZE::default();
        let _ = GetTextExtentPoint32W(self.dc, text, &mut size);
        size.cx
    }

    /// Height of the text wrapped at `width` (and the widest line, for unbreakable words).
    unsafe fn wrapped(&self, text: &[u16], width: i32) -> (i32, i32) {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: width.max(1),
            bottom: 0,
        };
        let mut copy = text.to_vec();
        let _ = DrawTextW(
            self.dc,
            &mut copy,
            &mut rect,
            DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
        );
        (rect.bottom - rect.top, rect.right - rect.left)
    }
}

impl Drop for Measure {
    fn drop(&mut self) {
        unsafe {
            if let Some(previous) = self.previous {
                let _ = SelectObject(self.dc, previous);
            }
            let _ = ReleaseDC(self.hwnd, self.dc);
        }
    }
}

fn shown(text: &[u16]) -> String {
    String::from_utf16_lossy(text)
        .replace('\r', " ")
        .replace('\n', " ")
        .replace('&', "")
}

unsafe fn audit_static(hwnd: HWND, findings: &mut Vec<String>) {
    let text = window_text(hwnd);
    if text.is_empty() {
        return;
    }
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    let (width, height) = (client.right - client.left, client.bottom - client.top);
    if width <= 0 || height <= 0 {
        return;
    }
    let Some(measure) = Measure::new(hwnd, hwnd) else {
        return;
    };
    let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
    let kind = style & 0x1f;
    // SS_LEFTNOWORDWRAP (0x0C) and SS_SIMPLE (0x0B) never wrap; with SS_ENDELLIPSIS/PATHELLIPSIS
    // (0xC000) a static also stays on one line.
    let single_line = kind == 0x0c || kind == 0x0b || style & 0xc000 != 0;
    let line = measure.width(&text);
    if single_line || !text.contains(&0x000a) && line <= width {
        if line > width {
            findings.push(format!(
                "Static \"{}\" 一行需要 {line}px，只有 {width}px（会被截断）",
                shown(&text)
            ));
        }
        return;
    }
    let (needed, widest) = measure.wrapped(&text, width);
    if needed > height + 1 {
        findings.push(format!(
            "Static \"{}\" 换行后需要 {needed}px 高，只有 {height}px（下面几行看不见）",
            shown(&text)
        ));
    } else if widest > width {
        findings.push(format!(
            "Static \"{}\" 有一段无法换行，需要 {widest}px 宽，只有 {width}px",
            shown(&text)
        ));
    }
}

unsafe fn audit_button(hwnd: HWND, findings: &mut Vec<String>) {
    let text = window_text(hwnd);
    if text.is_empty() {
        return;
    }
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    let (width, height) = (client.right - client.left, client.bottom - client.top);
    if width <= 0 || height <= 0 {
        return;
    }
    let dpi = GetDpiForWindow(hwnd).max(96);
    let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
    let kind = style & 0x0f;
    let check_like = matches!(kind, 2 | 3 | 4 | 5 | 6 | 9);
    if kind == 7 {
        return; // group box
    }
    let Some(measure) = Measure::new(hwnd, hwnd) else {
        return;
    };
    // Push buttons draw their caption centred across the whole button (ellipsis only beyond
    // that); anything closer than 4 px to either edge already looks cramped and is reported.
    let available = if check_like {
        width - scale(13, dpi) - scale(8, dpi)
    } else {
        width - scale(8, dpi)
    };
    let needed = measure.width(&text);
    if style & 0x2000 != 0 {
        // BS_MULTILINE
        let (lines, _) = measure.wrapped(&text, available);
        if lines > height {
            findings.push(format!(
                "{} \"{}\" 换行后需要 {lines}px 高，只有 {height}px",
                if check_like { "CheckBox" } else { "Button" },
                shown(&text)
            ));
        }
        return;
    }
    if needed > available {
        findings.push(format!(
            "{} \"{}\" 需要 {needed}px，只有 {}px（会显示成省略号）",
            if check_like { "CheckBox" } else { "Button" },
            shown(&text),
            available.max(0)
        ));
    }
}

unsafe fn audit_combo(hwnd: HWND, findings: &mut Vec<String>) {
    const CB_GETCURSEL: u32 = 0x0147;
    const CB_GETLBTEXTLEN: u32 = 0x0149;
    const CB_GETLBTEXT: u32 = 0x0148;
    const CB_GETCOUNT: u32 = 0x0146;
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    let width = client.right - client.left;
    if width <= 0 {
        return;
    }
    let dpi = GetDpiForWindow(hwnd).max(96);
    let Some(measure) = Measure::new(hwnd, hwnd) else {
        return;
    };
    let available = width - scale(34, dpi);
    let count = SendMessageW(hwnd, CB_GETCOUNT, WPARAM(0), LPARAM(0)).0.max(0) as usize;
    let selected = SendMessageW(hwnd, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
    for index in 0..count.min(64) {
        let length = SendMessageW(hwnd, CB_GETLBTEXTLEN, WPARAM(index), LPARAM(0)).0;
        if length <= 0 {
            continue;
        }
        let mut text = vec![0u16; length as usize + 1];
        let copied = SendMessageW(
            hwnd,
            CB_GETLBTEXT,
            WPARAM(index),
            LPARAM(text.as_mut_ptr() as isize),
        )
        .0
        .clamp(0, length) as usize;
        text.truncate(copied);
        let needed = measure.width(&text);
        if needed > available {
            findings.push(format!(
                "ComboBox 选项{} \"{}\" 需要 {needed}px，只有 {}px",
                if index as isize == selected { "(当前)" } else { "" },
                shown(&text),
                available.max(0)
            ));
        }
    }
}

unsafe fn audit_list_view(hwnd: HWND, findings: &mut Vec<String>) {
    use windows::Win32::UI::Controls::{HDITEMW, HDI_TEXT, LVITEMW};
    const LVM_GETHEADER: u32 = 0x101f;
    const LVM_GETITEMCOUNT: u32 = 0x1004;
    const LVM_GETCOLUMNWIDTH: u32 = 0x101d;
    const LVM_GETITEMTEXTW: u32 = 0x1073;
    const HDM_GETITEMCOUNT: u32 = 0x1200;
    const HDM_GETITEMW: u32 = 0x120b;
    let dpi = GetDpiForWindow(hwnd).max(96);
    let header = HWND(SendMessageW(hwnd, LVM_GETHEADER, WPARAM(0), LPARAM(0)).0 as *mut _);
    let columns = if header.is_invalid() {
        1
    } else {
        SendMessageW(header, HDM_GETITEMCOUNT, WPARAM(0), LPARAM(0))
            .0
            .max(1) as i32
    };
    let rows = SendMessageW(hwnd, LVM_GETITEMCOUNT, WPARAM(0), LPARAM(0))
        .0
        .clamp(0, 400) as usize;
    let Some(measure) = Measure::new(hwnd, hwnd) else {
        return;
    };
    let header_measure = if header.is_invalid() || !IsWindowVisible(header).as_bool() {
        None
    } else {
        Measure::new(header, header)
    };
    for column in 0..columns {
        let width = SendMessageW(hwnd, LVM_GETCOLUMNWIDTH, WPARAM(column as usize), LPARAM(0))
            .0 as i32;
        if width <= 0 {
            continue;
        }
        if let Some(header_measure) = &header_measure {
            let mut text = vec![0u16; 256];
            let mut item = HDITEMW {
                mask: HDI_TEXT,
                pszText: windows::core::PWSTR(text.as_mut_ptr()),
                cchTextMax: text.len() as i32,
                ..Default::default()
            };
            if SendMessageW(
                header,
                HDM_GETITEMW,
                WPARAM(column as usize),
                LPARAM((&mut item as *mut HDITEMW) as isize),
            )
            .0 != 0
            {
                let length = text.iter().position(|unit| *unit == 0).unwrap_or(0);
                text.truncate(length);
                let needed = header_measure.width(&text) + scale(14, dpi);
                if !text.is_empty() && needed > width {
                    findings.push(format!(
                        "列表列标题 \"{}\" 需要 {needed}px，列宽 {width}px",
                        shown(&text)
                    ));
                }
            }
        }
        let mut cut = 0usize;
        let mut sample = String::new();
        let mut widest = 0;
        for row in 0..rows {
            let mut text = vec![0u16; 512];
            let mut item = LVITEMW {
                iSubItem: column,
                pszText: windows::core::PWSTR(text.as_mut_ptr()),
                cchTextMax: text.len() as i32,
                ..Default::default()
            };
            let length = SendMessageW(
                hwnd,
                LVM_GETITEMTEXTW,
                WPARAM(row),
                LPARAM((&mut item as *mut LVITEMW) as isize),
            )
            .0
            .clamp(0, 511) as usize;
            text.truncate(length);
            if text.is_empty() {
                continue;
            }
            let needed = measure.width(&text) + scale(14, dpi);
            if needed > width {
                cut += 1;
                if needed > widest {
                    widest = needed;
                    sample = shown(&text);
                }
            }
        }
        if cut > 0 {
            findings.push(format!(
                "列表第 {} 列有 {cut} 个单元格显示不全，例如 \"{sample}\" 需要 {widest}px，列宽 {width}px",
                column + 1
            ));
        }
    }
}

struct Visit {
    findings: Vec<String>,
}

unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let context = &mut *(lparam.0 as *mut Visit);
    if !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1);
    }
    let class = class_name(hwnd);
    let before = context.findings.len();
    match class.to_ascii_lowercase().as_str() {
        "static" => audit_static(hwnd, &mut context.findings),
        "button" => audit_button(hwnd, &mut context.findings),
        "combobox" => audit_combo(hwnd, &mut context.findings),
        "syslistview32" => audit_list_view(hwnd, &mut context.findings),
        _ => {}
    }
    for finding in &mut context.findings[before..] {
        finding.insert_str(0, &format!("[{:?}] ", hwnd.0));
    }
    BOOL(1)
}

/// Audits every visible descendant of `root` and logs what does not fit.
pub(crate) unsafe fn audit_surface(root: HWND, surface: &str) {
    if std::env::var_os("LETRECOVERY_UI_AUDIT_DUMP").is_some() {
        unsafe extern "system" fn dump(window: HWND, _lparam: LPARAM) -> windows::Win32::Foundation::BOOL {
            let mut class = [0u16; 32];
            let class_length = windows::Win32::UI::WindowsAndMessaging::GetClassNameW(window, &mut class).max(0) as usize;
            let mut text = [0u16; 64];
            let text_length = windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(window, &mut text).max(0) as usize;
            let mut rect = windows::Win32::Foundation::RECT::default();
            let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(window, &mut rect);
            log::info!(
                "[UI 控件] {:?} {} \"{}\" ({},{})-({},{}) 可见={} 启用={}",
                window.0,
                String::from_utf16_lossy(&class[..class_length]),
                String::from_utf16_lossy(&text[..text_length]),
                rect.left, rect.top, rect.right, rect.bottom,
                windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(window).as_bool(),
                windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(window).as_bool(),
            );
            windows::Win32::Foundation::BOOL(1)
        }
        let _ = windows::Win32::UI::WindowsAndMessaging::EnumChildWindows(root, Some(dump), LPARAM(0));
    }
    if !enabled() {
        return;
    }
    let mut context = Visit {
        findings: Vec::new(),
    };
    let _ = EnumChildWindows(
        root,
        Some(visit),
        LPARAM(&mut context as *mut Visit as isize),
    );
    log::info!(
        "[UI 文本检查] {surface}：{} 处可能显示不全",
        context.findings.len()
    );
    for finding in context.findings {
        log::info!("[UI 文本检查] {surface}：{finding}");
    }
}
