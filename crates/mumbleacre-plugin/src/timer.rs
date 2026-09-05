//! A message-only window owns control ticks and removes the callback on unload.
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::WindowsAndMessaging::*,
};
const CLASS: *const u16 = windows_sys::w!("MumbleACRE.Control.1");
const TIMER_ID: usize = 1;
pub struct Timer {
    window: usize,
}
impl Timer {
    /// Must be created and dropped on Mumble's main thread.
    pub fn start() -> Result<Self, String> {
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: CLASS,
                ..std::mem::zeroed()
            };
            if RegisterClassW(&class) == 0 {
                return Err("could not register control window".into());
            }
            let window = CreateWindowExW(
                0,
                CLASS,
                CLASS,
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            if window.is_null() {
                UnregisterClassW(CLASS, instance);
                return Err("could not create control window".into());
            }
            if SetTimer(window, TIMER_ID, 20, None) == 0 {
                DestroyWindow(window);
                UnregisterClassW(CLASS, instance);
                return Err("could not create control timer".into());
            }
            Ok(Self {
                window: window as usize,
            })
        }
    }
}
impl Drop for Timer {
    fn drop(&mut self) {
        unsafe {
            let window = self.window as HWND;
            KillTimer(window, TIMER_ID);
            // No TIMERPROC function pointer is embedded in queued messages. Once
            // the window is destroyed, DispatchMessage cannot call our WNDPROC.
            DestroyWindow(window);
            UnregisterClassW(CLASS, GetModuleHandleW(std::ptr::null()));
        }
    }
}
unsafe extern "system" fn window_proc(window: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_TIMER && w == TIMER_ID {
        crate::runtime::tick();
        return 0;
    }
    unsafe { DefWindowProcW(window, msg, w, l) }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timer_is_owned_by_a_window_and_can_be_recreated() {
        let timer = Timer::start().unwrap();
        assert_ne!(unsafe { IsWindow(timer.window as HWND) }, 0);
        let window = timer.window;
        drop(timer);
        assert_eq!(unsafe { IsWindow(window as HWND) }, 0);
        drop(Timer::start().unwrap());
    }
}
