use eframe::egui::Pos2;
#[cfg(windows)]
use raw_window_handle::HasWindowHandle;

#[derive(Default)]
pub struct WindowHost {
    gesture: Option<Gesture>,
    input_hooked: bool,
}

#[derive(Clone, Copy)]
struct Gesture {
    screen_start: (i32, i32),
    window_start: (i32, i32),
    moved: bool,
}

impl WindowHost {
    pub fn update(
        &mut self,
        frame: &eframe::Frame,
        pressed: bool,
        held: bool,
        released: bool,
        local_release: Option<Pos2>,
        start_allowed: bool,
    ) -> Option<Pos2> {
        #[cfg(windows)]
        {
            if !self.input_hooked {
                self.input_hooked = install_input_hook(frame);
            }
            if pressed && start_allowed {
                self.gesture = read_start(frame).map(|(screen_start, window_start)| Gesture {
                    screen_start,
                    window_start,
                    moved: false,
                });
            } else if pressed {
                self.gesture = None;
            }
            if held
                && let Some(gesture) = &mut self.gesture
                && let Some(screen) = cursor_position()
            {
                let dx = screen.0 - gesture.screen_start.0;
                let dy = screen.1 - gesture.screen_start.1;
                let (drag_x, drag_y) = system_drag_threshold();
                if !gesture.moved && drag_threshold_crossed(dx, dy, drag_x, drag_y) {
                    gesture.moved = true;
                }
                if gesture.moved
                    && let Some(hwnd) = hwnd(frame)
                {
                    unsafe {
                        windows_sys::Win32::UI::WindowsAndMessaging::SetWindowPos(
                            hwnd,
                            std::ptr::null_mut(),
                            gesture.window_start.0 + dx,
                            gesture.window_start.1 + dy,
                            0,
                            0,
                            windows_sys::Win32::UI::WindowsAndMessaging::SWP_NOSIZE
                                | windows_sys::Win32::UI::WindowsAndMessaging::SWP_NOZORDER
                                | windows_sys::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE,
                        );
                    }
                }
            }
            if released {
                return self
                    .gesture
                    .take()
                    .and_then(|gesture| (!gesture.moved).then_some(local_release).flatten());
            }
            if !pressed && !held && !released {
                self.gesture = None;
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (frame, held);
            if released && start_allowed {
                return local_release;
            }
        }
        None
    }
}

fn drag_threshold_crossed(dx: i32, dy: i32, threshold_x: i32, threshold_y: i32) -> bool {
    dx.unsigned_abs() >= threshold_x.max(1) as u32 || dy.unsigned_abs() >= threshold_y.max(1) as u32
}

#[cfg(windows)]
fn system_drag_threshold() -> (i32, i32) {
    let x = unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
            windows_sys::Win32::UI::WindowsAndMessaging::SM_CXDRAG,
        )
    };
    let y = unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
            windows_sys::Win32::UI::WindowsAndMessaging::SM_CYDRAG,
        )
    };
    (x.max(1), y.max(1))
}

#[cfg(windows)]
static PASTE_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(windows)]
pub fn take_paste_request() -> bool {
    PASTE_REQUESTED.swap(false, std::sync::atomic::Ordering::AcqRel)
}

#[cfg(not(windows))]
pub fn take_paste_request() -> bool {
    false
}

#[cfg(windows)]
fn install_input_hook(frame: &eframe::Frame) -> bool {
    use raw_window_handle::RawWindowHandle;
    let Ok(handle) = frame.window_handle() else {
        return false;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return false;
    };
    unsafe {
        windows_sys::Win32::UI::Shell::SetWindowSubclass(
            handle.hwnd.get() as windows_sys::Win32::Foundation::HWND,
            Some(window_subclass),
            0x414e_494d,
            0,
        ) != 0
    }
}

#[cfg(windows)]
unsafe extern "system" fn window_subclass(
    hwnd: windows_sys::Win32::Foundation::HWND,
    message: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
    _subclass_id: usize,
    _ref_data: usize,
) -> windows_sys::Win32::Foundation::LRESULT {
    if message == windows_sys::Win32::UI::WindowsAndMessaging::WM_KEYDOWN
        && wparam == b'V' as usize
        && unsafe {
            windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyState(
                windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_CONTROL as i32,
            )
        } < 0
    {
        PASTE_REQUESTED.store(true, std::sync::atomic::Ordering::Release);
    }
    if message == windows_sys::Win32::UI::WindowsAndMessaging::WM_NCDESTROY {
        unsafe {
            windows_sys::Win32::UI::Shell::RemoveWindowSubclass(
                hwnd,
                Some(window_subclass),
                0x414e_494d,
            );
        }
    }
    unsafe { windows_sys::Win32::UI::Shell::DefSubclassProc(hwnd, message, wparam, lparam) }
}

#[cfg(windows)]
fn hwnd(frame: &eframe::Frame) -> Option<windows_sys::Win32::Foundation::HWND> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = frame.window_handle().ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(handle) => {
            Some(handle.hwnd.get() as windows_sys::Win32::Foundation::HWND)
        }
        _ => None,
    }
}

#[cfg(windows)]
fn cursor_position() -> Option<(i32, i32)> {
    let mut point = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
    (unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut point) } != 0)
        .then_some((point.x, point.y))
}

#[cfg(windows)]
fn read_start(frame: &eframe::Frame) -> Option<((i32, i32), (i32, i32))> {
    let cursor = cursor_position()?;
    let hwnd = hwnd(frame)?;
    let mut rect = windows_sys::Win32::Foundation::RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    (unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect) } != 0)
        .then_some((cursor, (rect.left, rect.top)))
}

#[cfg(test)]
mod tests {
    use super::drag_threshold_crossed;

    #[test]
    fn right_drag_uses_system_threshold_on_each_physical_axis() {
        assert!(!drag_threshold_crossed(3, 2, 4, 6));
        assert!(drag_threshold_crossed(4, 0, 4, 6));
        assert!(drag_threshold_crossed(0, 6, 4, 6));
        assert!(drag_threshold_crossed(0, 1, 0, 1));
    }
}
