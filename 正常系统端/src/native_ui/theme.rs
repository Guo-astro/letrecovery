use crate::native_ui::GetDpiForWindow;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{COLORREF, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_DONOTROUND, DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, ClientToScreen, CreateCompatibleBitmap, CreateCompatibleDC, CreatePen,
    CreateRectRgn, CreateSolidBrush, DeleteDC, DeleteObject, EndPaint, FillRect, GetTextMetricsW,
    GetWindowDC, InvalidateRect, RedrawWindow, ReleaseDC, RoundRect, ScreenToClient, SelectObject,
    SetBkMode, SetDIBitsToDevice, SetStretchBltMode, SetTextColor, SetWindowRgn, StretchDIBits,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, DT_END_ELLIPSIS, DT_NOPREFIX,
    DT_SINGLELINE, DT_VCENTER, HALFTONE, HBRUSH, HDC, PAINTSTRUCT, PEN_STYLE, RDW_ERASE, RDW_FRAME,
    RDW_INVALIDATE, RDW_NOERASE, RDW_UPDATENOW, SRCCOPY, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    GetComboBoxInfo, SetWindowTheme, CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDRF_DODEFAULT,
    CDRF_NOTIFYITEMDRAW, CDRF_SKIPDEFAULT, CDRF_SKIPPOSTPAINT, COMBOBOXINFO, HDITEMW, HDI_TEXT,
    LVIF_TEXT, LVITEMW, NMLVCUSTOMDRAW, NM_CUSTOMDRAW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, IsWindowEnabled, TrackMouseEvent, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT,
};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
#[cfg(test)]
use windows::Win32::UI::WindowsAndMessaging::WS_VSCROLL;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetClientRect, GetCursorPos, GetParent, GetPropW, GetWindowLongPtrW,
    GetWindowRect, GetWindowTextLengthW, GetWindowTextW, HideCaret, IsWindowVisible, PostMessageW,
    RemovePropW, SendMessageW, SetPropW, SetWindowLongPtrW, SetWindowPos, ShowCaret, GWL_EXSTYLE,
    GWL_STYLE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    WM_CANCELMODE, WM_CAPTURECHANGED, WM_ENABLE, WM_ERASEBKGND, WM_GETFONT, WM_KEYDOWN, WM_KEYUP,
    WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCDESTROY, WM_NCHITTEST,
    WM_NCPAINT, WM_NOTIFY, WM_PAINT, WM_SETCURSOR, WM_SETFOCUS, WM_SETTEXT, WM_SIZE,
    WM_THEMECHANGED, WS_BORDER, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_CLIENTEDGE, WS_EX_LAYERED,
    WS_EX_TRANSPARENT,
};
use winreg::enums::HKEY_CURRENT_USER;
use winreg::RegKey;

use super::controls::{
    alpha_blend_premultiplied_bgra, button_visual, draw_antialiased_control_frame,
    draw_antialiased_control_frame_with_vertical_interiors, draw_native_text,
    draw_opaque_surface_text, draw_progress, ensure_list_view_frame, fill_round_rect_antialiased,
    list_view_frame, list_view_frame_owner, publish_list_view_frame,
    rounded_control_frame_geometry, single_line_edit_frame, single_line_edit_frame_owner,
    ButtonRole, ControlState, InnoMetrics, ProgressRole,
};

const fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    COLORREF((red as u32) | ((green as u32) << 8) | ((blue as u32) << 16))
}

/// Inno Setup 6.7 `WizardStyle=modern ... windows11` color roles.
/// Values are taken from the fixed-reference screenshots and its Windows 11 VCL styles.
#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    pub window: COLORREF,
    pub nav: COLORREF,
    pub edit: COLORREF,
    pub button: COLORREF,
    pub button_hot: COLORREF,
    pub button_pressed: COLORREF,
    pub text: COLORREF,
    pub text_secondary: COLORREF,
    pub text_disabled: COLORREF,
    pub border: COLORREF,
    pub separator: COLORREF,
    pub accent_fill: COLORREF,
    pub accent_border: COLORREF,
    /// Highlighted Next/install action, selected navigation entry and selected report/list row.
    pub highlight_fill: COLORREF,
    pub highlight_border: COLORREF,
    /// Inno Modern Windows 11 task progress and checked-state accent.
    pub progress: COLORREF,
}

impl Palette {
    pub const LIGHT: Self = Self {
        dark: false,
        window: rgb(249, 249, 249),
        nav: rgb(249, 249, 249),
        edit: rgb(255, 255, 255),
        button: rgb(253, 253, 253),
        button_hot: rgb(249, 249, 249),
        button_pressed: rgb(233, 233, 233),
        text: rgb(0, 0, 0),
        text_secondary: rgb(59, 59, 59),
        text_disabled: rgb(157, 157, 157),
        border: rgb(230, 230, 230),
        separator: rgb(222, 222, 222),
        accent_fill: rgb(0, 95, 184),
        accent_border: rgb(0, 96, 184),
        highlight_fill: rgb(0, 95, 184),
        highlight_border: rgb(0, 96, 184),
        progress: rgb(113, 199, 132),
    };

    pub const DARK: Self = Self {
        dark: true,
        window: rgb(43, 43, 43),
        nav: rgb(43, 43, 43),
        edit: rgb(28, 28, 28),
        button: rgb(48, 48, 48),
        button_hot: rgb(55, 55, 55),
        button_pressed: rgb(41, 41, 41),
        text: rgb(255, 255, 255),
        text_secondary: rgb(214, 214, 214),
        text_disabled: rgb(120, 120, 120),
        border: rgb(61, 61, 61),
        separator: rgb(72, 72, 72),
        accent_fill: rgb(49, 72, 83),
        accent_border: rgb(66, 149, 192),
        // User-audited Windows 11 selection colour from the supplied RGB sample.
        highlight_fill: rgb(76, 194, 255),
        highlight_border: rgb(76, 194, 255),
        progress: rgb(113, 199, 132),
    };

    /// Returns the final opaque field color used by the native Edit client.
    pub(crate) const fn edit_brush_color(self) -> COLORREF {
        self.edit
    }

    pub(crate) unsafe fn edit_brush_color_for(self, _control: HWND) -> COLORREF {
        self.edit_brush_color()
    }

    pub(crate) const fn edit_text_color(self) -> COLORREF {
        self.text
    }

    pub(crate) unsafe fn edit_text_color_for(self, _control: HWND) -> COLORREF {
        self.edit_text_color()
    }

    const fn control_border(self) -> COLORREF {
        self.border
    }

    pub fn system() -> Self {
        #[cfg(feature = "non-elevated-tests")]
        match std::env::var("LETRECOVERY_UI_THEME").as_deref() {
            Ok("dark") => return Self::DARK,
            Ok("light") => return Self::LIGHT,
            _ => {}
        }

        let personalization = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize")
            .ok();
        let apps_use_light: Option<u32> = personalization
            .as_ref()
            .and_then(|key| key.get_value("AppsUseLightTheme").ok());
        if apps_use_light == Some(0) {
            Self::DARK
        } else {
            Self::LIGHT
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeControlKind {
    General,
    Field,
    ScrollableField,
    List,
    ListView,
    Header,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeThemeClass {
    Explorer,
    DarkExplorer,
    Cfd,
    DarkCfd,
    ItemsView,
    DarkItemsView,
}

const fn native_theme_class(kind: NativeControlKind, dark: bool) -> NativeThemeClass {
    match (kind, dark) {
        (NativeControlKind::Header, false) => NativeThemeClass::ItemsView,
        (NativeControlKind::Header, true) => NativeThemeClass::DarkItemsView,
        (NativeControlKind::Field, false) => NativeThemeClass::Cfd,
        (NativeControlKind::Field, true) => NativeThemeClass::DarkCfd,
        (
            NativeControlKind::General
            | NativeControlKind::ScrollableField
            | NativeControlKind::ListView
            | NativeControlKind::List,
            true,
        ) => NativeThemeClass::DarkExplorer,
        _ => NativeThemeClass::Explorer,
    }
}

/// Applies the native theme class that covers both the control client area and its non-client
/// scrollbar. Multiline edits deliberately use Explorer rather than CFD in dark mode because the
/// latter leaves a light Win32 scrollbar on several supported Windows builds.
///
/// Field frames (Edit / ComboBox / ListBox) use the host Windows 11 visual styles only. A previous
/// owner-drawn rounded overlay left residual system-accent “blue feet” at the four rectangular
/// corners and fought the Fluent control chrome, so it is no longer installed on those HWNDs.
const APPLIED_THEME_PROPERTY: windows::core::PCWSTR = windows::core::w!("LetRecovery.AppliedTheme");

static LAST_PALETTE_DARK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the palette most recently applied to controls is dark, for controls that are created
/// later and must start in the right theme family.
pub(crate) fn last_palette_dark() -> bool {
    LAST_PALETTE_DARK.load(std::sync::atomic::Ordering::Relaxed)
}

/// Fields and lists are painted partly through their window DC (frames, bands, closed combo
/// surface). Without WS_CLIPSIBLINGS such a paint also lands on any sibling that overlaps the
/// control for a moment during a layout pass, and USER32 then copies those pixels along when the
/// sibling moves: the "ghost of the edit box on the buttons" seen while resizing.
unsafe fn ensure_clip_siblings(control: HWND) {
    let style = GetWindowLongPtrW(control, GWL_STYLE);
    let bit = WS_CLIPSIBLINGS.0 as isize;
    if style & bit == 0 {
        let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(
            control,
            GWL_STYLE,
            style | bit,
        );
    }
}

pub unsafe fn apply_control_theme(control: HWND, palette: Palette, kind: NativeControlKind) {
    super::redraw::ui_detail(|| {
        let mut window = RECT::default();
        let _ = GetWindowRect(control, &mut window);
        format!(
            "apply_control_theme hwnd={:?} class={} kind={:?} style={:#x} exstyle={:#x} window={:?}",
            control.0,
            control_class_name(control),
            kind,
            GetWindowLongPtrW(control, GWL_STYLE),
            GetWindowLongPtrW(control, windows::Win32::UI::WindowsAndMessaging::GWL_EXSTYLE),
            window
        )
    });
    if matches!(
        kind,
        NativeControlKind::Field | NativeControlKind::ScrollableField | NativeControlKind::List
    ) {
        ensure_clip_siblings(control);
    }
    if let Some(edit) = single_line_edit_frame_owner(control) {
        apply_single_line_edit_frame_theme(control, edit, palette);
        return;
    }
    // Multi-line edits always keep the scrollable-field class. Tool dialogs prepare every Edit as a
    // plain field on each show; switching a report between the CFD and Explorer classes reloaded
    // its theme every time (a white flash when copying or refreshing) and the CFD edit style drew
    // its own square border inside the rounded frame.
    let kind = if matches!(kind, NativeControlKind::Field)
        && is_edit_class(&control_class_name(control))
        && !is_single_line_edit(control)
    {
        NativeControlKind::ScrollableField
    } else {
        kind
    };
    let class = match native_theme_class(kind, palette.dark) {
        NativeThemeClass::Explorer => w!("Explorer"),
        NativeThemeClass::DarkExplorer => w!("DarkMode_Explorer"),
        NativeThemeClass::Cfd => w!("CFD"),
        NativeThemeClass::DarkCfd => w!("DarkMode_CFD"),
        NativeThemeClass::ItemsView => w!("ItemsView"),
        NativeThemeClass::DarkItemsView => w!("DarkMode_ItemsView"),
    };
    // SetWindowTheme makes the control drop and reload its theme data and repaint its frame and
    // scrollbars; in between a multi-line edit visibly drew a light scrollbar. Re-applying the
    // same class (every page show and theme refresh did) is skipped.
    let class_tag = native_theme_class(kind, palette.dark) as usize + 1;
    if windows::Win32::UI::WindowsAndMessaging::GetPropW(control, APPLIED_THEME_PROPERTY).0 as usize
        != class_tag
    {
        let _ = SetWindowTheme(control, class, PCWSTR::null());
        let _ = windows::Win32::UI::WindowsAndMessaging::SetPropW(
            control,
            APPLIED_THEME_PROPERTY,
            windows::Win32::Foundation::HANDLE(class_tag as *mut core::ffi::c_void),
        );
    }
    LAST_PALETTE_DARK.store(palette.dark, std::sync::atomic::Ordering::Relaxed);
    let class_name = control_class_name(control);
    let is_edit = is_edit_class(&class_name);
    let is_combo = is_combo_class(&class_name);
    let control_style = GetWindowLongPtrW(control, GWL_STYLE);
    let transparent_carrier_control = is_auto_checkbox(&class_name, control_style)
        || is_auto_radio_button(&class_name, control_style);
    if transparent_carrier_control {
        let ex_style = GetWindowLongPtrW(control, GWL_EXSTYLE);
        let transparent = WS_EX_TRANSPARENT.0 as isize;
        let desired = if palette.window.0 == 0 {
            ex_style | transparent
        } else {
            ex_style & !transparent
        };
        if desired != ex_style {
            let _ = SetWindowLongPtrW(control, GWL_EXSTYLE, desired);
        }
    }
    if is_auto_checkbox(&class_name, control_style) {
        // Inno's themed checkbox state table is the Windows BUTTON theme: BP_CHECKBOX/CBS_*.
        // Keep USER32's state machine, keyboard handling, accessibility and BN_CLICKED semantics,
        // while the subclass asks UxTheme for the actual current Windows 11 glyph for every state.
        // Do not disable the theme here: the previous hand-drawn replacement is what produced the
        // coarse check mark and visibly different Win10/Win11 geometry reported by the user.
        let _ = SetWindowSubclass(
            control,
            Some(check_box_subclass),
            CHECK_BOX_SUBCLASS_ID,
            palette_reference(palette),
        );
        let _ = InvalidateRect(control, None, false);
    } else if is_auto_radio_button(&class_name, control_style) {
        // Radio buttons use the same Windows BUTTON theme, with BP_RADIOBUTTON/RBS_* states.
        // This also removes the extra focus ring created by the former custom ellipse renderer.
        let _ = SetWindowSubclass(
            control,
            Some(radio_button_subclass),
            RADIO_BUTTON_SUBCLASS_ID,
            palette_reference(palette),
        );
        let _ = InvalidateRect(control, None, false);
    }
    if is_edit && is_single_line_edit(control) {
        // The direct child Edit keeps native text/caret/selection/IME and accessibility. Its
        // borderless client is vertically centred over a separate sibling surface, so USER32
        // never paints into the full-height rounded frame or controls its optical baseline.
        let _ = SetWindowTheme(control, w!(""), w!(""));
        apply_borderless_style(control);
        disable_edit_layered_redirection(control);
        let _ = RemoveWindowSubclass(
            control,
            Some(rounded_control_subclass),
            ROUNDED_CONTROL_SUBCLASS_ID,
        );
        let _ = SetWindowSubclass(
            control,
            Some(single_line_edit_subclass),
            SINGLE_LINE_EDIT_SUBCLASS_ID,
            palette_reference(palette),
        );
        if let Some(frame) = single_line_edit_frame(control) {
            apply_single_line_edit_frame_theme(frame, control, palette);
        }
        let _ = SetWindowPos(
            control,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        clear_legacy_control_region(control);
        let _ = RedrawWindow(
            control,
            None,
            None,
            RDW_FRAME | RDW_INVALIDATE | RDW_NOERASE | RDW_UPDATENOW,
        );
    } else if is_edit && !is_single_line_edit(control) {
        // A multi-line field is a native EDIT (selection, caret, keyboard, IME and its own
        // WS_VSCROLL scrollbar stay USER32's) inside the same hollow rounded frame sibling as the
        // reports. The edit fills the frame up to its outline, so the scrollbar runs from the
        // top border to the bottom border and the frame's rounded corners clip it; the text keeps
        // its distance from every edge through the formatting rectangle.
        apply_borderless_style(control);
        let _ = RemoveWindowSubclass(
            control,
            Some(rounded_control_subclass),
            ROUNDED_CONTROL_SUBCLASS_ID,
        );
        let _ = SetWindowSubclass(
            control,
            Some(framed_edit_subclass),
            FRAMED_EDIT_SUBCLASS_ID,
            palette_reference(palette),
        );
        clear_legacy_control_region(control);
        // Give back a band an earlier pass may have reserved in the non-client area.
        refresh_frame_band(control);
        if let Some(frame) = ensure_list_view_frame(control) {
            let _ = SetWindowSubclass(
                frame,
                Some(list_view_frame_subclass),
                LIST_VIEW_FRAME_SUBCLASS_ID,
                palette_reference(palette),
            );
            let _ = InvalidateRect(frame, None, false);
            // Show the frame now if the field is already visible (a child of a dialog that is
            // shown as a whole never receives WM_SHOWWINDOW itself).
            publish_list_view_frame(control);
        }
        apply_edit_text_padding(control, true);
        let _ = RedrawWindow(
            control,
            None,
            None,
            RDW_INVALIDATE | RDW_FRAME | RDW_NOERASE,
        );
    } else if is_edit {
        apply_single_border_style(control);
    } else if is_combo && matches!(kind, NativeControlKind::Field) {
        // The closed selection field is fully painted by rounded_control_subclass. Leaving the
        // active UxTheme renderer enabled lets its hover timer briefly compose a rectangular frame
        // before our rounded overlay. Disable that renderer on the closed HWND only; the separate
        // ComboLBox popup is themed below and remains entirely native.
        let _ = SetWindowTheme(control, w!(""), w!(""));
        apply_borderless_style(control);
        let _ = SetWindowSubclass(
            control,
            Some(rounded_control_subclass),
            ROUNDED_CONTROL_SUBCLASS_ID,
            palette_reference(palette),
        );
        set_combo_selection_field_height(
            control,
            InnoMetrics::for_dpi(GetDpiForWindow(control).max(96)).field_height,
        );
        set_combo_popup_row_height(
            control,
            InnoMetrics::for_dpi(GetDpiForWindow(control).max(96)).field_height,
        );
        clip_combo_to_closed_field(control, palette);
        let _ = InvalidateRect(control, None, false);
    } else if matches!(kind, NativeControlKind::List) && is_list_box(control) {
        // Standalone ListBoxes retain their existing Inno row palette, but the HWND itself has one
        // clipped, borderless surface so USER32 cannot expose square blue/black corner pixels.
        // Every pixel of the client is composed by `list_box_subclass`; USER32 never paints rows.
        apply_borderless_style(control);
        install_list_box_subclass(control, palette);
        clear_legacy_control_region(control);
        let _ = RedrawWindow(
            control,
            None,
            None,
            RDW_INVALIDATE | RDW_FRAME | RDW_NOERASE,
        );
    } else if matches!(
        kind,
        NativeControlKind::Field | NativeControlKind::ScrollableField | NativeControlKind::List
    ) {
        // ComboBox and other field HWNDs: drop any earlier rounded-frame subclass so only the
        // Windows 11 themed border remains.
        let _ = RemoveWindowSubclass(
            control,
            Some(rounded_control_subclass),
            ROUNDED_CONTROL_SUBCLASS_ID,
        );
        clear_control_window_region(control);
        let _ = InvalidateRect(control, None, false);
    }

    // A native ComboBox owns a separate top-level ComboLBox window. The list is not covered by
    // theming the ComboBox HWND itself, which otherwise leaves a white popup in dark mode.
    let mut info = COMBOBOXINFO {
        cbSize: std::mem::size_of::<COMBOBOXINFO>() as u32,
        ..Default::default()
    };
    let _ = GetComboBoxInfo(control, &mut info);
    if is_combo && matches!(kind, NativeControlKind::Field) && is_drop_down_list(control) {
        install_combo_selection_item_subclass(control, palette);
    }
    if !info.hwndList.0.is_null() {
        // The popup is a ListBox, not another field frame.  DarkMode_CFD is correct for the
        // closed ComboBox but corrupts the popup/arrow painting on some Windows 11 builds (the
        // selected string is drawn a second time in the arrow area).  Explorer keeps the popup
        // dark without changing the closed ComboBox renderer.
        let popup_class = if palette.dark {
            w!("DarkMode_Explorer")
        } else {
            w!("Explorer")
        };
        let _ = SetWindowTheme(info.hwndList, popup_class, PCWSTR::null());
        set_combo_popup_row_height(
            control,
            InnoMetrics::for_dpi(GetDpiForWindow(control).max(96)).field_height,
        );
        // Inno's TNewComboBox is a stock TComboBox. Preserve USER32's normal rectangular popup
        // renderer; this removes the slower WM_DRAWITEM/rounded-overlay paths and keeps keyboard,
        // hover and accessibility behaviour identical to the native control.
        let _ = RemoveWindowSubclass(
            info.hwndList,
            Some(rounded_control_subclass),
            ROUNDED_CONTROL_SUBCLASS_ID,
        );
        let _ = RemovePropW(info.hwndList, LIST_BOX_HOT_PROPERTY);
        apply_combo_popup_native_chrome(info.hwndList, palette);
        if is_combo && is_drop_down_list(control) {
            let _ = SetWindowSubclass(
                info.hwndList,
                Some(combo_popup_close_subclass),
                COMBO_POPUP_CLOSE_SUBCLASS_ID,
                control.0 as usize,
            );
        }
        let _ = InvalidateRect(info.hwndList, None, false);
    }
}

/// Installs the compatibility painter on USER32's read-only closed selection child.
///
/// Some WinPE USER32 builds create or recreate this child only after the first focus/drop-down
/// transition, so this helper is intentionally idempotent and is called both during initial theme
/// application and from the parent ComboBox state transitions.
unsafe fn install_combo_selection_item_subclass(combo: HWND, palette: Palette) {
    let mut info = COMBOBOXINFO {
        cbSize: std::mem::size_of::<COMBOBOXINFO>() as u32,
        ..Default::default()
    };
    if GetComboBoxInfo(combo, &mut info).is_err()
        || info.hwndItem.0.is_null()
        || info.hwndItem == combo
    {
        return;
    }
    if GetPropW(info.hwndItem, COMBO_SELECTION_ITEM_PREPARED_PROPERTY).is_invalid() {
        let _ = SetWindowTheme(info.hwndItem, w!(""), w!(""));
        apply_borderless_style(info.hwndItem);
        let _ = SetPropW(
            info.hwndItem,
            COMBO_SELECTION_ITEM_PREPARED_PROPERTY,
            HANDLE(std::ptr::dangling_mut()),
        );
    }
    let _ = SetWindowSubclass(
        info.hwndItem,
        Some(combo_selection_item_subclass),
        COMBO_SELECTION_ITEM_SUBCLASS_ID,
        palette_reference(palette),
    );
    repaint_combo_selection_item_now(info.hwndItem, palette);
}

const COMBO_POPUP_CLOSE_SUBCLASS_ID: usize = 0x4c52_4350;

/// Choosing an item closes the popup, and USER32 then draws the chosen caption on the closed field
/// in the focused (system-blue) style, outside WM_PAINT. The popup hiding is the reliable moment
/// to queue our own repaint, which runs after that native drawing has finished.
unsafe extern "system" fn combo_popup_close_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    const WM_WINDOWPOSCHANGED_MESSAGE: u32 = 0x0047;
    const WM_SHOWWINDOW_MESSAGE: u32 = 0x0018;
    const SWP_HIDEWINDOW_FLAG: u32 = 0x0080;
    let result = DefSubclassProc(hwnd, message, wparam, lparam);
    let hidden = match message {
        WM_WINDOWPOSCHANGED_MESSAGE if lparam.0 != 0 => {
            let position =
                &*(lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::WINDOWPOS);
            position.flags.0 & SWP_HIDEWINDOW_FLAG != 0
        }
        WM_SHOWWINDOW_MESSAGE => wparam.0 == 0,
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(
                hwnd,
                Some(combo_popup_close_subclass),
                COMBO_POPUP_CLOSE_SUBCLASS_ID,
            );
            false
        }
        _ => false,
    };
    if hidden {
        let combo = HWND(reference_data as *mut _);
        super::redraw::ui_detail(|| format!("combo {:?} popup closed: repaint queued", combo.0));
        let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
            combo,
            WM_REPAINT_TRACKING_COMBO,
            WPARAM(0),
            LPARAM(0),
        );
    }
    result
}

unsafe fn apply_combo_popup_native_chrome(popup: HWND, palette: Palette) {
    // ComboLBox is a separate top-level HWND and does not inherit dark mode from the owner.
    // Explicitly opt out of DWM rounding: the user requested the stock rectangular Windows popup,
    // while its client rows, keyboard navigation and accessibility remain entirely USER32-owned.
    let corner_preference = DWMWCP_DONOTROUND;
    let _ = DwmSetWindowAttribute(
        popup,
        DWMWA_WINDOW_CORNER_PREFERENCE,
        (&corner_preference as *const DWM_WINDOW_CORNER_PREFERENCE).cast(),
        std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
    );
    let immersive_dark = if palette.dark { 1i32 } else { 0i32 };
    let _ = DwmSetWindowAttribute(
        popup,
        DWMWA_USE_IMMERSIVE_DARK_MODE,
        (&immersive_dark as *const i32).cast(),
        std::mem::size_of_val(&immersive_dark) as u32,
    );
}

unsafe fn control_class_name(control: HWND) -> String {
    let mut buffer = [0u16; 64];
    let length = GetClassNameW(control, &mut buffer);
    String::from_utf16_lossy(&buffer[..usize::try_from(length.max(0)).unwrap_or(0)])
}

fn is_edit_class(class_name: &str) -> bool {
    class_name.eq_ignore_ascii_case("Edit")
}

fn is_combo_class(class_name: &str) -> bool {
    class_name.eq_ignore_ascii_case("ComboBox")
}

const fn button_style_is_auto_radio(style: isize) -> bool {
    const BUTTON_TYPE_MASK: isize = 0x000f;
    const BS_AUTORADIOBUTTON_VALUE: isize = 0x0009;
    style & BUTTON_TYPE_MASK == BS_AUTORADIOBUTTON_VALUE
}

fn is_auto_radio_button(class_name: &str, style: isize) -> bool {
    class_name.eq_ignore_ascii_case("Button") && button_style_is_auto_radio(style)
}

const fn button_style_is_auto_checkbox(style: isize) -> bool {
    const BUTTON_TYPE_MASK: isize = 0x000f;
    const BS_AUTOCHECKBOX_VALUE: isize = 0x0003;
    style & BUTTON_TYPE_MASK == BS_AUTOCHECKBOX_VALUE
}

fn is_auto_checkbox(class_name: &str, style: isize) -> bool {
    class_name.eq_ignore_ascii_case("Button") && button_style_is_auto_checkbox(style)
}

unsafe fn is_single_line_edit(control: HWND) -> bool {
    const ES_MULTILINE: isize = 0x0004;
    GetWindowLongPtrW(control, GWL_STYLE) & ES_MULTILINE == 0
}

unsafe fn is_read_only_edit(control: HWND) -> bool {
    const ES_READONLY: isize = 0x0800;
    GetWindowLongPtrW(control, GWL_STYLE) & ES_READONLY != 0
}

fn borderless_style_bits(style: isize, ex_style: isize) -> (isize, isize) {
    (
        style & !(WS_BORDER.0 as isize),
        ex_style & !(WS_EX_CLIENTEDGE.0 as isize),
    )
}

fn single_border_style_bits(style: isize, ex_style: isize) -> (isize, isize) {
    (
        style | WS_BORDER.0 as isize,
        ex_style & !(WS_EX_CLIENTEDGE.0 as isize),
    )
}

unsafe fn apply_borderless_style(control: HWND) {
    let style = GetWindowLongPtrW(control, GWL_STYLE);
    let ex_style = GetWindowLongPtrW(control, GWL_EXSTYLE);
    let (style, ex_style) = borderless_style_bits(style, ex_style);
    apply_control_frame_styles(control, style, ex_style);
}

unsafe fn apply_single_border_style(control: HWND) {
    let style = GetWindowLongPtrW(control, GWL_STYLE);
    let ex_style = GetWindowLongPtrW(control, GWL_EXSTYLE);
    let (style, ex_style) = single_border_style_bits(style, ex_style);
    apply_control_frame_styles(control, style, ex_style);
}

unsafe fn apply_control_frame_styles(control: HWND, style: isize, ex_style: isize) {
    let current_style = GetWindowLongPtrW(control, GWL_STYLE);
    let current_ex_style = GetWindowLongPtrW(control, GWL_EXSTYLE);
    if current_style == style && current_ex_style == ex_style {
        return;
    }
    let _ = SetWindowLongPtrW(control, GWL_STYLE, style);
    let _ = SetWindowLongPtrW(control, GWL_EXSTYLE, ex_style);
    let _ = SetWindowPos(
        control,
        None,
        0,
        0,
        0,
        0,
        SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
    );
    let _ = InvalidateRect(control, None, false);
}

/// Themes both halves of a report ListView. The header is a separate HWND and otherwise retains a
/// light background even when the list client colors are explicitly dark.
pub unsafe fn apply_list_view_theme(list: HWND, palette: Palette) -> Option<HWND> {
    // A report's header is a real child HWND. Microsoft documents that a parent without
    // WS_CLIPCHILDREN may draw over a child and make the child repaint afterward. ListView theme
    // timers then expose precisely that intermediate frame (body colour covering the header).
    // Apply the style centrally to every report before installing either painter.
    let style = GetWindowLongPtrW(list, GWL_STYLE);
    let clipping = (WS_CLIPCHILDREN | WS_CLIPSIBLINGS).0 as isize;
    if style & clipping != clipping {
        let _ = SetWindowLongPtrW(list, GWL_STYLE, style | clipping);
        let _ = SetWindowPos(
            list,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    // Keep the real report borderless. Its deterministic frame is a non-interactive sibling,
    // because comctl32 copies pixels inside the ListView while scrolling and would otherwise copy
    // the fixed right edge repeatedly across the report body.
    apply_borderless_style(list);
    let frame = ensure_list_view_frame(list);
    apply_control_theme(list, palette, NativeControlKind::ListView);
    clear_legacy_control_region(list);
    // Comctl32 v6 explicitly provides double-buffered report painting for this purpose.  Some
    // callers already request it while creating their ListView, but applying it here as well keeps
    // every report (including the main install list in reduced WinPE builds) on the same path.
    const LVM_GETEXTENDEDLISTVIEWSTYLE: u32 = 0x1037;
    const LVM_SETEXTENDEDLISTVIEWSTYLE: u32 = 0x1036;
    const LVS_EX_DOUBLEBUFFER: isize = 0x0001_0000;
    let extended = SendMessageW(list, LVM_GETEXTENDEDLISTVIEWSTYLE, WPARAM(0), LPARAM(0)).0;
    let _ = SendMessageW(
        list,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        WPARAM(0),
        LPARAM(extended | LVS_EX_DOUBLEBUFFER),
    );
    // Do not make every caller remember the three independent ListView colour messages.  In
    // particular, an empty report has no item custom-draw callback and otherwise exposes the
    // class default white body in dark mode.
    set_list_view_colors(list, palette);
    let _ = InvalidateRect(list, None, false);
    let _ = SetWindowSubclass(
        list,
        Some(list_view_subclass),
        LIST_VIEW_SUBCLASS_ID,
        palette_reference(palette),
    );
    schedule_list_column_fit(list);
    if let Some(frame) = frame {
        let _ = SetWindowSubclass(
            frame,
            Some(list_view_frame_subclass),
            LIST_VIEW_FRAME_SUBCLASS_ID,
            palette_reference(palette),
        );
        let _ = InvalidateRect(frame, None, false);
        publish_list_view_frame(list);
    }
    // Selection colour is delivered as NM_CUSTOMDRAW to the ListView parent rather than the
    // ListView itself. Install one keyed parent subclass per list, so dialogs containing two
    // reports remain independent and a real selected row uses the Inno highlighted-button fill.
    if let Ok(parent) = GetParent(list) {
        let list_value = list.0 as usize;
        let dark_flag = usize::from(palette.dark) << (usize::BITS - 1);
        let _ = SetWindowSubclass(
            parent,
            Some(list_view_parent_subclass),
            LIST_VIEW_PARENT_SUBCLASS_ID ^ list_value,
            list_value | dark_flag,
        );
    }
    let header = SendMessageW(list, 0x101F, WPARAM(0), LPARAM(0)); // LVM_GETHEADER
    if header.0 == 0 {
        return None;
    }
    let header = HWND(header.0 as *mut _);
    // The header is completely painted by `header_subclass`. Leaving ItemsView active at the same
    // time lets UxTheme run a buffered hot/focus transition over that finished frame: the custom
    // text disappears for several timer ticks and then returns, which looks like a permanently
    // flickering hardware table. Disable only the header's visual-style renderer; the ListView
    // body and its non-client scrollbar retain ItemsView, sizing/hit-testing stay native, and one
    // deterministic painter owns every visible header pixel.
    let _ = SetWindowTheme(header, w!(""), w!(""));
    // A report header is parented to the ListView itself, so HDF_OWNERDRAW sends WM_DRAWITEM to
    // the ListView instead of our dialog content window.  Subclassing the header is the only
    // deterministic way to avoid dark ItemsView drawing black text on a black header.
    let _ = SetWindowSubclass(
        header,
        Some(header_subclass),
        HEADER_SUBCLASS_ID,
        palette_reference(palette),
    );
    let _ = InvalidateRect(header, None, false);
    Some(header)
}

unsafe fn set_list_view_colors(list: HWND, palette: Palette) {
    // Send only colours that differ: each LVM_SET*COLOR invalidates the whole report, and this
    // runs on every size step, focus change and enable change.
    for (get, set, color) in [
        (0x1000, 0x1001, palette.edit), // LVM_GETBKCOLOR / LVM_SETBKCOLOR
        (0x1025, 0x1026, palette.edit), // LVM_GETTEXTBKCOLOR / LVM_SETTEXTBKCOLOR
        (0x1023, 0x1024, palette.text), // LVM_GETTEXTCOLOR / LVM_SETTEXTCOLOR
    ] {
        let current = SendMessageW(list, get, WPARAM(0), LPARAM(0)).0 as u32;
        if current != color.0 {
            let _ = SendMessageW(list, set, WPARAM(0), LPARAM(color.0 as isize));
        }
    }
}

/// Applies a deterministic Inno-style paint path to the one native progress control still used by
/// a tool dialog.  UxTheme's progress class ignores the app dark mode and otherwise leaves a light
/// trough in the partition-copy window.
pub unsafe fn apply_progress_theme(control: HWND, palette: Palette) {
    let _ = SetWindowTheme(control, PCWSTR::null(), PCWSTR::null());
    let _ = SetWindowSubclass(
        control,
        Some(progress_subclass),
        PROGRESS_SUBCLASS_ID,
        palette_reference(palette),
    );
    let _ = InvalidateRect(control, None, false);
}

/// Applies a deterministic dark/light paint path to the horizontal target-size trackbar.  The
/// standard trackbar theme has no supported dark variant and paints a nearly white channel/thumb.
pub unsafe fn apply_trackbar_theme(control: HWND, palette: Palette) {
    let _ = SetWindowTheme(control, PCWSTR::null(), PCWSTR::null());
    let _ = SetWindowSubclass(
        control,
        Some(trackbar_subclass),
        TRACKBAR_SUBCLASS_ID,
        palette_reference(palette),
    );
    let _ = InvalidateRect(control, None, false);
}

const HEADER_SUBCLASS_ID: usize = 0x4c52_4844;
const LIST_VIEW_SUBCLASS_ID: usize = 0x4c52_4c56;
const LIST_VIEW_FRAME_SUBCLASS_ID: usize = 0x4c52_4c46;
const LIST_VIEW_PARENT_SUBCLASS_ID: usize = 0x4c52_4c50;
const PROGRESS_SUBCLASS_ID: usize = 0x4c52_5052;
const TRACKBAR_SUBCLASS_ID: usize = 0x4c52_5442;
const CHECK_BOX_SUBCLASS_ID: usize = 0x4c52_4342;
const RADIO_BUTTON_SUBCLASS_ID: usize = 0x4c52_5242;
const ROUNDED_CONTROL_SUBCLASS_ID: usize = 0x4c52_5243;
const SINGLE_LINE_EDIT_SUBCLASS_ID: usize = 0x4c52_4544;
const SINGLE_LINE_EDIT_FRAME_SUBCLASS_ID: usize = 0x4c52_4546;
const COMBO_SELECTION_ITEM_SUBCLASS_ID: usize = 0x4c52_4353;
const LIST_BOX_HOT_PROPERTY: PCWSTR = w!("LetRecovery.InnoListBox.HotItem");
const ROUNDED_CONTROL_HOT_PROPERTY: PCWSTR = w!("LetRecovery.InnoControl.Hot");
const COMBO_CARET_HIDDEN_PROPERTY: PCWSTR = w!("LetRecovery.InnoCombo.CaretHidden");
const COMBO_TRACKING_DROPPED_PROPERTY: PCWSTR = w!("LetRecovery.InnoCombo.TrackingDropped");
const COMBO_SELECTION_ITEM_PREPARED_PROPERTY: PCWSTR =
    w!("LetRecovery.InnoCombo.SelectionPrepared");
const RADIO_BUTTON_HOT_PROPERTY: PCWSTR = w!("LetRecovery.InnoRadio.Hot");
const PALETTE_REFERENCE_DARK: usize = 0x1;
const CHECK_BOX_HOT_PROPERTY: PCWSTR = w!("LetRecovery.InnoCheck.Hot");
const WM_MOUSELEAVE_MESSAGE: u32 = 0x02a3;
const WM_NCMOUSEMOVE_MESSAGE: u32 = 0x00a0;
const WM_NCMOUSELEAVE_MESSAGE: u32 = 0x02a2;
const WM_REPAINT_TRACKING_COMBO: u32 = 0x8000 + 0x4c5;

/// Messages whose USER32/comctl32 default handling may repaint a native non-client scrollbar.
/// The rounded frame must be overlaid only after that handling finishes; otherwise the scrollbar
/// hover animation can restore square right-hand corners until the next full control repaint.
const fn native_scrollbar_may_repaint_frame(message: u32) -> bool {
    matches!(
        message,
        0x00a1 // WM_NCLBUTTONDOWN
            | 0x00a2 // WM_NCLBUTTONUP
            | 0x00a3 // WM_NCLBUTTONDBLCLK
            | 0x0113 // WM_TIMER (UxTheme scrollbar hover animation)
            | 0x0114 // WM_HSCROLL
            | 0x0115 // WM_VSCROLL
            | 0x020a // WM_MOUSEWHEEL
            | 0x020e // WM_MOUSEHWHEEL
            | 0x02a0 // WM_NCMOUSEHOVER
    )
}

const fn palette_reference(palette: Palette) -> usize {
    if palette.dark {
        PALETTE_REFERENCE_DARK
    } else {
        0
    }
}

const fn palette_from_reference(reference_data: usize) -> Palette {
    if reference_data & PALETTE_REFERENCE_DARK != 0 {
        Palette::DARK
    } else {
        Palette::LIGHT
    }
}

unsafe fn redraw_control_frame(control: HWND) {
    let _ = RedrawWindow(
        control,
        None,
        None,
        RDW_FRAME | RDW_INVALIDATE | RDW_NOERASE,
    );
}

unsafe fn repaint_list_view_header_now(list: HWND) {
    let header = SendMessageW(list, 0x101f, WPARAM(0), LPARAM(0)); // LVM_GETHEADER
    if header.0 == 0 {
        return;
    }
    let _ = RedrawWindow(
        HWND(header.0 as *mut _),
        None,
        None,
        RDW_INVALIDATE | RDW_NOERASE | RDW_UPDATENOW,
    );
}

unsafe fn invalidate_control_visual(control: HWND) {
    // Mouse hot-state changes are delivered in bursts (WM_SETCURSOR followed by one or more
    // WM_MOUSEMOVE messages).  Queue one paint transaction instead of synchronously forcing every
    // message through WM_PAINT; USER32 can then coalesce the update without an intermediate frame.
    let _ = RedrawWindow(
        control,
        None,
        None,
        RDW_FRAME | RDW_INVALIDATE | RDW_NOERASE,
    );
}

/// Starts leave tracking and records the hot marker. Returns true only for the transition into
/// the hot state, so callers repaint once per enter instead of once per mouse message.
unsafe fn begin_hot_tracking(hwnd: HWND, property: PCWSTR, non_client: bool) -> bool {
    if !GetPropW(hwnd, property).is_invalid() {
        return false;
    }
    let mut tracking = TRACKMOUSEEVENT {
        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE
            | if non_client {
                TME_NONCLIENT
            } else {
                Default::default()
            },
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    TrackMouseEvent(&mut tracking).is_ok()
        && SetPropW(hwnd, property, HANDLE(std::ptr::dangling_mut())).is_ok()
}

unsafe fn ensure_hot_tracking(hwnd: HWND, property: PCWSTR, non_client: bool) -> bool {
    let entered = begin_hot_tracking(hwnd, property, non_client);
    if entered {
        redraw_control_frame(hwnd);
    }
    entered
}

/// Removes the hot marker. Returns true only for the transition out of the hot state.
unsafe fn end_hot_tracking(hwnd: HWND, property: PCWSTR) -> bool {
    RemovePropW(hwnd, property).is_ok_and(|handle| !handle.is_invalid())
}

unsafe fn clear_hot_tracking(hwnd: HWND, property: PCWSTR) -> bool {
    let left = end_hot_tracking(hwnd, property);
    if left {
        redraw_control_frame(hwnd);
    }
    left
}

const BS_NOTIFY_STYLE: isize = 0x4000;
const WM_SETREDRAW_MESSAGE: u32 = 0x000b;
const WM_NCCALCSIZE_MESSAGE: u32 = 0x0083;
const WM_UPDATEUISTATE_MESSAGE: u32 = 0x0128;
const WM_PRINTCLIENT_MESSAGE: u32 = 0x0318;
const BM_SETSTATE_MESSAGE: u32 = 0x00f3;

/// Lets USER32/comctl32 change a custom-painted BUTTON's state (check, push, focus, enable, UI
/// state, caption) without drawing its stock glyph straight to the screen.
///
/// The stock BUTTON paints these changes immediately through GetDC instead of invalidating, so
/// before this our own glyph only replaced the native one on the next WM_PAINT: every click and
/// every programmatic BM_SETCHECK briefly showed the system checkbox (light in dark mode). The
/// change runs with drawing disabled and our own appearance of the new state is then published.
/// WM_SETREDRAW(TRUE) sets WS_VISIBLE, so hidden or invisible buttons are never toggled; those
/// cannot draw anyway. BS_NOTIFY buttons are left alone because they notify their parent from
/// these messages.
unsafe fn run_button_state_change_without_native_paint(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
    let visible_bit = windows::Win32::UI::WindowsAndMessaging::WS_VISIBLE.0 as isize;
    let suppress =
        style & visible_bit != 0 && style & BS_NOTIFY_STYLE == 0 && IsWindowVisible(hwnd).as_bool();
    if !suppress {
        let result = DefSubclassProc(hwnd, message, wparam, lparam);
        invalidate_control_visual(hwnd);
        return result;
    }
    let _ = DefSubclassProc(hwnd, WM_SETREDRAW_MESSAGE, WPARAM(0), LPARAM(0));
    let result = DefSubclassProc(hwnd, message, wparam, lparam);
    if !windows::Win32::UI::WindowsAndMessaging::IsWindow(hwnd).as_bool() {
        return result;
    }
    let _ = DefSubclassProc(hwnd, WM_SETREDRAW_MESSAGE, WPARAM(1), LPARAM(0));
    // Pointer and keyboard feedback is published at once; a programmatic change (page switch,
    // configuration load) joins the normal paint queue and is painted once with its neighbours.
    let interactive =
        windows::Win32::UI::Input::KeyboardAndMouse::GetCapture() == hwnd || GetFocus() == hwnd;
    let _ = RedrawWindow(
        hwnd,
        None,
        None,
        RDW_INVALIDATE
            | RDW_NOERASE
            | if interactive {
                RDW_UPDATENOW
            } else {
                Default::default()
            },
    );
    result
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CheckBoxGeometry {
    glyph: RECT,
    text: RECT,
}

fn check_box_geometry(width: i32, height: i32, dpi: u32) -> Option<CheckBoxGeometry> {
    let width = width.max(0);
    let height = height.max(0);
    if width == 0 || height == 0 {
        return None;
    }
    let size = scale(13, dpi).max(1).min(width).min(height);
    let top = (height - size) / 2;
    Some(CheckBoxGeometry {
        glyph: RECT {
            left: 0,
            top,
            right: size,
            bottom: top + size,
        },
        text: RECT {
            left: (size + scale(5, dpi)).min(width),
            top: 0,
            right: width,
            bottom: height,
        },
    })
}

unsafe extern "system" fn check_box_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    const BM_GETCHECK_MESSAGE: u32 = 0x00f0;
    const BM_SETCHECK_MESSAGE: u32 = 0x00f1;
    const BM_GETSTATE_MESSAGE: u32 = 0x00f2;
    const BST_CHECKED_VALUE: isize = 0x0001;
    const BST_PUSHED_VALUE: isize = 0x0004;

    match message {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let checked = SendMessageW(hwnd, BM_GETCHECK_MESSAGE, WPARAM(0), LPARAM(0)).0
                == BST_CHECKED_VALUE;
            let button_state = SendMessageW(hwnd, BM_GETSTATE_MESSAGE, WPARAM(0), LPARAM(0)).0;
            paint_check_box(
                hwnd,
                palette_from_reference(reference_data),
                ControlState {
                    hot: !GetPropW(hwnd, CHECK_BOX_HOT_PROPERTY).is_invalid(),
                    pressed: button_state & BST_PUSHED_VALUE != 0,
                    disabled: !IsWindowEnabled(hwnd).as_bool(),
                    focused: GetFocus() == hwnd,
                },
                checked,
            );
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            ensure_hot_tracking(hwnd, CHECK_BOX_HOT_PROPERTY, false);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_MOUSELEAVE_MESSAGE => {
            clear_hot_tracking(hwnd, CHECK_BOX_HOT_PROPERTY);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        BM_SETCHECK_MESSAGE
        | BM_SETSTATE_MESSAGE
        | WM_SETFOCUS
        | WM_KILLFOCUS
        | WM_ENABLE
        | WM_UPDATEUISTATE_MESSAGE
        | WM_SETTEXT => {
            if message == WM_ENABLE && wparam.0 == 0 {
                let _ = RemovePropW(hwnd, CHECK_BOX_HOT_PROPERTY);
            }
            run_button_state_change_without_native_paint(hwnd, message, wparam, lparam)
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_KEYDOWN | WM_KEYUP | WM_CAPTURECHANGED
        | WM_THEMECHANGED => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            invalidate_control_visual(hwnd);
            result
        }
        WM_CANCELMODE => {
            let _ = RemovePropW(hwnd, CHECK_BOX_HOT_PROPERTY);
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            let _ = InvalidateRect(hwnd, None, false);
            result
        }
        WM_NCDESTROY => {
            let _ = RemovePropW(hwnd, CHECK_BOX_HOT_PROPERTY);
            let _ = RemoveWindowSubclass(hwnd, Some(check_box_subclass), CHECK_BOX_SUBCLASS_ID);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

unsafe fn paint_check_box(hwnd: HWND, palette: Palette, state: ControlState, checked: bool) {
    paint_embedded_windows11_checkbox(hwnd, palette, state, checked);
}

#[derive(Clone, Copy)]
struct EmbeddedButtonGlyph {
    width: i32,
    height: i32,
    bgra: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/win11_button_theme.rs"));

const fn embedded_theme_dpi_index(dpi: u32) -> usize {
    if dpi < 108 {
        0
    } else if dpi < 132 {
        1
    } else if dpi < 168 {
        2
    } else {
        3
    }
}

const fn themed_button_state(state: ControlState, checked: bool) -> usize {
    let base = if checked { 4 } else { 0 };
    base + if state.disabled {
        3
    } else if state.pressed {
        2
    } else if state.hot {
        1
    } else {
        0
    }
}

fn embedded_button_glyph(
    dark: bool,
    dpi: u32,
    state: ControlState,
    checked: bool,
) -> &'static EmbeddedButtonGlyph {
    let mode = usize::from(dark);
    let dpi = embedded_theme_dpi_index(dpi);
    let state = themed_button_state(state, checked);
    &WIN11_CHECKBOX_THEME_GLYPHS[((mode * 4 + dpi) * 8) + state]
}

unsafe fn draw_embedded_button_glyph(
    dc: HDC,
    rect: RECT,
    glyph: &EmbeddedButtonGlyph,
    background: COLORREF,
) {
    let width = (rect.right - rect.left).max(0);
    let height = (rect.bottom - rect.top).max(0);
    if width == 0 || height == 0 || glyph.width <= 0 || glyph.height <= 0 {
        return;
    }
    let background = background.0;
    let background_red = background & 0xff;
    let background_green = (background >> 8) & 0xff;
    let background_blue = (background >> 16) & 0xff;
    let mut composed = Vec::with_capacity(glyph.bgra.len());
    for pixel in glyph.bgra.chunks_exact(4) {
        let alpha = u32::from(pixel[3]);
        let inverse = 255 - alpha;
        // UxTheme's buffered BUTTON renders are premultiplied BGRA. Multiplying the stored RGB a
        // second time darkens the partially covered corner pixels, which is why an unchecked box
        // showed four dark feet on a light page. Fully transparent PNG pixels carry an arbitrary
        // white RGB value, so treat alpha=0 as the destination background explicitly.
        let compose = |source: u8, destination: u32| {
            if alpha == 0 {
                destination
            } else {
                (u32::from(source) + (destination * inverse + 127) / 255).min(255)
            }
        };
        let blue = compose(pixel[0], background_blue);
        let green = compose(pixel[1], background_green);
        let red = compose(pixel[2], background_red);
        composed.extend_from_slice(&[blue as u8, green as u8, red as u8, 255]);
    }

    let bitmap = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: glyph.width,
            // Negative height declares the generated BGRA rows as top-down.
            biHeight: -glyph.height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (glyph.width * glyph.height * 4) as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    if width == glyph.width && height == glyph.height {
        // Every supported DPI bucket is generated at its final physical size.  Use the
        // non-scaling DIB path so GDI cannot sample the transparent corner texels back into the
        // rounded Win11 glyph (which showed up as four dark pixels on a light page).
        let _ = SetDIBitsToDevice(
            dc,
            rect.left,
            rect.top,
            width as u32,
            height as u32,
            0,
            0,
            0,
            glyph.height as u32,
            composed.as_ptr().cast(),
            &bitmap,
            DIB_RGB_COLORS,
        );
    } else {
        let _ = SetStretchBltMode(dc, HALFTONE);
        let _ = StretchDIBits(
            dc,
            rect.left,
            rect.top,
            width,
            height,
            0,
            0,
            glyph.width,
            glyph.height,
            Some(composed.as_ptr().cast()),
            &bitmap,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
    }
}

/// USER32 continues to own the real checkbox state machine, keyboard handling, accessibility and
/// BN_CLICKED semantics. Only the visible checkbox glyph comes from the fixed Windows 11
/// `Aero.msstyles` reference, so Win10 and Win11 do not silently select different host themes.
unsafe fn paint_embedded_windows11_checkbox(
    hwnd: HWND,
    palette: Palette,
    state: ControlState,
    checked: bool,
) {
    let mut paint = PAINTSTRUCT::default();
    let target_dc = BeginPaint(hwnd, &mut paint);
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    // The whole client is filled below: compose only the invalid part, without a read-back.
    let buffer = super::redraw::PaintBuffer::begin_opaque(target_dc, client, paint.rcPaint);
    let dc = buffer.dc();
    fill(dc, &client, palette.window);
    let dpi = GetDpiForWindow(hwnd).max(96);
    let width = (client.right - client.left).max(0);
    let height = (client.bottom - client.top).max(0);
    if width > 0 && height > 0 {
        if let Some(geometry) = check_box_geometry(width, height, dpi) {
            let (glyph_rect, caption_rect) = (geometry.glyph, geometry.text);
            draw_embedded_button_glyph(
                dc,
                glyph_rect,
                embedded_button_glyph(palette.dark, dpi, state, checked),
                palette.window,
            );

            let text_length = GetWindowTextLengthW(hwnd).max(0) as usize;
            if text_length > 0 && caption_rect.right > caption_rect.left {
                let mut text = vec![0u16; text_length + 1];
                let copied = GetWindowTextW(hwnd, &mut text).max(0) as usize;
                text.truncate(copied);
                let font = SendMessageW(hwnd, WM_GETFONT, WPARAM(0), LPARAM(0));
                let old_font = (font.0 != 0).then(|| {
                    SelectObject(dc, windows::Win32::Graphics::Gdi::HGDIOBJ(font.0 as *mut _))
                });
                let mut text_rect = caption_rect;
                let flags = DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX;
                let color = if state.disabled {
                    palette.text_disabled
                } else {
                    palette.text
                };
                draw_native_text(dc, &text, &mut text_rect, flags, color);
                if let Some(old_font) = old_font {
                    let _ = SelectObject(dc, old_font);
                }
            }
        }
    }
    buffer.present();
    drop(buffer);
    let _ = EndPaint(hwnd, &paint);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RadioGeometry {
    glyph: RECT,
    text: RECT,
}

/// Computes the complete radio geometry from the real client rectangle. The separate left/right
/// halves keep odd, DPI-scaled glyphs centred without allowing the last pixel to escape the HWND.
fn radio_geometry(width: i32, height: i32, dpi: u32) -> Option<RadioGeometry> {
    let width = width.max(0);
    let height = height.max(0);
    if width == 0 || height == 0 {
        return None;
    }
    // The fixed Inno reference is roughly 19-20 physical pixels at 150% scaling.  A 13px
    // logical baseline matches that footprint; 18px made the glyph a conspicuous 27px disc.
    let preferred = scale(13, dpi).max(1);
    let glyph_size = preferred.min(height).min(width);
    let glyph_top = (height - glyph_size) / 2;
    let glyph = RECT {
        left: 0,
        top: glyph_top,
        right: glyph_size,
        bottom: glyph_top + glyph_size,
    };
    let text_left = (glyph.right + scale(5, dpi)).min(width);
    Some(RadioGeometry {
        glyph,
        text: RECT {
            left: text_left,
            top: 0,
            right: width,
            bottom: height,
        },
    })
}

/// Returns the fixed Windows 11 radio colours for the real USER32 state. The old captured PNGs
/// were already composited against a theme surface and exposed only binary alpha, so their dark
/// edge texels could never be recomposited correctly on the light page. Keep the audited colours,
/// but generate true coverage at the final physical size instead of stretching those captures.
fn radio_state_colors(
    palette: Palette,
    state: ControlState,
    checked: bool,
) -> (COLORREF, COLORREF) {
    if checked {
        let fill = if palette.dark {
            if state.disabled {
                rgb(74, 74, 74)
            } else if state.pressed {
                rgb(85, 172, 212)
            } else if state.hot {
                rgb(91, 189, 233)
            } else {
                rgb(96, 205, 255)
            }
        } else if state.disabled {
            rgb(195, 195, 195)
        } else if state.pressed {
            rgb(50, 126, 197)
        } else if state.hot {
            rgb(25, 110, 191)
        } else {
            palette.accent_fill
        };
        let centre = if palette.dark {
            rgb(0, 0, 0)
        } else {
            rgb(255, 255, 255)
        };
        (fill, centre)
    } else if palette.dark {
        if state.disabled {
            (rgb(74, 74, 74), rgb(55, 55, 55))
        } else if state.pressed {
            (rgb(69, 69, 69), rgb(50, 50, 50))
        } else if state.hot {
            (rgb(170, 170, 170), rgb(49, 49, 49))
        } else {
            (rgb(170, 170, 170), rgb(36, 36, 36))
        }
    } else if state.disabled {
        (rgb(195, 195, 195), palette.window)
    } else if state.pressed {
        (rgb(195, 195, 195), rgb(226, 226, 226))
    } else if state.hot {
        (rgb(98, 98, 98), rgb(234, 234, 234))
    } else {
        (rgb(98, 98, 98), rgb(243, 243, 243))
    }
}

fn weighted_radio_color(
    background: COLORREF,
    background_samples: u32,
    primary: COLORREF,
    primary_samples: u32,
    secondary: COLORREF,
    secondary_samples: u32,
) -> COLORREF {
    let total = background_samples + primary_samples + secondary_samples;
    let channel = |shift: u32| {
        ((((background.0 >> shift) & 0xff) * background_samples
            + ((primary.0 >> shift) & 0xff) * primary_samples
            + ((secondary.0 >> shift) & 0xff) * secondary_samples
            + total / 2)
            / total)
            << shift
    };
    COLORREF(channel(0) | channel(8) | channel(16))
}

/// Produces a top-down opaque BGRA glyph using eight-by-eight coverage at the final DPI size.
fn radio_glyph_bgra(side: i32, palette: Palette, state: ControlState, checked: bool) -> Vec<u8> {
    const SAMPLES: i32 = 8;
    let side = side.max(1);
    let centre = f64::from(side) / 2.0;
    let outer_radius = centre;
    let ring_width = (f64::from(side) / 13.0).max(1.0);
    let inner_radius = (outer_radius - ring_width).max(0.0);
    let dot_radius = f64::from(side) * 2.5 / 13.0;
    let (primary, secondary) = radio_state_colors(palette, state, checked);
    let mut pixels = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            let mut background_samples = 0u32;
            let mut primary_samples = 0u32;
            let mut secondary_samples = 0u32;
            for sample_y in 0..SAMPLES {
                for sample_x in 0..SAMPLES {
                    let px = f64::from(x) + (f64::from(sample_x) + 0.5) / f64::from(SAMPLES);
                    let py = f64::from(y) + (f64::from(sample_y) + 0.5) / f64::from(SAMPLES);
                    let dx = px - centre;
                    let dy = py - centre;
                    let distance_squared = dx * dx + dy * dy;
                    if distance_squared > outer_radius * outer_radius {
                        background_samples += 1;
                    } else if checked {
                        if distance_squared <= dot_radius * dot_radius {
                            secondary_samples += 1;
                        } else {
                            primary_samples += 1;
                        }
                    } else if distance_squared >= inner_radius * inner_radius {
                        primary_samples += 1;
                    } else {
                        secondary_samples += 1;
                    }
                }
            }
            let color = weighted_radio_color(
                palette.window,
                background_samples,
                primary,
                primary_samples,
                secondary,
                secondary_samples,
            )
            .0;
            pixels.extend_from_slice(&[
                ((color >> 16) & 0xff) as u8,
                ((color >> 8) & 0xff) as u8,
                (color & 0xff) as u8,
                255,
            ]);
        }
    }
    pixels
}

unsafe fn draw_radio_glyph(
    dc: HDC,
    rect: RECT,
    palette: Palette,
    state: ControlState,
    checked: bool,
) {
    let side = (rect.right - rect.left)
        .max(0)
        .min((rect.bottom - rect.top).max(0));
    if side == 0 {
        return;
    }
    let pixels = radio_glyph_bgra(side, palette, state, checked);
    let bitmap = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: side,
            biHeight: -side,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (side * side * 4) as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    let _ = SetDIBitsToDevice(
        dc,
        rect.left,
        rect.top,
        side as u32,
        side as u32,
        0,
        0,
        0,
        side as u32,
        pixels.as_ptr().cast(),
        &bitmap,
        DIB_RGB_COLORS,
    );
}

unsafe extern "system" fn radio_button_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    const BM_GETCHECK_MESSAGE: u32 = 0x00f0;
    const BM_SETCHECK_MESSAGE: u32 = 0x00f1;
    const BM_GETSTATE_MESSAGE: u32 = 0x00f2;
    const BST_CHECKED_VALUE: isize = 0x0001;
    const BST_PUSHED_VALUE: isize = 0x0004;
    const WM_MOUSELEAVE_LOCAL: u32 = 0x02a3;

    match message {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let palette = palette_from_reference(reference_data);
            let checked = SendMessageW(hwnd, BM_GETCHECK_MESSAGE, WPARAM(0), LPARAM(0)).0
                == BST_CHECKED_VALUE;
            let button_state = SendMessageW(hwnd, BM_GETSTATE_MESSAGE, WPARAM(0), LPARAM(0)).0;
            let state = ControlState {
                hot: !GetPropW(hwnd, RADIO_BUTTON_HOT_PROPERTY).is_invalid(),
                pressed: button_state & BST_PUSHED_VALUE != 0,
                disabled: !IsWindowEnabled(hwnd).as_bool(),
                focused: GetFocus() == hwnd,
            };
            paint_radio_button(hwnd, palette, state, checked);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            ensure_hot_tracking(hwnd, RADIO_BUTTON_HOT_PROPERTY, false);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_MOUSELEAVE_LOCAL => {
            clear_hot_tracking(hwnd, RADIO_BUTTON_HOT_PROPERTY);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        BM_SETCHECK_MESSAGE
        | BM_SETSTATE_MESSAGE
        | WM_SETFOCUS
        | WM_KILLFOCUS
        | WM_ENABLE
        | WM_UPDATEUISTATE_MESSAGE
        | WM_SETTEXT => {
            if message == WM_ENABLE && wparam.0 == 0 {
                let _ = RemovePropW(hwnd, RADIO_BUTTON_HOT_PROPERTY);
            }
            run_button_state_change_without_native_paint(hwnd, message, wparam, lparam)
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_KEYDOWN | WM_KEYUP | WM_CAPTURECHANGED
        | WM_THEMECHANGED => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            invalidate_control_visual(hwnd);
            result
        }
        WM_NCDESTROY => {
            let _ = RemovePropW(hwnd, RADIO_BUTTON_HOT_PROPERTY);
            let _ =
                RemoveWindowSubclass(hwnd, Some(radio_button_subclass), RADIO_BUTTON_SUBCLASS_ID);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

unsafe fn paint_radio_button(hwnd: HWND, palette: Palette, state: ControlState, checked: bool) {
    let mut paint = PAINTSTRUCT::default();
    let target_dc = BeginPaint(hwnd, &mut paint);
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    let buffer = super::redraw::PaintBuffer::begin_opaque(target_dc, client, paint.rcPaint);
    let dc = buffer.dc();
    fill(dc, &client, palette.window);
    let dpi = GetDpiForWindow(hwnd).max(96);
    let width = (client.right - client.left).max(0);
    let height = (client.bottom - client.top).max(0);
    if let Some(geometry) = radio_geometry(width, height, dpi) {
        draw_radio_glyph(dc, geometry.glyph, palette, state, checked);

        let text_length = GetWindowTextLengthW(hwnd).max(0) as usize;
        if text_length > 0 && geometry.text.right > geometry.text.left {
            let mut text = vec![0u16; text_length + 1];
            let copied = GetWindowTextW(hwnd, &mut text).max(0) as usize;
            text.truncate(copied);
            let font = SendMessageW(hwnd, WM_GETFONT, WPARAM(0), LPARAM(0));
            let old_font = (font.0 != 0).then(|| {
                SelectObject(dc, windows::Win32::Graphics::Gdi::HGDIOBJ(font.0 as *mut _))
            });
            let mut text_rect = geometry.text;
            let flags = DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX;
            let color = if state.disabled {
                palette.text_disabled
            } else {
                palette.text
            };
            draw_native_text(dc, &text, &mut text_rect, flags, color);
            if let Some(old_font) = old_font {
                let _ = SelectObject(dc, old_font);
            }
        }
    }
    buffer.present();
    drop(buffer);
    let _ = EndPaint(hwnd, &paint);
}

unsafe extern "system" fn header_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    match message {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint_header(hwnd, palette_from_reference(reference_data));
            LRESULT(0)
        }
        WM_THEMECHANGED => {
            let _ = InvalidateRect(hwnd, None, false);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(header_subclass), HEADER_SUBCLASS_ID);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

const HDM_LAYOUT_MESSAGE: u32 = 0x1205;

/// Height of the report's upper frame band that continues the header, minus the outline itself.
unsafe fn header_band_overlap(list: HWND) -> i32 {
    if list.is_invalid() || !control_class_name(list).eq_ignore_ascii_case("SysListView32") {
        return 0;
    }
    let mut window = RECT::default();
    if GetWindowRect(list, &mut window).is_err() {
        return 0;
    }
    frame_band_insets(list, window.right - window.left, window.bottom - window.top)
        .map_or(0, |(side, band)| (band - side).max(0))
}

/// Captions are centred on the whole visible header block (band plus header window), as before
/// the band existed, as far as the header window leaves room above the text.
unsafe fn header_caption_lift(header: HWND, dc: HDC, height: i32) -> i32 {
    let overlap = GetParent(header).map_or(0, |list| header_band_overlap(list));
    if overlap <= 0 {
        return 0;
    }
    let mut metrics = windows::Win32::Graphics::Gdi::TEXTMETRICW::default();
    if !windows::Win32::Graphics::Gdi::GetTextMetricsW(dc, &mut metrics).as_bool() {
        return 0;
    }
    (overlap / 2).min(((height - metrics.tmHeight) / 2).max(0))
}

unsafe fn paint_header(hwnd: HWND, palette: Palette) {
    let mut paint = PAINTSTRUCT::default();
    let target = BeginPaint(hwnd, &mut paint);
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    // The band background, captions and separators are composed off-screen: painting them to the
    // screen one after another blanked every caption for a frame whenever the report was resized.
    let buffer = super::redraw::PaintBuffer::begin_opaque(target, client, paint.rcPaint);
    let dc = buffer.dc();
    fill(dc, &client, palette.button);
    let font = SendMessageW(hwnd, WM_GETFONT, WPARAM(0), LPARAM(0));
    let old_font = if font.0 != 0 {
        Some(SelectObject(
            dc,
            windows::Win32::Graphics::Gdi::HGDIOBJ(font.0 as *mut _),
        ))
    } else {
        None
    };
    let _ = SetBkMode(dc, TRANSPARENT);
    let _ = SetTextColor(dc, palette.text);
    let dpi = GetDpiForWindow(hwnd).max(96);
    let inset = scale(8, dpi);
    let count = SendMessageW(hwnd, 0x1200, WPARAM(0), LPARAM(0)).0 as i32; // HDM_GETITEMCOUNT
    for index in 0..count.max(0) {
        let mut rect = RECT::default();
        if SendMessageW(
            hwnd,
            0x1207, // HDM_GETITEMRECT
            WPARAM(index as usize),
            LPARAM((&mut rect as *mut RECT) as isize),
        )
        .0 == 0
        {
            continue;
        }
        let mut text = vec![0u16; 256];
        let mut item = HDITEMW {
            mask: HDI_TEXT,
            pszText: windows::core::PWSTR(text.as_mut_ptr()),
            cchTextMax: text.len() as i32,
            ..Default::default()
        };
        let _ = SendMessageW(
            hwnd,
            0x120B, // HDM_GETITEMW
            WPARAM(index as usize),
            LPARAM((&mut item as *mut HDITEMW) as isize),
        );
        text.truncate(
            text.iter()
                .position(|value| *value == 0)
                .unwrap_or(text.len()),
        );
        let mut text_rect = rect;
        text_rect.left += inset;
        text_rect.right -= inset.min((text_rect.right - text_rect.left).max(0));
        draw_native_text(
            dc,
            &text,
            &mut text_rect,
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
            palette.text,
        );
        let separator = RECT {
            left: rect.right - 1,
            top: rect.top + scale(4, dpi),
            right: rect.right,
            bottom: rect.bottom - scale(4, dpi),
        };
        fill(dc, &separator, palette.separator);
    }
    if let Some(old_font) = old_font {
        let _ = SelectObject(dc, old_font);
    }
    buffer.present();
    drop(buffer);
    let _ = EndPaint(hwnd, &paint);
}

unsafe extern "system" fn list_view_frame_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    match message {
        WM_NCHITTEST => LRESULT(-1), // HTTRANSPARENT: preserve native ListView hit testing.
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            // Validate, then publish the complete hollow frame in one composed BitBlt. The former
            // sequence (client fill, then frame fill, arc, outline and header rails straight to
            // the screen) was visible as a flashing edge whenever the report repainted.
            let mut paint = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut paint);
            let _ = EndPaint(hwnd, &paint);
            paint_list_view_frame(hwnd, palette_from_reference(reference_data));
            LRESULT(0)
        }
        WM_ENABLE | WM_SIZE | WM_THEMECHANGED => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            let _ = InvalidateRect(hwnd, None, false);
            result
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(
                hwnd,
                Some(list_view_frame_subclass),
                LIST_VIEW_FRAME_SUBCLASS_ID,
            );
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

unsafe fn paint_list_view_frame(frame: HWND, palette: Palette) {
    let dpi = GetDpiForWindow(frame).max(96);
    // A frame whose owner has no visible column header (or a category list) is the plain rounded
    // outline over the report surface.
    let header = list_view_frame_owner(frame).and_then(|list| {
        let style = GetWindowLongPtrW(list, GWL_STYLE);
        if is_category_list_view(style) {
            // Category selection is an inset client-area surface. It must never become the
            // interior colour of the sibling frame: when WS_VSCROLL is present, the upper-right
            // frame corner meets the scrollbar rather than the selected item.
            return None;
        }
        let header = HWND(SendMessageW(list, 0x101f, WPARAM(0), LPARAM(0)).0 as *mut _);
        (!header.is_invalid() && IsWindowVisible(header).as_bool()).then_some(header)
    });
    let mut window = RECT::default();
    if GetWindowRect(frame, &mut window).is_err() {
        return;
    }
    let mut header_window = RECT::default();
    let header_window = header
        .filter(|header| GetWindowRect(*header, &mut header_window).is_ok())
        .map(|_| header_window);
    let scrollbar_column = list_view_frame_owner(frame)
        .and_then(|list| list_view_scrollbar_column(list, window, palette));
    let interior = list_view_frame_owner(frame)
        .filter(|owner| is_edit_class(&control_class_name(*owner)))
        .map_or(palette.edit, |edit| edit_surface_color(edit, palette));
    super::redraw::ui_detail_changed(frame, "listview-frame", || {
        format!(
            "window={:?} header={:?} scrollbar_column={:?}",
            window, header_window, scrollbar_column
        )
    });
    super::redraw::paint_window_buffered(frame, |dc, rect| {
        let (width, height) = (rect.right, rect.bottom);
        fill(dc, &rect, interior);
        let Some(geometry) = rounded_control_frame_geometry(width, height, dpi) else {
            return;
        };
        if header.is_none() {
            draw_antialiased_control_frame(
                dc,
                rect,
                geometry,
                interior,
                palette.control_border(),
                rounded_control_exterior(palette),
            );
            recolor_scrollbar_corners(dc, rect, geometry, scrollbar_column, palette);
            return;
        }
        // The overlay owns only the hollow frame region.  Prime its complete upper arc with the
        // adjacent header surface before drawing the outline so fully-interior corner samples
        // cannot retain the ListView row colour.  The lower frame continues to meet the row
        // surface.
        fill(
            dc,
            &RECT {
                left: 0,
                top: 0,
                right: width,
                bottom: geometry.arc_band.min(height),
            },
            palette.button,
        );
        draw_antialiased_control_frame_with_vertical_interiors(
            dc,
            rect,
            geometry,
            palette.button,
            palette.edit,
            palette.control_border(),
            rounded_control_exterior(palette),
        );
        if let Some(header_band) = header_window.and_then(|header_window| {
            list_view_frame_header_band(window, header_window, width, height, geometry.arc_band)
        }) {
            // The sibling's hollow region is two or more pixels wide at scaled DPI. Painting
            // that entire region with the report body colour and then adding a vertical outline
            // leaves a dark rail on both sides of the independently painted header. Publish the
            // header surface through the same clipped overlay after the outline instead: only the
            // overlay's side strips are touched, while the fixed upper rounded arc remains above
            // `header_band.top` and scrolling still cannot copy frame pixels into the report.
            fill(dc, &header_band, palette.button);
            for side_border in list_view_frame_header_side_borders(header_band, geometry.side_band)
            {
                fill(dc, &side_border, palette.control_border());
            }
        }
        recolor_scrollbar_corners(dc, rect, geometry, scrollbar_column, palette);
    });
}

/// Where the report's vertical scrollbar lies inside its frame, and the colours USER32 paints it
/// with (upper arrow, track, lower arrow), sampled from an off-screen WM_PRINT of the report's
/// non-client area along the scrollbar's outer edge, away from the thumb.
#[derive(Clone, Copy, Debug)]
struct ScrollbarColumn {
    left: i32,
    top_end: i32,
    bottom_start: i32,
    top: COLORREF,
    track: COLORREF,
    bottom: COLORREF,
}

/// Sampled scrollbar colours (upper arrow, track, lower arrow), keyed by (report window,
/// dark theme, scrollbar width).
type ScrollbarColorCacheEntry = ((isize, bool, i32), (COLORREF, COLORREF, COLORREF));

thread_local! {
    static SCROLLBAR_COLOR_CACHE: std::cell::RefCell<Vec<ScrollbarColorCacheEntry>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Whether a settings broadcast concerns colours or theme. WM_SETTINGCHANGE also arrives for
/// unrelated changes (environment, work area, input settings...), and re-theming every window
/// for those is expensive; the light/dark switch arrives as "ImmersiveColorSet".
pub(crate) unsafe fn settings_change_affects_theme(
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> bool {
    const SPI_SETHIGHCONTRAST_VALUE: usize = 0x0043;
    if message != windows::Win32::UI::WindowsAndMessaging::WM_SETTINGCHANGE {
        return true;
    }
    if wparam.0 == SPI_SETHIGHCONTRAST_VALUE {
        return true;
    }
    if lparam.0 == 0 {
        return false;
    }
    let area = PCWSTR(lparam.0 as *const u16)
        .to_string()
        .unwrap_or_default();
    ["ImmersiveColorSet", "WindowsThemeElement", "WindowMetrics"]
        .iter()
        .any(|name| area.eq_ignore_ascii_case(name))
}

unsafe fn list_view_scrollbar_column(
    list: HWND,
    frame_window: RECT,
    palette: Palette,
) -> Option<ScrollbarColumn> {
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, GetDC, GetPixel,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetScrollBarInfo, OBJID_VSCROLL, SCROLLBARINFO};
    const WM_PRINT_MESSAGE: u32 = 0x0317;
    const PRF_NONCLIENT_FLAG: isize = 0x0002;
    let mut info = SCROLLBARINFO {
        cbSize: std::mem::size_of::<SCROLLBARINFO>() as u32,
        ..Default::default()
    };
    GetScrollBarInfo(list, OBJID_VSCROLL, &mut info).ok()?;
    if info.rgstate[0] & (0x0000_8000 | 0x0001_0000) != 0 {
        return None;
    }
    let bar = info.rcScrollBar;
    let bar_width = bar.right - bar.left;
    if bar_width <= 2 || bar.bottom - bar.top < 4 {
        return None;
    }
    let arrow = bar_width.min((bar.bottom - bar.top) / 2);
    let mut list_window = RECT::default();
    GetWindowRect(list, &mut list_window).ok()?;
    let width = (list_window.right - list_window.left).max(1);
    let height = (list_window.bottom - list_window.top).max(1);
    let mut colors = (palette.edit, palette.edit, palette.edit);
    // Sampling renders the whole non-client area off-screen. Doing that on every frame repaint
    // made each live-resize step pay for it; the colours only change with the theme.
    let cache_key = (list.0 as isize, palette.dark, bar_width);
    let cached = SCROLLBAR_COLOR_CACHE.with(|cache| {
        cache
            .borrow()
            .iter()
            .find(|(key, _)| *key == cache_key)
            .map(|(_, colors)| *colors)
    });
    let screen = if let Some(cached) = cached {
        colors = cached;
        HDC::default()
    } else {
        GetDC(HWND::default())
    };
    if !screen.is_invalid() {
        let dc = CreateCompatibleDC(screen);
        let bitmap = CreateCompatibleBitmap(screen, width, height);
        let _ = ReleaseDC(HWND::default(), screen);
        if !dc.is_invalid() && !bitmap.is_invalid() {
            let previous = SelectObject(dc, bitmap);
            let _ = SendMessageW(
                list,
                WM_PRINT_MESSAGE,
                WPARAM(dc.0 as usize),
                LPARAM(PRF_NONCLIENT_FLAG),
            );
            // The outermost column but one: the thumb never reaches the scrollbar's edge.
            let x = bar.right - list_window.left - 2;
            let usable = |color: COLORREF| color.0 != 0xffff_ffff && color.0 != 0;
            let sample = |y: i32, fallback: COLORREF| {
                let color = GetPixel(dc, x, y - list_window.top);
                if usable(color) {
                    color
                } else {
                    fallback
                }
            };
            let top = sample(bar.top + 1, palette.edit);
            let bottom = sample(bar.bottom - 2, palette.edit);
            // The track colour is the most frequent of several samples along it.
            let mut votes: Vec<(u32, usize)> = Vec::new();
            let track_top = bar.top + arrow;
            let track_bottom = bar.bottom - arrow;
            for step in 1..=7 {
                let y = track_top + (track_bottom - track_top) * step / 8;
                let color = sample(y, top);
                match votes.iter_mut().find(|(value, _)| *value == color.0) {
                    Some((_, count)) => *count += 1,
                    None => votes.push((color.0, 1)),
                }
            }
            let track = votes
                .iter()
                .max_by_key(|(_, count)| *count)
                .map_or(top, |(value, _)| COLORREF(*value));
            colors = (top, track, bottom);
            SCROLLBAR_COLOR_CACHE.with(|cache| {
                let mut cache = cache.borrow_mut();
                cache.retain(|(key, _)| key.0 != cache_key.0);
                if cache.len() >= 64 {
                    cache.remove(0);
                }
                cache.push((cache_key, colors));
            });
            let _ = SelectObject(dc, previous);
        }
        if !bitmap.is_invalid() {
            let _ = DeleteObject(bitmap);
        }
        if !dc.is_invalid() {
            let _ = DeleteDC(dc);
        }
    }
    Some(ScrollbarColumn {
        left: bar.left - frame_window.left,
        top_end: bar.top - frame_window.top + arrow,
        bottom_start: bar.bottom - frame_window.top - arrow,
        top: colors.0,
        track: colors.1,
        bottom: colors.2,
    })
}

/// The frame ring beside the report's vertical scrollbar used to carry the header or row colour:
/// a light wedge in the upper rounded corner, a thin light strip along the scrollbar next to the
/// header and a block of another colour under it. Every ring pixel in the scrollbar's column now
/// takes the colour of the scrollbar part next to it, so the scrollbar looks cleanly clipped by the
/// rounded outline.
unsafe fn recolor_scrollbar_corners(
    dc: HDC,
    rect: RECT,
    geometry: super::controls::RoundedControlFrameGeometry,
    column: Option<ScrollbarColumn>,
    palette: Palette,
) {
    use windows::Win32::Graphics::Gdi::{IntersectClipRect, RestoreDC, SaveDC};
    let Some(column) = column else {
        return;
    };
    let top_end = column.top_end.clamp(rect.top, rect.bottom);
    let bottom_start = column.bottom_start.clamp(top_end, rect.bottom);
    for (top, bottom, color) in [
        (rect.top, top_end, column.top),
        (top_end, bottom_start, column.track),
        (bottom_start, rect.bottom, column.bottom),
    ] {
        let area = RECT {
            left: column.left.max(rect.left),
            top,
            right: rect.right,
            bottom,
        };
        if area.right <= area.left || area.bottom <= area.top {
            continue;
        }
        let saved = SaveDC(dc);
        let _ = IntersectClipRect(dc, area.left, area.top, area.right, area.bottom);
        fill(dc, &area, color);
        draw_antialiased_control_frame(
            dc,
            rect,
            geometry,
            color,
            palette.control_border(),
            rounded_control_exterior(palette),
        );
        if saved != 0 {
            let _ = RestoreDC(dc, saved);
        }
    }
}

fn list_view_frame_header_band(
    frame_window: RECT,
    header_window: RECT,
    frame_width: i32,
    frame_height: i32,
    arc_band: i32,
) -> Option<RECT> {
    if frame_width <= 0 || frame_height <= 0 {
        return None;
    }
    let top = (header_window.top - frame_window.top)
        .max(arc_band.max(0))
        .clamp(0, frame_height);
    let bottom = (header_window.bottom - frame_window.top).clamp(0, frame_height);
    (bottom > top).then_some(RECT {
        left: 0,
        top,
        right: frame_width,
        bottom,
    })
}

fn list_view_frame_header_side_borders(header_band: RECT, side_band: i32) -> [RECT; 2] {
    let side = side_band
        .max(1)
        .min((header_band.right - header_band.left).max(1));
    [
        RECT {
            right: header_band.left + side,
            ..header_band
        },
        RECT {
            left: header_band.right - side,
            ..header_band
        },
    ]
}

const WM_FIT_LIST_COLUMNS: u32 = 0x8000 + 0x4e5;
const LIST_FIT_PENDING_PROPERTY: PCWSTR = w!("LetRecovery.InnoListView.FitPending");

thread_local! {
    /// Width each report column needs for its header and its widest cell, per list.
    static LIST_COLUMN_NEEDS: std::cell::RefCell<Vec<(isize, Vec<i32>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Lists without a column header (category lists) keep the width their page gives them.
unsafe fn list_fits_columns(list: HWND) -> bool {
    const LVS_NOCOLUMNHEADER_STYLE: isize = 0x4000;
    GetWindowLongPtrW(list, GWL_STYLE) & LVS_NOCOLUMNHEADER_STYLE == 0
}

unsafe fn schedule_list_column_fit(list: HWND) {
    if !list_fits_columns(list) || !GetPropW(list, LIST_FIT_PENDING_PROPERTY).is_invalid() {
        return;
    }
    if SetPropW(
        list,
        LIST_FIT_PENDING_PROPERTY,
        HANDLE(std::ptr::dangling_mut()),
    )
    .is_ok()
    {
        let _ = PostMessageW(list, WM_FIT_LIST_COLUMNS, WPARAM(0), LPARAM(0));
    }
}

unsafe fn list_column_need(list: HWND, column: i32) -> i32 {
    let known = LIST_COLUMN_NEEDS.with(|needs| {
        needs
            .borrow()
            .iter()
            .any(|(key, _)| *key == list.0 as isize)
    });
    if !known {
        measure_list_column_needs(list);
    }
    LIST_COLUMN_NEEDS.with(|needs| {
        needs
            .borrow()
            .iter()
            .find(|(key, _)| *key == list.0 as isize)
            .and_then(|(_, columns)| columns.get(column.max(0) as usize).copied())
            .unwrap_or(0)
    })
}

/// Measures every header and cell text of a report and widens the columns that are too narrow
/// for them. Translations are often much longer than the Chinese text the column widths were
/// chosen for; a column is never narrower than its header and its widest cell. When the columns
/// together are wider than the list, the list scrolls horizontally instead of cutting text.
unsafe fn fit_list_view_columns(list: HWND) {
    if !list_fits_columns(list) {
        return;
    }
    let needs = measure_list_column_needs(list);
    const LVM_GETCOLUMNWIDTH_MESSAGE: u32 = 0x101d;
    const LVM_SETCOLUMNWIDTH_MESSAGE: u32 = 0x101e;
    for (column, need) in needs.into_iter().enumerate() {
        let current =
            SendMessageW(list, LVM_GETCOLUMNWIDTH_MESSAGE, WPARAM(column), LPARAM(0)).0 as i32;
        if current > 0 && current < need {
            let _ = SendMessageW(
                list,
                LVM_SETCOLUMNWIDTH_MESSAGE,
                WPARAM(column),
                LPARAM(need as isize),
            );
        }
    }
}

/// Measures the width every column needs (header and widest cell) and remembers it.
unsafe fn measure_list_column_needs(list: HWND) -> Vec<i32> {
    use windows::Win32::Graphics::Gdi::GetTextExtentPoint32W;
    use windows::Win32::UI::Controls::{HDITEMW, HDI_TEXT};
    const LVM_GETHEADER: u32 = 0x101f;
    const LVM_GETITEMCOUNT: u32 = 0x1004;
    const LVM_GETCOLUMNWIDTH: u32 = 0x101d;
    const LVM_SETCOLUMNWIDTH: u32 = 0x101e;
    const LVM_GETITEMTEXTW: u32 = 0x1073;
    const HDM_GETITEMCOUNT: u32 = 0x1200;
    const HDM_GETITEMW: u32 = 0x120b;
    let header = HWND(SendMessageW(list, LVM_GETHEADER, WPARAM(0), LPARAM(0)).0 as *mut _);
    if header.is_invalid() {
        return Vec::new();
    }
    let columns = SendMessageW(header, HDM_GETITEMCOUNT, WPARAM(0), LPARAM(0))
        .0
        .max(0) as i32;
    if columns == 0 {
        return Vec::new();
    }
    let rows = SendMessageW(list, LVM_GETITEMCOUNT, WPARAM(0), LPARAM(0))
        .0
        .clamp(0, 2000) as usize;
    let dpi = GetDpiForWindow(list).max(96);
    let dc = windows::Win32::Graphics::Gdi::GetDC(list);
    if dc.is_invalid() {
        return Vec::new();
    }
    let measure = |font_owner: HWND, text: &[u16]| -> i32 {
        let font = SendMessageW(font_owner, WM_GETFONT, WPARAM(0), LPARAM(0));
        let previous = (font.0 != 0)
            .then(|| SelectObject(dc, windows::Win32::Graphics::Gdi::HGDIOBJ(font.0 as *mut _)));
        let mut size = windows::Win32::Foundation::SIZE::default();
        let _ = GetTextExtentPoint32W(dc, text, &mut size);
        if let Some(previous) = previous {
            let _ = SelectObject(dc, previous);
        }
        size.cx
    };
    let checkbox_slot = if rows > 0 {
        list_view_label_left(list, 0)
            .map(|label| {
                let mut bounds = RECT::default();
                let _ = SendMessageW(
                    list,
                    0x100E,
                    WPARAM(0),
                    LPARAM((&mut bounds as *mut RECT) as isize),
                );
                (label - bounds.left).max(0)
            })
            .unwrap_or(0)
    } else {
        0
    };
    let padding = scale(16, dpi);
    let mut needs = Vec::with_capacity(columns as usize);
    for column in 0..columns {
        let mut text = vec![0u16; 260];
        let mut item = HDITEMW {
            mask: HDI_TEXT,
            pszText: PWSTR(text.as_mut_ptr()),
            cchTextMax: text.len() as i32,
            ..Default::default()
        };
        let mut need = 0;
        if SendMessageW(
            header,
            HDM_GETITEMW,
            WPARAM(column as usize),
            LPARAM((&mut item as *mut HDITEMW) as isize),
        )
        .0 != 0
        {
            let length = text.iter().position(|unit| *unit == 0).unwrap_or(0);
            if length > 0 {
                need = measure(header, &text[..length]) + padding + scale(4, dpi);
            }
        }
        for row in 0..rows {
            let mut cell = vec![0u16; 512];
            let mut item = LVITEMW {
                iSubItem: column,
                pszText: PWSTR(cell.as_mut_ptr()),
                cchTextMax: cell.len() as i32,
                ..Default::default()
            };
            let length = SendMessageW(
                list,
                LVM_GETITEMTEXTW,
                WPARAM(row),
                LPARAM((&mut item as *mut LVITEMW) as isize),
            )
            .0
            .clamp(0, 511) as usize;
            if length > 0 {
                let slot = if column == 0 { checkbox_slot } else { 0 };
                need = need.max(measure(list, &cell[..length]) + padding + slot);
            }
        }
        needs.push(need);
    }
    let _ = ReleaseDC(list, dc);
    LIST_COLUMN_NEEDS.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.retain(|(key, _)| *key != list.0 as isize);
        if cache.len() >= 256 {
            cache.remove(0);
        }
        cache.push((list.0 as isize, needs.clone()));
    });
    needs
}

unsafe extern "system" fn list_view_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    match message {
        // A column may be set narrower by a page layout; never below what its text needs. A
        // request for the width the column already has does nothing (layouts set every column
        // on every size step, and each real change makes the report recalculate).
        0x101e if (lparam.0 & 0xffff) as u16 as i16 > 0 => {
            let _profile = super::redraw::profile_scope("列表设置列宽");
            let requested = (lparam.0 & 0xffff) as u16 as i16 as i32;
            let width = if list_fits_columns(hwnd) {
                requested.max(list_column_need(hwnd, wparam.0 as i32))
            } else {
                requested
            };
            if SendMessageW(hwnd, 0x101d, wparam, LPARAM(0)).0 as i32 == width {
                return LRESULT(1);
            }
            DefSubclassProc(hwnd, message, wparam, LPARAM(width as isize))
        }
        WM_NCPAINT => {
            if super::redraw::layout_commit_active() {
                super::redraw::defer_frame_paint(hwnd);
                return LRESULT(0);
            }
            let _profile =
                super::redraw::paint_scope("列表边框/滚动条绘制", "(排版中)列表边框/滚动条绘制");
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        // Rows or columns changed: measure again once the current batch of messages is done.
        0x104d | 0x1074 | 0x104c | 0x1061 | 0x1060 | 0x1008 | 0x0030 => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            schedule_list_column_fit(hwnd);
            result
        }
        0x0047 => {
            let _profile = super::redraw::profile_scope("列表 WM_WINDOWPOSCHANGED");
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_FIT_LIST_COLUMNS => {
            let _ = RemovePropW(hwnd, LIST_FIT_PENDING_PROPERTY);
            fit_list_view_columns(hwnd);
            LRESULT(0)
        }
        // The complete report, background included, is composed off-screen in WM_PAINT. An erase
        // pass drawn straight to the screen blanked every row for a frame before each repaint.
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT if super::redraw::layout_commit_active() => {
            // Column width changes inside the layout commit made comctl32 repaint the report
            // synchronously, and the step painted it again afterwards. Validate without drawing
            // (an invalid region left behind made every following comctl32 update ask again) and
            // invalidate it after the commit, so the step's paint pass draws it once.
            let mut paint = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut paint);
            let _ = EndPaint(hwnd, &paint);
            super::redraw::defer_frame_paint(hwnd);
            LRESULT(0)
        }
        WM_PAINT => {
            let _profile = super::redraw::paint_scope("列表绘制", "(排版中)列表绘制");
            let palette = palette_from_reference(reference_data);
            let trace_start = super::redraw::trace_now();
            let mut paint = PAINTSTRUCT::default();
            let target = BeginPaint(hwnd, &mut paint);
            if list_view_hold_active(hwnd) {
                // A rebuild (delete all, then refill) is still running in the current message.
                // Keep what is on screen; the finished report is painted once when it is done.
                let _ = EndPaint(hwnd, &paint);
                return LRESULT(0);
            }
            let mut client = RECT::default();
            let _ = GetClientRect(hwnd, &mut client);
            // Compose the whole report off-screen and publish it with one BitBlt: background,
            // the native rows (WM_PRINTCLIENT runs comctl32's own paint routine, custom draw
            // included), the body below the last row and the checkbox glyphs. Doing those steps
            // on screen made every refresh flash: the stock white body below the rows and the
            // native light check boxes were visible until they were painted over.
            // Only the invalid part is composed: the memory DC's clip box is that area, so the
            // WM_PRINTCLIENT pass below lets comctl32 draw just the rows being published (a hover
            // repaints one row instead of the whole report). Everything is filled first, so no
            // read-back of the screen is needed.
            let buffer = super::redraw::PaintBuffer::begin_opaque(target, client, paint.rcPaint);
            let dc = buffer.dc();
            fill(dc, &client, palette.edit);
            let item_count = SendMessageW(hwnd, 0x1004, WPARAM(0), LPARAM(0)).0; // LVM_GETITEMCOUNT
            if !list_view_needs_empty_body_paint(item_count) {
                const WM_PRINTCLIENT_MESSAGE: u32 = 0x0318;
                const PRF_CLIENT_FLAG: isize = 0x0004;
                let _ = DefSubclassProc(
                    hwnd,
                    WM_PRINTCLIENT_MESSAGE,
                    WPARAM(dc.0 as usize),
                    LPARAM(PRF_CLIENT_FLAG),
                );
                paint_list_view_trailing_body(hwnd, dc, palette);
                paint_list_view_checkboxes(hwnd, dc, palette);
            }
            // The column header is a child window that paints itself; never copy the body
            // image over it (that is what used to require repainting the header after every
            // report paint, a flicker of its own).
            exclude_list_view_header(hwnd, target);
            buffer.present();
            drop(buffer);
            let _ = EndPaint(hwnd, &paint);
            // The hollow frame is a sibling above the report. Once the report clips its siblings
            // (ensure_list_view_frame sets WS_CLIPSIBLINGS) its paints can no longer cover the
            // frame, so the frame is not re-published after every row repaint.
            if GetWindowLongPtrW(hwnd, GWL_STYLE) & WS_CLIPSIBLINGS.0 as isize == 0 {
                if let Some(frame) = list_view_frame(hwnd) {
                    paint_list_view_frame(frame, palette);
                }
            }
            super::redraw::trace_list_view_paint(trace_start);
            LRESULT(0)
        }
        0x1009 => {
            // LVM_DELETEALLITEMS starts a rebuild: the caller refills the report in the same
            // message. Hold painting until that message is finished so the empty intermediate
            // state is never shown and the refilled report appears in a single paint.
            begin_list_view_hold(hwnd);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ if message != 0 && message == list_view_hold_release_message() => {
            end_list_view_hold(hwnd);
            LRESULT(0)
        }
        WM_ENABLE | WM_SETFOCUS | WM_KILLFOCUS | WM_SIZE | WM_THEMECHANGED => {
            let _profile =
                (message == WM_SIZE).then(|| super::redraw::profile_scope("列表 WM_SIZE"));
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            // Comctl32 can restore class-default colours while changing enabled/theme state.
            // Reassert all three ListView colours together; partial updates cause a white empty
            // body or black text background until the next full refresh.
            let palette = palette_from_reference(reference_data);
            set_list_view_colors(hwnd, palette);
            if matches!(message, WM_SIZE | WM_THEMECHANGED) {
                clear_legacy_control_region(hwnd);
            }
            if let Some(frame) = list_view_frame(hwnd) {
                let _ = InvalidateRect(frame, None, false);
            }
            let _ = InvalidateRect(hwnd, None, false);
            result
        }
        WM_NCDESTROY => {
            let _ =
                windows::Win32::UI::WindowsAndMessaging::RemovePropW(hwnd, LIST_VIEW_HOLD_PROPERTY);
            let _ = RemoveWindowSubclass(hwnd, Some(list_view_subclass), LIST_VIEW_SUBCLASS_ID);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

const LIST_VIEW_HOLD_PROPERTY: windows::core::PCWSTR =
    windows::core::w!("LetRecovery.ListViewHold");

fn list_view_hold_release_message() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static MESSAGE: AtomicU32 = AtomicU32::new(0);
    let current = MESSAGE.load(Ordering::Relaxed);
    if current != 0 {
        return current;
    }
    let registered = unsafe {
        windows::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW(windows::core::w!(
            "LetRecovery.ListViewHoldRelease"
        ))
    };
    MESSAGE.store(registered, Ordering::Relaxed);
    registered
}

unsafe fn list_view_hold_active(list: HWND) -> bool {
    !windows::Win32::UI::WindowsAndMessaging::GetPropW(list, LIST_VIEW_HOLD_PROPERTY).is_invalid()
}

/// Holds painting of a report until the message that is rebuilding it has returned. The release
/// is a posted message, so it runs only after the synchronous delete-and-refill has finished.
unsafe fn begin_list_view_hold(list: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{IsWindowVisible, PostMessageW, SetPropW};
    if list_view_hold_active(list) || !IsWindowVisible(list).as_bool() {
        return;
    }
    let message = list_view_hold_release_message();
    if message == 0 {
        return;
    }
    if SetPropW(
        list,
        LIST_VIEW_HOLD_PROPERTY,
        windows::Win32::Foundation::HANDLE(std::ptr::dangling_mut::<core::ffi::c_void>()),
    )
    .is_err()
    {
        return;
    }
    if PostMessageW(list, message, WPARAM(0), LPARAM(0)).is_err() {
        let _ = windows::Win32::UI::WindowsAndMessaging::RemovePropW(list, LIST_VIEW_HOLD_PROPERTY);
    }
}

unsafe fn end_list_view_hold(list: HWND) {
    if !list_view_hold_active(list) {
        return;
    }
    let _ = windows::Win32::UI::WindowsAndMessaging::RemovePropW(list, LIST_VIEW_HOLD_PROPERTY);
    let _ = windows::Win32::Graphics::Gdi::RedrawWindow(
        list,
        None,
        None,
        windows::Win32::Graphics::Gdi::RDW_INVALIDATE
            | windows::Win32::Graphics::Gdi::RDW_FRAME
            | windows::Win32::Graphics::Gdi::RDW_UPDATENOW,
    );
}

/// Removes the report's column header from `dc`'s clip region.
unsafe fn exclude_list_view_header(list: HWND, dc: HDC) {
    let header = HWND(SendMessageW(list, 0x101f, WPARAM(0), LPARAM(0)).0 as *mut _); // LVM_GETHEADER
    if header.is_invalid()
        || !windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(header).as_bool()
    {
        return;
    }
    let mut rect = RECT::default();
    if windows::Win32::UI::WindowsAndMessaging::GetWindowRect(header, &mut rect).is_err() {
        return;
    }
    let mut corners = [
        windows::Win32::Foundation::POINT {
            x: rect.left,
            y: rect.top,
        },
        windows::Win32::Foundation::POINT {
            x: rect.right,
            y: rect.bottom,
        },
    ];
    let _ = windows::Win32::Graphics::Gdi::MapWindowPoints(HWND::default(), list, &mut corners);
    let _ = windows::Win32::Graphics::Gdi::ExcludeClipRect(
        dc,
        corners[0].x,
        corners[0].y,
        corners[1].x,
        corners[1].y,
    );
}

const fn list_view_needs_empty_body_paint(item_count: isize) -> bool {
    item_count == 0
}

fn list_view_trailing_body_rect(
    client: RECT,
    item_count: isize,
    last_item_bottom: Option<i32>,
) -> Option<RECT> {
    if item_count <= 0 {
        return None;
    }
    let top = last_item_bottom?.clamp(client.top, client.bottom);
    (top < client.bottom).then_some(RECT {
        left: client.left,
        top,
        right: client.right,
        bottom: client.bottom,
    })
}

unsafe fn paint_list_view_trailing_body(hwnd: HWND, dc: HDC, palette: Palette) {
    const LVM_GETITEMCOUNT: u32 = 0x1004;
    const LVM_GETITEMRECT: u32 = 0x100e;
    const LVIR_BOUNDS: i32 = 0;

    let item_count = SendMessageW(hwnd, LVM_GETITEMCOUNT, WPARAM(0), LPARAM(0)).0;
    if item_count <= 0 {
        return;
    }
    let mut client = RECT::default();
    if GetClientRect(hwnd, &mut client).is_err() {
        return;
    }
    let mut last = RECT {
        left: LVIR_BOUNDS,
        ..Default::default()
    };
    let last_bottom = (SendMessageW(
        hwnd,
        LVM_GETITEMRECT,
        WPARAM((item_count - 1) as usize),
        LPARAM((&mut last as *mut RECT) as isize),
    )
    .0 != 0)
        .then_some(last.bottom);
    let Some(rect) = list_view_trailing_body_rect(client, item_count, last_bottom) else {
        return;
    };
    fill(dc, &rect, palette.edit);
}

unsafe fn apply_single_line_edit_frame_theme(frame: HWND, edit: HWND, palette: Palette) {
    let _ = SetWindowTheme(frame, w!(""), w!(""));
    apply_borderless_style(frame);
    let _ = SetWindowSubclass(
        frame,
        Some(single_line_edit_frame_subclass),
        SINGLE_LINE_EDIT_FRAME_SUBCLASS_ID,
        palette_reference(palette),
    );
    paint_single_line_edit_frame(frame, edit, palette);
}

unsafe fn repaint_single_line_edit_frame(edit: HWND, immediate: bool) {
    let Some(frame) = single_line_edit_frame(edit) else {
        return;
    };
    let flags = RDW_INVALIDATE
        | RDW_NOERASE
        | if immediate {
            RDW_UPDATENOW
        } else {
            Default::default()
        };
    let _ = RedrawWindow(frame, None, None, flags);
}

unsafe fn paint_single_line_edit_frame(frame: HWND, edit: HWND, palette: Palette) {
    let mut paint = PAINTSTRUCT::default();
    let _ = BeginPaint(frame, &mut paint);
    let _ = EndPaint(frame, &paint);

    let dpi = GetDpiForWindow(edit).max(96);
    let interior = edit_surface_color(edit, palette);
    let hot = !GetPropW(edit, ROUNDED_CONTROL_HOT_PROPERTY).is_invalid()
        || !GetPropW(frame, ROUNDED_CONTROL_HOT_PROPERTY).is_invalid();
    let border = if !IsWindowEnabled(edit).as_bool() {
        palette.control_border()
    } else if GetFocus() == edit {
        palette.accent_border
    } else if hot {
        palette.separator
    } else {
        palette.control_border()
    };
    // Surface and outline are composed together and published in one BitBlt. Filling the ring
    // and then drawing the outline on screen made the outline vanish for a frame on every repaint.
    super::redraw::paint_window_buffered(frame, |dc, rect| {
        fill(dc, &rect, interior);
        if let Some(geometry) = rounded_control_frame_geometry(rect.right, rect.bottom, dpi) {
            draw_antialiased_control_frame(
                dc,
                rect,
                geometry,
                interior,
                border,
                rounded_control_exterior(palette),
            );
        }
    });
}

unsafe fn forward_edit_frame_pointer_message(
    frame: HWND,
    edit: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if !IsWindowEnabled(edit).as_bool() {
        return LRESULT(0);
    }
    let packed = lparam.0 as u32;
    let mut point = POINT {
        x: (packed as u16 as i16) as i32,
        y: ((packed >> 16) as u16 as i16) as i32,
    };
    if !ClientToScreen(frame, &mut point).as_bool() || !ScreenToClient(edit, &mut point).as_bool() {
        return LRESULT(0);
    }
    let mut client = RECT::default();
    if GetClientRect(edit, &mut client).is_err() {
        return LRESULT(0);
    }
    point.x = point
        .x
        .clamp(client.left, (client.right - 1).max(client.left));
    point.y = point
        .y
        .clamp(client.top, (client.bottom - 1).max(client.top));
    let forwarded = (u32::from(point.x as u16)) | (u32::from(point.y as u16) << 16);
    SendMessageW(edit, message, wparam, LPARAM(forwarded as isize))
}

unsafe extern "system" fn single_line_edit_frame_subclass(
    frame: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    let owner = single_line_edit_frame_owner(frame);
    match message {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            if let Some(edit) = owner {
                paint_single_line_edit_frame(frame, edit, palette_from_reference(reference_data));
            } else {
                let mut paint = PAINTSTRUCT::default();
                let _ = BeginPaint(frame, &mut paint);
                let _ = EndPaint(frame, &paint);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            // Only the enter transition changes the outline colour; the former per-message
            // repaint republished the frame for every pixel the pointer moved.
            let entered = begin_hot_tracking(frame, ROUNDED_CONTROL_HOT_PROPERTY, false);
            if let Some(edit) = owner {
                let result =
                    forward_edit_frame_pointer_message(frame, edit, message, wparam, lparam);
                if entered {
                    repaint_single_line_edit_frame(edit, false);
                }
                result
            } else {
                DefSubclassProc(frame, message, wparam, lparam)
            }
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP => owner.map_or_else(
            || DefSubclassProc(frame, message, wparam, lparam),
            |edit| forward_edit_frame_pointer_message(frame, edit, message, wparam, lparam),
        ),
        WM_MOUSELEAVE_MESSAGE => {
            if end_hot_tracking(frame, ROUNDED_CONTROL_HOT_PROPERTY) {
                if let Some(edit) = owner {
                    repaint_single_line_edit_frame(edit, false);
                }
            }
            DefSubclassProc(frame, message, wparam, lparam)
        }
        WM_THEMECHANGED | WM_ENABLE => {
            let result = DefSubclassProc(frame, message, wparam, lparam);
            if let Some(edit) = owner {
                repaint_single_line_edit_frame(edit, true);
            }
            result
        }
        WM_NCDESTROY => {
            let _ = RemovePropW(frame, ROUNDED_CONTROL_HOT_PROPERTY);
            let _ = RemoveWindowSubclass(
                frame,
                Some(single_line_edit_frame_subclass),
                SINGLE_LINE_EDIT_FRAME_SUBCLASS_ID,
            );
            DefSubclassProc(frame, message, wparam, lparam)
        }
        _ => DefSubclassProc(frame, message, wparam, lparam),
    }
}

unsafe extern "system" fn single_line_edit_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    _reference_data: usize,
) -> LRESULT {
    const WM_SETTEXT_MESSAGE: u32 = 0x000c;
    const WM_CONTEXTMENU_MESSAGE: u32 = 0x007b;
    const WM_CAPTURECHANGED_MESSAGE: u32 = 0x0215;
    match message {
        WM_NCPAINT => DefSubclassProc(hwnd, message, wparam, lparam),
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => paint_edit_buffered(hwnd, palette_from_reference(_reference_data)),
        WM_CONTEXTMENU_MESSAGE => {
            super::context_menu::show_for_edit(
                hwnd,
                lparam,
                palette_from_reference(_reference_data),
            );
            LRESULT(0)
        }
        WM_SETTEXT_MESSAGE if edit_text_unchanged(hwnd, lparam) => LRESULT(1),
        WM_MOUSEMOVE => {
            // Drag-selecting: USER32 would paint the new selection straight onto the screen in
            // the system colours; publish it through the buffered painter instead.
            let result = if wparam.0 & 0x0001 != 0 {
                run_single_line_edit_message(hwnd, message, wparam, lparam)
            } else {
                DefSubclassProc(hwnd, message, wparam, lparam)
            };
            // The hot outline lives on the sibling frame. Invalidating the Edit itself on hover
            // made USER32 erase and redraw the text, and repainting the frame on every mouse
            // message republished it continuously; only the enter transition matters. While the
            // Edit holds the mouse (drag-selecting) the field simply stays hot: leave tracking
            // does not work under capture, and toggling hot on and off made the frame blink.
            let already_hot = !GetPropW(hwnd, ROUNDED_CONTROL_HOT_PROPERTY).is_invalid();
            if !(already_hot && windows::Win32::UI::Input::KeyboardAndMouse::GetCapture() == hwnd)
                && begin_hot_tracking(hwnd, ROUNDED_CONTROL_HOT_PROPERTY, false)
            {
                repaint_single_line_edit_frame(hwnd, false);
            }
            result
        }
        WM_MOUSELEAVE_MESSAGE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if windows::Win32::UI::Input::KeyboardAndMouse::GetCapture() != hwnd
                && end_hot_tracking(hwnd, ROUNDED_CONTROL_HOT_PROPERTY)
            {
                repaint_single_line_edit_frame(hwnd, false);
            }
            result
        }
        WM_CAPTURECHANGED_MESSAGE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            // Selection drag finished: the field is hot only if the pointer is still over it.
            let mut cursor = POINT::default();
            let mut window = RECT::default();
            let inside = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut cursor).is_ok()
                && GetWindowRect(hwnd, &mut window).is_ok()
                && cursor.x >= window.left
                && cursor.x < window.right
                && cursor.y >= window.top
                && cursor.y < window.bottom;
            if !inside && end_hot_tracking(hwnd, ROUNDED_CONTROL_HOT_PROPERTY) {
                repaint_single_line_edit_frame(hwnd, false);
            }
            result
        }
        WM_SETFOCUS | WM_KILLFOCUS => {
            // Focus shows or hides the selection and creates or destroys the caret.
            let result = run_single_line_edit_message(hwnd, message, wparam, lparam);
            repaint_single_line_edit_frame(hwnd, true);
            result
        }
        WM_ENABLE | WM_SIZE | WM_THEMECHANGED => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            repaint_single_line_edit_frame(hwnd, true);
            result
        }
        WM_NCDESTROY => {
            let _ = RemovePropW(hwnd, ROUNDED_CONTROL_HOT_PROPERTY);
            let _ = RemovePropW(hwnd, EDIT_SELECTION_PROPERTY);
            let _ = RemovePropW(hwnd, EDIT_CARET_HIDDEN_PROPERTY);
            let _ = RemoveWindowSubclass(
                hwnd,
                Some(single_line_edit_subclass),
                SINGLE_LINE_EDIT_SUBCLASS_ID,
            );
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        // Typing, EM_REPLACESEL and cut/paste/clear/undo change the text (the field notifies its
        // parent meanwhile); they never paint a selection, so USER32 keeps drawing them.
        0x0102 | 0x00c2 | 0x0300 | 0x0302 | 0x0303 | 0x0304 => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            refresh_edit_selection_paint(hwnd);
            result
        }
        _ if edit_selection_message(message, wparam) => {
            // Clicks, drags, keys and EM_SETSEL: one buffered frame with LetRecovery's selection
            // colours instead of USER32's direct system-blue drawing.
            run_single_line_edit_message(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

/// Runs a message that can change what a single-line field shows (selection, caret position,
/// text) with USER32's immediate drawing suspended - it would draw the selection in the system
/// highlight colours straight onto the screen - and then publishes the field once through the
/// buffered painter, which draws the selection in LetRecovery's colours. The caret is hidden
/// while text is selected.
unsafe fn run_single_line_edit_message(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    const WM_SETREDRAW: u32 = 0x000b;
    let visible = IsWindowVisible(hwnd).as_bool();
    if visible {
        let _ = SendMessageW(hwnd, WM_SETREDRAW, WPARAM(0), LPARAM(0));
    }
    let result = DefSubclassProc(hwnd, message, wparam, lparam);
    if visible {
        let _ = SendMessageW(hwnd, WM_SETREDRAW, WPARAM(1), LPARAM(0));
        let _ = RedrawWindow(
            hwnd,
            None,
            None,
            RDW_INVALIDATE | RDW_UPDATENOW | RDW_NOERASE,
        );
    }
    store_edit_selection_key(hwnd);
    sync_edit_selection_caret(hwnd);
    result
}

unsafe extern "system" fn combo_selection_item_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    const WM_PRINTCLIENT: u32 = 0x0318;
    let palette = palette_from_reference(reference_data);
    match message {
        WM_LBUTTONDOWN | 0x0203 => {
            // A click on the closed selection child opens LetRecovery's list for its combo.
            if let Ok(combo) = GetParent(hwnd) {
                if is_drop_down_list(combo) {
                    if windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() != combo {
                        let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(combo);
                    }
                    super::combo_popup::toggle(combo, palette);
                    return LRESULT(0);
                }
            }
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_ERASEBKGND | WM_PRINTCLIENT => {
            let dc = HDC(wparam.0 as *mut _);
            if !dc.is_invalid() {
                paint_combo_selection_item_to_dc(hwnd, palette, dc);
            }
            LRESULT(1)
        }
        WM_PAINT => {
            // Always validate the child update region. Returning without BeginPaint leaves a
            // permanent WM_PAINT loop on reduced WinPE USER32 builds.
            let mut paint = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut paint);
            let _ = EndPaint(hwnd, &paint);
            paint_combo_selection_item_window(hwnd, palette);
            LRESULT(0)
        }
        WM_NCPAINT => {
            paint_combo_selection_item_window(hwnd, palette);
            LRESULT(0)
        }
        WM_ENABLE
        | WM_SETTEXT
        | WM_SETFOCUS
        | WM_KILLFOCUS
        | WM_THEMECHANGED
        | WM_LBUTTONUP
        | WM_KEYDOWN
        | WM_KEYUP
        | 0x0127 // WM_CHANGEUISTATE
        | 0x0128 // WM_UPDATEUISTATE
        => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            // USER32 paints a focus underline directly while processing focus/click/UI-state
            // messages on the read-only selection child. An asynchronous invalidation leaves that
            // underline visible until another pointer event, especially in reduced WinPE builds.
            // Publish the complete child surface and its parent frame in the same transaction.
            repaint_combo_selection_item_now(hwnd, palette);
            result
        }
        WM_NCDESTROY => {
            let _ = RemovePropW(hwnd, COMBO_SELECTION_ITEM_PREPARED_PROPERTY);
            let _ = RemoveWindowSubclass(
                hwnd,
                Some(combo_selection_item_subclass),
                COMBO_SELECTION_ITEM_SUBCLASS_ID,
            );
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

unsafe extern "system" fn rounded_control_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    const CB_SHOWDROPDOWN: u32 = 0x014f;
    const CB_GETDROPPEDSTATE_MESSAGE: u32 = 0x0157;
    const WM_GETDLGCODE_MESSAGE: u32 = 0x0087;
    const WM_SYSKEYDOWN_MESSAGE: u32 = 0x0104;
    const WM_SHOWWINDOW_MESSAGE: u32 = 0x0018;
    const WM_LBUTTONDBLCLK_MESSAGE: u32 = 0x0203;
    const DLGC_WANTALLKEYS_CODE: isize = 0x0004;
    // The drop-down list is LetRecovery's own popup (see combo_popup). It closes whenever the
    // field loses focus, is disabled, hidden or destroyed.
    if (matches!(message, WM_KILLFOCUS | WM_NCDESTROY)
        || (matches!(message, WM_ENABLE | WM_SHOWWINDOW_MESSAGE) && wparam.0 == 0))
        && super::combo_popup::is_open(hwnd)
    {
        super::combo_popup::close(hwnd, false);
    }
    match message {
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK_MESSAGE if is_drop_down_list(hwnd) => {
            // Never enter USER32's drop-list tracking: the click opens (or closes) our list.
            if windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(hwnd).as_bool() {
                if windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() != hwnd {
                    let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(hwnd);
                }
                super::combo_popup::toggle(hwnd, palette_from_reference(reference_data));
            }
            LRESULT(0)
        }
        CB_SHOWDROPDOWN if is_drop_down_list(hwnd) => {
            if wparam.0 != 0 {
                if !super::combo_popup::is_open(hwnd) {
                    super::combo_popup::open(hwnd, palette_from_reference(reference_data));
                }
            } else {
                super::combo_popup::close(hwnd, false);
            }
            LRESULT(1)
        }
        CB_GETDROPPEDSTATE_MESSAGE if is_drop_down_list(hwnd) => {
            LRESULT(isize::from(super::combo_popup::is_open(hwnd)))
        }
        WM_GETDLGCODE_MESSAGE if is_drop_down_list(hwnd) && super::combo_popup::is_open(hwnd) => {
            // While the list is open, Enter and Escape belong to it, not to the dialog buttons.
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            LRESULT(result.0 | DLGC_WANTALLKEYS_CODE)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN_MESSAGE | 0x0102 | 0x020a
            if is_drop_down_list(hwnd)
                && super::combo_popup::handle_combo_input(
                    hwnd,
                    palette_from_reference(reference_data),
                    message,
                    wparam,
                    lparam,
                ) =>
        {
            LRESULT(0)
        }
        WM_REPAINT_TRACKING_COMBO if is_drop_down_list(hwnd) => {
            // A real mouse click enters USER32's nested drop-list tracking loop. The stock theme
            // can repaint the closed selection after WM_LBUTTONDOWN but before that call returns,
            // so a conventional after-default overlay runs too late. This posted message executes
            // inside the nested loop and restores the complete deterministic surface while the
            // popup is actually visible.
            repaint_combo_closed_now(hwnd, palette_from_reference(reference_data));
            LRESULT(0)
        }
        WM_KEYDOWN | WM_KEYUP | 0x0102 | 0x020a | 0x014e | 0x0143 | 0x014a | 0x0144 | 0x014b
        | WM_ENABLE
            if is_drop_down_list(hwnd) =>
        {
            // Arrow keys, typing, the wheel, CB_SETCURSEL, item changes and enabling make USER32
            // draw the closed field straight to the screen, in its own style and with its own
            // caption position; alternating with ours made the caption jump left and right.
            // Suppress its drawing and publish ours.
            let result = call_combo_without_native_paint(hwnd, message, wparam, lparam);
            repaint_combo_closed_now(hwnd, palette_from_reference(reference_data));
            result
        }
        WM_ERASEBKGND if is_drop_down_list(hwnd) => {
            // WinPE's reduced USER32/UxTheme stack can still erase the borderless ComboBox with
            // the stock class brush before WM_PAINT. That erase survives as a bright underline
            // because the PE renderer does not agree with the closed-field height reported by
            // COMBOBOXINFO. Publish the complete deterministic closed surface (field, chevron,
            // text and frame composed together) and report the erase as complete. The separate
            // ComboLBox popup remains native.
            repaint_combo_closed_now(hwnd, palette_from_reference(reference_data));
            LRESULT(1)
        }
        WM_NCCALCSIZE_MESSAGE if uses_frame_band(hwnd) => band_nccalcsize(hwnd, wparam, lparam),
        WM_PAINT => {
            if is_drop_down_list(hwnd) {
                paint_combo_closed(hwnd, palette_from_reference(reference_data));
                return LRESULT(0);
            }
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            // A banded frame is non-client: client paints cannot reach it.
            if reserved_frame_band(hwnd).is_none() {
                paint_rounded_control_frame(hwnd, palette_from_reference(reference_data));
            }
            result
        }
        WM_NCPAINT => {
            if is_drop_down_list(hwnd) {
                // WS_BORDER and WS_EX_CLIENTEDGE are removed when this subclass is installed.
                // Calling the stock non-client painter anyway is harmless on full Windows, but
                // WinPE paints a legacy bright bottom edge after our client surface. This frame
                // is the complete non-client result, so do not run that incompatible renderer.
                paint_rounded_control_frame(hwnd, palette_from_reference(reference_data));
                return LRESULT(0);
            }
            let result = if uses_frame_band(hwnd) {
                paint_native_scrollbars_only(hwnd, wparam, lparam)
            } else {
                DefSubclassProc(hwnd, message, wparam, lparam)
            };
            paint_rounded_control_frame(hwnd, palette_from_reference(reference_data));
            result
        }
        WM_SETCURSOR if is_drop_down_list(hwnd) => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            ensure_hot_tracking(hwnd, ROUNDED_CONTROL_HOT_PROPERTY, false);
            result
        }
        WM_MOUSEMOVE | WM_NCMOUSEMOVE_MESSAGE | WM_MOUSELEAVE_MESSAGE | WM_NCMOUSELEAVE_MESSAGE
            if reserved_frame_band(hwnd).is_some() =>
        {
            // Read-only report with its frame in the band: hover never changes the outline and
            // the scrollbar cannot cover it, so pointer traffic needs no painting at all.
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        message
            if native_scrollbar_may_repaint_frame(message)
                && reserved_frame_band(hwnd).is_some() =>
        {
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_MOUSEMOVE => {
            const MK_LBUTTON_FLAG: usize = 0x0001;
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            let entered = ensure_hot_tracking(hwnd, ROUNDED_CONTROL_HOT_PROPERTY, false);
            // Hovering never changes this frame (the hot colour is queued once by the enter
            // transition above). Only a drag inside a read-only report lets the native control
            // draw selection over the frame edge, so the frame is restored for drags only instead
            // of being republished for every pointer message.
            if !entered && !is_drop_down_list(hwnd) && wparam.0 & MK_LBUTTON_FLAG != 0 {
                paint_rounded_control_frame(hwnd, palette_from_reference(reference_data));
            }
            result
        }
        WM_NCMOUSEMOVE_MESSAGE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            ensure_hot_tracking(hwnd, ROUNDED_CONTROL_HOT_PROPERTY, true);
            paint_rounded_control_frame(hwnd, palette_from_reference(reference_data));
            result
        }
        WM_MOUSELEAVE_MESSAGE | WM_NCMOUSELEAVE_MESSAGE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            clear_hot_tracking(hwnd, ROUNDED_CONTROL_HOT_PROPERTY);
            if !is_drop_down_list(hwnd) {
                paint_rounded_control_frame(hwnd, palette_from_reference(reference_data));
            }
            result
        }
        message if native_scrollbar_may_repaint_frame(message) => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if is_drop_down_list(hwnd) {
                // A closed ComboBox has no scrollbar of its own; its popup ComboLBox is a
                // separate HWND.  UxTheme nevertheless posts WM_TIMER while the pointer crosses
                // adjacent controls.  Synchronously repainting the complete field for every
                // timer tick produces the visible WinPE flash.  Ignore animation-only timers and
                // coalesce the remaining state/content changes into the normal paint queue.
                if message != 0x0113 {
                    invalidate_control_visual(hwnd);
                }
            } else {
                paint_rounded_control_frame(hwnd, palette_from_reference(reference_data));
            }
            result
        }
        WM_SETFOCUS if is_drop_down_list(hwnd) => {
            let result = call_combo_without_native_paint(hwnd, message, wparam, lparam);
            install_combo_selection_item_subclass(hwnd, palette_from_reference(reference_data));
            // USER32 creates and shows a caret when a CBS_DROPDOWNLIST receives focus. The closed
            // field is read-only and fully painted by this subclass, so that caret has no editing
            // meaning and otherwise appears as a one-frame vertical line after a click.
            if GetPropW(hwnd, COMBO_CARET_HIDDEN_PROPERTY).is_invalid()
                && HideCaret(hwnd).is_ok()
                && SetPropW(
                    hwnd,
                    COMBO_CARET_HIDDEN_PROPERTY,
                    HANDLE(std::ptr::dangling_mut()),
                )
                .is_err()
            {
                let _ = ShowCaret(hwnd);
            }
            repaint_combo_closed_now(hwnd, palette_from_reference(reference_data));
            result
        }
        WM_KILLFOCUS if is_drop_down_list(hwnd) => {
            if RemovePropW(hwnd, COMBO_CARET_HIDDEN_PROPERTY)
                .is_ok_and(|handle| !handle.is_invalid())
            {
                let _ = ShowCaret(hwnd);
            }
            let result = call_combo_without_native_paint(hwnd, message, wparam, lparam);
            repaint_combo_closed_now(hwnd, palette_from_reference(reference_data));
            result
        }
        WM_LBUTTONDOWN if is_drop_down_list(hwnd) => {
            let marked = SetPropW(
                hwnd,
                COMBO_TRACKING_DROPPED_PROPERTY,
                HANDLE(std::ptr::dangling_mut()),
            )
            .is_ok();
            repaint_combo_closed_now(hwnd, palette_from_reference(reference_data));
            let _ = PostMessageW(hwnd, WM_REPAINT_TRACKING_COMBO, WPARAM(0), LPARAM(0));
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if marked {
                let _ = RemovePropW(hwnd, COMBO_TRACKING_DROPPED_PROPERTY);
            }
            repaint_combo_closed_now(hwnd, palette_from_reference(reference_data));
            result
        }
        WM_ENABLE | WM_SETFOCUS | WM_KILLFOCUS | WM_SIZE | WM_THEMECHANGED | WM_LBUTTONUP => {
            let _profile = (message == WM_SIZE).then(|| {
                super::redraw::profile_scope(if is_drop_down_list(hwnd) {
                    "下拉框改尺寸"
                } else {
                    "圆角控件改尺寸"
                })
            });
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if is_drop_down_list(hwnd) {
                if matches!(message, WM_SIZE | WM_THEMECHANGED) {
                    let height = InnoMetrics::for_dpi(GetDpiForWindow(hwnd).max(96)).field_height;
                    set_combo_selection_field_height(hwnd, height);
                    set_combo_popup_row_height(hwnd, height);
                    clip_combo_to_closed_field(hwnd, palette_from_reference(reference_data));
                }
                install_combo_selection_item_subclass(hwnd, palette_from_reference(reference_data));
                if matches!(message, WM_LBUTTONDOWN | WM_LBUTTONUP | WM_SETFOCUS) {
                    repaint_combo_closed_now(hwnd, palette_from_reference(reference_data));
                } else {
                    invalidate_control_visual(hwnd);
                }
            } else {
                if matches!(message, WM_SIZE | WM_THEMECHANGED) {
                    clear_legacy_control_region(hwnd);
                }
                let _ = InvalidateRect(hwnd, None, false);
            }
            result
        }
        CB_SHOWDROPDOWN => {
            if wparam.0 != 0 {
                let height = InnoMetrics::for_dpi(GetDpiForWindow(hwnd).max(96)).field_height;
                set_combo_selection_field_height(hwnd, height);
                set_combo_popup_row_height(hwnd, height);
                clip_combo_to_closed_field(hwnd, palette_from_reference(reference_data));
            }
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            install_combo_selection_item_subclass(hwnd, palette_from_reference(reference_data));
            let mut info = COMBOBOXINFO {
                cbSize: std::mem::size_of::<COMBOBOXINFO>() as u32,
                ..Default::default()
            };
            if wparam.0 != 0
                && GetComboBoxInfo(hwnd, &mut info).is_ok()
                && !info.hwndList.0.is_null()
            {
                apply_combo_popup_native_chrome(
                    info.hwndList,
                    palette_from_reference(reference_data),
                );
            }
            repaint_combo_closed_now(hwnd, palette_from_reference(reference_data));
            result
        }
        WM_NCDESTROY => {
            if RemovePropW(hwnd, COMBO_CARET_HIDDEN_PROPERTY)
                .is_ok_and(|handle| !handle.is_invalid())
            {
                let _ = ShowCaret(hwnd);
            }
            let _ = RemovePropW(hwnd, LIST_BOX_HOT_PROPERTY);
            let _ = RemovePropW(hwnd, ROUNDED_CONTROL_HOT_PROPERTY);
            let _ = RemovePropW(hwnd, COMBO_TRACKING_DROPPED_PROPERTY);
            let _ = RemoveWindowSubclass(
                hwnd,
                Some(rounded_control_subclass),
                ROUNDED_CONTROL_SUBCLASS_ID,
            );
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

// ------------------------------------------------------------------------------------------
// Frame band for scrollable fields (read-only multi-line Edit, standalone ListBox).
// ------------------------------------------------------------------------------------------
//
// The rounded frame of these controls used to be painted over the window's own outer pixels,
// including the native vertical scrollbar. USER32/UxTheme repaint that scrollbar by themselves
// (hover, press, fade animations driven by internal timers), which wiped the frame edge beside
// it until the next mouse message restored it: the frame visibly came and went while the pointer
// rested on the scrollbar. The frame now lives in a thin non-client band reserved through
// WM_NCCALCSIZE. USER32 lays the scrollbar out inside that band (a scrollbar is always placed
// beside the client rectangle), so the scrollbar and the frame never share a pixel and neither
// can erase the other.

unsafe fn uses_frame_band(hwnd: HWND) -> bool {
    let class_name = control_class_name(hwnd);
    let _ = class_name;
    is_list_box(hwnd)
}

const FRAMED_EDIT_SUBCLASS_ID: usize = 0x4c52_4645;
const EDIT_LAYOUT_BUSY_PROPERTY: PCWSTR = w!("LetRecovery.FramedEdit.LayoutBusy");

/// Paints an Edit off-screen and publishes it with one BitBlt. USER32 erased the whole field and
/// then drew the text on top of the screen pixels, so every text update (a progress line, a
/// hash result) and every auto-scroll while selecting showed an empty field for a moment: the
/// flicker. The Edit's own painter still draws everything (WM_PRINTCLIENT) - text, selection,
/// scroll position - only into the off-screen surface, over its own background colour.
unsafe fn paint_edit_buffered(hwnd: HWND, palette: Palette) -> LRESULT {
    const WM_PRINTCLIENT: u32 = 0x0318;
    const PRF_CLIENT: isize = 0x0004;
    let mut paint = PAINTSTRUCT::default();
    let target = BeginPaint(hwnd, &mut paint);
    let mut client = RECT::default();
    if !target.is_invalid() && GetClientRect(hwnd, &mut client).is_ok() {
        let area = intersect_rects(client, paint.rcPaint);
        if area.right > area.left && area.bottom > area.top {
            let background = edit_surface_color(hwnd, palette);
            let buffer = super::redraw::PaintBuffer::begin_opaque(target, client, area);
            let dc = buffer.dc();
            fill(dc, &client, background);
            let _ = SendMessageW(
                hwnd,
                WM_PRINTCLIENT,
                WPARAM(dc.0 as usize),
                LPARAM(PRF_CLIENT),
            );
            overlay_edit_selection(hwnd, dc, palette);
            let caret_hidden = HideCaret(hwnd).is_ok();
            buffer.present();
            if caret_hidden {
                let _ = ShowCaret(hwnd);
            }
        }
    }
    let _ = EndPaint(hwnd, &paint);
    LRESULT(0)
}

const STATIC_TEXT_SUBCLASS_ID: usize = 0x4c52_5354;

/// Text labels (STATIC with left, centred, right, simple or no-wrap text) get the same treatment
/// as the edits: an unchanged text is not set again, and the label is painted off-screen and
/// published with one BitBlt, so status and progress labels that change many times a second
/// never show an erased, empty label in between.
pub(crate) unsafe fn install_static_text_subclass(label: HWND) {
    let kind = GetWindowLongPtrW(label, GWL_STYLE) & 0x1f;
    if matches!(kind, 0x00 | 0x01 | 0x02 | 0x0b | 0x0c) {
        let _ = SetWindowSubclass(
            label,
            Some(static_text_subclass),
            STATIC_TEXT_SUBCLASS_ID,
            0,
        );
    }
}

unsafe extern "system" fn static_text_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    _reference_data: usize,
) -> LRESULT {
    const WM_SETTEXT_MESSAGE: u32 = 0x000c;
    const WM_PRINTCLIENT: u32 = 0x0318;
    const PRF_CLIENT: isize = 0x0004;
    match message {
        WM_SETTEXT_MESSAGE if edit_text_unchanged(hwnd, lparam) => LRESULT(1),
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let target = BeginPaint(hwnd, &mut paint);
            let mut client = RECT::default();
            if !target.is_invalid() && GetClientRect(hwnd, &mut client).is_ok() {
                let area = intersect_rects(client, paint.rcPaint);
                if area.right > area.left && area.bottom > area.top {
                    let buffer = super::redraw::PaintBuffer::begin_opaque(target, client, area);
                    let dc = buffer.dc();
                    // The background the label itself would use: the parent's static brush.
                    let brush = GetParent(hwnd)
                        .map(|parent| {
                            SendMessageW(
                                parent,
                                windows::Win32::UI::WindowsAndMessaging::WM_CTLCOLORSTATIC,
                                WPARAM(dc.0 as usize),
                                LPARAM(hwnd.0 as isize),
                            )
                            .0
                        })
                        .unwrap_or(0);
                    if brush != 0 {
                        let _ = FillRect(
                            dc,
                            &client,
                            windows::Win32::Graphics::Gdi::HBRUSH(brush as *mut _),
                        );
                    }
                    let _ = SendMessageW(
                        hwnd,
                        WM_PRINTCLIENT,
                        WPARAM(dc.0 as usize),
                        LPARAM(PRF_CLIENT),
                    );
                    buffer.present();
                }
            }
            let _ = EndPaint(hwnd, &paint);
            LRESULT(0)
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(static_text_subclass), STATIC_TEXT_SUBCLASS_ID);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

const EDIT_SELECTION_PROPERTY: PCWSTR = w!("LetRecovery.Edit.PaintedSelection");

unsafe fn edit_selection(edit: HWND) -> (u32, u32) {
    let mut start = 0u32;
    let mut end = 0u32;
    let _ = SendMessageW(
        edit,
        0x00b0, // EM_GETSEL
        WPARAM(&mut start as *mut u32 as usize),
        LPARAM(&mut end as *mut u32 as isize),
    );
    (start.min(end), start.max(end))
}

/// Selected text in LetRecovery's selection colours (the same as selected list rows) instead of
/// the system highlight: after the Edit's own painter, every selected segment is filled and its
/// text drawn again on top, line by line, at the positions the Edit reports.
unsafe fn overlay_edit_selection(edit: HWND, dc: HDC, palette: Palette) {
    const ES_MULTILINE: isize = 0x0004;
    const ES_PASSWORD: isize = 0x0020;
    const ES_NOHIDESEL: isize = 0x0100;
    const EM_GETRECT: u32 = 0x00b2;
    const EM_LINEINDEX: u32 = 0x00bb;
    const EM_LINELENGTH: u32 = 0x00c1;
    const EM_LINEFROMCHAR: u32 = 0x00c9;
    const EM_POSFROMCHAR: u32 = 0x00d6;
    let style = GetWindowLongPtrW(edit, GWL_STYLE);
    if style & ES_PASSWORD != 0 {
        return;
    }
    let focused = windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() == edit;
    if !focused && style & ES_NOHIDESEL == 0 {
        return;
    }
    let (start, end) = edit_selection(edit);
    if start >= end {
        return;
    }
    let length =
        windows::Win32::UI::WindowsAndMessaging::GetWindowTextLengthW(edit).max(0) as usize;
    if length == 0 {
        return;
    }
    let mut text = vec![0u16; length + 1];
    let copied =
        windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(edit, &mut text).max(0) as usize;
    text.truncate(copied);
    let (selected_text, selected_fill) = navigation_selection_colors(palette, false);
    let font = SendMessageW(edit, WM_GETFONT, WPARAM(0), LPARAM(0));
    let previous = (font.0 != 0)
        .then(|| SelectObject(dc, windows::Win32::Graphics::Gdi::HGDIOBJ(font.0 as *mut _)));
    let mut metrics = windows::Win32::Graphics::Gdi::TEXTMETRICW::default();
    let _ = windows::Win32::Graphics::Gdi::GetTextMetricsW(dc, &mut metrics);
    let line_height = metrics.tmHeight.max(1);
    let mut format = RECT::default();
    let _ = SendMessageW(
        edit,
        EM_GETRECT,
        WPARAM(0),
        LPARAM((&mut format as *mut RECT) as isize),
    );
    let saved = windows::Win32::Graphics::Gdi::SaveDC(dc);
    let _ = windows::Win32::Graphics::Gdi::IntersectClipRect(
        dc,
        format.left,
        format.top,
        format.right,
        format.bottom,
    );
    let _ = SetBkMode(dc, TRANSPARENT);
    let _ = SetTextColor(dc, selected_text);
    let multiline = style & ES_MULTILINE != 0;
    let (first_line, last_line) = if multiline {
        (
            SendMessageW(edit, EM_LINEFROMCHAR, WPARAM(start as usize), LPARAM(0))
                .0
                .max(0),
            SendMessageW(edit, EM_LINEFROMCHAR, WPARAM(end as usize), LPARAM(0))
                .0
                .max(0),
        )
    } else {
        (0, 0)
    };
    for line in first_line..=last_line {
        let line_start = if multiline {
            SendMessageW(edit, EM_LINEINDEX, WPARAM(line as usize), LPARAM(0)).0
        } else {
            0
        };
        if line_start < 0 {
            continue;
        }
        let line_start = line_start as usize;
        let line_length = if multiline {
            SendMessageW(edit, EM_LINELENGTH, WPARAM(line_start), LPARAM(0))
                .0
                .max(0) as usize
        } else {
            text.len()
        };
        let segment_start = (start as usize).max(line_start);
        let segment_end = (end as usize).min(line_start + line_length).min(text.len());
        if segment_end <= segment_start {
            continue;
        }
        let position = SendMessageW(edit, EM_POSFROMCHAR, WPARAM(segment_start), LPARAM(0)).0;
        if position == -1 {
            continue;
        }
        let x = (position & 0xffff) as u16 as i16 as i32;
        let y = ((position >> 16) & 0xffff) as u16 as i16 as i32;
        let segment = &text[segment_start..segment_end];
        let mut size = windows::Win32::Foundation::SIZE::default();
        let _ = windows::Win32::Graphics::Gdi::GetTextExtentPoint32W(dc, segment, &mut size);
        fill(
            dc,
            &RECT {
                left: x,
                top: y,
                right: x + size.cx,
                bottom: y + line_height,
            },
            selected_fill,
        );
        let _ = windows::Win32::Graphics::Gdi::TextOutW(dc, x, y, segment);
    }
    if saved != 0 {
        let _ = windows::Win32::Graphics::Gdi::RestoreDC(dc, saved);
    }
    if let Some(previous) = previous {
        let _ = SelectObject(dc, previous);
    }
}

fn edit_selection_key(start: u32, end: u32, focused: bool) -> usize {
    ((start as usize) << 32) | ((end as usize) << 1) | usize::from(focused)
}

/// Records the selection the field now shows, so the next refresh does not paint it again.
unsafe fn store_edit_selection_key(edit: HWND) {
    let (start, end) = edit_selection(edit);
    let focused = windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() == edit;
    let key = edit_selection_key(start, end, focused);
    let _ = SetPropW(edit, EDIT_SELECTION_PROPERTY, HANDLE(key as *mut _));
}

const EDIT_CARET_HIDDEN_PROPERTY: PCWSTR = w!("LetRecovery.Edit.CaretHidden");

/// Hides the blinking caret while the focused field has selected text and shows it again once
/// the selection is collapsed. USER32 counts HideCaret/ShowCaret, so exactly one hide is held
/// per field; the field destroys its caret (and that count) when it loses focus.
unsafe fn sync_edit_selection_caret(edit: HWND) {
    let focused = windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() == edit;
    let (start, end) = edit_selection(edit);
    let want_hidden = focused && start != end;
    let hidden = !GetPropW(edit, EDIT_CARET_HIDDEN_PROPERTY).is_invalid();
    if want_hidden && !hidden {
        if HideCaret(edit).is_ok() {
            let _ = SetPropW(
                edit,
                EDIT_CARET_HIDDEN_PROPERTY,
                HANDLE(std::ptr::dangling_mut()),
            );
        }
    } else if !want_hidden && hidden {
        let _ = RemovePropW(edit, EDIT_CARET_HIDDEN_PROPERTY);
        if focused {
            let _ = ShowCaret(edit);
        }
    }
}

/// After anything that can change the selection: when it did, the field is repainted at once
/// (buffered), so USER32's own system-blue selection drawing never stays on screen.
unsafe fn refresh_edit_selection_paint(edit: HWND) {
    sync_edit_selection_caret(edit);
    let (start, end) = edit_selection(edit);
    let focused = windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() == edit;
    let key = edit_selection_key(start, end, focused);
    let previous = GetPropW(edit, EDIT_SELECTION_PROPERTY).0 as usize;
    let had_selection = previous != 0 && ((previous >> 32) != ((previous >> 1) & 0x7fff_ffff));
    if key == previous || (start == end && !had_selection) {
        let _ = SetPropW(edit, EDIT_SELECTION_PROPERTY, HANDLE(key as *mut _));
        return;
    }
    let _ = SetPropW(edit, EDIT_SELECTION_PROPERTY, HANDLE(key as *mut _));
    let _ = RedrawWindow(
        edit,
        None,
        None,
        RDW_INVALIDATE | RDW_UPDATENOW | RDW_NOERASE,
    );
}

fn edit_selection_message(message: u32, wparam: WPARAM) -> bool {
    match message {
        0x0201 | 0x0202 | 0x0203 | 0x0100 | 0x0101 | 0x00b1 | 0x0113 | 0x0007 | 0x0008 | 0x0102
        | 0x00c2 => true,
        0x0200 => wparam.0 & 0x0001 != 0, // WM_MOUSEMOVE with the left button down
        _ => false,
    }
}

/// WM_SETTEXT with exactly the text the Edit already shows: nothing to do. Setting it again reset
/// the caret and scroll position and repainted the whole field for every repeated status update.
unsafe fn edit_text_unchanged(hwnd: HWND, lparam: LPARAM) -> bool {
    let length =
        windows::Win32::UI::WindowsAndMessaging::GetWindowTextLengthW(hwnd).max(0) as usize;
    if lparam.0 == 0 {
        return length == 0;
    }
    let pointer = PCWSTR(lparam.0 as *const u16);
    let incoming = pointer.as_wide();
    if incoming.len() != length {
        return false;
    }
    let mut current = vec![0u16; length + 1];
    let copied =
        windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(hwnd, &mut current).max(0) as usize;
    current[..copied.min(length)] == *incoming
}

unsafe fn edit_line_height(edit: HWND) -> i32 {
    let dc = windows::Win32::Graphics::Gdi::GetDC(edit);
    if dc.is_invalid() {
        return scale(18, GetDpiForWindow(edit).max(96));
    }
    let font = SendMessageW(edit, WM_GETFONT, WPARAM(0), LPARAM(0));
    let previous = (font.0 != 0)
        .then(|| SelectObject(dc, windows::Win32::Graphics::Gdi::HGDIOBJ(font.0 as *mut _)));
    let mut metrics = windows::Win32::Graphics::Gdi::TEXTMETRICW::default();
    let measured = windows::Win32::Graphics::Gdi::GetTextMetricsW(dc, &mut metrics).as_bool();
    if let Some(previous) = previous {
        let _ = SelectObject(dc, previous);
    }
    let _ = ReleaseDC(edit, dc);
    if measured {
        metrics.tmHeight.max(1)
    } else {
        scale(18, GetDpiForWindow(edit).max(96))
    }
}

/// Text placement of a framed multi-line field:
///  - the vertical scrollbar is shown only while the text needs scrolling (hidden while the text
///    fits, shown again as soon as it does not; the switch is made with painting suspended and
///    published in one buffered paint, so it never flickers);
///  - text that fits and fills at least half of the field is centred vertically, so the first
///    line is as far from the top edge as the last line is from the bottom edge;
///  - otherwise the text keeps its normal inner margins.
unsafe fn refresh_edit_layout(edit: HWND, repaint: bool) {
    const EM_SETRECTNP: u32 = 0x00b4;
    const EM_GETLINECOUNT: u32 = 0x00ba;
    if !GetPropW(edit, EDIT_LAYOUT_BUSY_PROPERTY).is_invalid() {
        return;
    }
    let _ = SetPropW(
        edit,
        EDIT_LAYOUT_BUSY_PROPERTY,
        HANDLE(std::ptr::dangling_mut()),
    );
    let dpi = GetDpiForWindow(edit).max(96);
    let horizontal = scale(6, dpi);
    let vertical = scale(4, dpi);
    let line = edit_line_height(edit);
    let mut client = RECT::default();
    let _ = GetClientRect(edit, &mut client);
    let raw_lines = SendMessageW(edit, EM_GETLINECOUNT, WPARAM(0), LPARAM(0))
        .0
        .max(1) as i32;
    // A final line break adds an empty last line that is not content.
    let length =
        windows::Win32::UI::WindowsAndMessaging::GetWindowTextLengthW(edit).max(0) as usize;
    let ends_with_break = length > 0 && {
        let mut text = vec![0u16; length + 1];
        let copied = windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(edit, &mut text).max(0)
            as usize;
        copied > 0 && text[copied - 1] == u16::from(b'\n')
    };
    let visible_lines = if ends_with_break {
        (raw_lines - 1).max(1)
    } else {
        raw_lines
    };
    let raw_content = raw_lines * line;
    let content = if length == 0 { 0 } else { visible_lines * line };
    let height = client.bottom - client.top;
    let has_bar = GetWindowLongPtrW(edit, GWL_STYLE)
        & windows::Win32::UI::WindowsAndMessaging::WS_VSCROLL.0 as isize
        != 0;
    let fits_without_bar = raw_content <= height - vertical - line / 2;
    let needs_bar = raw_content > height - vertical;
    let want_bar = if has_bar {
        !fits_without_bar
    } else {
        needs_bar
    };
    let mut repaint = repaint;
    if want_bar != has_bar {
        let _ = SendMessageW(edit, 0x000b, WPARAM(0), LPARAM(0)); // WM_SETREDRAW off
        let _ = windows::Win32::UI::Controls::ShowScrollBar(
            edit,
            windows::Win32::UI::WindowsAndMessaging::SB_VERT,
            want_bar,
        );
        let _ = SendMessageW(edit, 0x000b, WPARAM(1), LPARAM(0));
        let _ = GetClientRect(edit, &mut client);
        repaint = true;
    }
    let height = client.bottom - client.top;
    let available = (height - vertical * 2).max(line);
    let top = if !want_bar && content > 0 && content * 2 >= available {
        ((height - content) / 2).max(vertical)
    } else {
        vertical
    };
    let format = RECT {
        left: client.left + horizontal,
        top: client.top + top,
        right: (client.right - horizontal).max(client.left + horizontal + 1),
        bottom: if want_bar {
            (client.bottom - vertical).max(client.top + top + 1)
        } else {
            client.bottom.max(client.top + top + 1)
        },
    };
    let _ = SendMessageW(
        edit,
        EM_SETRECTNP,
        WPARAM(0),
        LPARAM((&format as *const RECT) as isize),
    );
    let _ = RemovePropW(edit, EDIT_LAYOUT_BUSY_PROPERTY);
    if repaint {
        let _ = RedrawWindow(edit, None, None, RDW_INVALIDATE | RDW_FRAME | RDW_NOERASE);
        if let Some(frame) = list_view_frame(edit) {
            let _ = InvalidateRect(frame, None, false);
        }
    }
}

/// Keeps the text of a framed multi-line edit away from the frame: a formatting rectangle
/// inset from the client area (USER32 resets it on every size or font change).
unsafe fn apply_edit_text_padding(edit: HWND, redraw: bool) {
    refresh_edit_layout(edit, redraw);
}

unsafe extern "system" fn framed_edit_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    const WM_SETFONT_MESSAGE: u32 = 0x0030;
    const WM_SETTEXT_MESSAGE: u32 = 0x000c;
    const WM_CONTEXTMENU_MESSAGE: u32 = 0x007b;
    const EM_REPLACESEL: u32 = 0x00c2;
    match message {
        WM_NCPAINT if super::redraw::layout_commit_active() => {
            super::redraw::defer_frame_paint(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => paint_edit_buffered(hwnd, palette_from_reference(reference_data)),
        WM_CONTEXTMENU_MESSAGE => {
            super::context_menu::show_for_edit(
                hwnd,
                lparam,
                palette_from_reference(reference_data),
            );
            LRESULT(0)
        }
        WM_SETTEXT_MESSAGE => {
            if edit_text_unchanged(hwnd, lparam) {
                return LRESULT(1);
            }
            // Replace the text with painting suspended, place it (scrollbar, centring), then
            // publish the finished field once.
            let _ = SendMessageW(hwnd, 0x000b, WPARAM(0), LPARAM(0));
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            let _ = SendMessageW(hwnd, 0x000b, WPARAM(1), LPARAM(0));
            refresh_edit_layout(hwnd, true);
            result
        }
        EM_REPLACESEL | 0x0102 | 0x0300 | 0x0302 | 0x0303 | 0x0304 => {
            // Typing, pasting, cutting, clearing or undoing can change the number of lines.
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            refresh_edit_layout(hwnd, false);
            refresh_edit_selection_paint(hwnd);
            result
        }
        WM_SIZE | WM_SETFONT_MESSAGE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            refresh_edit_layout(hwnd, message == WM_SETFONT_MESSAGE);
            result
        }
        WM_NCCALCSIZE_MESSAGE => {
            // The scrollbar appeared or disappeared: the frame recolours its right corners.
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if let Some(frame) = list_view_frame(hwnd) {
                let _ = InvalidateRect(frame, None, false);
            }
            result
        }
        WM_ENABLE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if let Some(frame) = list_view_frame(hwnd) {
                let _ = InvalidateRect(frame, None, false);
            }
            result
        }
        WM_NCDESTROY => {
            let _ = RemovePropW(hwnd, EDIT_SELECTION_PROPERTY);
            let _ = RemovePropW(hwnd, EDIT_CARET_HIDDEN_PROPERTY);
            let _ = RemoveWindowSubclass(hwnd, Some(framed_edit_subclass), FRAMED_EDIT_SUBCLASS_ID);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ if edit_selection_message(message, wparam) => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            refresh_edit_selection_paint(hwnd);
            result
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

/// (side, top/bottom) widths of the band for a window of this size, or None for windows too small
/// to give up the space.
unsafe fn frame_band_insets(hwnd: HWND, width: i32, height: i32) -> Option<(i32, i32)> {
    let geometry = rounded_control_frame_geometry(width, height, GetDpiForWindow(hwnd).max(96))?;
    let side = geometry.side_band.max(1);
    let band = geometry.arc_band.max(1);
    (width > side * 2 + 16 && height > band * 2 + 8).then_some((side, band))
}

/// WM_NCCALCSIZE of a banded field. The proposed client area is shrunk by the band before
/// USER32 subtracts its scrollbars (so the scrollbars are placed inside the band). Whatever border
/// the native control still reserves on top of that (a leftover WS_BORDER, a client edge, a
/// themed edit border) is then given back to the client, so no square outline can ever appear
/// between the rounded frame and the text.
unsafe fn band_nccalcsize(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if lparam.0 == 0 {
        return DefSubclassProc(hwnd, WM_NCCALCSIZE_MESSAGE, wparam, lparam);
    }
    let rect: *mut RECT = if wparam.0 != 0 {
        &mut (*(lparam.0 as *mut windows::Win32::UI::WindowsAndMessaging::NCCALCSIZE_PARAMS)).rgrc
            [0]
    } else {
        lparam.0 as *mut RECT
    };
    let proposed = *rect;
    let reserved = frame_band_insets(
        hwnd,
        proposed.right - proposed.left,
        proposed.bottom - proposed.top,
    )
    .map(|(side, band)| RECT {
        left: proposed.left + side,
        top: proposed.top + band,
        right: proposed.right - side,
        bottom: proposed.bottom - band,
    });
    if let Some(reserved) = reserved {
        *rect = reserved;
    }
    let result = DefSubclassProc(hwnd, WM_NCCALCSIZE_MESSAGE, wparam, lparam);
    let native = *rect;
    if let Some(reserved) = reserved {
        let client = &mut *rect;
        let extra_x = (client.left - reserved.left).max(0);
        let extra_y = (client.top - reserved.top).max(0);
        if extra_x > 0 || extra_y > 0 {
            client.left -= extra_x;
            client.top -= extra_y;
            client.right = (client.right + extra_x).min(reserved.right);
            client.bottom = (client.bottom + extra_y).min(reserved.bottom);
        }
    }
    super::redraw::ui_detail_changed(hwnd, "nccalcsize", || {
        format!(
            "proposed={proposed:?} reserved={reserved:?} native={native:?} final={:?} style={:#x} exstyle={:#x}",
            *rect,
            GetWindowLongPtrW(hwnd, GWL_STYLE),
            GetWindowLongPtrW(hwnd, windows::Win32::UI::WindowsAndMessaging::GWL_EXSTYLE)
        )
    });
    result
}

/// Re-runs WM_NCCALCSIZE so a newly installed (or re-themed) control reserves its band.
unsafe fn refresh_frame_band(hwnd: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    };
    let _ = SetWindowPos(
        hwnd,
        HWND::default(),
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
    );
}

/// Client and visible native scrollbars of a banded field, in window coordinates.
struct FrameBandLayout {
    width: i32,
    height: i32,
    client: RECT,
    bars: Vec<RECT>,
    /// Bounding box of client and scrollbars (the size box between two bars included).
    inner: RECT,
}

/// Measures what the native control really occupies instead of assuming it: a field that still
/// carries a border style (or any other non-client extra) simply has a smaller client, and the
/// frame band then covers that border too.
unsafe fn frame_band_layout(hwnd: HWND) -> Option<FrameBandLayout> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetScrollBarInfo, OBJID_HSCROLL, OBJID_VSCROLL, SCROLLBARINFO,
    };
    const STATE_SYSTEM_INVISIBLE: u32 = 0x0000_8000;
    const STATE_SYSTEM_OFFSCREEN: u32 = 0x0001_0000;
    let mut window = RECT::default();
    GetWindowRect(hwnd, &mut window).ok()?;
    let width = (window.right - window.left).max(0);
    let height = (window.bottom - window.top).max(0);
    let (side, band) = frame_band_insets(hwnd, width, height)?;
    let mut client_rect = RECT::default();
    GetClientRect(hwnd, &mut client_rect).ok()?;
    let mut origin = POINT { x: 0, y: 0 };
    if !ClientToScreen(hwnd, &mut origin).as_bool() {
        return None;
    }
    let client = RECT {
        left: origin.x - window.left,
        top: origin.y - window.top,
        right: origin.x - window.left + client_rect.right,
        bottom: origin.y - window.top + client_rect.bottom,
    };
    let mut bars = Vec::new();
    let mut inner = client;
    for object in [OBJID_VSCROLL, OBJID_HSCROLL] {
        let mut info = SCROLLBARINFO {
            cbSize: std::mem::size_of::<SCROLLBARINFO>() as u32,
            ..Default::default()
        };
        if GetScrollBarInfo(hwnd, object, &mut info).is_ok()
            && info.rgstate[0] & (STATE_SYSTEM_INVISIBLE | STATE_SYSTEM_OFFSCREEN) == 0
        {
            let bar = RECT {
                left: info.rcScrollBar.left - window.left,
                top: info.rcScrollBar.top - window.top,
                right: info.rcScrollBar.right - window.left,
                bottom: info.rcScrollBar.bottom - window.top,
            };
            if bar.right > bar.left && bar.bottom > bar.top {
                inner.left = inner.left.min(bar.left);
                inner.top = inner.top.min(bar.top);
                inner.right = inner.right.max(bar.right);
                inner.bottom = inner.bottom.max(bar.bottom);
                bars.push(bar);
            }
        }
    }
    // The band exists only once WM_NCCALCSIZE has reserved it on every side.
    if inner.left < side
        || inner.top < band
        || inner.right > width - side
        || inner.bottom > height - band
    {
        return None;
    }
    Some(FrameBandLayout {
        width,
        height,
        client,
        bars,
        inner,
    })
}

/// The inner rectangle (client plus native scrollbars, window coordinates) when the band is
/// actually reserved for the current window size; None before WM_NCCALCSIZE has applied it.
unsafe fn reserved_frame_band(hwnd: HWND) -> Option<(RECT, i32, i32)> {
    frame_band_layout(hwnd).map(|layout| (layout.inner, layout.width, layout.height))
}

/// WM_NCPAINT of a banded field: USER32/UxTheme may paint only the native scrollbars. Everything
/// else of the non-client area, the size box between two scrollbars included, is the frame band.
/// A themed Edit otherwise draws its own square border around the client inside the rounded frame
/// ("a box inside the box") and the size box keeps the light system colour.
unsafe fn paint_native_scrollbars_only(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    use windows::Win32::Graphics::Gdi::{CombineRgn, CreateRectRgn, HRGN, RGN_AND, RGN_OR};
    let Some(layout) = frame_band_layout(hwnd) else {
        return DefSubclassProc(hwnd, WM_NCPAINT, wparam, lparam);
    };
    if layout.bars.is_empty() {
        return LRESULT(0);
    }
    let mut window = RECT::default();
    if GetWindowRect(hwnd, &mut window).is_err() {
        return DefSubclassProc(hwnd, WM_NCPAINT, wparam, lparam);
    }
    // WM_NCPAINT regions are in screen coordinates.
    let region = CreateRectRgn(0, 0, 0, 0);
    if region.is_invalid() {
        return DefSubclassProc(hwnd, WM_NCPAINT, wparam, lparam);
    }
    for bar in &layout.bars {
        let part = CreateRectRgn(
            window.left + bar.left,
            window.top + bar.top,
            window.left + bar.right,
            window.top + bar.bottom,
        );
        if !part.is_invalid() {
            let _ = CombineRgn(region, region, part, RGN_OR);
            let _ = DeleteObject(part);
        }
    }
    if wparam.0 > 1 {
        let _ = CombineRgn(region, region, HRGN(wparam.0 as *mut _), RGN_AND);
    }
    let result = DefSubclassProc(hwnd, WM_NCPAINT, WPARAM(region.0 as usize), lparam);
    let _ = DeleteObject(region);
    result
}

/// The colour a text field really paints its background with: what its parent answers to the
/// WM_CTLCOLOREDIT / WM_CTLCOLORSTATIC query the field itself sends before painting. Read-only
/// and disabled fields use WM_CTLCOLORSTATIC, and a dialog may answer that differently from an
/// editable field; a frame that assumed one fixed colour showed a strip of another colour above
/// and below a disabled field.
unsafe fn edit_surface_color(control: HWND, palette: Palette) -> COLORREF {
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, DeleteDC, GetBkColor, GetObjectW, SetBkColor, BS_SOLID, HGDIOBJ,
        LOGBRUSH,
    };
    const ES_READONLY_STYLE: isize = 0x0800;
    const WM_CTLCOLOREDIT_MESSAGE: u32 = 0x0133;
    const WM_CTLCOLORSTATIC_MESSAGE: u32 = 0x0138;
    let fallback = palette.edit_brush_color_for(control);
    let Ok(parent) = GetParent(control) else {
        return fallback;
    };
    if parent.is_invalid() {
        return fallback;
    }
    let message = if GetWindowLongPtrW(control, GWL_STYLE) & ES_READONLY_STYLE != 0
        || !IsWindowEnabled(control).as_bool()
    {
        WM_CTLCOLORSTATIC_MESSAGE
    } else {
        WM_CTLCOLOREDIT_MESSAGE
    };
    let dc = CreateCompatibleDC(HDC::default());
    if dc.is_invalid() {
        return fallback;
    }
    let _ = SetBkColor(dc, fallback);
    let brush = SendMessageW(
        parent,
        message,
        WPARAM(dc.0 as usize),
        LPARAM(control.0 as isize),
    )
    .0;
    let mut color = GetBkColor(dc);
    if brush != 0 {
        let mut description = LOGBRUSH::default();
        if GetObjectW(
            HGDIOBJ(brush as *mut _),
            std::mem::size_of::<LOGBRUSH>() as i32,
            Some((&mut description as *mut LOGBRUSH).cast()),
        ) > 0
            && description.lbStyle == BS_SOLID
        {
            color = description.lbColor;
        }
    }
    let _ = DeleteDC(dc);
    if color.0 == 0xffff_ffff {
        fallback
    } else {
        color
    }
}

/// Paints outline, antialiased corners and interior-coloured padding into the band, composed
/// off-screen and published in one BitBlt. The client and the native scrollbars are excluded, so
/// they are never touched; the size box between two scrollbars is part of the band. Returns false
/// when no band is reserved.
unsafe fn paint_frame_band(hwnd: HWND, palette: Palette) -> bool {
    let Some(layout) = frame_band_layout(hwnd) else {
        super::redraw::ui_detail_changed(hwnd, "band", || {
            let mut window = RECT::default();
            let mut client = RECT::default();
            let _ = GetWindowRect(hwnd, &mut window);
            let _ = GetClientRect(hwnd, &mut client);
            format!("NOT reserved: window={window:?} client={client:?}")
        });
        return false;
    };
    super::redraw::ui_detail_changed(hwnd, "band", || {
        format!(
            "window={}x{} client={:?} bars={:?} inner={:?}",
            layout.width, layout.height, layout.client, layout.bars, layout.inner
        )
    });
    let Some(geometry) =
        rounded_control_frame_geometry(layout.width, layout.height, GetDpiForWindow(hwnd).max(96))
    else {
        return false;
    };
    let class_name = control_class_name(hwnd);
    let interior = if is_edit_class(&class_name) {
        edit_surface_color(hwnd, palette)
    } else {
        palette.edit
    };
    // A report with a visible column header continues the header surface into the upper band,
    // exactly like the header row of a native bordered ListView.
    let header_top = class_name.eq_ignore_ascii_case("SysListView32")
        && !is_category_list_view(GetWindowLongPtrW(hwnd, GWL_STYLE))
        && {
            let header = HWND(SendMessageW(hwnd, 0x101f, WPARAM(0), LPARAM(0)).0 as *mut _);
            !header.is_invalid() && IsWindowVisible(header).as_bool()
        };
    let top_interior = if header_top { palette.button } else { interior };
    let dc = GetWindowDC(hwnd);
    if dc.is_invalid() {
        return false;
    }
    let client = layout.client;
    let _ = windows::Win32::Graphics::Gdi::ExcludeClipRect(
        dc,
        client.left,
        client.top,
        client.right,
        client.bottom,
    );
    for bar in &layout.bars {
        let _ = windows::Win32::Graphics::Gdi::ExcludeClipRect(
            dc, bar.left, bar.top, bar.right, bar.bottom,
        );
    }
    let rect = RECT {
        left: 0,
        top: 0,
        right: layout.width,
        bottom: layout.height,
    };
    {
        let buffer = super::redraw::PaintBuffer::begin_opaque(dc, rect, rect);
        let surface = buffer.dc();
        fill(surface, &rect, interior);
        if top_interior != interior {
            fill(
                surface,
                &RECT {
                    left: 0,
                    top: 0,
                    right: layout.width,
                    bottom: layout.inner.top,
                },
                top_interior,
            );
            draw_antialiased_control_frame_with_vertical_interiors(
                surface,
                rect,
                geometry,
                top_interior,
                interior,
                palette.control_border(),
                rounded_control_exterior(palette),
            );
        } else {
            draw_antialiased_control_frame(
                surface,
                rect,
                geometry,
                interior,
                palette.control_border(),
                rounded_control_exterior(palette),
            );
        }
        buffer.present();
    }
    let _ = ReleaseDC(hwnd, dc);
    true
}

unsafe fn paint_rounded_control_frame(hwnd: HWND, palette: Palette) {
    if uses_frame_band(hwnd) && paint_frame_band(hwnd, palette) {
        return;
    }
    let dc = GetWindowDC(hwnd);
    if dc.0.is_null() {
        return;
    }
    let class_name = control_class_name(hwnd);
    let interior = if is_combo_class(&class_name) {
        // The antialiased inner edge must blend against the same stateful surface as the closed
        // selection field. Using the normal button colour here leaves a white/grey crescent when
        // the field has already switched to its hot or dropped colour.
        combo_closed_surface(hwnd, palette)
    } else {
        palette.edit
    };
    if is_edit_class(&class_name) && is_single_line_edit(hwnd) {
        paint_single_line_edit_nonclient_bands(dc, hwnd, palette.edit_brush_color_for(hwnd));
    }
    draw_rounded_control_frame_to_dc(dc, hwnd, palette, interior);
    let _ = ReleaseDC(hwnd, dc);
}

/// Covers USER32's stock single-border band before the deterministic rounded frame is published.
/// The client rectangle remains untouched, so USER32 keeps its native formatting rectangle and
/// continues to own text, selection, caret, horizontal scrolling and IME.
unsafe fn paint_single_line_edit_nonclient_bands(dc: HDC, hwnd: HWND, interior: COLORREF) {
    let mut window = RECT::default();
    if GetWindowRect(hwnd, &mut window).is_err() {
        return;
    }
    let width = (window.right - window.left).max(0);
    let height = (window.bottom - window.top).max(0);
    if rounded_control_frame_geometry(width, height, GetDpiForWindow(hwnd).max(96)).is_none() {
        return;
    }
    let mut client = RECT::default();
    if GetClientRect(hwnd, &mut client).is_err() {
        return;
    }
    let mut client_top_left = POINT {
        x: client.left,
        y: client.top,
    };
    let mut client_bottom_right = POINT {
        x: client.right,
        y: client.bottom,
    };
    if !ClientToScreen(hwnd, &mut client_top_left).as_bool()
        || !ClientToScreen(hwnd, &mut client_bottom_right).as_bool()
    {
        return;
    }
    let client_left = (client_top_left.x - window.left).clamp(0, width);
    let client_top = (client_top_left.y - window.top).clamp(0, height);
    let client_right = (client_bottom_right.x - window.left).clamp(client_left, width);
    let client_bottom = (client_bottom_right.y - window.top).clamp(client_top, height);
    let brush = CreateSolidBrush(interior);
    if brush.0.is_null() {
        return;
    }
    for rect in [
        RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: client_top,
        },
        RECT {
            left: 0,
            top: client_bottom,
            right: width,
            bottom: height,
        },
        RECT {
            left: 0,
            top: client_top,
            right: client_left,
            bottom: client_bottom,
        },
        RECT {
            left: client_right,
            top: client_top,
            right: width,
            bottom: client_bottom,
        },
    ] {
        let _ = FillRect(dc, &rect, brush);
    }
    let _ = DeleteObject(brush);
}

unsafe fn is_drop_down_list(hwnd: HWND) -> bool {
    const COMBO_TYPE_MASK: isize = 0x0003;
    const CBS_DROPDOWNLIST_VALUE: isize = 0x0003;
    is_combo_class(&control_class_name(hwnd))
        && GetWindowLongPtrW(hwnd, GWL_STYLE) & COMBO_TYPE_MASK == CBS_DROPDOWNLIST_VALUE
}

/// Sets the closed selection field to the shared Inno control baseline.  This is deliberately
/// independent from the native popup row height: Microsoft documents the selection field as
/// component 1 for CB_SETITEMHEIGHT, while popup items remain component 0.
unsafe fn set_combo_selection_field_height(hwnd: HWND, height: i32) {
    const CB_SETITEMHEIGHT: u32 = 0x0153;
    const SELECTION_FIELD: usize = 1;
    const CB_ERR: isize = -1;

    if height <= 0 {
        return;
    }
    // Already at this height: setting it again made the combo recalculate and resize itself and
    // forced a frame change on every size step of the page.
    const CB_GETITEMHEIGHT: u32 = 0x0154;
    if SendMessageW(hwnd, CB_GETITEMHEIGHT, WPARAM(SELECTION_FIELD), LPARAM(0)).0 == height as isize
    {
        return;
    }
    let result = SendMessageW(
        hwnd,
        CB_SETITEMHEIGHT,
        WPARAM(SELECTION_FIELD),
        LPARAM(height as isize),
    );
    if result.0 != CB_ERR {
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// Fixes stock popup rows to the same DPI-scaled baseline on both the initial theme transaction
/// and later theme changes. USER32 otherwise keeps the pre-font row height until the first theme
/// switch, which makes the initial dark popup visibly shorter than the same popup afterwards.
unsafe fn set_combo_popup_row_height(hwnd: HWND, height: i32) {
    const CB_SETITEMHEIGHT: u32 = 0x0153;
    const POPUP_ROWS: usize = 0;
    const CB_GETITEMHEIGHT: u32 = 0x0154;
    if height > 0
        && SendMessageW(hwnd, CB_GETITEMHEIGHT, WPARAM(POPUP_ROWS), LPARAM(0)).0 != height as isize
    {
        let _ = SendMessageW(
            hwnd,
            CB_SETITEMHEIGHT,
            WPARAM(POPUP_ROWS),
            LPARAM(height as isize),
        );
    }
}

/// Restricts the visible/hit-test region of a closed drop-down ComboBox to its selection field.
///
/// The height passed to `MoveWindow` is the fully expanded list height. Full Windows clips the
/// closed control internally, but reduced WinPE USER32 builds can expose pixels from that retained
/// height below the field. The popup list is a separate top-level ComboLBox, so clipping this HWND
/// does not change native popup, keyboard or accessibility behaviour.
unsafe fn clip_combo_to_closed_field(hwnd: HWND, _palette: Palette) {
    if !is_drop_down_list(hwnd) {
        return;
    }
    let mut window = RECT::default();
    if GetWindowRect(hwnd, &mut window).is_err() {
        return;
    }
    let width = (window.right - window.left).max(0);
    let full_height = (window.bottom - window.top).max(0);
    if width == 0 || full_height == 0 {
        return;
    }
    let dpi = GetDpiForWindow(hwnd).max(96);
    let closed_height =
        combo_closed_height(hwnd, InnoMetrics::for_dpi(dpi).field_height).clamp(1, full_height);
    // The same region is already set: nothing to do (this runs on every size change).
    let mut current = RECT::default();
    if windows::Win32::Graphics::Gdi::GetWindowRgnBox(hwnd, &mut current).0 != 0
        && current
            == (RECT {
                left: 0,
                top: 0,
                right: width,
                bottom: closed_height,
            })
    {
        return;
    }
    // Keep this region rectangular. CreateRoundRectRgn is a binary pixel mask; using it as the
    // visible silhouette clips away partially covered pixels and leaves a stair-stepped bracket.
    // The painter masks the four corners; this region only hides USER32's retained list height.
    let region = CreateRectRgn(0, 0, width, closed_height);
    // Microsoft recommends redrawing a visible window when changing its region. Without that
    // redraw, the pixels removed from USER32's retained drop-list height survive as a narrow line
    // immediately below the deterministic field until some unrelated parent repaint occurs.
    if !region.is_invalid() {
        if SetWindowRgn(hwnd, region, true) == 0 {
            let _ = DeleteObject(region);
        } else if full_height > closed_height {
            // The removed tail belonged to the child before SetWindowRgn, but becomes parent
            // surface afterwards. SetWindowRgn redraws the child only; explicitly invalidate that
            // parent strip or its last stock UxTheme row can survive as the reported bottom line.
            let parent = GetParent(hwnd).unwrap_or_default();
            let mut parent_origin = POINT::default();
            if !parent.is_invalid() && ClientToScreen(parent, &mut parent_origin).as_bool() {
                let exposed_tail = RECT {
                    left: window.left - parent_origin.x,
                    top: window.top - parent_origin.y + closed_height,
                    right: window.right - parent_origin.x,
                    bottom: window.bottom - parent_origin.y,
                };
                let _ = RedrawWindow(
                    parent,
                    Some(&exposed_tail),
                    None,
                    RDW_INVALIDATE | RDW_ERASE | RDW_FRAME,
                );
            }
        }
    }
}

/// Removes any earlier binary rounded region from a stock child HWND.
///
/// A GDI region has no partial coverage and cannot represent the visible antialiased edge. The
/// deterministic last-paint transaction replaces the small exterior corner pixels instead.
unsafe fn clear_legacy_control_region(hwnd: HWND) {
    // Runs on every size step. Only touch windows that really carry a region: SetWindowRgn forces
    // a complete frame recalculation and repaint even when it removes nothing.
    let mut bounds = RECT::default();
    if windows::Win32::Graphics::Gdi::GetWindowRgnBox(hwnd, &mut bounds).0 == 0 {
        return;
    }
    clear_control_window_region(hwnd);
}

unsafe fn disable_edit_layered_redirection(hwnd: HWND) {
    let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
    let desired = edit_ex_style_without_layering(current);
    if desired != current {
        let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, desired);
    }
}

const fn edit_ex_style_without_layering(ex_style: isize) -> isize {
    ex_style & !(WS_EX_LAYERED.0 as isize)
}

unsafe fn clear_control_window_region(hwnd: HWND) {
    let _ = SetWindowRgn(hwnd, windows::Win32::Graphics::Gdi::HRGN::default(), true);
}

/// Returns the height of the visible, closed selection field of a stock drop-down list.
///
/// `CB_SETITEMHEIGHT(1)` establishes the selection field independently from popup rows. Some
/// USER32 builds nevertheless report `COMBOBOXINFO` item/button rectangles a few pixels taller
/// than the requested field because they include stock non-client padding. Our borderless custom
/// painter owns that padding, so using the reported height would expose a second bottom strip. The
/// shared Inno field baseline is therefore the authoritative visible and layout height.
pub(crate) unsafe fn combo_closed_height(hwnd: HWND, fallback: i32) -> i32 {
    combo_closed_visual_height(fallback, GetDpiForWindow(hwnd).max(96))
}

fn combo_closed_visual_height(requested: i32, dpi: u32) -> i32 {
    requested.max(1).clamp(scale(18, dpi), scale(36, dpi))
}

/// Paints the complete closed CBS_DROPDOWNLIST in one WM_PAINT transaction. USER32 continues to own
/// hit testing, selection, keyboard navigation, accessibility and the separate native popup. The
/// closed HWND never calls its default WM_PAINT renderer, so a Windows 10/11 UxTheme animation cannot
/// briefly compose a rectangular frame underneath the rounded Inno field.
unsafe fn paint_combo_closed(hwnd: HWND, palette: Palette) {
    let mut paint = PAINTSTRUCT::default();
    let _ = BeginPaint(hwnd, &mut paint);
    let _ = EndPaint(hwnd, &paint);

    // BeginPaint clips its HDC to the current update region. Hover/focus changes can invalidate only
    // the selection or arrow half, so painting the complete compatibility surface through that HDC
    // leaves the other half stale and can omit part of an edge. Validate the update above, then
    // publish the whole closed client through an unclipped client DC and its frame through an
    // unclipped window DC.
    // A stock ComboBox can retain a themed two-pixel client inset even after WS_BORDER and
    // WS_EX_CLIENTEDGE are removed. GetDC starts inside that inset, leaving USER32's pale top arc
    // untouched. Paint the complete closed window surface through the window DC, then publish our
    // single deterministic frame last.
    repaint_combo_closed_now(hwnd, palette);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ComboClosedState {
    Normal,
    Hot,
    Dropped,
}

unsafe fn combo_closed_state(hwnd: HWND) -> ComboClosedState {
    const CB_GETDROPPEDSTATE: u32 = 0x0157;
    const STATE_SYSTEM_PRESSED: u32 = 0x0000_0008;
    let mut info = COMBOBOXINFO {
        cbSize: std::mem::size_of::<COMBOBOXINFO>() as u32,
        ..Default::default()
    };
    let _ = GetComboBoxInfo(hwnd, &mut info);
    let pointer_inside = {
        let mut pointer = POINT::default();
        let mut window = RECT::default();
        GetCursorPos(&mut pointer).is_ok()
            && GetWindowRect(hwnd, &mut window).is_ok()
            && pointer.x >= window.left
            && pointer.x < window.right
            && pointer.y >= window.top
            && pointer.y < window.bottom
    };
    let hot = pointer_inside || !GetPropW(hwnd, ROUNDED_CONTROL_HOT_PROPERTY).is_invalid();
    let dropped = !GetPropW(hwnd, COMBO_TRACKING_DROPPED_PROPERTY).is_invalid()
        || info.stateButton.0 & STATE_SYSTEM_PRESSED != 0
        || SendMessageW(hwnd, CB_GETDROPPEDSTATE, WPARAM(0), LPARAM(0)).0 != 0;
    if dropped {
        ComboClosedState::Dropped
    } else if hot && IsWindowEnabled(hwnd).as_bool() {
        ComboClosedState::Hot
    } else {
        ComboClosedState::Normal
    }
}

unsafe fn combo_closed_surface(hwnd: HWND, palette: Palette) -> COLORREF {
    match combo_closed_state(hwnd) {
        ComboClosedState::Dropped => {
            if palette.dark {
                palette.button_pressed
            } else {
                rgb(204, 228, 247)
            }
        }
        ComboClosedState::Hot => {
            if palette.dark {
                palette.button_hot
            } else {
                rgb(229, 241, 251)
            }
        }
        ComboClosedState::Normal => palette.button,
    }
}

unsafe fn draw_combo_selected_text(hwnd: HWND, dc: HDC, mut text_rect: RECT, palette: Palette) {
    const CB_GETCURSEL: u32 = 0x0147;
    const CB_GETLBTEXT: u32 = 0x0148;
    const CB_GETLBTEXTLEN: u32 = 0x0149;
    const CB_ERR: isize = -1;
    let selected = SendMessageW(hwnd, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
    if selected == CB_ERR {
        return;
    }
    let length = SendMessageW(hwnd, CB_GETLBTEXTLEN, WPARAM(selected as usize), LPARAM(0)).0;
    if length < 0 {
        return;
    }
    let mut text = vec![0u16; length as usize + 1];
    let copied = SendMessageW(
        hwnd,
        CB_GETLBTEXT,
        WPARAM(selected as usize),
        LPARAM(text.as_mut_ptr() as isize),
    )
    .0
    .max(0) as usize;
    text.truncate(copied.min(text.len()));
    let font = SendMessageW(hwnd, WM_GETFONT, WPARAM(0), LPARAM(0));
    let old_font = (font.0 != 0)
        .then(|| SelectObject(dc, windows::Win32::Graphics::Gdi::HGDIOBJ(font.0 as *mut _)));
    let _ = SetBkMode(dc, TRANSPARENT);
    let _ = SetTextColor(
        dc,
        if IsWindowEnabled(hwnd).as_bool() {
            palette.text
        } else {
            palette.text_disabled
        },
    );
    let dpi = GetDpiForWindow(hwnd).max(96);
    text_rect.left += scale(6, dpi);
    text_rect.right -= scale(3, dpi);
    let mut text_metrics = windows::Win32::Graphics::Gdi::TEXTMETRICW::default();
    let measured = GetTextMetricsW(dc, &mut text_metrics).as_bool();
    if measured {
        let available = (text_rect.bottom - text_rect.top).max(0);
        let text_height = text_metrics.tmHeight.clamp(1, available.max(1));
        let spare = available.saturating_sub(text_height);
        text_rect.top += (spare + 1) / 2;
        text_rect.bottom = text_rect.top + text_height;
    }
    let flags = if measured {
        DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX
    } else {
        DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX
    };
    let color = if IsWindowEnabled(hwnd).as_bool() {
        palette.text
    } else {
        palette.text_disabled
    };
    draw_native_text(dc, &text, &mut text_rect, flags, color);
    if let Some(old_font) = old_font {
        let _ = SelectObject(dc, old_font);
    }
}

unsafe fn paint_combo_selection_item_to_dc(item: HWND, palette: Palette, dc: HDC) {
    let Ok(combo) = GetParent(item) else {
        return;
    };
    if combo.0.is_null() || !is_drop_down_list(combo) {
        return;
    }
    let mut client = RECT::default();
    if GetClientRect(item, &mut client).is_err() || client.right <= client.left {
        return;
    }
    fill(dc, &client, combo_closed_surface(combo, palette));
    draw_combo_selected_text(combo, dc, client, palette);
}

unsafe fn repaint_combo_selection_item_now(item: HWND, palette: Palette) {
    paint_combo_selection_item_window(item, palette);
    if let Ok(combo) = GetParent(item) {
        if !combo.0.is_null() && is_drop_down_list(combo) {
            repaint_combo_closed_now(combo, palette);
        }
    }
}

/// Paints both the client and non-client pixels of USER32's closed selection child.
///
/// Reduced WinPE USER32 builds keep a focus underline in the child's non-client bottom band.
/// Painting only `GetDC(item)` therefore leaves a blue line while focused and a dark line after
/// focus moves away. A window DC lets the deterministic closed-field surface replace that band in
/// the same synchronous transaction as the parent ComboBox frame.
unsafe fn paint_combo_selection_item_window(item: HWND, palette: Palette) {
    let Ok(combo) = GetParent(item) else {
        return;
    };
    if combo.0.is_null() || !is_drop_down_list(combo) {
        return;
    }

    let mut window = RECT::default();
    if GetWindowRect(item, &mut window).is_err() {
        return;
    }
    let width = window.right - window.left;
    let height = window.bottom - window.top;
    if width <= 0 || height <= 0 {
        return;
    }

    // Surface and caption are composed together; filling first and drawing the caption on the
    // screen afterwards blanked the selected text for a frame on every state change.
    let surface = combo_closed_surface(combo, palette);
    super::redraw::paint_window_buffered(item, |dc, bounds| {
        fill(dc, &bounds, surface);
        draw_combo_selected_text(combo, dc, bounds, palette);
    });
}

/// Publishes the complete closed ComboBox (surface, chevron, caption and rounded frame) in one
/// composed BitBlt through its window DC.
///
/// Hover, focus and click transitions repaint this control several times. Writing the surface,
/// then the chevron and caption, then the four edges and the antialiased corners straight to the
/// screen let DWM present the half-finished states (caption blanked, square edge) as a flicker.
/// Only the closed field is published: where USER32 keeps a taller window rectangle (the drop
/// height), nothing below the field may be painted, or a page-coloured block covers the controls
/// underneath.
/// Runs USER32's handling of a message with the combo's own painting suspended (WM_SETREDRAW off
/// hides it from direct drawing); the caller then publishes the field with our painter.
unsafe fn call_combo_without_native_paint(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let visible = IsWindowVisible(hwnd).as_bool();
    if visible {
        let _ = SendMessageW(hwnd, 0x000b, WPARAM(0), LPARAM(0));
    }
    let result = DefSubclassProc(hwnd, message, wparam, lparam);
    if visible {
        let _ = SendMessageW(hwnd, 0x000b, WPARAM(1), LPARAM(0));
    }
    result
}

/// Surface colour of a closed drop-down field (the popup list uses the same colour).
/// Publishes the closed field of a drop-down combo (used by combo_popup after open/close).
pub(crate) unsafe fn repaint_drop_down_field(combo: HWND, palette: Palette) {
    repaint_combo_closed_now(combo, palette);
}

unsafe fn repaint_combo_closed_now(combo: HWND, palette: Palette) {
    let mut window = RECT::default();
    if GetWindowRect(combo, &mut window).is_err() {
        return;
    }
    let dpi = GetDpiForWindow(combo).max(96);
    let width = (window.right - window.left).max(0);
    let height = combo_closed_height(combo, InnoMetrics::for_dpi(dpi).field_height)
        .min((window.bottom - window.top).max(0));
    if width == 0 || height <= 0 {
        return;
    }
    let bounds = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };
    let interior = combo_closed_surface(combo, palette);
    let dc = GetWindowDC(combo);
    if dc.is_invalid() {
        return;
    }
    {
        let buffer = super::redraw::PaintBuffer::begin_opaque(dc, bounds, bounds);
        let surface = buffer.dc();
        fill(surface, &bounds, rounded_control_exterior(palette));
        paint_combo_closed_window_to_dc(combo, palette, surface);
        draw_rounded_control_frame_to_dc(surface, combo, palette, interior);
        buffer.present();
    }
    let _ = ReleaseDC(combo, dc);
}

unsafe fn paint_combo_closed_to_dc(hwnd: HWND, palette: Palette, dc: HDC) {
    let dpi = GetDpiForWindow(hwnd).max(96);
    let mut client = RECT::default();
    if GetClientRect(hwnd, &mut client).is_err() {
        return;
    }
    let available_height = (client.bottom - client.top).max(1);
    client.bottom = client.top
        + combo_closed_height(hwnd, InnoMetrics::for_dpi(dpi).field_height).min(available_height);
    paint_combo_closed_bounds_to_dc(hwnd, palette, dc, client);
}

unsafe fn paint_combo_closed_window_to_dc(hwnd: HWND, palette: Palette, dc: HDC) {
    let dpi = GetDpiForWindow(hwnd).max(96);
    let mut window = RECT::default();
    if GetWindowRect(hwnd, &mut window).is_err() {
        return;
    }
    let width = (window.right - window.left).max(1);
    let available_height = (window.bottom - window.top).max(1);
    let bounds = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: combo_closed_height(hwnd, InnoMetrics::for_dpi(dpi).field_height)
            .min(available_height),
    };
    paint_combo_closed_bounds_to_dc(hwnd, palette, dc, bounds);
}

unsafe fn paint_combo_closed_bounds_to_dc(hwnd: HWND, palette: Palette, dc: HDC, client: RECT) {
    let mut info = COMBOBOXINFO {
        cbSize: std::mem::size_of::<COMBOBOXINFO>() as u32,
        ..Default::default()
    };
    if GetComboBoxInfo(hwnd, &mut info).is_err() {
        return;
    }
    let dpi = GetDpiForWindow(hwnd).max(96);
    // The native arrow width is stable, but COMBOBOXINFO rectangle origins differ across the
    // USER32 implementations we support. Rebuild client-local rectangles from the actual HWND
    // so the compatibility surface always covers the complete closed control.
    let arrow_width = (info.rcButton.right - info.rcButton.left)
        .max(scale(17, dpi))
        .min((client.right - client.left).max(0) / 2);
    let button = RECT {
        left: client.right - arrow_width,
        top: client.top + scale(1, dpi),
        right: client.right - scale(1, dpi),
        bottom: client.bottom - scale(1, dpi),
    };
    let field = RECT {
        left: client.left + scale(1, dpi),
        top: client.top + scale(1, dpi),
        right: button.left,
        bottom: client.bottom - scale(1, dpi),
    };
    if field.right <= field.left || field.bottom <= field.top {
        return;
    }

    // Use one surface decision for the parent, its selection child and the native-arrow-sized
    // compatibility glyph. This prevents WinPE from showing a differently coloured seam.
    let surface = combo_closed_surface(hwnd, palette);
    fill(dc, &client, surface);
    fill(dc, &field, surface);
    fill(dc, &button, surface);

    // Keep the popup itself native. Only the closed chevron is repainted so its hot/pressed
    // feedback changes in the same frame as the selection field, without UxTheme transition lag.
    let glyph_width = scale(8, dpi).max(7);
    let glyph_height = scale(5, dpi).max(5);
    let glyph_x = (button.left + button.right - glyph_width) / 2;
    let glyph_y = (button.top + button.bottom - glyph_height) / 2;
    let glyph = combo_chevron_pixels(
        glyph_width,
        glyph_height,
        if IsWindowEnabled(hwnd).as_bool() {
            palette.text_secondary
        } else {
            palette.text_disabled
        },
    );
    let _ = alpha_blend_premultiplied_bgra(dc, glyph_x, glyph_y, glyph_width, glyph_height, &glyph);

    draw_combo_selected_text(hwnd, dc, field, palette);
}

fn combo_chevron_pixels(width: i32, height: i32, color: COLORREF) -> Vec<u8> {
    const SAMPLES: i32 = 4;
    let width = width.max(1);
    let height = height.max(1);
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    let left = (0.75f64, 0.75f64);
    let middle = (f64::from(width - 1) / 2.0, f64::from(height - 1) - 0.5);
    let right = (f64::from(width - 1) - 0.75, 0.75f64);
    let radius = 0.72f64;
    for y in 0..height {
        for x in 0..width {
            let mut covered = 0u32;
            for sample_y in 0..SAMPLES {
                for sample_x in 0..SAMPLES {
                    let px = f64::from(x) + (f64::from(sample_x) + 0.5) / f64::from(SAMPLES);
                    let py = f64::from(y) + (f64::from(sample_y) + 0.5) / f64::from(SAMPLES);
                    if point_segment_distance(px, py, left, middle) <= radius
                        || point_segment_distance(px, py, middle, right) <= radius
                    {
                        covered += 1;
                    }
                }
            }
            let alpha = ((covered * 255 + (SAMPLES * SAMPLES / 2) as u32)
                / (SAMPLES * SAMPLES) as u32) as u8;
            let index = (y as usize * width as usize + x as usize) * 4;
            pixels[index] = premultiply_channel(((color.0 >> 16) & 0xff) as u8, alpha);
            pixels[index + 1] = premultiply_channel(((color.0 >> 8) & 0xff) as u8, alpha);
            pixels[index + 2] = premultiply_channel((color.0 & 0xff) as u8, alpha);
            pixels[index + 3] = alpha;
        }
    }
    pixels
}

fn point_segment_distance(x: f64, y: f64, start: (f64, f64), end: (f64, f64)) -> f64 {
    let dx = end.0 - start.0;
    let dy = end.1 - start.1;
    let length_squared = dx * dx + dy * dy;
    let projection = if length_squared <= f64::EPSILON {
        0.0
    } else {
        (((x - start.0) * dx + (y - start.1) * dy) / length_squared).clamp(0.0, 1.0)
    };
    let nearest_x = start.0 + projection * dx;
    let nearest_y = start.1 + projection * dy;
    (x - nearest_x).hypot(y - nearest_y)
}

const fn premultiply_channel(channel: u8, alpha: u8) -> u8 {
    ((channel as u16 * alpha as u16 + 127) / 255) as u8
}

unsafe fn draw_rounded_control_frame_to_dc(
    dc: HDC,
    hwnd: HWND,
    palette: Palette,
    interior: COLORREF,
) {
    let mut window = RECT::default();
    if GetWindowRect(hwnd, &mut window).is_err() {
        return;
    }
    let class_name = control_class_name(hwnd);
    let full_height = (window.bottom - window.top).max(0);
    let visible_height = if is_combo_class(&class_name) && is_drop_down_list(hwnd) {
        combo_closed_height(
            hwnd,
            InnoMetrics::for_dpi(GetDpiForWindow(hwnd).max(96)).field_height,
        )
        .min(full_height)
    } else {
        full_height
    };
    let rect = RECT {
        left: 0,
        top: 0,
        right: (window.right - window.left).max(0),
        bottom: visible_height,
    };
    let Some(geometry) =
        rounded_control_frame_geometry(rect.right, rect.bottom, GetDpiForWindow(hwnd).max(96))
    else {
        return;
    };
    let interactive_field =
        is_combo_class(&class_name) || (is_edit_class(&class_name) && is_single_line_edit(hwnd));
    let hot = !GetPropW(hwnd, ROUNDED_CONTROL_HOT_PROPERTY).is_invalid();
    let focus = GetFocus();
    let focused = focus == hwnd
        || if is_combo_class(&class_name) && is_drop_down_list(hwnd) {
            let mut info = COMBOBOXINFO {
                cbSize: std::mem::size_of::<COMBOBOXINFO>() as u32,
                ..Default::default()
            };
            GetComboBoxInfo(hwnd, &mut info).is_ok()
                && !info.hwndItem.0.is_null()
                && focus == info.hwndItem
        } else {
            false
        };
    let combo_active = is_combo_class(&class_name)
        && matches!(
            combo_closed_state(hwnd),
            ComboClosedState::Hot | ComboClosedState::Dropped
        );
    let border = if !IsWindowEnabled(hwnd).as_bool() {
        palette.control_border()
    } else if interactive_field && (focused || combo_active) {
        palette.accent_border
    } else if interactive_field && hot {
        palette.separator
    } else {
        palette.control_border()
    };
    // CreateRoundRectRgn/FrameRgn is an integer region operation and therefore cannot be the
    // visible outline: at 96-200 DPI it produces the grainy staircase reported by the user. The
    // deterministic coverage calculation paints the straight stroke and every corner sample.
    let exterior = rounded_control_exterior(palette);
    draw_antialiased_control_frame(dc, rect, geometry, interior, border, exterior);
}

const fn rounded_control_exterior(palette: Palette) -> COLORREF {
    palette.window
}

unsafe fn is_list_box(hwnd: HWND) -> bool {
    matches!(control_class_name(hwnd).as_str(), "ListBox" | "ComboLBox")
}

// ------------------------------------------------------------------------------------------
// Standalone ListBox: one composed surface, row-precise invalidation, no stock drawing.
// ------------------------------------------------------------------------------------------
//
// The former overlay let USER32 paint the whole list (white rows, system-blue selection) and then
// painted every visible row again through GetDC, so each WM_PAINT reached the screen twice. Every
// row change of the pointer invalidated the complete list, and every mouse message republished the
// rounded frame through GetWindowDC one pixel at a time. USER32 also draws a click, a key press or
// LB_SETCURSEL straight to the screen without WM_PAINT. The result was a list that flickered while
// the pointer merely moved over it.
//
// Now:
// * WM_PAINT composes background, rows, text and the frame edges inside the client off-screen and
//   publishes them with one BitBlt; USER32's own WM_PAINT is never called.
// * Hover invalidates only the row that lost and the row that gained the hot state.
// * Every message that lets USER32 change selection, caret, focus or scroll position runs with the
//   control's own redraw switched off (WM_SETREDRAW, which a ListBox handles internally), then the
//   rows whose state actually changed are invalidated. USER32 therefore never draws system blue.
// * The dotted focus rectangle is an XOR drawn directly on screen; it is kept hidden through the
//   documented UISF_HIDEFOCUS UI state, as Raymond Chen describes for controls that must never show
//   one (the row highlight already marks the current item).

const LIST_BOX_SUBCLASS_ID: usize = 0x4c52_4c42;
const LB_DELETESTRING_MESSAGE: u32 = 0x0182;
const LB_SELITEMRANGEEX_MESSAGE: u32 = 0x0183;
const LB_RESETCONTENT_MESSAGE: u32 = 0x0184;
const LB_SETSEL_MESSAGE: u32 = 0x0185;
const LB_SETCURSEL_MESSAGE: u32 = 0x0186;
const LB_GETSEL_MESSAGE: u32 = 0x0187;
const LB_GETCURSEL_MESSAGE: u32 = 0x0188;
const LB_GETTEXT_MESSAGE: u32 = 0x0189;
const LB_GETTEXTLEN_MESSAGE: u32 = 0x018a;
const LB_GETCOUNT_MESSAGE: u32 = 0x018b;
const LB_SELECTSTRING_MESSAGE: u32 = 0x018c;
const LB_GETTOPINDEX_MESSAGE: u32 = 0x018e;
const LB_SETTOPINDEX_MESSAGE: u32 = 0x0197;
const LB_GETITEMRECT_MESSAGE: u32 = 0x0198;
const LB_SELITEMRANGE_MESSAGE: u32 = 0x019b;
const LB_SETANCHORINDEX_MESSAGE: u32 = 0x019c;
const LB_SETCARETINDEX_MESSAGE: u32 = 0x019e;
const LB_SETITEMHEIGHT_MESSAGE: u32 = 0x01a0;
const LB_GETITEMHEIGHT_MESSAGE: u32 = 0x01a1;
const LB_ITEMFROMPOINT_MESSAGE: u32 = 0x01a9;
const LBS_MULTIPLESEL_STYLE: isize = 0x0008;
const LBS_EXTENDEDSEL_STYLE: isize = 0x0800;
const LBS_NOSEL_STYLE: isize = 0x4000;
const WM_CHAR_MESSAGE: u32 = 0x0102;
const WM_TIMER_MESSAGE: u32 = 0x0113;
const WM_HSCROLL_MESSAGE: u32 = 0x0114;
const WM_VSCROLL_MESSAGE: u32 = 0x0115;
const WM_LBUTTONDBLCLK_MESSAGE: u32 = 0x0203;
const WM_MOUSEWHEEL_MESSAGE: u32 = 0x020a;
const UIS_SET_ACTION: usize = 1;
const UISF_HIDEFOCUS_FLAG: usize = 0x1;

#[derive(Clone, Copy, Default)]
struct ListBoxVisualState {
    hot: Option<usize>,
    leave_tracked: bool,
    application_redraw_off: bool,
    native_redraw_suppressed: bool,
}

thread_local! {
    static LIST_BOX_VISUAL_STATES: std::cell::RefCell<Vec<(isize, ListBoxVisualState)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn list_box_state(hwnd: HWND) -> ListBoxVisualState {
    LIST_BOX_VISUAL_STATES.with(|states| {
        states
            .borrow()
            .iter()
            .find(|(key, _)| *key == hwnd.0 as isize)
            .map(|(_, state)| *state)
            .unwrap_or_default()
    })
}

fn update_list_box_state(hwnd: HWND, update: impl FnOnce(&mut ListBoxVisualState)) {
    LIST_BOX_VISUAL_STATES.with(|states| {
        let mut states = states.borrow_mut();
        let key = hwnd.0 as isize;
        if let Some((_, state)) = states.iter_mut().find(|(candidate, _)| *candidate == key) {
            update(state);
        } else {
            let mut state = ListBoxVisualState::default();
            update(&mut state);
            states.push((key, state));
        }
    });
}

fn forget_list_box_state(hwnd: HWND) {
    LIST_BOX_VISUAL_STATES.with(|states| {
        states
            .borrow_mut()
            .retain(|(key, _)| *key != hwnd.0 as isize);
    });
}

unsafe fn install_list_box_subclass(control: HWND, palette: Palette) {
    // Earlier builds used the generic overlay subclass and window properties for this control.
    let _ = RemoveWindowSubclass(
        control,
        Some(rounded_control_subclass),
        ROUNDED_CONTROL_SUBCLASS_ID,
    );
    let _ = RemovePropW(control, LIST_BOX_HOT_PROPERTY);
    let _ = RemovePropW(control, ROUNDED_CONTROL_HOT_PROPERTY);
    update_list_box_state(control, |_| {});
    let _ = SetWindowSubclass(
        control,
        Some(list_box_subclass),
        LIST_BOX_SUBCLASS_ID,
        palette_reference(palette),
    );
    // The frame lives in a reserved non-client band, beside (never over) the native scrollbar.
    refresh_frame_band(control);
    let _ = SendMessageW(
        control,
        WM_UPDATEUISTATE_MESSAGE,
        WPARAM(UIS_SET_ACTION | (UISF_HIDEFOCUS_FLAG << 16)),
        LPARAM(0),
    );
}

unsafe fn list_box_item_rect(hwnd: HWND, index: usize) -> Option<RECT> {
    let mut rect = RECT::default();
    (SendMessageW(
        hwnd,
        LB_GETITEMRECT_MESSAGE,
        WPARAM(index),
        LPARAM((&mut rect as *mut RECT) as isize),
    )
    .0 >= 0)
        .then_some(rect)
}

unsafe fn invalidate_list_box_row(hwnd: HWND, index: usize) {
    if let Some(rect) = list_box_item_rect(hwnd, index) {
        let _ = InvalidateRect(hwnd, Some(&rect), false);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ListBoxSelectionMode {
    Single,
    Multiple,
    None,
}

unsafe fn list_box_selection_mode(hwnd: HWND) -> ListBoxSelectionMode {
    let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
    if style & LBS_NOSEL_STYLE != 0 {
        ListBoxSelectionMode::None
    } else if style & (LBS_MULTIPLESEL_STYLE | LBS_EXTENDEDSEL_STYLE) != 0 {
        ListBoxSelectionMode::Multiple
    } else {
        ListBoxSelectionMode::Single
    }
}

/// What the painted rows depend on. Comparing a snapshot taken before and after a native state
/// change tells exactly which rows need a repaint.
#[derive(PartialEq, Eq)]
struct ListBoxSnapshot {
    top: isize,
    count: isize,
    item_height: isize,
    /// Selection flag of every row that is at least partly visible, from `top` down.
    rows: Vec<bool>,
}

unsafe fn list_box_snapshot(hwnd: HWND) -> ListBoxSnapshot {
    let top = SendMessageW(hwnd, LB_GETTOPINDEX_MESSAGE, WPARAM(0), LPARAM(0)).0;
    let count = SendMessageW(hwnd, LB_GETCOUNT_MESSAGE, WPARAM(0), LPARAM(0)).0;
    let item_height = SendMessageW(hwnd, LB_GETITEMHEIGHT_MESSAGE, WPARAM(0), LPARAM(0)).0;
    let mut rows = Vec::new();
    let mut client = RECT::default();
    if top >= 0 && count > 0 && GetClientRect(hwnd, &mut client).is_ok() {
        let mode = list_box_selection_mode(hwnd);
        let current = if mode == ListBoxSelectionMode::Single {
            SendMessageW(hwnd, LB_GETCURSEL_MESSAGE, WPARAM(0), LPARAM(0)).0
        } else {
            -1
        };
        for index in top..count {
            let Some(rect) = list_box_item_rect(hwnd, index as usize) else {
                break;
            };
            if rect.top >= client.bottom {
                break;
            }
            rows.push(match mode {
                ListBoxSelectionMode::None => false,
                ListBoxSelectionMode::Single => current == index,
                ListBoxSelectionMode::Multiple => {
                    SendMessageW(hwnd, LB_GETSEL_MESSAGE, WPARAM(index as usize), LPARAM(0)).0 > 0
                }
            });
        }
    }
    ListBoxSnapshot {
        top,
        count,
        item_height,
        rows,
    }
}

/// Invalidates what differs between two snapshots. Returns true if the visible range moved.
unsafe fn invalidate_list_box_changes(
    hwnd: HWND,
    before: &ListBoxSnapshot,
    after: &ListBoxSnapshot,
) -> bool {
    if before.top != after.top
        || before.count != after.count
        || before.item_height != after.item_height
        || before.rows.len() != after.rows.len()
    {
        // Scrolled, refilled or re-measured: the whole client is republished as one composed paint
        // (no pixels are blitted around by USER32). The scrollbar is refreshed by USER32 itself
        // when redraw is switched back on; forcing a second non-client repaint here would draw
        // the scrollbar twice per wheel step.
        let _ = RedrawWindow(hwnd, None, None, RDW_INVALIDATE | RDW_NOERASE);
        return true;
    }
    for (offset, (was, is)) in before.rows.iter().zip(after.rows.iter()).enumerate() {
        if was != is {
            invalidate_list_box_row(hwnd, after.top.max(0) as usize + offset);
        }
    }
    false
}

/// Runs a message through the stock ListBox with its drawing suspended, then invalidates the rows
/// whose appearance changed. `immediate` publishes the result before returning (pointer and
/// keyboard feedback).
unsafe fn list_box_native_state_change(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    immediate: bool,
    palette: Palette,
) -> LRESULT {
    let state = list_box_state(hwnd);
    // A ListBox implements WM_SETREDRAW itself (it does not touch WS_VISIBLE). A hidden list cannot
    // draw, and a list the application has frozen, or that is already suspended by an outer
    // message, must keep that state.
    let suppress = !state.native_redraw_suppressed
        && !state.application_redraw_off
        && IsWindowVisible(hwnd).as_bool();
    let before = list_box_snapshot(hwnd);
    if suppress {
        update_list_box_state(hwnd, |state| state.native_redraw_suppressed = true);
        let _ = DefSubclassProc(hwnd, WM_SETREDRAW_MESSAGE, WPARAM(0), LPARAM(0));
    }
    let result = DefSubclassProc(hwnd, message, wparam, lparam);
    if !windows::Win32::UI::WindowsAndMessaging::IsWindow(hwnd).as_bool() {
        return result;
    }
    if suppress {
        let _ = DefSubclassProc(hwnd, WM_SETREDRAW_MESSAGE, WPARAM(1), LPARAM(0));
        update_list_box_state(hwnd, |state| state.native_redraw_suppressed = false);
    }
    if list_box_state(hwnd).application_redraw_off {
        // The application froze this list (WM_SETREDRAW FALSE) and repaints it when it re-enables
        // drawing; do not publish intermediate states in between.
        return result;
    }
    let after = list_box_snapshot(hwnd);
    if invalidate_list_box_changes(hwnd, &before, &after) {
        // Content moved under a resting pointer: the hot row follows the pointer.
        refresh_list_box_hot_row_from_cursor(hwnd);
    }
    if suppress {
        // Re-enabling redraw lets USER32 refresh its scrollbar directly, which covers the rounded
        // frame edge drawn over it. Restore that edge (no-op when there is no scrollbar).
        paint_list_box_nonclient_frame(hwnd, palette);
    }
    if immediate {
        let _ = windows::Win32::Graphics::Gdi::UpdateWindow(hwnd);
    }
    result
}

unsafe fn set_list_box_hot_row(hwnd: HWND, hot: Option<usize>) {
    let previous = list_box_state(hwnd).hot;
    if previous == hot {
        return;
    }
    update_list_box_state(hwnd, |state| state.hot = hot);
    if let Some(index) = previous {
        invalidate_list_box_row(hwnd, index);
    }
    if let Some(index) = hot {
        invalidate_list_box_row(hwnd, index);
    }
}

unsafe fn list_box_row_from_point(hwnd: HWND, packed_point: LPARAM) -> Option<usize> {
    let packed = SendMessageW(hwnd, LB_ITEMFROMPOINT_MESSAGE, WPARAM(0), packed_point).0 as u32;
    let outside = packed >> 16 != 0;
    let index = (packed & 0xffff) as isize;
    let count = SendMessageW(hwnd, LB_GETCOUNT_MESSAGE, WPARAM(0), LPARAM(0)).0;
    (!outside && index < count).then_some(index as usize)
}

unsafe fn update_list_box_hot_row(hwnd: HWND, packed_point: LPARAM) {
    let hot = list_box_row_from_point(hwnd, packed_point);
    if hot.is_some() && !list_box_state(hwnd).leave_tracked {
        let mut tracking = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: hwnd,
            dwHoverTime: 0,
        };
        if TrackMouseEvent(&mut tracking).is_ok() {
            update_list_box_state(hwnd, |state| state.leave_tracked = true);
        }
    }
    set_list_box_hot_row(hwnd, hot);
}

unsafe fn refresh_list_box_hot_row_from_cursor(hwnd: HWND) {
    if list_box_state(hwnd).hot.is_none() {
        return;
    }
    let mut point = POINT::default();
    if GetCursorPos(&mut point).is_err()
        || windows::Win32::UI::WindowsAndMessaging::WindowFromPoint(point) != hwnd
        || !ScreenToClient(hwnd, &mut point).as_bool()
    {
        set_list_box_hot_row(hwnd, None);
        return;
    }
    let packed = ((point.x as u16 as u32) | ((point.y as u16 as u32) << 16)) as isize;
    set_list_box_hot_row(hwnd, list_box_row_from_point(hwnd, LPARAM(packed)));
}

/// Client origin relative to the window rectangle, and the window size.
unsafe fn list_box_window_geometry(hwnd: HWND) -> Option<(i32, i32, i32, i32)> {
    let mut window = RECT::default();
    GetWindowRect(hwnd, &mut window).ok()?;
    let mut origin = POINT { x: 0, y: 0 };
    if !ClientToScreen(hwnd, &mut origin).as_bool() {
        return None;
    }
    Some((
        origin.x - window.left,
        origin.y - window.top,
        (window.right - window.left).max(0),
        (window.bottom - window.top).max(0),
    ))
}

unsafe extern "system" fn list_box_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    let palette = palette_from_reference(reference_data);
    match message {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint_list_box(hwnd, palette);
            LRESULT(0)
        }
        WM_PRINTCLIENT_MESSAGE => {
            let dc = HDC(wparam.0 as *mut _);
            let mut client = RECT::default();
            if !dc.is_invalid() && GetClientRect(hwnd, &mut client).is_ok() {
                paint_list_box_surface(hwnd, dc, client, client, palette);
            }
            LRESULT(0)
        }
        WM_NCCALCSIZE_MESSAGE => band_nccalcsize(hwnd, wparam, lparam),
        WM_NCPAINT => {
            let result = paint_native_scrollbars_only(hwnd, wparam, lparam);
            if !paint_frame_band(hwnd, palette) {
                paint_list_box_nonclient_frame(hwnd, palette);
            }
            result
        }
        WM_SETREDRAW_MESSAGE => {
            let enabling = wparam.0 != 0;
            let was_off = list_box_state(hwnd).application_redraw_off;
            update_list_box_state(hwnd, |state| state.application_redraw_off = !enabling);
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if enabling && was_off {
                // A ListBox does not reliably repaint itself when redraw is switched back on.
                let _ = RedrawWindow(hwnd, None, None, RDW_INVALIDATE | RDW_FRAME | RDW_NOERASE);
            }
            result
        }
        WM_MOUSEMOVE => {
            let result = if windows::Win32::UI::Input::KeyboardAndMouse::GetCapture() == hwnd {
                // Button held: USER32 moves the selection with the pointer.
                list_box_native_state_change(hwnd, message, wparam, lparam, true, palette)
            } else {
                DefSubclassProc(hwnd, message, wparam, lparam)
            };
            update_list_box_hot_row(hwnd, lparam);
            result
        }
        WM_MOUSELEAVE_MESSAGE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            update_list_box_state(hwnd, |state| state.leave_tracked = false);
            set_list_box_hot_row(hwnd, None);
            result
        }
        WM_LBUTTONDOWN
        | WM_LBUTTONDBLCLK_MESSAGE
        | WM_LBUTTONUP
        | WM_KEYDOWN
        | WM_CHAR_MESSAGE
        | WM_VSCROLL_MESSAGE
        | WM_HSCROLL_MESSAGE
        | WM_MOUSEWHEEL_MESSAGE => {
            list_box_native_state_change(hwnd, message, wparam, lparam, true, palette)
        }
        WM_TIMER_MESSAGE if windows::Win32::UI::Input::KeyboardAndMouse::GetCapture() == hwnd => {
            // Automatic scrolling while a selection is dragged outside the list.
            list_box_native_state_change(hwnd, message, wparam, lparam, true, palette)
        }
        WM_SETFOCUS
        | WM_KILLFOCUS
        | WM_CAPTURECHANGED
        | WM_CANCELMODE
        | LB_SETCURSEL_MESSAGE
        | LB_SETSEL_MESSAGE
        | LB_SETCARETINDEX_MESSAGE
        | LB_SELECTSTRING_MESSAGE
        | LB_SELITEMRANGE_MESSAGE
        | LB_SELITEMRANGEEX_MESSAGE
        | LB_SETTOPINDEX_MESSAGE
        | LB_SETANCHORINDEX_MESSAGE => {
            list_box_native_state_change(hwnd, message, wparam, lparam, false, palette)
        }
        WM_ENABLE => {
            let result =
                list_box_native_state_change(hwnd, message, wparam, lparam, false, palette);
            if wparam.0 == 0 {
                update_list_box_state(hwnd, |state| state.hot = None);
            }
            let _ = RedrawWindow(hwnd, None, None, RDW_INVALIDATE | RDW_FRAME | RDW_NOERASE);
            result
        }
        WM_UPDATEUISTATE_MESSAGE => {
            // Anyone may hide focus cues; nobody may show the XOR focus rectangle again.
            let action = wparam.0 & 0xffff;
            let mut flags = (wparam.0 >> 16) & 0xffff;
            if action != UIS_SET_ACTION {
                flags &= !UISF_HIDEFOCUS_FLAG;
            }
            list_box_native_state_change(
                hwnd,
                message,
                WPARAM(action | (flags << 16)),
                lparam,
                false,
                palette,
            )
        }
        WM_SIZE | WM_THEMECHANGED | LB_SETITEMHEIGHT_MESSAGE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if message != LB_SETITEMHEIGHT_MESSAGE {
                clear_legacy_control_region(hwnd);
            }
            let _ = RedrawWindow(hwnd, None, None, RDW_INVALIDATE | RDW_FRAME | RDW_NOERASE);
            result
        }
        LB_RESETCONTENT_MESSAGE | LB_DELETESTRING_MESSAGE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            let count = SendMessageW(hwnd, LB_GETCOUNT_MESSAGE, WPARAM(0), LPARAM(0)).0;
            if list_box_state(hwnd)
                .hot
                .is_some_and(|hot| hot as isize >= count.max(0))
            {
                update_list_box_state(hwnd, |state| state.hot = None);
            }
            result
        }
        message
            if native_scrollbar_may_repaint_frame(message)
                || message == WM_NCMOUSEMOVE_MESSAGE
                || message == WM_NCMOUSELEAVE_MESSAGE =>
        {
            // USER32/UxTheme may repaint the non-client scrollbar directly (hover, press); the
            // frame edge over it is restored afterwards. The client is not touched.
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            paint_list_box_nonclient_frame(hwnd, palette);
            result
        }
        WM_NCDESTROY => {
            forget_list_box_state(hwnd);
            let _ = RemovePropW(hwnd, LIST_BOX_HOT_PROPERTY);
            let _ = RemoveWindowSubclass(hwnd, Some(list_box_subclass), LIST_BOX_SUBCLASS_ID);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

unsafe fn paint_list_box(hwnd: HWND, palette: Palette) {
    let trace_start = super::redraw::trace_now();
    let mut paint = PAINTSTRUCT::default();
    let target = BeginPaint(hwnd, &mut paint);
    let mut client = RECT::default();
    if !target.is_invalid() && GetClientRect(hwnd, &mut client).is_ok() {
        let area = intersect_rects(client, paint.rcPaint);
        if area.right > area.left && area.bottom > area.top {
            let buffer = super::redraw::PaintBuffer::begin_opaque(target, client, area);
            paint_list_box_surface(hwnd, buffer.dc(), client, area, palette);
            buffer.present();
        }
    }
    let _ = EndPaint(hwnd, &paint);
    super::redraw::trace_list_view_paint(trace_start);
}

/// Paints background, the rows that intersect `area`, and the frame edges that lie inside the
/// client. `area` is in client coordinates; the DC may be an off-screen surface.
unsafe fn paint_list_box_surface(hwnd: HWND, dc: HDC, client: RECT, area: RECT, palette: Palette) {
    fill(dc, &area, palette.edit);
    let count = SendMessageW(hwnd, LB_GETCOUNT_MESSAGE, WPARAM(0), LPARAM(0)).0;
    let dpi = GetDpiForWindow(hwnd).max(96);
    if count > 0 {
        let mode = list_box_selection_mode(hwnd);
        let current = if mode == ListBoxSelectionMode::Single {
            SendMessageW(hwnd, LB_GETCURSEL_MESSAGE, WPARAM(0), LPARAM(0)).0
        } else {
            -1
        };
        let hot = list_box_state(hwnd).hot;
        let top = SendMessageW(hwnd, LB_GETTOPINDEX_MESSAGE, WPARAM(0), LPARAM(0))
            .0
            .max(0);
        let font = SendMessageW(hwnd, WM_GETFONT, WPARAM(0), LPARAM(0));
        let old_font = (font.0 != 0)
            .then(|| SelectObject(dc, windows::Win32::Graphics::Gdi::HGDIOBJ(font.0 as *mut _)));
        let _ = SetBkMode(dc, TRANSPARENT);
        let inset = scale(7, dpi);
        let mut text: Vec<u16> = Vec::with_capacity(128);
        for index in top..count {
            let Some(row) = list_box_item_rect(hwnd, index as usize) else {
                break;
            };
            if row.top >= client.bottom || row.top >= area.bottom {
                break;
            }
            if row.bottom <= area.top {
                continue;
            }
            let is_selected = match mode {
                ListBoxSelectionMode::None => false,
                ListBoxSelectionMode::Single => current == index,
                ListBoxSelectionMode::Multiple => {
                    SendMessageW(hwnd, LB_GETSEL_MESSAGE, WPARAM(index as usize), LPARAM(0)).0 > 0
                }
            };
            let is_hot = hot == Some(index as usize);
            let (text_color, background) = if is_selected || is_hot {
                navigation_selection_colors(palette, is_hot)
            } else {
                (palette.text, palette.edit)
            };
            if background != palette.edit {
                fill(dc, &row, background);
            }
            let length = SendMessageW(
                hwnd,
                LB_GETTEXTLEN_MESSAGE,
                WPARAM(index as usize),
                LPARAM(0),
            )
            .0;
            if length <= 0 {
                continue;
            }
            text.clear();
            text.resize(length as usize + 1, 0);
            let copied = SendMessageW(
                hwnd,
                LB_GETTEXT_MESSAGE,
                WPARAM(index as usize),
                LPARAM(text.as_mut_ptr() as isize),
            )
            .0;
            if copied <= 0 {
                continue;
            }
            text.truncate((copied as usize).min(length as usize));
            let _ = SetTextColor(dc, text_color);
            let mut text_rect = row;
            text_rect.left += inset;
            text_rect.right -= inset;
            draw_native_text(
                dc,
                &text,
                &mut text_rect,
                DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
                text_color,
            );
        }
        if let Some(old_font) = old_font {
            let _ = SelectObject(dc, old_font);
        }
    }
    // With the frame band reserved the outline is entirely non-client. Otherwise (a list too small
    // for the band) the part of the frame inside the client is composed here, in the same surface
    // as the rows, so it is published in the same BitBlt.
    if reserved_frame_band(hwnd).is_some() {
        return;
    }
    if let Some((client_x, client_y, width, height)) = list_box_window_geometry(hwnd) {
        if let Some(geometry) = rounded_control_frame_geometry(width, height, dpi) {
            let _ =
                windows::Win32::Graphics::Gdi::OffsetViewportOrgEx(dc, -client_x, -client_y, None);
            draw_antialiased_control_frame(
                dc,
                RECT {
                    left: 0,
                    top: 0,
                    right: width,
                    bottom: height,
                },
                geometry,
                palette.edit,
                palette.control_border(),
                rounded_control_exterior(palette),
            );
            let _ =
                windows::Win32::Graphics::Gdi::OffsetViewportOrgEx(dc, client_x, client_y, None);
        }
    }
}

/// Paints the frame edge that lies over the non-client scrollbar, never touching the client
/// (which WM_PAINT owns). Without a scrollbar there is no non-client area and nothing to do.
unsafe fn paint_list_box_nonclient_frame(hwnd: HWND, palette: Palette) {
    // A reserved band is painted only by WM_NCPAINT; the scrollbar never reaches it.
    if reserved_frame_band(hwnd).is_some() {
        return;
    }
    let Some((client_x, client_y, width, height)) = list_box_window_geometry(hwnd) else {
        return;
    };
    let mut client = RECT::default();
    if GetClientRect(hwnd, &mut client).is_err() {
        return;
    }
    if client_x == 0 && client_y == 0 && client.right >= width && client.bottom >= height {
        return;
    }
    let Some(geometry) =
        rounded_control_frame_geometry(width, height, GetDpiForWindow(hwnd).max(96))
    else {
        return;
    };
    let dc = GetWindowDC(hwnd);
    if dc.is_invalid() {
        return;
    }
    let _ = windows::Win32::Graphics::Gdi::ExcludeClipRect(
        dc,
        client_x,
        client_y,
        client_x + client.right,
        client_y + client.bottom,
    );
    draw_antialiased_control_frame(
        dc,
        RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        },
        geometry,
        palette.edit,
        palette.control_border(),
        rounded_control_exterior(palette),
    );
    let _ = ReleaseDC(hwnd, dc);
}

unsafe extern "system" fn list_view_parent_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    match message {
        WM_NOTIFY if lparam.0 != 0 => {
            let draw = &mut *(lparam.0 as *mut NMLVCUSTOMDRAW);
            let dark_flag = 1usize << (usize::BITS - 1);
            let list = HWND((reference_data & !dark_flag) as *mut _);
            if draw.nmcd.hdr.hwndFrom == list && draw.nmcd.hdr.code == NM_CUSTOMDRAW {
                let palette = palette_from_reference(
                    usize::from(reference_data & dark_flag != 0) * PALETTE_REFERENCE_DARK,
                );
                if draw.nmcd.dwDrawStage == CDDS_PREPAINT {
                    return LRESULT(CDRF_NOTIFYITEMDRAW as isize);
                }
                if draw.nmcd.dwDrawStage == CDDS_ITEMPREPAINT {
                    // Always remove the native selected bit from this transient paint snapshot.
                    // Clearing it only for the currently selected row allows a stale focused row
                    // to be overpainted with the system-blue selection after the real selection
                    // has already moved elsewhere.
                    draw.nmcd.uItemState.0 = list_view_custom_draw_state(draw.nmcd.uItemState.0);
                    // `uItemState` is a custom-draw state snapshot, not the authoritative
                    // ListView selection state. Depending on comctl32 version and focus changes it
                    // can retain CDIS_SELECTED for rows that are no longer selected. Query the
                    // row itself so only the actual LVIS_SELECTED item receives the highlight.
                    const LVM_GETITEMSTATE: u32 = 0x102c;
                    const LVIS_SELECTED: isize = 0x0002;
                    let item_state = SendMessageW(
                        list,
                        LVM_GETITEMSTATE,
                        WPARAM(draw.nmcd.dwItemSpec),
                        LPARAM(LVIS_SELECTED),
                    )
                    .0;
                    let selected = item_state & LVIS_SELECTED != 0;
                    if selected && paint_list_view_row(list, draw, palette, true) {
                        // Windows 11's v6 ItemsView theme paints COLOR_HIGHLIGHT over clrTextBk
                        // after NM_CUSTOMDRAW.  Skip only that one selected row after reproducing
                        // its report-mode text layout; every unselected row remains native.
                        return LRESULT((CDRF_SKIPDEFAULT | CDRF_SKIPPOSTPAINT) as isize);
                    }
                    return LRESULT(CDRF_DODEFAULT as isize);
                }
                return LRESULT(CDRF_DODEFAULT as isize);
            }
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(list_view_parent_subclass), subclass_id);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

unsafe fn paint_list_view_row(
    list: HWND,
    draw: &mut NMLVCUSTOMDRAW,
    palette: Palette,
    selected: bool,
) -> bool {
    const LVM_GETHEADER: u32 = 0x101f;
    const LVM_GETITEMRECT: u32 = 0x100e;
    const LVM_GETITEMSTATE: u32 = 0x102c;
    const LVM_GETSUBITEMRECT: u32 = 0x1038;
    const LVM_GETITEMTEXTW: u32 = 0x1073;
    const HDM_GETITEMCOUNT: u32 = 0x1200;
    const LVIR_BOUNDS: i32 = 0;
    const LVIS_STATEIMAGEMASK: isize = 0xf000;

    let item_index = draw.nmcd.dwItemSpec;
    let mut row = RECT {
        left: LVIR_BOUNDS,
        ..Default::default()
    };
    if SendMessageW(
        list,
        LVM_GETITEMRECT,
        WPARAM(item_index),
        LPARAM((&mut row as *mut RECT) as isize),
    )
    .0 == 0
    {
        return false;
    }

    let mut client = RECT::default();
    if GetClientRect(list, &mut client).is_err() {
        return false;
    }
    let raw_row = row;
    row.left = row.left.max(client.left);
    row.top = row.top.max(client.top);
    row.right = row.right.min(client.right);
    row.bottom = row.bottom.min(client.bottom);
    if row.right <= row.left || row.bottom <= row.top {
        return false;
    }

    let (text_color, selection_fill) = list_view_row_colors(palette, selected);
    let dpi = GetDpiForWindow(list).max(96);
    let style = GetWindowLongPtrW(list, GWL_STYLE);
    let category_selection = selected
        .then(|| category_selection_corners(style, raw_row, client))
        .flatten()
        .map(|corners| {
            let geometry = category_selection_geometry(row, corners, dpi);
            paint_category_selection_region(
                draw.nmcd.hdc,
                geometry,
                row,
                selection_fill,
                palette.edit,
            );
            geometry
        });
    if category_selection.is_none() {
        fill(draw.nmcd.hdc, &row, selection_fill);
    }

    let font = SendMessageW(list, WM_GETFONT, WPARAM(0), LPARAM(0));
    let old_font = (font.0 != 0).then(|| {
        SelectObject(
            draw.nmcd.hdc,
            windows::Win32::Graphics::Gdi::HGDIOBJ(font.0 as *mut _),
        )
    });
    let _ = SetBkMode(draw.nmcd.hdc, TRANSPARENT);
    let _ = SetTextColor(draw.nmcd.hdc, text_color);

    let header = HWND(SendMessageW(list, LVM_GETHEADER, WPARAM(0), LPARAM(0)).0 as *mut _);
    let column_count = if header.0.is_null() {
        1
    } else {
        SendMessageW(header, HDM_GETITEMCOUNT, WPARAM(0), LPARAM(0))
            .0
            .max(1) as i32
    };
    let inset = scale(7, dpi);
    let state_image = SendMessageW(
        list,
        LVM_GETITEMSTATE,
        WPARAM(item_index),
        LPARAM(LVIS_STATEIMAGEMASK),
    )
    .0 as u32;

    for subitem in 0..column_count {
        let mut text_rect = RECT {
            left: LVIR_BOUNDS,
            top: subitem,
            ..Default::default()
        };
        if SendMessageW(
            list,
            LVM_GETSUBITEMRECT,
            WPARAM(item_index),
            LPARAM((&mut text_rect as *mut RECT) as isize),
        )
        .0 == 0
        {
            continue;
        }
        // The text keeps its column's own extent. Clipping it to the visible client area put the
        // ellipsis at the window edge; a horizontal scroll then moved that cut text (and its
        // "...") into the middle of the row, and scrolling back left fragments behind.
        if subitem == 0 {
            // For the first column LVM_GETSUBITEMRECT returns the whole item: use the column.
            let width = SendMessageW(list, 0x101D, WPARAM(0), LPARAM(0)).0 as i32; // LVM_GETCOLUMNWIDTH
            if width > 0 {
                text_rect.right = text_rect.left + width;
            }
        }
        if text_rect.right <= text_rect.left
            || text_rect.right <= client.left
            || text_rect.left >= client.right
        {
            continue;
        }

        let mut text = vec![0u16; 1024];
        let mut item = LVITEMW {
            mask: LVIF_TEXT,
            iSubItem: subitem,
            pszText: PWSTR(text.as_mut_ptr()),
            cchTextMax: text.len() as i32,
            ..Default::default()
        };
        let copied = SendMessageW(
            list,
            LVM_GETITEMTEXTW,
            WPARAM(item_index),
            LPARAM((&mut item as *mut LVITEMW) as isize),
        )
        .0
        .max(0) as usize;
        text.truncate(copied.min(text.len()));

        text_rect.left += inset;
        if subitem == 0 && state_image & LVIS_STATEIMAGEMASK as u32 != 0 {
            // Selected rows start their text where comctl32 starts it on the other rows: at the
            // label, right after the state-image slot our checkbox is painted into.
            if let Some(label) = list_view_label_left(list, item_index) {
                text_rect.left = text_rect.left.max(label + scale(2, dpi));
            } else {
                text_rect.left += scale(24, dpi);
            }
        }
        text_rect.right -= inset.min((text_rect.right - text_rect.left).max(0));
        draw_opaque_surface_text(
            draw.nmcd.hdc,
            &text,
            &mut text_rect,
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
            text_color,
            selection_fill,
        );
    }

    if let Some(old_font) = old_font {
        let _ = SelectObject(draw.nmcd.hdc, old_font);
    }
    if let Some(geometry) = category_selection {
        // Opaque ClearType text renders on a solid selected surface. Restore only the non-text
        // edge bands afterwards; the left band ends exactly where the first glyph begins.
        restore_category_selection_edges(draw.nmcd.hdc, geometry, selection_fill, palette.edit);
    }
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CategorySelectionCorners {
    All,
    Top,
    Square,
    Bottom,
}

#[derive(Clone, Copy, Debug)]
struct CategorySelectionGeometry {
    row: RECT,
    fill: RECT,
    corners: CategorySelectionCorners,
    radius: i32,
}

fn category_selection_geometry(
    row: RECT,
    corners: CategorySelectionCorners,
    dpi: u32,
) -> CategorySelectionGeometry {
    let gap = scale(2, dpi).max(1);
    // The selection is inset from the same rounded control frame, so its arc must be the
    // concentric inner arc: outer frame radius minus the physical inset. Reusing the frame
    // geometry keeps the two curves parallel at every DPI instead of merely choosing a similar
    // looking independent radius.
    let outer_radius = rounded_control_frame_geometry(
        (row.right - row.left).max(0),
        (row.bottom - row.top).max(scale(23, dpi)),
        dpi,
    )
    .map_or(scale(5, dpi).max(2), |geometry| geometry.radius);
    let radius = outer_radius
        .saturating_sub(gap)
        .max(1)
        .min((row.bottom - row.top).max(0) / 2);
    let mut selected = RECT {
        left: (row.left + gap).min(row.right),
        top: row.top,
        right: (row.right - gap).max(row.left),
        bottom: row.bottom,
    };
    if matches!(
        corners,
        CategorySelectionCorners::Top | CategorySelectionCorners::All
    ) {
        selected.top = (selected.top + gap).min(selected.bottom);
    }
    if matches!(
        corners,
        CategorySelectionCorners::Bottom | CategorySelectionCorners::All
    ) {
        selected.bottom = (selected.bottom - gap).max(selected.top);
    }
    CategorySelectionGeometry {
        row,
        fill: selected,
        corners,
        radius,
    }
}

unsafe fn restore_category_selection_edges(
    dc: HDC,
    geometry: CategorySelectionGeometry,
    selected: COLORREF,
    background: COLORREF,
) {
    let left_edge = RECT {
        right: (geometry.fill.left + geometry.radius).min(geometry.row.right),
        ..geometry.row
    };
    let right_edge = RECT {
        left: (geometry.fill.right - geometry.radius).max(geometry.row.left),
        ..geometry.row
    };
    paint_category_selection_region(dc, geometry, left_edge, selected, background);
    paint_category_selection_region(dc, geometry, right_edge, selected, background);

    if geometry.fill.top > geometry.row.top {
        paint_category_selection_region(
            dc,
            geometry,
            RECT {
                bottom: geometry.fill.top,
                ..geometry.row
            },
            selected,
            background,
        );
    }
    if geometry.fill.bottom < geometry.row.bottom {
        paint_category_selection_region(
            dc,
            geometry,
            RECT {
                top: geometry.fill.bottom,
                ..geometry.row
            },
            selected,
            background,
        );
    }
}

unsafe fn paint_category_selection_region(
    dc: HDC,
    geometry: CategorySelectionGeometry,
    region: RECT,
    selected: COLORREF,
    background: COLORREF,
) {
    let region = RECT {
        left: region.left.max(geometry.row.left),
        top: region.top.max(geometry.row.top),
        right: region.right.min(geometry.row.right),
        bottom: region.bottom.min(geometry.row.bottom),
    };
    let width = (region.right - region.left).max(0);
    let height = (region.bottom - region.top).max(0);
    if width == 0 || height == 0 {
        return;
    }

    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    for y in 0..height {
        for x in 0..width {
            let coverage =
                category_selection_pixel_coverage(region.left + x, region.top + y, geometry);
            let color = blend_category_selection(background, selected, coverage, 64);
            let offset = (y as usize * width as usize + x as usize) * 4;
            pixels[offset] = ((color.0 >> 16) & 0xff) as u8;
            pixels[offset + 1] = ((color.0 >> 8) & 0xff) as u8;
            pixels[offset + 2] = (color.0 & 0xff) as u8;
            pixels[offset + 3] = 255;
        }
    }
    let bitmap_info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let _ = SetDIBitsToDevice(
        dc,
        region.left,
        region.top,
        width as u32,
        height as u32,
        0,
        0,
        0,
        height as u32,
        pixels.as_ptr().cast(),
        &bitmap_info,
        DIB_RGB_COLORS,
    );
}

fn category_selection_pixel_coverage(
    pixel_x: i32,
    pixel_y: i32,
    geometry: CategorySelectionGeometry,
) -> u32 {
    const SAMPLES: i32 = 8;
    let mut inside = 0u32;
    for sample_y in 0..SAMPLES {
        for sample_x in 0..SAMPLES {
            let x = pixel_x as f64 + (sample_x as f64 + 0.5) / SAMPLES as f64;
            let y = pixel_y as f64 + (sample_y as f64 + 0.5) / SAMPLES as f64;
            if category_selection_contains_sample(x, y, geometry) {
                inside += 1;
            }
        }
    }
    inside
}

fn category_selection_contains_sample(x: f64, y: f64, geometry: CategorySelectionGeometry) -> bool {
    let fill = geometry.fill;
    if x < fill.left as f64
        || x >= fill.right as f64
        || y < fill.top as f64
        || y >= fill.bottom as f64
    {
        return false;
    }
    let radius = geometry
        .radius
        .min((fill.right - fill.left).max(0) / 2)
        .min((fill.bottom - fill.top).max(0) / 2) as f64;
    if radius <= 0.0 {
        return true;
    }
    let rounds_top = matches!(
        geometry.corners,
        CategorySelectionCorners::Top | CategorySelectionCorners::All
    );
    let rounds_bottom = matches!(
        geometry.corners,
        CategorySelectionCorners::Bottom | CategorySelectionCorners::All
    );
    let left_center = fill.left as f64 + radius;
    let right_center = fill.right as f64 - radius;
    let top_center = fill.top as f64 + radius;
    let bottom_center = fill.bottom as f64 - radius;

    if rounds_top && y < top_center {
        if x < left_center {
            return (x - left_center).powi(2) + (y - top_center).powi(2) <= radius.powi(2);
        }
        if x >= right_center {
            return (x - right_center).powi(2) + (y - top_center).powi(2) <= radius.powi(2);
        }
    }
    if rounds_bottom && y >= bottom_center {
        if x < left_center {
            return (x - left_center).powi(2) + (y - bottom_center).powi(2) <= radius.powi(2);
        }
        if x >= right_center {
            return (x - right_center).powi(2) + (y - bottom_center).powi(2) <= radius.powi(2);
        }
    }
    true
}

fn blend_category_selection(
    background: COLORREF,
    foreground: COLORREF,
    foreground_weight: u32,
    total_weight: u32,
) -> COLORREF {
    let blend_channel = |shift: u32| {
        let background = (background.0 >> shift) & 0xff;
        let foreground = (foreground.0 >> shift) & 0xff;
        ((background * (total_weight - foreground_weight)
            + foreground * foreground_weight
            + total_weight / 2)
            / total_weight) as u8
    };
    rgb(blend_channel(0), blend_channel(8), blend_channel(16))
}

fn category_selection_corners(
    style: isize,
    row: RECT,
    client: RECT,
) -> Option<CategorySelectionCorners> {
    if !is_category_list_view(style)
        || row.right <= client.left
        || row.left >= client.right
        || row.bottom <= client.top
        || row.top >= client.bottom
    {
        return None;
    }

    // GetClientRect uses an exclusive bottom-right coordinate.  A category highlight receives a
    // rounded side only when its visible bounds actually reach that client edge.  In particular,
    // the final data item is not a bottom-edge item when blank ListView body remains below it.
    let touches_top = row.top <= client.top;
    let touches_bottom = row.bottom >= client.bottom;
    Some(match (touches_top, touches_bottom) {
        (true, true) => CategorySelectionCorners::All,
        (true, false) => CategorySelectionCorners::Top,
        (false, true) => CategorySelectionCorners::Bottom,
        (false, false) => CategorySelectionCorners::Square,
    })
}

const fn is_category_list_view(style: isize) -> bool {
    // Documented ListView styles: LVS_SINGLESEL=0x0004 and LVS_NOCOLUMNHEADER=0x4000.
    const LVS_SINGLESEL_STYLE: isize = 0x0004;
    const LVS_NOCOLUMNHEADER_STYLE: isize = 0x4000;
    let category_styles = LVS_SINGLESEL_STYLE | LVS_NOCOLUMNHEADER_STYLE;
    style & category_styles == category_styles
}

fn list_view_row_colors(palette: Palette, selected: bool) -> (COLORREF, COLORREF) {
    if selected {
        // A selected report row must stay identical to the resting selected navigation button.
        // Pointer hover must not silently switch it to the brighter hot-button colour.
        navigation_selection_colors(palette, false)
    } else {
        (palette.text, palette.edit)
    }
}

/// Text and fill of a highlighted list row, shared with the drop-down list (combo_popup).
pub(crate) fn list_selection_colors(palette: Palette, hot: bool) -> (COLORREF, COLORREF) {
    navigation_selection_colors(palette, hot)
}

/// Reuses the exact normal/hot palette of the selected left navigation entry. Keeping this as the
/// single source of truth prevents report rows and standalone lists drifting back to system blue.
fn navigation_selection_colors(palette: Palette, hot: bool) -> (COLORREF, COLORREF) {
    let visual = button_visual(
        palette,
        ButtonRole::Navigation { selected: true },
        ControlState {
            hot,
            ..ControlState::default()
        },
    );
    (visual.text, visual.fill)
}

/// Clears native selection, hot and focus paint bits from the transient custom-draw snapshot. The
/// authoritative ListView item state is queried separately and remains unchanged; suppressing the
/// snapshot bits prevents the system light theme from painting a white/focus overlay after the
/// application supplied the selected navigation colour.
const fn list_view_custom_draw_state(snapshot: u32) -> u32 {
    const CDIS_SELECTED: u32 = 0x0001;
    const CDIS_FOCUS: u32 = 0x0010;
    const CDIS_HOT: u32 = 0x0040;
    snapshot & !(CDIS_SELECTED | CDIS_FOCUS | CDIS_HOT)
}

/// Left edge of an item's label (LVIR_LABEL): where comctl32 starts drawing the item text.
unsafe fn list_view_label_left(list: HWND, index: usize) -> Option<i32> {
    let mut label = RECT {
        left: 2, // LVIR_LABEL
        ..Default::default()
    };
    (SendMessageW(
        list,
        0x100E, // LVM_GETITEMRECT
        WPARAM(index),
        LPARAM((&mut label as *mut RECT) as isize),
    )
    .0 != 0)
        .then_some(label.left)
}

/// The checkbox centred in the state-image slot, never wider than the slot leaves room for.
fn list_view_checkbox_rect_in(slot: RECT, dpi: u32) -> RECT {
    let slot_width = (slot.right - slot.left).max(1);
    let row_height = (slot.bottom - slot.top).max(1);
    let size = scale(13, dpi)
        .max(1)
        .min((slot_width - scale(4, dpi)).max(scale(9, dpi)))
        .min(row_height);
    let left = slot.left + (slot_width - size) / 2;
    let top = slot.top + (row_height - size) / 2;
    RECT {
        left,
        top,
        right: left + size,
        bottom: top + size,
    }
}

#[allow(dead_code)]
fn list_view_checkbox_rect(row: RECT, dpi: u32) -> RECT {
    let slot_width = scale(24, dpi).max(1);
    let row_height = (row.bottom - row.top).max(1);
    let size = scale(13, dpi).max(1).min(slot_width).min(row_height);
    let left = row.left + (slot_width - size) / 2;
    let top = row.top + (row_height - size) / 2;
    RECT {
        left,
        top,
        right: left + size,
        bottom: top + size,
    }
}

unsafe fn paint_list_view_checkboxes(hwnd: HWND, dc: HDC, palette: Palette) {
    const LVIS_STATEIMAGEMASK: isize = 0xf000;
    let top = SendMessageW(hwnd, 0x1027, WPARAM(0), LPARAM(0)).0.max(0) as i32; // TOPINDEX
    let visible = SendMessageW(hwnd, 0x1028, WPARAM(0), LPARAM(0)).0.max(0) as i32; // PERPAGE
    let count = SendMessageW(hwnd, 0x1004, WPARAM(0), LPARAM(0)).0.max(0) as i32; // ITEMCOUNT
    if count == 0 {
        return;
    }
    let dpi = GetDpiForWindow(hwnd).max(96);
    let control_state = ControlState {
        disabled: !IsWindowEnabled(hwnd).as_bool(),
        ..ControlState::default()
    };
    for index in top..(top + visible + 1).min(count) {
        let state = SendMessageW(
            hwnd,
            0x102C, // LVM_GETITEMSTATE
            WPARAM(index as usize),
            LPARAM(LVIS_STATEIMAGEMASK),
        )
        .0 as u32;
        let state_image = (state >> 12) & 0xf;
        if state_image == 0 {
            continue;
        }
        let mut row = RECT {
            left: 0, // LVIR_BOUNDS
            ..Default::default()
        };
        if SendMessageW(
            hwnd,
            0x100E, // LVM_GETITEMRECT
            WPARAM(index as usize),
            LPARAM((&mut row as *mut RECT) as isize),
        )
        .0 == 0
        {
            continue;
        }
        // The state image precedes LVIR_ICON. Painting inside LVIR_ICON produced the visible
        // double-checkbox regression (native white state image plus our dark box). Clear and
        // replace the actual leading state-image slot instead.
        let selected = SendMessageW(
            hwnd,
            0x102C, // LVM_GETITEMSTATE
            WPARAM(index as usize),
            LPARAM(0x0002), // LVIS_SELECTED
        )
        .0 != 0;
        // The slot ends where comctl32 starts the label text. A fixed 24 px slot was wider than
        // the native state image at several DPIs and covered the first letters of the text.
        let slot_right = list_view_label_left(hwnd, index as usize)
            .filter(|label| *label > row.left + scale(6, dpi))
            .unwrap_or(row.left + scale(24, dpi));
        let slot = RECT {
            left: row.left,
            top: row.top,
            right: slot_right,
            bottom: row.bottom,
        };
        let background = list_view_row_colors(palette, selected).1;
        fill(dc, &slot, background);
        let box_rect = list_view_checkbox_rect_in(slot, dpi);
        let checked = state_image == 2;
        draw_embedded_button_glyph(
            dc,
            box_rect,
            embedded_button_glyph(palette.dark, dpi, control_state, checked),
            background,
        );
    }
}

unsafe extern "system" fn progress_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    match message {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint_progress(hwnd, palette_from_reference(reference_data));
            LRESULT(0)
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(progress_subclass), PROGRESS_SUBCLASS_ID);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if (0x0401..=0x0410).contains(&message) {
                let _ = InvalidateRect(hwnd, None, false);
            }
            result
        }
    }
}

/// Native progress bars (partition copy and any other tool) look exactly like the install page:
/// the same 10 px capsule and colours, followed by the percentage in a 48 px slot after an 8 px
/// gap, all inside the control. Error and paused colours come from PBM_GETSTATE. Composed
/// off-screen and published with one BitBlt.
unsafe fn paint_progress(hwnd: HWND, palette: Palette) {
    let mut paint = PAINTSTRUCT::default();
    let target = BeginPaint(hwnd, &mut paint);
    let mut rect = RECT::default();
    let _ = GetClientRect(hwnd, &mut rect);
    let maximum = SendMessageW(hwnd, 0x0407, WPARAM(0), LPARAM(0)).0.max(1) as u64;
    let position = (SendMessageW(hwnd, 0x0408, WPARAM(0), LPARAM(0)).0.max(0) as u64).min(maximum);
    let role = match SendMessageW(hwnd, 0x0411, WPARAM(0), LPARAM(0)).0 {
        2 => ProgressRole::Error,
        3 => ProgressRole::Paused,
        _ => ProgressRole::Normal,
    };
    let dpi = GetDpiForWindow(hwnd).max(96);
    let width = (rect.right - rect.left).max(0);
    let height = (rect.bottom - rect.top).max(0);
    let percent_width = scale(48, dpi).min(width / 3);
    let gap = scale(8, dpi).min(width / 10);
    let bar_height = scale(10, dpi).min(height).max(1);
    let bar_top = rect.top + (height - bar_height) / 2;
    let bar = RECT {
        left: rect.left,
        top: bar_top,
        right: (rect.right - percent_width - gap).max(rect.left),
        bottom: bar_top + bar_height,
    };
    let percent = position.saturating_mul(100) / maximum;
    super::redraw::ui_detail_changed(hwnd, "progress", || {
        format!("client={rect:?} bar={bar:?} value={position}/{maximum} role={role:?}")
    });
    {
        let buffer = super::redraw::PaintBuffer::begin_opaque(target, rect, paint.rcPaint);
        let dc = buffer.dc();
        fill(dc, &rect, palette.window);
        draw_progress(dc, bar, position, maximum, role, palette);
        let font = SendMessageW(hwnd, WM_GETFONT, WPARAM(0), LPARAM(0));
        let old_font = (font.0 != 0)
            .then(|| SelectObject(dc, windows::Win32::Graphics::Gdi::HGDIOBJ(font.0 as *mut _)));
        let _ = SetBkMode(dc, TRANSPARENT);
        let text: Vec<u16> = format!("{percent}%").encode_utf16().collect();
        let mut text_rect = RECT {
            left: bar.right + gap,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        };
        draw_native_text(
            dc,
            &text,
            &mut text_rect,
            DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
            palette.text,
        );
        if let Some(old_font) = old_font {
            let _ = SelectObject(dc, old_font);
        }
        buffer.present();
    }
    let _ = EndPaint(hwnd, &paint);
}

unsafe extern "system" fn trackbar_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    match message {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint_trackbar(hwnd, palette_from_reference(reference_data));
            LRESULT(0)
        }
        WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_CAPTURECHANGED | WM_KEYDOWN
        | WM_KEYUP | WM_SETFOCUS | WM_KILLFOCUS | WM_ENABLE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            let _ = InvalidateRect(hwnd, None, false);
            result
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(trackbar_subclass), TRACKBAR_SUBCLASS_ID);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            // Only setters invalidate. paint_trackbar reads TBM_GETPOS,
            // TBM_GETRANGEMIN and TBM_GETRANGEMAX; treating those queries as mutations causes
            // WM_PAINT -> TBM_GET* -> synchronous WM_PAINT recursion and leaves every child after
            // the slider with its initial white USER32 surface.
            if matches!(message, 0x0405..=0x0408) {
                let _ = InvalidateRect(hwnd, None, false);
            }
            result
        }
    }
}

unsafe fn paint_trackbar(hwnd: HWND, palette: Palette) {
    let mut paint = PAINTSTRUCT::default();
    let dc = BeginPaint(hwnd, &mut paint);
    let mut rect = RECT::default();
    let _ = GetClientRect(hwnd, &mut rect);
    let width = (rect.right - rect.left).max(0);
    let height = (rect.bottom - rect.top).max(0);
    if width == 0 || height == 0 {
        let _ = EndPaint(hwnd, &paint);
        return;
    }
    let memory_dc = CreateCompatibleDC(dc);
    let bitmap = CreateCompatibleBitmap(dc, width, height);
    if memory_dc.is_invalid() || bitmap.is_invalid() {
        if !memory_dc.is_invalid() {
            let _ = DeleteDC(memory_dc);
        }
        if !bitmap.is_invalid() {
            let _ = DeleteObject(bitmap);
        }
        let _ = EndPaint(hwnd, &paint);
        return;
    }
    let old_bitmap = SelectObject(memory_dc, bitmap);
    let local = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };
    fill(memory_dc, &local, palette.window);
    let dpi = GetDpiForWindow(hwnd).max(96);
    let minimum = SendMessageW(hwnd, 0x0401, WPARAM(0), LPARAM(0)).0 as i64;
    let maximum = SendMessageW(hwnd, 0x0402, WPARAM(0), LPARAM(0)).0 as i64;
    let position = SendMessageW(hwnd, 0x0400, WPARAM(0), LPARAM(0)).0 as i64;
    let Some(geometry) = trackbar_geometry(width, height, dpi, minimum, maximum, position) else {
        let _ = BitBlt(dc, 0, 0, width, height, memory_dc, 0, 0, SRCCOPY);
        let _ = SelectObject(memory_dc, old_bitmap);
        let _ = DeleteObject(bitmap);
        let _ = DeleteDC(memory_dc);
        let _ = EndPaint(hwnd, &paint);
        return;
    };
    let enabled = IsWindowEnabled(hwnd).as_bool();
    let visual = trackbar_visual(palette, enabled);
    let channel = geometry.channel.as_rect();
    fill_round_rect_antialiased(
        memory_dc,
        channel,
        geometry.channel_radius,
        visual.track_fill,
        visual.track_border,
        visual.background,
    );
    // The progress fill stays inside the channel's final outline.  The previous paint path drew a
    // second rounded control over the complete channel and could erase the outer stroke at the
    // split or maximum endpoint.  Keeping the fill inside a DPI-scaled inset makes the right-hand
    // track line stable in both themes and across repeated drag paints.
    let selected = geometry
        .channel
        .inset(geometry.channel_border)
        .with_right(geometry.position_x);
    if selected.right > selected.left {
        fill_round_rect_antialiased(
            memory_dc,
            selected.as_rect(),
            ((selected.bottom - selected.top) / 2).max(1),
            visual.progress,
            visual.progress,
            visual.track_fill,
        );
    }
    fill_round_rect_antialiased(
        memory_dc,
        geometry.thumb.as_rect(),
        geometry.thumb_radius,
        visual.thumb_fill,
        visual.thumb_border,
        visual.background,
    );
    let _ = BitBlt(dc, 0, 0, width, height, memory_dc, 0, 0, SRCCOPY);
    let _ = SelectObject(memory_dc, old_bitmap);
    let _ = DeleteObject(bitmap);
    let _ = DeleteDC(memory_dc);
    let _ = EndPaint(hwnd, &paint);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TrackbarRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl TrackbarRect {
    const fn as_rect(self) -> RECT {
        RECT {
            left: self.left,
            top: self.top,
            right: self.right,
            bottom: self.bottom,
        }
    }

    fn inset(self, value: i32) -> Self {
        let value = value.max(0);
        let horizontal = value.min((self.right - self.left).max(0) / 2);
        let vertical = value.min((self.bottom - self.top).max(0) / 2);
        Self {
            left: self.left + horizontal,
            top: self.top + vertical,
            right: self.right - horizontal,
            bottom: self.bottom - vertical,
        }
    }

    fn with_right(mut self, right: i32) -> Self {
        self.right = right.clamp(self.left, self.right);
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TrackbarGeometry {
    channel: TrackbarRect,
    thumb: TrackbarRect,
    position_x: i32,
    channel_radius: i32,
    channel_border: i32,
    thumb_radius: i32,
}

fn trackbar_geometry(
    width: i32,
    height: i32,
    dpi: u32,
    minimum: i64,
    maximum: i64,
    position: i64,
) -> Option<TrackbarGeometry> {
    if width <= 0 || height <= 0 {
        return None;
    }
    let thumb_width = scale(14, dpi).max(10).min(width);
    let thumb_height = scale(22, dpi).max(1).min(height);
    // Split odd dimensions asymmetrically so the exclusive right/bottom edge remains inside the
    // client area at 125%, 150% and other DPI values that produce odd-sized thumbs.
    let thumb_left_half = thumb_width / 2;
    let thumb_right_half = thumb_width - thumb_left_half;
    let thumb_top_half = thumb_height / 2;
    let thumb_bottom_half = thumb_height - thumb_top_half;
    let endpoint_left = thumb_left_half;
    let endpoint_right = (width - thumb_right_half).max(endpoint_left);
    let span = (maximum - minimum).max(1);
    let position_x = endpoint_left
        + (((endpoint_right - endpoint_left) as i64 * (position - minimum).clamp(0, span)) / span)
            as i32;
    let center_y = height / 2;
    let channel_half_height = scale(3, dpi).max(2).min((height / 2).max(1));
    let channel = TrackbarRect {
        left: endpoint_left,
        top: (center_y - channel_half_height).max(0),
        // RECT uses an exclusive right edge. Include the maximum endpoint so its rounded cap and
        // outline are not clipped one pixel before the thumb centre.
        right: (endpoint_right + 1).min(width),
        bottom: (center_y + channel_half_height).min(height),
    };
    let thumb = TrackbarRect {
        left: position_x - thumb_left_half,
        top: center_y - thumb_top_half,
        right: position_x + thumb_right_half,
        bottom: center_y + thumb_bottom_half,
    };
    Some(TrackbarGeometry {
        channel,
        thumb,
        position_x,
        channel_radius: ((channel.bottom - channel.top) / 2).max(1),
        channel_border: scale(1, dpi).max(1),
        thumb_radius: (thumb_width / 2).max(3).min((thumb_height / 2).max(1)),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TrackbarVisual {
    background: COLORREF,
    track_fill: COLORREF,
    track_border: COLORREF,
    progress: COLORREF,
    thumb_fill: COLORREF,
    thumb_border: COLORREF,
}

fn trackbar_visual(palette: Palette, enabled: bool) -> TrackbarVisual {
    TrackbarVisual {
        background: palette.window,
        // A white edit-field trough made the light track look unthemed.  Inno's restrained light
        // channel is closer to the pressed-button/separator pair, while dark mode keeps its deep
        // edit surface and visible border.
        track_fill: if palette.dark {
            palette.edit
        } else {
            palette.button_pressed
        },
        track_border: if palette.dark {
            palette.border
        } else {
            palette.separator
        },
        progress: if enabled {
            palette.progress
        } else {
            palette.separator
        },
        thumb_fill: if enabled {
            palette.button
        } else {
            palette.window
        },
        thumb_border: if enabled {
            palette.text_secondary
        } else {
            palette.text_disabled
        },
    }
}

fn scale(value: i32, dpi: u32) -> i32 {
    ((value as i64 * dpi.max(1) as i64 + 48) / 96) as i32
}

/// Solid fill without creating a brush object per call: ExtTextOut(ETO_OPAQUE) paints the
/// rectangle with the DC background colour (the classic GDI FillSolidRect). Frames, rows and
/// surfaces call this dozens of times per paint.
unsafe fn fill(dc: windows::Win32::Graphics::Gdi::HDC, rect: &RECT, color: COLORREF) {
    const CLR_INVALID_VALUE: u32 = 0xffff_ffff;
    let previous = windows::Win32::Graphics::Gdi::SetBkColor(dc, color);
    if previous.0 != CLR_INVALID_VALUE {
        let filled = windows::Win32::Graphics::Gdi::ExtTextOutW(
            dc,
            0,
            0,
            windows::Win32::Graphics::Gdi::ETO_OPAQUE,
            Some(rect as *const RECT),
            PCWSTR::null(),
            0,
            None,
        )
        .as_bool();
        let _ = windows::Win32::Graphics::Gdi::SetBkColor(dc, previous);
        if filled {
            return;
        }
    }
    let brush = CreateSolidBrush(color);
    let _ = FillRect(dc, rect, brush);
    let _ = DeleteObject(brush);
}

fn intersect_rects(first: RECT, second: RECT) -> RECT {
    let left = first.left.max(second.left);
    let top = first.top.max(second.top);
    RECT {
        left,
        top,
        right: first.right.min(second.right).max(left),
        bottom: first.bottom.min(second.bottom).max(top),
    }
}

unsafe fn stroke(dc: windows::Win32::Graphics::Gdi::HDC, rect: RECT, color: COLORREF) {
    let pen = CreatePen(PEN_STYLE(0), 1, color);
    let hollow =
        windows::Win32::Graphics::Gdi::GetStockObject(windows::Win32::Graphics::Gdi::NULL_BRUSH);
    let old_pen = SelectObject(dc, pen);
    let old_brush = SelectObject(dc, hollow);
    let _ =
        windows::Win32::Graphics::Gdi::Rectangle(dc, rect.left, rect.top, rect.right, rect.bottom);
    let _ = SelectObject(dc, old_brush);
    let _ = SelectObject(dc, old_pen);
    let _ = DeleteObject(pen);
}

unsafe fn round_rect(
    dc: windows::Win32::Graphics::Gdi::HDC,
    rect: RECT,
    radius: i32,
    background: COLORREF,
    border: COLORREF,
) {
    let brush = CreateSolidBrush(background);
    let pen = CreatePen(PEN_STYLE(0), 1, border);
    let old_brush = SelectObject(dc, brush);
    let old_pen = SelectObject(dc, pen);
    let diameter = radius.saturating_mul(2);
    let _ = RoundRect(
        dc,
        rect.left,
        rect.top,
        rect.right,
        rect.bottom,
        diameter,
        diameter,
    );
    let _ = SelectObject(dc, old_pen);
    let _ = SelectObject(dc, old_brush);
    let _ = DeleteObject(pen);
    let _ = DeleteObject(brush);
}

pub struct Brushes {
    pub window: HBRUSH,
    pub nav: HBRUSH,
    pub edit: HBRUSH,
    pub edit_opaque: HBRUSH,
    pub list: HBRUSH,
}

impl Brushes {
    pub fn new(palette: Palette) -> Self {
        unsafe {
            Self {
                window: CreateSolidBrush(palette.window),
                nav: CreateSolidBrush(palette.nav),
                edit: CreateSolidBrush(palette.edit_brush_color()),
                edit_opaque: CreateSolidBrush(palette.edit),
                list: CreateSolidBrush(palette.edit),
            }
        }
    }
}

impl Drop for Brushes {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.window);
            let _ = DeleteObject(self.nav);
            let _ = DeleteObject(self.edit);
            let _ = DeleteObject(self.edit_opaque);
            let _ = DeleteObject(self.list);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inno_windows11_reference_colors_are_stable() {
        assert_eq!(Palette::LIGHT.window, rgb(249, 249, 249));
        assert_eq!(Palette::LIGHT.edit, rgb(255, 255, 255));
        assert_eq!(Palette::LIGHT.border, rgb(230, 230, 230));
        assert_eq!(Palette::LIGHT.separator, rgb(222, 222, 222));
        assert_eq!(Palette::LIGHT.accent_fill, rgb(0, 95, 184));
        assert_eq!(Palette::DARK.window, rgb(43, 43, 43));
        assert_eq!(Palette::DARK.edit, rgb(28, 28, 28));
        assert_eq!(Palette::DARK.button, rgb(48, 48, 48));
        assert_eq!(Palette::DARK.separator, rgb(72, 72, 72));
        assert_eq!(Palette::DARK.accent_border, rgb(66, 149, 192));
        assert_eq!(Palette::DARK.highlight_fill, rgb(76, 194, 255));
        assert_eq!(Palette::DARK.highlight_border, rgb(76, 194, 255));
    }

    #[test]
    fn light_combo_chevron_has_real_alpha_and_dark_rgb() {
        let color = Palette::LIGHT.text_secondary;
        let pixels = combo_chevron_pixels(8, 5, color);
        assert_eq!(pixels.len(), 8 * 5 * 4);
        let bottom_left_alpha = pixels[((5 - 1) * 8 * 4) + 3];
        assert_eq!(
            bottom_left_alpha, 0,
            "pixels outside the V silhouette must remain transparent"
        );
        let strongest = pixels
            .chunks_exact(4)
            .max_by_key(|pixel| pixel[3])
            .expect("chevron pixels");
        assert!(strongest[3] >= 240, "chevron needs an opaque visible core");
        assert!(strongest[0] < 96 && strongest[1] < 96 && strongest[2] < 96);
    }

    #[test]
    fn native_edit_never_keeps_layered_redirection() {
        let other_styles = 0x0000_0004isize | 0x0001_0000isize;
        assert_eq!(
            edit_ex_style_without_layering(other_styles | WS_EX_LAYERED.0 as isize),
            other_styles
        );
        assert_eq!(edit_ex_style_without_layering(other_styles), other_styles);
    }

    #[test]
    fn native_theme_classes_cover_headers_scrollbars_and_fields() {
        assert_eq!(
            native_theme_class(NativeControlKind::Header, true),
            NativeThemeClass::DarkItemsView
        );
        assert_eq!(
            native_theme_class(NativeControlKind::ScrollableField, true),
            NativeThemeClass::DarkExplorer
        );
        assert_eq!(
            native_theme_class(NativeControlKind::List, true),
            NativeThemeClass::DarkExplorer
        );
        assert_eq!(
            native_theme_class(NativeControlKind::Field, true),
            NativeThemeClass::DarkCfd
        );
        assert_eq!(
            native_theme_class(NativeControlKind::Field, false),
            NativeThemeClass::Cfd
        );
    }

    #[test]
    fn trackbar_geometry_keeps_minimum_and_maximum_endpoints_inside_the_client() {
        for dpi in [96, 120, 144, 192] {
            let minimum = trackbar_geometry(801, 34, dpi, 10, 90, 10).unwrap();
            let maximum = trackbar_geometry(801, 34, dpi, 10, 90, 90).unwrap();

            for geometry in [minimum, maximum] {
                assert!(geometry.channel.left >= 0);
                assert!(geometry.channel.right <= 801);
                assert!(geometry.channel.top >= 0);
                assert!(geometry.channel.bottom <= 34);
                assert!(geometry.thumb.left >= 0);
                assert!(geometry.thumb.right <= 801);
                assert!(geometry.thumb.top >= 0);
                assert!(geometry.thumb.bottom <= 34);
                assert!(geometry.channel.right > geometry.channel.left);
                assert!(geometry.thumb.right > geometry.thumb.left);

                let inner = geometry.channel.inset(geometry.channel_border);
                let selected = inner.with_right(geometry.position_x);
                assert!(selected.left >= geometry.channel.left + geometry.channel_border);
                assert!(selected.right <= geometry.channel.right - geometry.channel_border);
            }

            let minimum_inner = minimum.channel.inset(minimum.channel_border);
            let maximum_inner = maximum.channel.inset(maximum.channel_border);
            assert_eq!(
                minimum_inner.with_right(minimum.position_x).right,
                minimum_inner.left
            );
            assert_eq!(
                maximum_inner.with_right(maximum.position_x).right,
                maximum_inner.right
            );
        }
    }

    #[test]
    fn trackbar_geometry_clamps_positions_and_handles_tiny_surfaces() {
        let below = trackbar_geometry(300, 24, 96, 20, 40, -100).unwrap();
        let above = trackbar_geometry(300, 24, 96, 20, 40, 999).unwrap();
        assert_eq!(below.position_x, below.channel.left);
        assert_eq!(above.position_x, above.channel.right - 1);
        assert!(trackbar_geometry(0, 24, 96, 0, 1, 0).is_none());
        assert!(trackbar_geometry(300, 0, 96, 0, 1, 0).is_none());

        let tiny = trackbar_geometry(1, 1, 144, 0, 0, 0).unwrap();
        assert_eq!(
            tiny.thumb,
            TrackbarRect {
                left: 0,
                top: 0,
                right: 1,
                bottom: 1
            }
        );
        assert!(tiny.channel.top >= 0 && tiny.channel.bottom <= 1);
    }

    #[test]
    fn trackbar_visuals_are_theme_specific_and_idempotent() {
        let light = trackbar_visual(Palette::LIGHT, true);
        let dark = trackbar_visual(Palette::DARK, true);
        assert_eq!(light, trackbar_visual(Palette::LIGHT, true));
        assert_eq!(dark, trackbar_visual(Palette::DARK, true));
        assert_eq!(light.track_fill, Palette::LIGHT.button_pressed);
        assert_eq!(light.track_border, Palette::LIGHT.separator);
        assert_ne!(light.track_fill, Palette::LIGHT.edit);
        assert_ne!(light.track_fill, Palette::LIGHT.window);
        assert_eq!(dark.track_fill, Palette::DARK.edit);
        assert_eq!(dark.track_border, Palette::DARK.border);
        assert_eq!(light.progress, Palette::LIGHT.progress);
        assert_eq!(dark.progress, Palette::DARK.progress);

        let disabled = trackbar_visual(Palette::DARK, false);
        assert_eq!(disabled.progress, Palette::DARK.separator);
        assert_eq!(disabled.thumb_border, Palette::DARK.text_disabled);
    }

    #[test]
    fn field_styles_remove_competing_native_edges() {
        assert!(is_edit_class("Edit"));
        assert!(is_edit_class("EDIT"));
        assert!(!is_edit_class("ComboBox"));
        assert!(is_combo_class("ComboBox"));
        let (style, ex_style) = borderless_style_bits(
            0x1000 | WS_BORDER.0 as isize,
            WS_EX_CLIENTEDGE.0 as isize | 0x2000,
        );
        assert_eq!(style & WS_BORDER.0 as isize, 0);
        assert_eq!(ex_style & WS_EX_CLIENTEDGE.0 as isize, 0);
        assert_ne!(ex_style & 0x2000, 0);

        let (list_style, list_ex_style) =
            single_border_style_bits(0x1000, WS_EX_CLIENTEDGE.0 as isize | 0x2000);
        assert_ne!(list_style & WS_BORDER.0 as isize, 0);
        assert_eq!(list_ex_style & WS_EX_CLIENTEDGE.0 as isize, 0);

        let (scrollable_style, _) =
            borderless_style_bits(WS_BORDER.0 as isize | WS_VSCROLL.0 as isize, 0);
        assert_eq!(scrollable_style & WS_BORDER.0 as isize, 0);
        assert_ne!(scrollable_style & WS_VSCROLL.0 as isize, 0);
    }

    #[test]
    fn combo_closed_height_uses_the_shared_field_baseline_at_every_supported_dpi() {
        for dpi in [96, 120, 144, 168, 192] {
            let requested = InnoMetrics::for_dpi(dpi).field_height;
            assert_eq!(combo_closed_visual_height(requested, dpi), requested);
            // A taller COMBOBOXINFO measurement must never leak into the visible HWND region.
            assert_eq!(combo_closed_visual_height(requested, dpi), scale(23, dpi));
        }
        assert_eq!(combo_closed_visual_height(0, 96), scale(18, 96));
        assert_eq!(combo_closed_visual_height(1000, 192), scale(36, 192));
    }

    #[test]
    fn native_scrollbar_messages_require_the_rounded_frame_to_be_painted_last() {
        for message in [
            0x00a1, 0x00a2, 0x00a3, 0x0113, 0x0114, 0x0115, 0x020a, 0x020e, 0x02a0,
        ] {
            assert!(native_scrollbar_may_repaint_frame(message));
        }
        for message in [WM_SETTEXT, WM_ENABLE, WM_SIZE, WM_PAINT] {
            assert!(!native_scrollbar_may_repaint_frame(message));
        }
    }

    #[test]
    fn list_view_header_surface_replaces_only_the_straight_side_rails() {
        let frame = RECT {
            left: 30,
            top: 31,
            right: 974,
            bottom: 260,
        };
        let header = RECT {
            left: 33,
            top: 34,
            right: 970,
            bottom: 63,
        };
        let band = list_view_frame_header_band(frame, header, 944, 229, 6);
        assert_eq!(
            band,
            Some(RECT {
                left: 0,
                top: 6,
                right: 944,
                bottom: 32,
            })
        );
        assert_eq!(
            list_view_frame_header_side_borders(band.unwrap(), 2),
            [
                RECT {
                    left: 0,
                    top: 6,
                    right: 2,
                    bottom: 32,
                },
                RECT {
                    left: 942,
                    top: 6,
                    right: 944,
                    bottom: 32,
                },
            ]
        );
    }

    #[test]
    fn list_view_header_surface_fails_closed_outside_the_frame() {
        let frame = RECT {
            left: 100,
            top: 100,
            right: 900,
            bottom: 340,
        };
        assert_eq!(
            list_view_frame_header_band(
                frame,
                RECT {
                    left: 101,
                    top: 102,
                    right: 899,
                    bottom: 105,
                },
                800,
                240,
                6,
            ),
            None
        );
        assert_eq!(list_view_frame_header_band(frame, frame, 0, 240, 6), None);
    }

    #[test]
    fn list_view_selection_matches_navigation_and_does_not_shift_on_hover() {
        let dark_nav = button_visual(
            Palette::DARK,
            ButtonRole::Navigation { selected: true },
            ControlState::default(),
        );
        let light_nav = button_visual(
            Palette::LIGHT,
            ButtonRole::Navigation { selected: true },
            ControlState::default(),
        );
        assert_eq!(
            list_view_row_colors(Palette::DARK, false),
            (Palette::DARK.text, Palette::DARK.edit)
        );
        assert_eq!(
            list_view_row_colors(Palette::DARK, true),
            (dark_nav.text, dark_nav.fill)
        );
        assert_eq!(dark_nav.fill, rgb(76, 194, 255));
        assert_eq!(
            list_view_row_colors(Palette::LIGHT, true),
            (light_nav.text, light_nav.fill)
        );
        assert_eq!(
            list_view_row_colors(Palette::DARK, false),
            (Palette::DARK.text, Palette::DARK.edit)
        );
        assert_eq!(
            list_view_row_colors(Palette::LIGHT, false),
            (Palette::LIGHT.text, Palette::LIGHT.edit)
        );
        assert_eq!(
            list_view_row_colors(Palette::DARK, true),
            (dark_nav.text, dark_nav.fill)
        );
        assert_ne!(light_nav.fill, Palette::LIGHT.edit);
        assert_ne!(light_nav.fill, Palette::LIGHT.window);
        assert_ne!(Palette::DARK.accent_fill, Palette::DARK.progress);
    }

    #[test]
    fn empty_list_view_owns_its_body_paint() {
        assert!(list_view_needs_empty_body_paint(0));
        assert!(!list_view_needs_empty_body_paint(1));
        assert!(!list_view_needs_empty_body_paint(32));
    }

    #[test]
    fn stale_list_view_snapshot_never_overrides_real_selection_colors() {
        const CDIS_SELECTED: u32 = 0x0001;
        const CDIS_FOCUS: u32 = 0x0010;
        const CDIS_HOT: u32 = 0x0040;
        let stale_snapshot = CDIS_SELECTED | CDIS_FOCUS | CDIS_HOT;

        assert_eq!(list_view_custom_draw_state(stale_snapshot), 0);
        assert_eq!(
            list_view_row_colors(Palette::DARK, false),
            (Palette::DARK.text, Palette::DARK.edit)
        );
        let selected = button_visual(
            Palette::DARK,
            ButtonRole::Navigation { selected: true },
            ControlState::default(),
        );
        assert_eq!(
            list_view_row_colors(Palette::DARK, true),
            (selected.text, selected.fill)
        );
    }

    #[test]
    fn category_selection_corners_follow_the_visible_client_edges() {
        const LVS_SINGLESEL_STYLE: isize = 0x0004;
        const LVS_NOCOLUMNHEADER_STYLE: isize = 0x4000;
        const CATEGORY_STYLES: isize = LVS_SINGLESEL_STYLE | LVS_NOCOLUMNHEADER_STYLE;
        let client = RECT {
            left: 0,
            top: 0,
            right: 280,
            bottom: 300,
        };

        assert_eq!(
            category_selection_corners(
                CATEGORY_STYLES,
                RECT {
                    left: 0,
                    top: 0,
                    right: 280,
                    bottom: 24,
                },
                client,
            ),
            Some(CategorySelectionCorners::Top)
        );
        assert_eq!(
            category_selection_corners(
                CATEGORY_STYLES,
                RECT {
                    left: 0,
                    top: 24,
                    right: 280,
                    bottom: 48,
                },
                client,
            ),
            Some(CategorySelectionCorners::Square)
        );
        assert_eq!(
            category_selection_corners(
                CATEGORY_STYLES,
                RECT {
                    left: 0,
                    top: 276,
                    right: 280,
                    bottom: 300,
                },
                client,
            ),
            Some(CategorySelectionCorners::Bottom)
        );
        assert_eq!(
            category_selection_corners(CATEGORY_STYLES, client, client),
            Some(CategorySelectionCorners::All)
        );

        assert_eq!(
            category_selection_corners(LVS_NOCOLUMNHEADER_STYLE, client, client),
            None
        );
        assert_eq!(
            category_selection_corners(LVS_SINGLESEL_STYLE, client, client),
            None
        );
        assert_eq!(
            category_selection_corners(
                CATEGORY_STYLES,
                RECT {
                    left: 0,
                    top: 300,
                    right: 280,
                    bottom: 324,
                },
                client,
            ),
            None
        );
    }

    #[test]
    fn native_theme_still_supplies_control_content_beneath_deterministic_frames() {
        assert_eq!(
            native_theme_class(NativeControlKind::Field, true),
            NativeThemeClass::DarkCfd
        );
        assert_eq!(
            native_theme_class(NativeControlKind::ListView, true),
            NativeThemeClass::DarkExplorer
        );
        // Keep the report's native non-client scrollbar in the supported dark Explorer family.
        // FlatSB colour overrides are unavailable in comctl32 v6 and ItemsView produces a bright
        // white scrollbar trough on the dark page.
        assert_eq!(
            native_theme_class(NativeControlKind::Field, false),
            NativeThemeClass::Cfd
        );
    }

    #[test]
    fn shared_radio_painter_is_limited_to_real_auto_radio_buttons() {
        assert!(button_style_is_auto_radio(0x0009));
        assert!(button_style_is_auto_radio(0x5001_0009));
        assert!(!button_style_is_auto_radio(0x0003)); // auto checkbox
        assert!(!is_auto_radio_button("Static", 0x0009));
        assert!(is_auto_radio_button("BUTTON", 0x0009));
    }

    #[test]
    fn radio_geometry_is_centered_and_bounded_at_supported_dpi() {
        for dpi in [96, 120, 144, 192] {
            let width = scale(300, dpi);
            let height = scale(24, dpi);
            let geometry = radio_geometry(width, height, dpi).unwrap();
            let glyph_size = geometry.glyph.right - geometry.glyph.left;
            assert_eq!(glyph_size, scale(13, dpi));
            assert_eq!(geometry.glyph.bottom - geometry.glyph.top, glyph_size);
            assert_eq!(geometry.glyph.top, (height - glyph_size) / 2);
            assert!(geometry.glyph.left >= 0 && geometry.glyph.right <= width);
            assert!(geometry.glyph.top >= 0 && geometry.glyph.bottom <= height);
            assert_eq!(geometry.text.left, geometry.glyph.right + scale(5, dpi));
            assert_eq!(geometry.text.right, width);
        }
    }

    #[test]
    fn radio_geometry_fails_closed_for_empty_and_clamps_tiny_controls() {
        assert_eq!(radio_geometry(0, 24, 96), None);
        assert_eq!(radio_geometry(100, 0, 96), None);
        let geometry = radio_geometry(7, 5, 192).unwrap();
        assert_eq!(geometry.glyph.right, 5);
        assert_eq!(geometry.glyph.bottom, 5);
        assert!(geometry.text.left <= 7);
    }

    #[test]
    fn list_view_checkbox_uses_the_regular_checkbox_size_and_stays_in_its_slot() {
        for dpi in [96, 120, 144, 168, 192] {
            let row = RECT {
                left: 7,
                top: 11,
                right: 407,
                bottom: 11 + scale(24, dpi),
            };
            let rect = list_view_checkbox_rect(row, dpi);
            let expected_size = scale(13, dpi);
            assert_eq!(rect.right - rect.left, expected_size);
            assert_eq!(rect.bottom - rect.top, expected_size);
            assert_eq!(rect.top - row.top, (scale(24, dpi) - expected_size) / 2);
            assert!(rect.left >= row.left);
            assert!(rect.right <= row.left + scale(24, dpi));
            assert!(rect.top >= row.top);
            assert!(rect.bottom <= row.bottom);
        }
    }

    #[test]
    fn embedded_win11_button_theme_maps_all_states_and_dpi_buckets() {
        let normal = ControlState {
            hot: false,
            pressed: false,
            disabled: false,
            focused: false,
        };
        assert_eq!(embedded_theme_dpi_index(96), 0);
        assert_eq!(embedded_theme_dpi_index(120), 1);
        assert_eq!(embedded_theme_dpi_index(144), 2);
        assert_eq!(embedded_theme_dpi_index(192), 3);
        assert_eq!(themed_button_state(normal, false), 0);
        assert_eq!(themed_button_state(normal, true), 4);
        assert_eq!(
            themed_button_state(
                ControlState {
                    hot: true,
                    ..normal
                },
                true
            ),
            5
        );
        assert_eq!(
            themed_button_state(
                ControlState {
                    pressed: true,
                    ..normal
                },
                true
            ),
            6
        );
        assert_eq!(
            themed_button_state(
                ControlState {
                    disabled: true,
                    ..normal
                },
                true
            ),
            7
        );
        for dark in [false, true] {
            for dpi in [96, 120, 144, 192] {
                let glyph = embedded_button_glyph(dark, dpi, normal, true);
                assert_eq!(glyph.width, scale(13, dpi));
                assert_eq!(glyph.height, scale(13, dpi));
                assert_eq!(glyph.bgra.len(), (glyph.width * glyph.height * 4) as usize);
            }
        }
    }

    #[test]
    fn generated_radio_glyphs_are_symmetric_and_light_mode_has_no_black_spurs() {
        for side in [13, 16, 17, 20, 23, 26] {
            for palette in [Palette::LIGHT, Palette::DARK] {
                for checked in [false, true] {
                    for state in [
                        ControlState::default(),
                        ControlState {
                            hot: true,
                            ..ControlState::default()
                        },
                        ControlState {
                            pressed: true,
                            ..ControlState::default()
                        },
                        ControlState {
                            disabled: true,
                            ..ControlState::default()
                        },
                    ] {
                        let pixels = radio_glyph_bgra(side, palette, state, checked);
                        let pixel = |x: i32, y: i32| {
                            let offset = ((y * side + x) * 4) as usize;
                            &pixels[offset..offset + 4]
                        };
                        assert_eq!(pixels.len(), (side * side * 4) as usize);
                        for y in 0..side {
                            for x in 0..side {
                                assert_eq!(pixel(x, y), pixel(side - 1 - x, y));
                                assert_eq!(pixel(x, y), pixel(x, side - 1 - y));
                                assert_eq!(pixel(x, y)[3], 255);
                                if !palette.dark {
                                    assert_ne!(&pixel(x, y)[0..3], &[0, 0, 0]);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn list_view_trailing_fill_begins_after_the_final_row() {
        let client = RECT {
            left: 0,
            top: 0,
            right: 900,
            bottom: 180,
        };
        assert_eq!(
            list_view_trailing_body_rect(client, 3, Some(72)),
            Some(RECT {
                left: 0,
                top: 72,
                right: 900,
                bottom: 180,
            })
        );
        assert_eq!(list_view_trailing_body_rect(client, 0, None), None);
        assert_eq!(list_view_trailing_body_rect(client, 3, Some(180)), None);
        assert_eq!(list_view_trailing_body_rect(client, 3, Some(240)), None);
    }
}
