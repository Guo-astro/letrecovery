//! Redraw transactions for the native UI.
//!
//! `WM_SETREDRAW(FALSE)` is implemented by DefWindowProc by clearing `WS_VISIBLE`. On a top-level
//! window that removes the window from hit testing, so a click lands on whatever window is behind
//! LetRecovery, and DWM may stop presenting it for a frame, which shows up as a full-window flash.
//! Redraw is therefore only suspended on child windows; a top-level transaction repaints once.
//! Repaints never request `RDW_ERASE`: every LetRecovery surface paints its own background, and an
//! erase pass with a class brush is exactly the intermediate frame that is seen as flicker.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    RedrawWindow, RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RDW_NOCHILDREN,
    RDW_UPDATENOW, REDRAW_WINDOW_FLAGS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, GetAncestor, IsWindowVisible, SendMessageW, SystemParametersInfoW, GA_ROOT,
    HTBOTTOMRIGHT, HTCAPTION, HTLEFT, SPI_GETDRAGFULLWINDOWS, SPI_SETDRAGFULLWINDOWS,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WM_NCLBUTTONDOWN, WM_SETREDRAW,
};

/// A repaint transaction: either a screen cover (see `begin_page_transition`) or a freeze.
pub(crate) struct SuspendedRedraw {
    root: HWND,
    frozen: bool,
    captured: bool,
    cover: Option<ScreenCover>,
    trace: Option<TraceSpan>,
    finished: bool,
}

impl Drop for SuspendedRedraw {
    fn drop(&mut self) {
        // A transaction that is never resumed (early return, unwinding) must still give the
        // window its visibility and the mouse back, and must not leave a cover on screen.
        if self.finished {
            return;
        }
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetCapture, ReleaseCapture};
        unsafe {
            if self.frozen {
                let _ = SendMessageW(self.root, WM_SETREDRAW, WPARAM(1), LPARAM(0));
            }
            if self.captured && GetCapture() == self.root {
                let _ = ReleaseCapture();
            }
            let _ = RedrawWindow(
                self.root,
                None,
                None,
                RDW_INVALIDATE | RDW_FRAME | RDW_ALLCHILDREN | RDW_UPDATENOW,
            );
        }
        // The cover (a field) is destroyed after this repaint.
    }
}

unsafe fn is_top_level(window: HWND) -> bool {
    GetAncestor(window, GA_ROOT) == window
}

/// Freezes painting of `root` while a batch of controls is shown, hidden, moved and relabelled.
/// Nothing reaches the screen until `resume`: neither WM_PAINT nor controls that draw at once
/// through GetDC.
///
/// WM_SETREDRAW(FALSE) works by clearing WS_VISIBLE, and for a top-level window that also takes
/// it out of mouse hit-testing, so a click during the freeze would reach the window behind. The
/// mouse is therefore captured for the short duration of a top-level freeze.
pub(crate) unsafe fn suspend(root: HWND) -> Option<SuspendedRedraw> {
    freeze(root, TraceSpan::begin("重绘事务"))
}

unsafe fn freeze(root: HWND, trace: Option<TraceSpan>) -> Option<SuspendedRedraw> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetCapture, SetCapture};
    if !IsWindowVisible(root).as_bool() {
        return None;
    }
    let captured = is_top_level(root) && GetCapture().is_invalid() && {
        let _ = SetCapture(root);
        GetCapture() == root
    };
    let _ = SendMessageW(root, WM_SETREDRAW, WPARAM(0), LPARAM(0));
    Some(SuspendedRedraw {
        root,
        frozen: true,
        captured,
        cover: None,
        trace,
        finished: false,
    })
}

/// Starts a page-sized change (page switch, advanced options, progress page, language, theme)
/// that must appear in one frame however long the repaint takes.
///
/// With DWM composition every top-level window owns its own surface. A borderless owned popup
/// showing a snapshot of the current client area is placed exactly over it; the real window is
/// then changed and completely repainted underneath, where nobody can see it, and the cover is
/// removed. The next DWM frame shows the finished page at once: slow painting on a weak PC only
/// makes the switch later, never piecemeal. The cover also takes any click made meanwhile, so
/// nothing reaches the window behind. Without DWM composition (some WinPE builds) this falls back
/// to the freeze of `suspend`.
pub(crate) unsafe fn begin_page_transition(root: HWND, label: &str) -> Option<SuspendedRedraw> {
    if !IsWindowVisible(root).as_bool() {
        return None;
    }
    let mut trace = TraceSpan::begin(label);
    if is_top_level(root) {
        if let Some(cover) = show_screen_cover(root) {
            if let Some(trace) = trace.as_mut() {
                trace.phase("遮罩");
            }
            return Some(SuspendedRedraw {
                root,
                frozen: false,
                captured: false,
                cover: Some(cover),
                trace,
                finished: false,
            });
        }
        if ui_trace_enabled() {
            log::info!("[UI 渲染] {label}：桌面合成不可用，改用冻结重绘");
        }
    }
    freeze(root, trace)
}

pub(crate) unsafe fn resume(root: HWND, transaction: Option<SuspendedRedraw>) {
    resume_with_flags(
        root,
        transaction,
        RDW_INVALIDATE | RDW_FRAME | RDW_ALLCHILDREN | RDW_UPDATENOW,
        false,
    );
}

pub(crate) unsafe fn resume_client(root: HWND, transaction: Option<SuspendedRedraw>) {
    resume_with_flags(
        root,
        transaction,
        RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_UPDATENOW,
        true,
    );
}

pub(crate) unsafe fn invalidate_client_tree(root: HWND) {
    let _ = RedrawWindow(root, None, None, client_refresh_flags());
}

fn client_refresh_flags() -> REDRAW_WINDOW_FLAGS {
    RDW_INVALIDATE | RDW_ERASE | RDW_NOCHILDREN
}

unsafe fn resume_with_flags(
    root: HWND,
    transaction: Option<SuspendedRedraw>,
    flags: REDRAW_WINDOW_FLAGS,
    client_only: bool,
) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetCapture, ReleaseCapture};
    let Some(mut transaction) = transaction else {
        return;
    };
    transaction.finished = true;
    if let Some(trace) = transaction.trace.as_mut() {
        trace.phase("状态更新");
    }
    if transaction.frozen {
        let _ = SendMessageW(root, WM_SETREDRAW, WPARAM(1), LPARAM(0));
    }
    if transaction.captured && GetCapture() == root {
        let _ = ReleaseCapture();
    }
    let paint_flags = if transaction.cover.is_some() && client_only {
        // Nothing was frozen under the cover, so every change has already invalidated itself:
        // paint exactly that, which keeps a page switch as short as possible.
        RDW_UPDATENOW | RDW_ALLCHILDREN
    } else {
        flags | RDW_UPDATENOW
    };
    if transaction.cover.is_none() && is_top_level(root) {
        // Freeze path: start the single repaint pass right after a DWM composition, so it is
        // normally finished before the next one. Fails harmlessly without DWM.
        let _ = windows::Win32::Graphics::Dwm::DwmFlush();
    }
    // No RDW_ERASE: every surface paints its own background, an erase pass is a visible frame.
    if transaction.cover.is_some() && client_only {
        // Under the cover only the areas already invalid were painted. A spot no control covers
        // any more but that no ShowWindow/MoveWindow invalidated (MoveWindow without repaint)
        // kept stale pixels on every page. The window's own surface is repainted here; its
        // children are clipped out (WS_CLIPCHILDREN), so no control repaints for this.
        let style = windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
            root,
            windows::Win32::UI::WindowsAndMessaging::GWL_STYLE,
        );
        if style & windows::Win32::UI::WindowsAndMessaging::WS_CLIPCHILDREN.0 as isize != 0 {
            let _ = RedrawWindow(
                root,
                None,
                None,
                windows::Win32::Graphics::Gdi::RDW_INVALIDATE
                    | windows::Win32::Graphics::Gdi::RDW_NOCHILDREN,
            );
        }
    }
    let _ = RedrawWindow(root, None, None, paint_flags);
    let _ = windows::Win32::Graphics::Gdi::GdiFlush();
    if let Some(trace) = transaction.trace.as_mut() {
        trace.phase("绘制");
    }
    if let Some(cover) = transaction.cover.take() {
        drop(cover);
        if let Some(trace) = transaction.trace.as_mut() {
            trace.phase("揭开遮罩");
        }
    }
}

/// Many WinPE builds, and some desktops, turn off "Show window contents while dragging". Windows
/// then moves and resizes with an outline only and applies the new size when the mouse button is
/// released. For the duration of one move/size loop on a LetRecovery window, full-window dragging
/// is enabled for the current session only (never written to the user profile, no graphics driver
/// involved) and the previous value is restored as soon as the loop ends.
pub(crate) unsafe fn nc_left_button_down_with_live_drag(
    hwnd: HWND,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let hit = wparam.0 as u32;
    let moving_or_sizing = hit == HTCAPTION || (HTLEFT..=HTBOTTOMRIGHT).contains(&hit);
    let mut full_drag = BOOL(1);
    let mut enabled_here = false;
    if moving_or_sizing
        && SystemParametersInfoW(
            SPI_GETDRAGFULLWINDOWS,
            0,
            Some(&mut full_drag as *mut BOOL as *mut core::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok()
        && !full_drag.as_bool()
    {
        enabled_here = SystemParametersInfoW(
            SPI_SETDRAGFULLWINDOWS,
            1,
            None,
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok();
    }
    // DefWindowProc runs the whole modal move/size loop inside this call.
    let result = DefWindowProcW(hwnd, WM_NCLBUTTONDOWN, wparam, lparam);
    if enabled_here {
        let _ = SystemParametersInfoW(
            SPI_SETDRAGFULLWINDOWS,
            0,
            None,
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    result
}

// ------------------------------------------------------------------------------------------
// Off-screen surfaces.
// ------------------------------------------------------------------------------------------

/// One reusable 32-bit top-down DIB section selected into its own memory DC.
///
/// The former buffer created a new screen-sized bitmap for every paint message and first copied
/// the pixels that were on screen into it. Reading a display DC is the slow direction under DWM
/// (Microsoft's DWM guidance explicitly advises against it), and doing it for the whole client on
/// every resize step or page switch was a large part of the measured paint time. The UI thread now
/// keeps a few surfaces alive, grows them in coarse steps and never reads the screen for painters
/// that cover their whole area. Pure GDI: no GPU, DirectX or graphics driver is involved.
struct CachedSurface {
    dc: windows::Win32::Graphics::Gdi::HDC,
    bitmap: windows::Win32::Graphics::Gdi::HBITMAP,
    previous: windows::Win32::Graphics::Gdi::HGDIOBJ,
    width: i32,
    height: i32,
    in_use: bool,
}

impl CachedSurface {
    unsafe fn create(width: i32, height: i32) -> Option<Self> {
        use windows::Win32::Graphics::Gdi::{
            CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, ReleaseDC,
            SelectObject,
        };
        if width <= 0 || height <= 0 {
            return None;
        }
        // A bitmap compatible with the display, exactly like the per-paint buffers this cache
        // replaced. Owner-draw code derives scratch bitmaps from the buffer DC with
        // CreateCompatibleBitmap; from a DIB-section DC that call yields a bottom-up DIB, and
        // DrawThemeTextEx(DTT_COMPOSITED) then renders every caption upside down.
        let screen = GetDC(HWND::default());
        if screen.is_invalid() {
            return None;
        }
        let dc = CreateCompatibleDC(screen);
        let bitmap = CreateCompatibleBitmap(screen, width, height);
        let _ = ReleaseDC(HWND::default(), screen);
        if dc.is_invalid() || bitmap.is_invalid() {
            if !bitmap.is_invalid() {
                let _ = DeleteObject(bitmap);
            }
            if !dc.is_invalid() {
                let _ = DeleteDC(dc);
            }
            return None;
        }
        let previous = SelectObject(dc, bitmap);
        Some(Self {
            dc,
            bitmap,
            previous,
            width,
            height,
            in_use: false,
        })
    }

    unsafe fn destroy(&mut self) {
        use windows::Win32::Graphics::Gdi::{DeleteDC, DeleteObject, SelectObject};
        if !self.dc.is_invalid() {
            let _ = SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.bitmap);
            let _ = DeleteDC(self.dc);
        }
        self.dc = Default::default();
        self.bitmap = Default::default();
    }
}

thread_local! {
    static CACHED_SURFACES: std::cell::RefCell<Vec<CachedSurface>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Nested paints (a report painting while its parent composes an owner-drawn item, a dialog
/// painting inside a page transition) each need their own surface.
const MAX_CACHED_SURFACES: usize = 4;
/// Surfaces grow in 128-pixel steps so a live resize reuses one allocation for many steps.
const SURFACE_GRANULARITY: i32 = 128;

fn surface_extent(value: i32) -> i32 {
    let value = value.max(1);
    value.saturating_add(SURFACE_GRANULARITY - 1) / SURFACE_GRANULARITY * SURFACE_GRANULARITY
}

enum SurfaceLease {
    None,
    Cached(usize),
    Owned(CachedSurface),
}

unsafe fn lease_surface(
    width: i32,
    height: i32,
) -> (windows::Win32::Graphics::Gdi::HDC, SurfaceLease) {
    let leased = CACHED_SURFACES.with(|cell| {
        let Ok(mut surfaces) = cell.try_borrow_mut() else {
            return None;
        };
        if let Some(index) = surfaces.iter().position(|surface| {
            !surface.in_use && surface.width >= width && surface.height >= height
        }) {
            surfaces[index].in_use = true;
            return Some((surfaces[index].dc, SurfaceLease::Cached(index)));
        }
        let wanted_width = surface_extent(width);
        let wanted_height = surface_extent(height);
        if let Some(index) = surfaces.iter().position(|surface| !surface.in_use) {
            // Grow the largest free surface instead of keeping several undersized ones.
            let replacement = CachedSurface::create(
                wanted_width.max(surfaces[index].width),
                wanted_height.max(surfaces[index].height),
            )?;
            surfaces[index].destroy();
            surfaces[index] = replacement;
            surfaces[index].in_use = true;
            return Some((surfaces[index].dc, SurfaceLease::Cached(index)));
        }
        if surfaces.len() < MAX_CACHED_SURFACES {
            let mut surface = CachedSurface::create(wanted_width, wanted_height)?;
            surface.in_use = true;
            let dc = surface.dc;
            surfaces.push(surface);
            return Some((dc, SurfaceLease::Cached(surfaces.len() - 1)));
        }
        None
    });
    if let Some(leased) = leased {
        return leased;
    }
    match CachedSurface::create(width, height) {
        Some(surface) => (surface.dc, SurfaceLease::Owned(surface)),
        None => (Default::default(), SurfaceLease::None),
    }
}

unsafe fn release_surface(lease: SurfaceLease) {
    match lease {
        SurfaceLease::None => {}
        SurfaceLease::Cached(index) => CACHED_SURFACES.with(|cell| {
            if let Ok(mut surfaces) = cell.try_borrow_mut() {
                if let Some(surface) = surfaces.get_mut(index) {
                    surface.in_use = false;
                }
            }
        }),
        SurfaceLease::Owned(mut surface) => surface.destroy(),
    }
}

fn intersect_rect(
    first: windows::Win32::Foundation::RECT,
    second: windows::Win32::Foundation::RECT,
) -> windows::Win32::Foundation::RECT {
    let left = first.left.max(second.left);
    let top = first.top.max(second.top);
    let right = first.right.min(second.right).max(left);
    let bottom = first.bottom.min(second.bottom).max(top);
    windows::Win32::Foundation::RECT {
        left,
        top,
        right,
        bottom,
    }
}

/// Off-screen buffer for one paint pass. Painting goes into a cached memory surface and reaches
/// the screen in a single BitBlt, so a background fill is never visible on its own. The painter
/// keeps using its ordinary client coordinates; the buffer only covers the area that is actually
/// being published. When GDI cannot provide a surface, painting falls back to the target DC.
pub(crate) struct PaintBuffer {
    target: windows::Win32::Graphics::Gdi::HDC,
    memory: windows::Win32::Graphics::Gdi::HDC,
    lease: Option<SurfaceLease>,
    saved: i32,
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    started: Option<std::time::Instant>,
}

impl PaintBuffer {
    /// Buffer over `rect` that starts from the pixels currently on screen, for owner-draw code
    /// that may leave parts of its item untouched.
    pub(crate) unsafe fn begin(
        target: windows::Win32::Graphics::Gdi::HDC,
        rect: windows::Win32::Foundation::RECT,
    ) -> Self {
        Self::start(target, rect, true)
    }

    /// Buffer over the part of `rect` that is inside `update` (normally `PAINTSTRUCT::rcPaint`).
    /// The painter must cover every pixel of that area; nothing is read back from the screen.
    pub(crate) unsafe fn begin_opaque(
        target: windows::Win32::Graphics::Gdi::HDC,
        rect: windows::Win32::Foundation::RECT,
        update: windows::Win32::Foundation::RECT,
    ) -> Self {
        Self::start(target, intersect_rect(rect, update), false)
    }

    unsafe fn start(
        target: windows::Win32::Graphics::Gdi::HDC,
        area: windows::Win32::Foundation::RECT,
        copy_screen: bool,
    ) -> Self {
        use windows::Win32::Graphics::Gdi::{
            GetBkColor, GetCurrentObject, GetTextColor, IntersectClipRect, RestoreDC, SaveDC,
            SelectObject, SetBkColor, SetTextColor, SetViewportOrgEx, OBJ_FONT,
        };
        let width = (area.right - area.left).max(0);
        let height = (area.bottom - area.top).max(0);
        let mut buffer = Self {
            target,
            memory: Default::default(),
            lease: None,
            saved: 0,
            left: area.left,
            top: area.top,
            width,
            height,
            started: trace_now(),
        };
        if width == 0 || height == 0 || target.is_invalid() {
            return buffer;
        }
        let (memory, lease) = lease_surface(width, height);
        if memory.is_invalid() {
            release_surface(lease);
            return buffer;
        }
        let saved = SaveDC(memory);
        if saved == 0 {
            release_surface(lease);
            return buffer;
        }
        // Client coordinates of `area.left/top` land on the surface origin, and the clip box is
        // exactly the published area: controls that paint "what is invalid" (ListView through
        // WM_PRINTCLIENT) therefore draw only the rows that are being published.
        if !SetViewportOrgEx(memory, -area.left, -area.top, None).as_bool() {
            let _ = RestoreDC(memory, saved);
            release_surface(lease);
            return buffer;
        }
        let _ = IntersectClipRect(memory, area.left, area.top, area.right, area.bottom);
        if copy_screen {
            let _ = windows::Win32::Graphics::Gdi::BitBlt(
                memory,
                area.left,
                area.top,
                width,
                height,
                target,
                area.left,
                area.top,
                windows::Win32::Graphics::Gdi::SRCCOPY,
            );
        }
        // Carry over the DC state that owner-draw code may rely on.
        let _ = SelectObject(memory, GetCurrentObject(target, OBJ_FONT));
        let _ = SetTextColor(memory, GetTextColor(target));
        let _ = SetBkColor(memory, GetBkColor(target));
        buffer.memory = memory;
        buffer.lease = Some(lease);
        buffer.saved = saved;
        buffer
    }

    pub(crate) fn dc(&self) -> windows::Win32::Graphics::Gdi::HDC {
        if self.memory.is_invalid() {
            self.target
        } else {
            self.memory
        }
    }

    pub(crate) unsafe fn present(&self) {
        if !self.memory.is_invalid() {
            let _ = windows::Win32::Graphics::Gdi::BitBlt(
                self.target,
                self.left,
                self.top,
                self.width,
                self.height,
                self.memory,
                self.left,
                self.top,
                windows::Win32::Graphics::Gdi::SRCCOPY,
            );
        }
        trace_surface_painted(self.started);
    }
}

impl Drop for PaintBuffer {
    fn drop(&mut self) {
        unsafe {
            if !self.memory.is_invalid() {
                // Restores the viewport, clip region and every object a painter left selected, so
                // a reused surface never keeps a caller's font or brush alive.
                let _ = windows::Win32::Graphics::Gdi::RestoreDC(self.memory, self.saved);
            }
            if let Some(lease) = self.lease.take() {
                release_surface(lease);
            }
        }
    }
}

/// Composes a whole window (client and non-client) off-screen and publishes it with one BitBlt
/// through the window DC. The window DC keeps its normal clipping (window region, siblings), so
/// only the pixels this window owns are replaced. Used by the sibling frame overlays, whose
/// background, outline and header band used to be written to the screen one after another.
pub(crate) unsafe fn paint_window_buffered(
    hwnd: HWND,
    paint: impl FnOnce(windows::Win32::Graphics::Gdi::HDC, windows::Win32::Foundation::RECT),
) {
    use windows::Win32::Graphics::Gdi::{GetWindowDC, ReleaseDC};
    let mut window = windows::Win32::Foundation::RECT::default();
    if windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut window).is_err() {
        return;
    }
    let rect = windows::Win32::Foundation::RECT {
        left: 0,
        top: 0,
        right: (window.right - window.left).max(0),
        bottom: (window.bottom - window.top).max(0),
    };
    if rect.right == 0 || rect.bottom == 0 {
        return;
    }
    let target = GetWindowDC(hwnd);
    if target.is_invalid() {
        return;
    }
    {
        let buffer = PaintBuffer::begin_opaque(target, rect, rect);
        paint(buffer.dc(), rect);
        buffer.present();
    }
    let _ = ReleaseDC(hwnd, target);
}

/// Owner-drawn buttons and custom statics are composed off-screen and copied in one BitBlt.
/// Other owner-draw kinds (headers, menus, list items) and items that do not start at 0,0 are
/// drawn directly. The off-screen result is copied only when `draw` reports it handled the item.
pub(crate) unsafe fn draw_item_buffered(
    item: &windows::Win32::UI::Controls::DRAWITEMSTRUCT,
    draw: impl FnOnce(&windows::Win32::UI::Controls::DRAWITEMSTRUCT) -> bool,
) -> bool {
    if ui_trace_enabled() {
        TRACE_OWNER_DRAW.fetch_add(1, Ordering::Relaxed);
    }
    const ODT_BUTTON_KIND: u32 = 4;
    const ODT_STATIC_KIND: u32 = 5;
    let kind = item.CtlType.0;
    if kind != ODT_BUTTON_KIND && kind != ODT_STATIC_KIND {
        return draw(item);
    }
    let buffer = PaintBuffer::begin(item.hDC, item.rcItem);
    let mut buffered = *item;
    buffered.hDC = buffer.dc();
    let handled = draw(&buffered);
    if handled {
        buffer.present();
    }
    handled
}

/// Sets a window's text only when it actually changes. Every WM_SETTEXT repaints the control, and
/// task timers refresh their labels many times per second.
pub(crate) unsafe fn set_window_text_if_changed(hwnd: HWND, value: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowTextLengthW, GetWindowTextW, SetWindowTextW,
    };
    let mut wanted: Vec<u16> = value.encode_utf16().collect();
    let current_length = GetWindowTextLengthW(hwnd);
    if current_length >= 0 && current_length as usize == wanted.len() {
        let mut current = vec![0u16; wanted.len() + 1];
        let copied = GetWindowTextW(hwnd, &mut current);
        if copied >= 0 && copied as usize == wanted.len() && current[..wanted.len()] == wanted[..] {
            return;
        }
    }
    wanted.push(0);
    let _ = SetWindowTextW(hwnd, windows::core::PCWSTR(wanted.as_ptr()));
}

/// Shows a top-level window whose first visible frame is already complete (the approach Chromium
/// uses against the dark-mode white flash). DWM keeps the window cloaked while it is shown and
/// painted synchronously; GDI output is flushed and DWM is given one composition to pick the
/// surface up before the window is uncloaked. Without DWM (or before Windows 8) cloaking is not
/// available and the window is shown and painted exactly as before.
pub(crate) unsafe fn show_top_level_without_flash(hwnd: HWND) {
    let trace_start = trace_now();
    use windows::Win32::Graphics::Dwm::{DwmFlush, DwmSetWindowAttribute, DWMWA_CLOAK};
    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOW};
    let cloak = BOOL(1);
    let cloaked = DwmSetWindowAttribute(
        hwnd,
        DWMWA_CLOAK,
        &cloak as *const BOOL as *const core::ffi::c_void,
        std::mem::size_of::<BOOL>() as u32,
    )
    .is_ok();
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = RedrawWindow(
        hwnd,
        None,
        None,
        RDW_INVALIDATE | RDW_FRAME | RDW_ALLCHILDREN | RDW_UPDATENOW,
    );
    if cloaked {
        let _ = windows::Win32::Graphics::Gdi::GdiFlush();
        let _ = DwmFlush();
        let uncloak = BOOL(0);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_CLOAK,
            &uncloak as *const BOOL as *const core::ffi::c_void,
            std::mem::size_of::<BOOL>() as u32,
        );
    }
    if let Some(start) = trace_start {
        log::info!(
            "[UI 渲染] 首次显示窗口：隐藏状态下绘制完成用时 {} ms",
            format_ms(micros_since(start))
        );
    }
}

// ------------------------------------------------------------------------------------------
// Screen cover for page transitions.
// ------------------------------------------------------------------------------------------

unsafe fn composition_enabled() -> bool {
    windows::Win32::Graphics::Dwm::DwmIsCompositionEnabled()
        .map(|enabled| enabled.as_bool())
        .unwrap_or(false)
}

/// Live resize: after a size step has been laid out and painted, wait until DWM has composed it.
/// DWM draws the new window frame as soon as the size changes; without this wait the modal size
/// loop starts the next step before the content painted for this one is on screen, so the content
/// trails the border by one or more frames and steps that are never displayed still get painted.
/// Waiting once per step keeps frame and content in the same composition frame. Without desktop
/// composition (Windows 7 basic theme) there is nothing to wait for.
pub(crate) unsafe fn present_live_resize_step() {
    if composition_enabled() {
        let _ = windows::Win32::Graphics::Dwm::DwmFlush();
    }
}

struct ScreenCover {
    window: HWND,
    bitmap: windows::Win32::Graphics::Gdi::HBITMAP,
}

impl Drop for ScreenCover {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(self.window);
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(self.bitmap);
        }
    }
}

const SCREEN_COVER_CLASS: windows::core::PCWSTR = windows::core::w!("LetRecoveryScreenCover");

unsafe extern "system" fn screen_cover_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, BitBlt, CreateCompatibleDC, DeleteDC, EndPaint, SelectObject, HBITMAP,
        PAINTSTRUCT, SRCCOPY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, GetClientRect, GetWindowLongPtrW, GWLP_USERDATA, WM_ERASEBKGND,
        WM_MOUSEACTIVATE, WM_NCHITTEST, WM_PAINT,
    };
    match message {
        WM_ERASEBKGND => LRESULT(1),
        // Never activate; absorb clicks made while a page is being built underneath.
        WM_MOUSEACTIVATE => LRESULT(3), // MA_NOACTIVATE
        WM_NCHITTEST => LRESULT(1),     // HTCLIENT
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            let bitmap = HBITMAP(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut core::ffi::c_void);
            if !bitmap.is_invalid() {
                let memory = CreateCompatibleDC(dc);
                if !memory.is_invalid() {
                    let previous = SelectObject(memory, bitmap);
                    let mut client = windows::Win32::Foundation::RECT::default();
                    let _ = GetClientRect(hwnd, &mut client);
                    let _ = BitBlt(dc, 0, 0, client.right, client.bottom, memory, 0, 0, SRCCOPY);
                    let _ = SelectObject(memory, previous);
                    let _ = DeleteDC(memory);
                }
            }
            let _ = EndPaint(hwnd, &paint);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

unsafe fn register_screen_cover_class() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        LoadCursorW, RegisterClassExW, IDC_ARROW, WNDCLASSEXW,
    };
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.load(Ordering::Relaxed) {
        return true;
    }
    let Ok(module) = windows::Win32::System::LibraryLoader::GetModuleHandleW(None) else {
        return false;
    };
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(screen_cover_proc),
        hInstance: windows::Win32::Foundation::HINSTANCE(module.0),
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        lpszClassName: SCREEN_COVER_CLASS,
        ..Default::default()
    };
    let registered = RegisterClassExW(&class) != 0
        || windows::Win32::Foundation::GetLastError()
            == windows::Win32::Foundation::ERROR_CLASS_ALREADY_EXISTS;
    if registered {
        REGISTERED.store(true, Ordering::Relaxed);
    }
    registered
}

/// Snapshots `owner`'s client area and shows it in a borderless owned popup at the same place.
/// Returns once DWM is presenting the cover, so nothing changed underneath can show through.
/// The window a screen cover is inserted after so that it lies directly above `owner`: the window
/// right above the owner in the z-order (a tool window, typically). The top of the z-order only
/// when nothing non-topmost is above the owner.
unsafe fn cover_insert_after(owner: HWND, cover: HWND) -> HWND {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindow, GetWindowLongPtrW, GWL_EXSTYLE, GW_HWNDPREV, HWND_TOP, WS_EX_TOPMOST,
    };
    match GetWindow(owner, GW_HWNDPREV) {
        Ok(above)
            if !above.is_invalid()
                && above != cover
                && GetWindowLongPtrW(above, GWL_EXSTYLE) & WS_EX_TOPMOST.0 as isize == 0 =>
        {
            above
        }
        _ => HWND_TOP,
    }
}

unsafe fn show_screen_cover(owner: HWND) -> Option<ScreenCover> {
    use windows::Win32::Graphics::Dwm::{DwmFlush, DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
    use windows::Win32::Graphics::Gdi::{
        BitBlt, ClientToScreen, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
        GdiFlush, GetDC, ReleaseDC, SelectObject, SRCCOPY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, GetClientRect, SetWindowLongPtrW, SetWindowPos, GWLP_USERDATA, HMENU,
        SWP_NOACTIVATE, SWP_SHOWWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
    };
    if !composition_enabled() || !register_screen_cover_class() {
        return None;
    }
    let mut client = windows::Win32::Foundation::RECT::default();
    if GetClientRect(owner, &mut client).is_err() {
        return None;
    }
    let (width, height) = (client.right - client.left, client.bottom - client.top);
    if width <= 0 || height <= 0 {
        return None;
    }
    let mut origin = windows::Win32::Foundation::POINT { x: 0, y: 0 };
    if !ClientToScreen(owner, &mut origin).as_bool() {
        return None;
    }
    let window_dc = GetDC(owner);
    if window_dc.is_invalid() {
        return None;
    }
    let memory = CreateCompatibleDC(window_dc);
    let bitmap = CreateCompatibleBitmap(window_dc, width, height);
    let mut captured = false;
    if !memory.is_invalid() && !bitmap.is_invalid() {
        let previous = SelectObject(memory, bitmap);
        captured = BitBlt(memory, 0, 0, width, height, window_dc, 0, 0, SRCCOPY).is_ok();
        let _ = SelectObject(memory, previous);
    }
    if !memory.is_invalid() {
        let _ = DeleteDC(memory);
    }
    let _ = ReleaseDC(owner, window_dc);
    if !captured {
        if !bitmap.is_invalid() {
            let _ = DeleteObject(bitmap);
        }
        return None;
    }
    let Ok(module) = windows::Win32::System::LibraryLoader::GetModuleHandleW(None) else {
        let _ = DeleteObject(bitmap);
        return None;
    };
    let window = match CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        SCREEN_COVER_CLASS,
        windows::core::PCWSTR::null(),
        WS_POPUP,
        origin.x,
        origin.y,
        width,
        height,
        owner,
        HMENU::default(),
        windows::Win32::Foundation::HINSTANCE(module.0),
        None,
    ) {
        Ok(window) => window,
        Err(_) => {
            let _ = DeleteObject(bitmap);
            return None;
        }
    };
    let cover = ScreenCover { window, bitmap };
    let _ = SetWindowLongPtrW(window, GWLP_USERDATA, bitmap.0 as isize);
    // No show animation and square corners: the cover must match the client area exactly.
    let disabled = BOOL(1);
    let _ = DwmSetWindowAttribute(
        window,
        DWMWINDOWATTRIBUTE(3), // DWMWA_TRANSITIONS_FORCEDISABLED
        &disabled as *const BOOL as *const core::ffi::c_void,
        std::mem::size_of::<BOOL>() as u32,
    );
    let square: u32 = 1; // DWMWCP_DONOTROUND
    let _ = DwmSetWindowAttribute(
        window,
        DWMWINDOWATTRIBUTE(33), // DWMWA_WINDOW_CORNER_PREFERENCE
        &square as *const u32 as *const core::ffi::c_void,
        std::mem::size_of::<u32>() as u32,
    );
    // Directly above its owner, never at the top of the z-order: at HWND_TOP the picture of the
    // main window covered the open tool windows (which sit above the main window) for the length
    // of the transition, so a theme switch looked like the tool window dropping behind the main
    // window and coming back.
    let insert_after = cover_insert_after(owner, window);
    if SetWindowPos(
        window,
        insert_after,
        origin.x,
        origin.y,
        width,
        height,
        SWP_NOACTIVATE | SWP_SHOWWINDOW,
    )
    .is_err()
    {
        return None;
    }
    let _ = RedrawWindow(window, None, None, RDW_INVALIDATE | RDW_UPDATENOW);
    let _ = GdiFlush();
    let _ = DwmFlush();
    Some(cover)
}

// ------------------------------------------------------------------------------------------
// UI trace: set LETRECOVERY_UI_TRACE=1 (or LR_UI_TRACE=1) to log rendering and timing lines.
// ------------------------------------------------------------------------------------------

/// Detailed UI diagnostics: set LETRECOVERY_UI_TRACE=2 (or "detail") before starting the program.
/// Every themed control, its styles and rectangles, non-client layout, dialog show/hide
/// decisions, progress bars and list frames are then written to the log with the "[UI 详细]"
/// prefix. Value 1 keeps the existing timing summaries only.
pub(crate) fn ui_detail_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        let enabled = ["LETRECOVERY_UI_TRACE", "LR_UI_TRACE"].iter().any(|name| {
            std::env::var(name)
                .map(|value| {
                    matches!(
                        value.trim().to_ascii_lowercase().as_str(),
                        "2" | "detail" | "detailed" | "verbose" | "all" | "full"
                    )
                })
                .unwrap_or(false)
        });
        if enabled {
            log::info!("[UI 详细] 已开启详细界面日志（LETRECOVERY_UI_TRACE=2）");
        }
        enabled
    })
}

pub(crate) fn ui_detail(text: impl FnOnce() -> String) {
    if ui_detail_enabled() {
        log::info!("[UI 详细] {}", text());
    }
}

thread_local! {
    static UI_DETAIL_LAST: std::cell::RefCell<std::collections::HashMap<(isize, &'static str), String>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Logs a per-window state line only when it differs from the last one logged for the same key,
/// so resizing does not flood the log with identical lines.
pub(crate) fn ui_detail_changed(hwnd: HWND, key: &'static str, text: impl FnOnce() -> String) {
    if !ui_detail_enabled() {
        return;
    }
    let text = text();
    let changed = UI_DETAIL_LAST.with(|cell| {
        let mut map = cell.borrow_mut();
        let slot = (hwnd.0 as isize, key);
        if map.get(&slot) == Some(&text) {
            return false;
        }
        if map.len() > 8192 {
            map.clear();
        }
        map.insert(slot, text.clone());
        true
    });
    if changed {
        log::info!("[UI 详细] {key} hwnd={:?}: {text}", hwnd.0);
    }
}

/// Detailed log of the visible direct children of `root` (class, rectangle in root client
/// coordinates, styles, window region), to find a window that leaves pixels on a page.
pub(crate) unsafe fn ui_detail_dump_children(root: HWND, label: &str) {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumChildWindows, GetClassNameW, GetParent, GetWindowLongPtrW, GetWindowRect, GWL_EXSTYLE,
        GWL_STYLE,
    };
    if !ui_detail_enabled() {
        return;
    }
    struct Visit {
        root: HWND,
        lines: Vec<String>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let context = &mut *(lparam.0 as *mut Visit);
        if GetParent(hwnd).ok() != Some(context.root) || !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1);
        }
        let mut class = [0u16; 64];
        let length = GetClassNameW(hwnd, &mut class).max(0) as usize;
        let mut window = RECT::default();
        let _ = GetWindowRect(hwnd, &mut window);
        let mut origin = [windows::Win32::Foundation::POINT::default(); 1];
        let _ = windows::Win32::Graphics::Gdi::ClientToScreen(context.root, &mut origin[0]);
        let mut region = RECT::default();
        let has_region = windows::Win32::Graphics::Gdi::GetWindowRgnBox(hwnd, &mut region).0 != 0;
        context.lines.push(format!(
            "  {:?} {} [{},{} {}x{}] style={:#x} exstyle={:#x} region={}",
            hwnd.0,
            String::from_utf16_lossy(&class[..length]),
            window.left - origin[0].x,
            window.top - origin[0].y,
            window.right - window.left,
            window.bottom - window.top,
            GetWindowLongPtrW(hwnd, GWL_STYLE),
            GetWindowLongPtrW(hwnd, GWL_EXSTYLE),
            if has_region {
                format!("{region:?}")
            } else {
                "none".to_owned()
            }
        ));
        BOOL(1)
    }
    let mut context = Visit {
        root,
        lines: Vec::new(),
    };
    let _ = EnumChildWindows(
        root,
        Some(visit),
        LPARAM(&mut context as *mut Visit as isize),
    );
    log::info!(
        "[UI 详细] {label}：{} 个可见子窗口\n{}",
        context.lines.len(),
        context.lines.join("\n")
    );
}

/// Inclusive time per named scope, collected only while the resize benchmark runs
/// (LETRECOVERY_UI_RESIZE_BENCH), to find what a resize step spends its time on.
pub(crate) fn profiling_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("LETRECOVERY_UI_RESIZE_BENCH").is_some())
}

thread_local! {
    static PROFILE: std::cell::RefCell<Vec<(&'static str, f64, u32)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

pub(crate) struct ProfileScope {
    name: &'static str,
    started: Option<std::time::Instant>,
}

thread_local! {
    static LAYOUT_COMMIT_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Marks the span in which a layout batch moves the child windows.
pub(crate) struct LayoutCommitMark;

pub(crate) fn mark_layout_commit() -> LayoutCommitMark {
    LAYOUT_COMMIT_DEPTH.with(|depth| depth.set(depth.get() + 1));
    LayoutCommitMark
}

impl Drop for LayoutCommitMark {
    fn drop(&mut self) {
        LAYOUT_COMMIT_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

thread_local! {
    static DEFERRED_FRAME_PAINTS: std::cell::RefCell<Vec<isize>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// True while a layout batch moves the child windows.
pub(crate) fn layout_commit_active() -> bool {
    LAYOUT_COMMIT_DEPTH.with(|depth| depth.get() > 0)
}

/// Moving a report makes USER32 (and the report's scrollbar update) paint its non-client area
/// synchronously, once per move, in the middle of the layout; the step then paints everything
/// again. While a layout commits, such a window skips that paint and is queued here instead.
pub(crate) fn defer_frame_paint(hwnd: HWND) {
    DEFERRED_FRAME_PAINTS.with(|windows| {
        let mut windows = windows.borrow_mut();
        let key = hwnd.0 as isize;
        if !windows.contains(&key) {
            windows.push(key);
        }
    });
}

/// Invalidates the frames that skipped their paint during the layout commit; the paint pass that
/// follows the layout draws each of them once.
pub(crate) unsafe fn flush_deferred_frame_paints() {
    let windows = DEFERRED_FRAME_PAINTS.with(|windows| std::mem::take(&mut *windows.borrow_mut()));
    for window in windows {
        let hwnd = HWND(window as *mut _);
        if windows::Win32::UI::WindowsAndMessaging::IsWindow(hwnd).as_bool() {
            let _ = RedrawWindow(
                hwnd,
                None,
                None,
                RDW_FRAME | RDW_INVALIDATE | windows::Win32::Graphics::Gdi::RDW_NOERASE,
            );
        }
    }
}

/// A paint scope named after whether it runs synchronously inside the layout commit (where it is
/// wasted work: the step paints everything again right afterwards) or in the normal paint pass.
pub(crate) fn paint_scope(normal: &'static str, inside_layout: &'static str) -> ProfileScope {
    let inside = LAYOUT_COMMIT_DEPTH.with(|depth| depth.get() > 0);
    profile_scope(if inside { inside_layout } else { normal })
}

pub(crate) fn profile_scope(name: &'static str) -> ProfileScope {
    ProfileScope {
        name,
        started: profiling_enabled().then(std::time::Instant::now),
    }
}

impl Drop for ProfileScope {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            PROFILE.with(|profile| {
                let mut profile = profile.borrow_mut();
                if let Some(entry) = profile.iter_mut().find(|entry| entry.0 == self.name) {
                    entry.1 += elapsed;
                    entry.2 += 1;
                } else {
                    profile.push((self.name, elapsed, 1));
                }
            });
        }
    }
}

pub(crate) fn take_profile() -> Vec<(&'static str, f64, u32)> {
    let mut entries = PROFILE.with(|profile| std::mem::take(&mut *profile.borrow_mut()));
    entries.sort_by(|a, b| b.1.total_cmp(&a.1));
    entries
}

pub(crate) fn ui_trace_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        let enabled = ["LETRECOVERY_UI_TRACE", "LR_UI_TRACE"].iter().any(|name| {
            std::env::var(name)
                .map(|value| {
                    let value = value.trim();
                    !value.is_empty()
                        && value != "0"
                        && !value.eq_ignore_ascii_case("false")
                        && !value.eq_ignore_ascii_case("off")
                })
                .unwrap_or(false)
        });
        if enabled {
            let composition = unsafe { composition_enabled() };
            log::info!(
                "[UI 渲染] 已开启界面渲染跟踪；桌面合成（DWM）：{}",
                if composition {
                    "已启用"
                } else {
                    "未启用"
                }
            );
        }
        enabled
    })
}

pub(crate) fn trace_now() -> Option<std::time::Instant> {
    ui_trace_enabled().then(std::time::Instant::now)
}

fn micros_since(start: std::time::Instant) -> u64 {
    start.elapsed().as_micros().min(u64::MAX as u128) as u64
}

fn format_ms(micros: u64) -> String {
    format!("{:.1}", micros as f64 / 1000.0)
}

static TRACE_SURFACES: AtomicU64 = AtomicU64::new(0);
static TRACE_SURFACE_MICROS: AtomicU64 = AtomicU64::new(0);
static TRACE_OWNER_DRAW: AtomicU64 = AtomicU64::new(0);
static TRACE_LIST_VIEWS: AtomicU64 = AtomicU64::new(0);
static TRACE_LIST_VIEW_MICROS: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Default)]
struct TraceCounters {
    surfaces: u64,
    surface_micros: u64,
    owner_draw: u64,
    list_views: u64,
    list_view_micros: u64,
}

fn trace_counters() -> TraceCounters {
    TraceCounters {
        surfaces: TRACE_SURFACES.load(Ordering::Relaxed),
        surface_micros: TRACE_SURFACE_MICROS.load(Ordering::Relaxed),
        owner_draw: TRACE_OWNER_DRAW.load(Ordering::Relaxed),
        list_views: TRACE_LIST_VIEWS.load(Ordering::Relaxed),
        list_view_micros: TRACE_LIST_VIEW_MICROS.load(Ordering::Relaxed),
    }
}

fn trace_surface_painted(start: Option<std::time::Instant>) {
    if let Some(start) = start {
        TRACE_SURFACES.fetch_add(1, Ordering::Relaxed);
        TRACE_SURFACE_MICROS.fetch_add(micros_since(start), Ordering::Relaxed);
    }
}

pub(crate) fn trace_list_view_paint(start: Option<std::time::Instant>) {
    if let Some(start) = start {
        let micros = micros_since(start);
        TRACE_LIST_VIEWS.fetch_add(1, Ordering::Relaxed);
        TRACE_LIST_VIEW_MICROS.fetch_add(micros, Ordering::Relaxed);
        if micros >= 16_000 {
            log::info!(
                "[UI 渲染] 单次列表绘制耗时 {} ms（超过一帧）",
                format_ms(micros)
            );
        }
    }
}

/// One logged line per transaction: total time, its phases and the paint work it caused.
pub(crate) struct TraceSpan {
    label: String,
    start: std::time::Instant,
    last: std::time::Instant,
    phases: Vec<(&'static str, u64)>,
    counters: TraceCounters,
}

impl TraceSpan {
    pub(crate) fn begin(label: &str) -> Option<Self> {
        if !ui_trace_enabled() {
            return None;
        }
        let now = std::time::Instant::now();
        Some(Self {
            label: label.to_owned(),
            start: now,
            last: now,
            phases: Vec::new(),
            counters: trace_counters(),
        })
    }

    pub(crate) fn phase(&mut self, name: &'static str) {
        let now = std::time::Instant::now();
        let micros = now
            .duration_since(self.last)
            .as_micros()
            .min(u64::MAX as u128) as u64;
        self.phases.push((name, micros));
        self.last = now;
    }
}

impl Drop for TraceSpan {
    fn drop(&mut self) {
        let total = micros_since(self.start);
        let now = trace_counters();
        let phases = self
            .phases
            .iter()
            .map(|(name, micros)| format!("{name} {} ms", format_ms(*micros)))
            .collect::<Vec<_>>()
            .join("，");
        log::info!(
            "[UI 渲染] {}：总计 {} ms（{}）；离屏绘制 {} 次共 {} ms，其中自绘控件 {} 次；列表绘制 {} 次共 {} ms",
            self.label,
            format_ms(total),
            if phases.is_empty() { "-".to_owned() } else { phases },
            now.surfaces.saturating_sub(self.counters.surfaces),
            format_ms(now.surface_micros.saturating_sub(self.counters.surface_micros)),
            now.owner_draw.saturating_sub(self.counters.owner_draw),
            now.list_views.saturating_sub(self.counters.list_views),
            format_ms(now.list_view_micros.saturating_sub(self.counters.list_view_micros)),
        );
    }
}

#[derive(Default)]
struct ResizeStats {
    steps: u64,
    layout_total: u64,
    layout_max: u64,
    paint_total: u64,
    paint_max: u64,
}

thread_local! {
    static RESIZE_STATS: std::cell::RefCell<Vec<(isize, ResizeStats)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Records one size step: layout from `start` to `layout_done`, painting from then to now.
pub(crate) fn trace_resize_step(
    hwnd: HWND,
    start: Option<std::time::Instant>,
    layout_done: Option<std::time::Instant>,
) {
    let (Some(start), Some(layout_done)) = (start, layout_done) else {
        return;
    };
    let layout = layout_done
        .duration_since(start)
        .as_micros()
        .min(u64::MAX as u128) as u64;
    let paint = micros_since(layout_done);
    let key = hwnd.0 as isize;
    RESIZE_STATS.with(|cell| {
        let mut all = cell.borrow_mut();
        let index = match all.iter().position(|(window, _)| *window == key) {
            Some(index) => index,
            None => {
                all.push((key, ResizeStats::default()));
                all.len() - 1
            }
        };
        let stats = &mut all[index].1;
        stats.steps += 1;
        stats.layout_total += layout;
        stats.layout_max = stats.layout_max.max(layout);
        stats.paint_total += paint;
        stats.paint_max = stats.paint_max.max(paint);
    });
}

/// Logs the summary of one border drag.
pub(crate) fn trace_resize_finished(hwnd: HWND, label: &str) {
    if !ui_trace_enabled() {
        return;
    }
    let key = hwnd.0 as isize;
    let stats = RESIZE_STATS.with(|cell| {
        let mut all = cell.borrow_mut();
        all.iter()
            .position(|(window, _)| *window == key)
            .map(|index| all.swap_remove(index).1)
    });
    if let Some(stats) = stats.filter(|stats| stats.steps > 0) {
        log::info!(
            "[UI 渲染] {label}拖动改大小：{} 步；排版平均 {} ms、最长 {} ms；绘制平均 {} ms、最长 {} ms",
            stats.steps,
            format_ms(stats.layout_total / stats.steps),
            format_ms(stats.layout_max),
            format_ms(stats.paint_total / stats.steps),
            format_ms(stats.paint_max),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_refresh_is_one_asynchronous_root_client_frame() {
        let flags = client_refresh_flags().0;
        assert_ne!(flags & RDW_INVALIDATE.0, 0);
        assert_ne!(flags & RDW_ERASE.0, 0);
        assert_ne!(flags & RDW_NOCHILDREN.0, 0);
        assert_eq!(flags & RDW_ALLCHILDREN.0, 0);
        assert_eq!(flags & RDW_FRAME.0, 0);
        assert_eq!(flags & RDW_UPDATENOW.0, 0);
    }
}
