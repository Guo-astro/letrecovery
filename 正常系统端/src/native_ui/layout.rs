//! Shared responsive layout primitives for the native Win32 frontend.
//!
//! This module deliberately owns measurements and geometry only.  It never creates controls or
//! performs business actions, so tool dialogs can share one spacing and text-measurement contract
//! without becoming coupled to each other's state.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    DrawTextW, GetDC, ReleaseDC, SelectObject, DT_CALCRECT, DT_NOPREFIX, DT_SINGLELINE,
    DT_WORDBREAK, HFONT,
};

use super::controls::InnoMetrics;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextSize {
    pub width: i32,
    pub height: i32,
}

/// One DPI-scaled spacing contract for pages and tool dialogs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutMetrics {
    pub outer_margin: i32,
    pub tight_gap: i32,
    pub control_gap: i32,
    pub section_gap: i32,
    pub label_height: i32,
    pub field_height: i32,
    pub button_height: i32,
    pub list_row_height: i32,
    pub command_margin: i32,
    pub command_height: i32,
}

impl LayoutMetrics {
    pub fn for_dpi(dpi: u32) -> Self {
        let inno = InnoMetrics::for_dpi(dpi);
        Self {
            outer_margin: scale(28, dpi),
            tight_gap: scale(4, dpi),
            control_gap: inno.control_gap,
            section_gap: scale(16, dpi),
            label_height: scale(20, dpi),
            field_height: inno.field_height,
            button_height: inno.button_height,
            list_row_height: inno.list_item_height,
            command_margin: scale(12, dpi),
            command_height: scale(46, dpi),
        }
    }
}

/// Measures with the exact font installed on the target dialog.  Wrapped text receives a real
/// maximum width; no language-specific character-count estimate is used.
pub unsafe fn measure_text(
    hwnd: HWND,
    font: HFONT,
    text: &str,
    maximum_width: Option<i32>,
) -> TextSize {
    if text.is_empty() {
        return TextSize::default();
    }
    let _profile = super::redraw::profile_scope("测量文字");
    // Layout runs on every resize step and page switch and asks for the same captions again and
    // again. Each former call opened a display DC for the window (GetDC), selected the font and
    // measured. The result depends only on the font description, the text and the wrap width, so
    // it is measured once in a private memory DC and remembered for the rest of the session.
    let key = MeasureKey {
        font: font_identity(font),
        maximum_width: maximum_width.map(|value| value.max(0)),
        text: text.to_owned(),
    };
    if let Some(size) = MEASURE_CACHE.with(|cache| cache.borrow().get(&key).copied()) {
        return size;
    }
    let (dc, owned_window_dc) = match measure_dc() {
        Some(dc) => (dc, false),
        None => (GetDC(hwnd), true),
    };
    if dc.is_invalid() {
        return TextSize::default();
    }
    let old_font = SelectObject(dc, font);
    let mut wide = text.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let mut bounds = RECT {
        right: maximum_width.unwrap_or_default().max(0),
        ..Default::default()
    };
    let flags = if maximum_width.is_some() {
        DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX
    } else {
        DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX
    };
    let measured = DrawTextW(dc, &mut wide, &mut bounds, flags) != 0;
    let _ = SelectObject(dc, old_font);
    if owned_window_dc {
        let _ = ReleaseDC(hwnd, dc);
    }
    let size = TextSize {
        width: (bounds.right - bounds.left).max(0),
        height: (bounds.bottom - bounds.top).max(0),
    };
    if measured {
        MEASURE_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.len() >= MEASURE_CACHE_LIMIT {
                cache.clear();
            }
            cache.insert(key, size);
        });
    }
    size
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct MeasureKey {
    font: u64,
    maximum_width: Option<i32>,
    text: String,
}

const MEASURE_CACHE_LIMIT: usize = 4096;

thread_local! {
    static MEASURE_CACHE: std::cell::RefCell<std::collections::HashMap<MeasureKey, TextSize>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    static MEASURE_DC: std::cell::Cell<Option<windows::Win32::Graphics::Gdi::HDC>> =
        const { std::cell::Cell::new(None) };
}

/// A screen-compatible memory DC kept for text measurement on the UI thread. Measuring there is
/// identical to measuring on a window DC (same device, same pixel-height font) without asking the
/// window manager for a display DC every time.
unsafe fn measure_dc() -> Option<windows::Win32::Graphics::Gdi::HDC> {
    MEASURE_DC.with(|cell| {
        if let Some(dc) = cell.get() {
            return Some(dc);
        }
        let dc = windows::Win32::Graphics::Gdi::CreateCompatibleDC(
            windows::Win32::Graphics::Gdi::HDC::default(),
        );
        if dc.is_invalid() {
            return None;
        }
        cell.set(Some(dc));
        Some(dc)
    })
}

/// Identifies a font by its full description rather than its handle: fonts are recreated on DPI
/// and language changes, and GDI may hand a released handle value to a different font later.
unsafe fn font_identity(font: HFONT) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut description = windows::Win32::Graphics::Gdi::LOGFONTW::default();
    let copied = windows::Win32::Graphics::Gdi::GetObjectW(
        font,
        std::mem::size_of::<windows::Win32::Graphics::Gdi::LOGFONTW>() as i32,
        Some((&mut description as *mut windows::Win32::Graphics::Gdi::LOGFONTW).cast()),
    );
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    if copied > 0 {
        description.lfHeight.hash(&mut hasher);
        description.lfWidth.hash(&mut hasher);
        description.lfEscapement.hash(&mut hasher);
        description.lfOrientation.hash(&mut hasher);
        description.lfWeight.hash(&mut hasher);
        description.lfItalic.hash(&mut hasher);
        description.lfUnderline.hash(&mut hasher);
        description.lfStrikeOut.hash(&mut hasher);
        description.lfCharSet.0.hash(&mut hasher);
        description.lfQuality.0.hash(&mut hasher);
        description.lfFaceName.hash(&mut hasher);
    } else {
        // Stock or invalid font: its handle is stable for the whole session.
        (font.0 as usize).hash(&mut hasher);
        u8::MAX.hash(&mut hasher);
    }
    hasher.finish()
}

/// A capacity given in MB, for display: below 1 GB in whole MB (a 16 MB MSR partition used to read
/// "0.0 GB"), from 1 GB on in GB with one decimal.
pub fn format_capacity_mb(value_mb: u64) -> String {
    if value_mb < 1024 {
        format!("{value_mb} MB")
    } else {
        format!("{:.1} GB", value_mb as f64 / 1024.0)
    }
}

/// The same for a capacity given in GB.
pub fn format_capacity_gb(value_gb: f64) -> String {
    let value_gb = value_gb.max(0.0);
    if value_gb < 1.0 {
        format!("{:.0} MB", value_gb * 1024.0)
    } else {
        format!("{value_gb:.1} GB")
    }
}

/// The caption of a control and its font, measured: what a button, label or check box needs.
pub unsafe fn control_text_width(control: HWND) -> i32 {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextLengthW, GetWindowTextW, SendMessageW};
    let length = GetWindowTextLengthW(control).max(0) as usize;
    if length == 0 {
        return 0;
    }
    let mut buffer = vec![0u16; length + 1];
    let copied = GetWindowTextW(control, &mut buffer).max(0) as usize;
    let text = String::from_utf16_lossy(&buffer[..copied]).replace('&', "");
    let font = SendMessageW(control, 0x0031, windows::Win32::Foundation::WPARAM(0), windows::Win32::Foundation::LPARAM(0));
    measure_text(control, HFONT(font.0 as *mut _), &text, None).width
}

/// Height the caption of `control` needs when wrapped at `width`.
pub unsafe fn control_wrapped_height(control: HWND, width: i32) -> i32 {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextLengthW, GetWindowTextW, SendMessageW};
    let length = GetWindowTextLengthW(control).max(0) as usize;
    if length == 0 || width <= 0 {
        return 0;
    }
    let mut buffer = vec![0u16; length + 1];
    let copied = GetWindowTextW(control, &mut buffer).max(0) as usize;
    let text = String::from_utf16_lossy(&buffer[..copied]);
    let font = SendMessageW(control, 0x0031, windows::Win32::Foundation::WPARAM(0), windows::Win32::Foundation::LPARAM(0));
    measure_text(control, HFONT(font.0 as *mut _), &text, Some(width)).height
}

/// Width a closed drop-down needs for its longest item: the text, its margins and the chevron.
pub unsafe fn combo_fitted_width(combo: HWND, dpi: u32, minimum: i32) -> i32 {
    use windows::Win32::UI::WindowsAndMessaging::SendMessageW;
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    let font = HFONT(SendMessageW(combo, 0x0031, WPARAM(0), LPARAM(0)).0 as *mut _);
    let count = SendMessageW(combo, 0x0146, WPARAM(0), LPARAM(0)).0.clamp(0, 256) as usize;
    let mut widest = 0;
    for index in 0..count {
        let length = SendMessageW(combo, 0x0149, WPARAM(index), LPARAM(0)).0;
        if length <= 0 {
            continue;
        }
        let mut buffer = vec![0u16; length as usize + 1];
        let copied = SendMessageW(combo, 0x0148, WPARAM(index), LPARAM(buffer.as_mut_ptr() as isize))
            .0
            .clamp(0, length) as usize;
        let text = String::from_utf16_lossy(&buffer[..copied]);
        widest = widest.max(measure_text(combo, font, &text, None).width);
    }
    (widest + scale(38, dpi)).max(minimum)
}

/// Width a push button needs for its own caption (caption plus the usual inner margins).
pub unsafe fn fitted_button_width(button: HWND, dpi: u32, minimum: i32) -> i32 {
    (control_text_width(button) + scale(24, dpi)).max(minimum)
}

pub unsafe fn measured_button_width(
    hwnd: HWND,
    font: HFONT,
    text: &str,
    dpi: u32,
    minimum: i32,
) -> i32 {
    let text_width = measure_text(hwnd, font, text, None).width;
    (text_width + scale(24, dpi)).max(minimum)
}

/// A list follows its inventory rather than consuming every unused pixel.  Empty inventories keep
/// a small usable body, ordinary inventories expose their rows, and larger inventories scroll.
pub fn preferred_list_height(
    item_count: usize,
    dpi: u32,
    minimum_rows: usize,
    maximum_rows: usize,
) -> i32 {
    let metrics = LayoutMetrics::for_dpi(dpi);
    let rows = item_count.clamp(minimum_rows.max(1), maximum_rows.max(minimum_rows.max(1)));
    // One header row plus a one-pixel logical frame at both edges.
    metrics.list_row_height * (rows as i32 + 1) + scale(2, dpi)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FieldArrangement {
    Inline {
        label_width: i32,
        control_x: i32,
        control_width: i32,
    },
    #[default]
    Stacked,
}

/// Keeps a label and field inline only while the measured label and useful field width both fit.
/// This naturally responds to long English text without a hard-coded language breakpoint.
pub fn arrange_field(
    available_width: i32,
    measured_label_width: i32,
    minimum_control_width: i32,
    dpi: u32,
) -> FieldArrangement {
    let gap = LayoutMetrics::for_dpi(dpi).control_gap;
    let label_width = measured_label_width.max(0);
    let control_x = label_width + gap;
    let control_width = available_width - control_x;
    if control_width >= minimum_control_width {
        FieldArrangement::Inline {
            label_width,
            control_x,
            control_width,
        }
    } else {
        FieldArrangement::Stacked
    }
}

pub fn scale(value: i32, dpi: u32) -> i32 {
    ((value as i64 * dpi.max(1) as i64 + 48) / 96) as i32
}

/// Centers an item of `item_height` inside a logical row without language- or DPI-specific
/// offsets. All values are already physical pixels.
pub fn centered_control_y(row_top: i32, row_height: i32, item_height: i32) -> i32 {
    row_top + (row_height.saturating_sub(item_height).max(0) / 2)
}

/// Centers an item while assigning an odd spare pixel to the top edge.
///
/// USER32 single-line fields and GDI text both otherwise leave that pixel below the item, which
/// makes a 23px field look one pixel higher than the neighbouring 24px control on 96-DPI layouts.
/// Keep this opt-in for mixed-height field rows instead of changing the geometry of every control.
pub fn centered_control_y_ceil(row_top: i32, row_height: i32, item_height: i32) -> i32 {
    let spare = row_height.saturating_sub(item_height).max(0);
    row_top + (spare + 1) / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_scale_once_and_keep_one_spacing_contract() {
        let normal = LayoutMetrics::for_dpi(96);
        let high = LayoutMetrics::for_dpi(192);
        assert_eq!(normal.control_gap, 8);
        assert_eq!(normal.section_gap, 16);
        assert_eq!(high.control_gap, 16);
        assert_eq!(high.section_gap, 32);
        assert_eq!(high.field_height, normal.field_height * 2);
    }

    #[test]
    fn long_labels_stack_instead_of_squeezing_the_field() {
        assert!(matches!(
            arrange_field(500, 100, 240, 96),
            FieldArrangement::Inline { .. }
        ));
        assert_eq!(arrange_field(360, 180, 240, 96), FieldArrangement::Stacked);
    }

    #[test]
    fn list_height_tracks_inventory_with_bounded_density() {
        assert_eq!(preferred_list_height(0, 96, 3, 8), 90);
        assert_eq!(preferred_list_height(5, 96, 3, 8), 134);
        assert_eq!(preferred_list_height(80, 96, 3, 8), 200);
    }

    #[test]
    fn mixed_height_field_rows_put_the_odd_pixel_above_the_field() {
        assert_eq!(centered_control_y(100, 24, 23), 100);
        assert_eq!(centered_control_y_ceil(100, 24, 23), 101);
        assert_eq!(centered_control_y_ceil(100, 24, 24), 100);
    }
}
