//! Selection colours of the native controls (Edit text selection in particular).
//!
//! The Edit control paints selected text with the system highlight colours (GetSysColor
//! COLOR_HIGHLIGHT / COLOR_HIGHLIGHTTEXT); there is no message to change them per control, and
//! SetSysColors would change them for every program. Instead, the import table of this process's
//! comctl32 is pointed at two small functions that answer those two indexes with LetRecovery's
//! selection colours (the same as selected list rows) and forward every other index unchanged.
//! Only this process and only the controls implemented in comctl32 are affected.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use windows::core::w;
use windows::Win32::Foundation::COLORREF;
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, DeleteObject, HBRUSH};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;

const COLOR_HIGHLIGHT: i32 = 13;
const COLOR_HIGHLIGHTTEXT: i32 = 14;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static SELECTION_BACKGROUND: AtomicU32 = AtomicU32::new(0x00ff_c24c);
static SELECTION_TEXT: AtomicU32 = AtomicU32::new(0);
static SELECTION_BRUSH: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_GET_SYS_COLOR: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_GET_SYS_COLOR_BRUSH: AtomicUsize = AtomicUsize::new(0);

#[link(name = "kernel32")]
extern "system" {
    fn VirtualProtect(
        address: *mut core::ffi::c_void,
        size: usize,
        protect: u32,
        old: *mut u32,
    ) -> i32;
}

#[link(name = "user32")]
extern "system" {
    fn GetSysColor(index: i32) -> u32;
    fn GetSysColorBrush(index: i32) -> isize;
}

unsafe extern "system" fn hooked_get_sys_color(index: i32) -> u32 {
    if ACTIVE.load(Ordering::Relaxed) {
        match index {
            COLOR_HIGHLIGHT => return SELECTION_BACKGROUND.load(Ordering::Relaxed),
            COLOR_HIGHLIGHTTEXT => return SELECTION_TEXT.load(Ordering::Relaxed),
            _ => {}
        }
    }
    let original = ORIGINAL_GET_SYS_COLOR.load(Ordering::Relaxed);
    if original == 0 {
        return GetSysColor(index);
    }
    let function: unsafe extern "system" fn(i32) -> u32 = std::mem::transmute(original);
    function(index)
}

unsafe extern "system" fn hooked_get_sys_color_brush(index: i32) -> isize {
    if ACTIVE.load(Ordering::Relaxed) && index == COLOR_HIGHLIGHT {
        let brush = SELECTION_BRUSH.load(Ordering::Relaxed);
        if brush != 0 {
            return brush as isize;
        }
    }
    let original = ORIGINAL_GET_SYS_COLOR_BRUSH.load(Ordering::Relaxed);
    if original == 0 {
        return GetSysColorBrush(index);
    }
    let function: unsafe extern "system" fn(i32) -> isize = std::mem::transmute(original);
    function(index)
}

/// Sets the selection colours (background, text) used by the native controls.
pub(crate) fn set_selection_colors(background: COLORREF, text: COLORREF) {
    SELECTION_BACKGROUND.store(background.0, Ordering::Relaxed);
    SELECTION_TEXT.store(text.0, Ordering::Relaxed);
    unsafe {
        let brush = CreateSolidBrush(background);
        let previous = SELECTION_BRUSH.swap(brush.0 as usize, Ordering::Relaxed);
        if previous != 0 {
            // Controls only use the brush while painting on this thread; the old one is no longer
            // handed out once swapped.
            let _ = DeleteObject(HBRUSH(previous as *mut _));
        }
    }
}

unsafe fn read<T: Copy>(base: *const u8, offset: usize) -> T {
    std::ptr::read_unaligned(base.add(offset) as *const T)
}

/// Points comctl32's imports of GetSysColor/GetSysColorBrush at the functions above. Safe to call
/// more than once; does nothing when comctl32 is not loaded or its headers are not as expected.
pub(crate) unsafe fn install() {
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let Ok(module) = GetModuleHandleW(w!("comctl32.dll")) else {
        INSTALLED.store(false, Ordering::SeqCst);
        return;
    };
    let base = module.0 as *const u8;
    if base.is_null() || read::<u16>(base, 0) != 0x5a4d {
        return;
    }
    let nt = read::<i32>(base, 0x3c) as usize;
    if read::<u32>(base, nt) != 0x0000_4550 || read::<u16>(base, nt + 24) != 0x20b {
        return; // not a PE32+ image
    }
    // Optional header (PE32+) starts at nt + 24; its data directories start 112 bytes in.
    let import_directory = nt + 24 + 112 + 8;
    let import_rva = read::<u32>(base, import_directory) as usize;
    if import_rva == 0 {
        return;
    }
    let mut descriptor = import_rva;
    let mut patched = 0;
    loop {
        let original_first_thunk = read::<u32>(base, descriptor) as usize;
        let name = read::<u32>(base, descriptor + 12) as usize;
        let first_thunk = read::<u32>(base, descriptor + 16) as usize;
        if name == 0 && first_thunk == 0 {
            break;
        }
        let names = if original_first_thunk != 0 {
            original_first_thunk
        } else {
            first_thunk
        };
        let mut index = 0usize;
        loop {
            let thunk = read::<u64>(base, names + index * 8);
            if thunk == 0 {
                break;
            }
            if thunk & (1u64 << 63) == 0 {
                let name_offset = (thunk & 0x7fff_ffff) as usize + 2;
                let mut bytes = Vec::with_capacity(24);
                for position in 0..40 {
                    let byte = read::<u8>(base, name_offset + position);
                    if byte == 0 {
                        break;
                    }
                    bytes.push(byte);
                }
                let replacement = match bytes.as_slice() {
                    b"GetSysColor" => {
                        Some((hooked_get_sys_color as usize, &ORIGINAL_GET_SYS_COLOR))
                    }
                    b"GetSysColorBrush" => Some((
                        hooked_get_sys_color_brush as usize,
                        &ORIGINAL_GET_SYS_COLOR_BRUSH,
                    )),
                    _ => None,
                };
                if let Some((function, original)) = replacement {
                    let slot = base.add(first_thunk + index * 8) as *mut usize;
                    let mut old = 0u32;
                    if VirtualProtect(slot.cast(), 8, 0x04, &mut old) != 0 {
                        let current = std::ptr::read_volatile(slot);
                        if current != function {
                            original.store(current, Ordering::SeqCst);
                            std::ptr::write_volatile(slot, function);
                            patched += 1;
                        }
                        let mut ignored = 0u32;
                        let _ = VirtualProtect(slot.cast(), 8, old, &mut ignored);
                    }
                }
            }
            index += 1;
        }
        descriptor += 20;
    }
    ACTIVE.store(patched > 0, Ordering::SeqCst);
    log::info!("[UI] 选中颜色：已替换 comctl32 的 {patched} 个系统颜色入口");
}
