//! 8787 唤起同花顺远航版（`modules/monitor_app/services/yuanhang.py` 的 Win32 移植）。
//!
//! 仅 Windows 生效：找窗口 → 拉回主屏 → 窗口模式（max/full/restore/keep）→ 置顶 →
//! 逐位键入代码 → 回车（键盘精灵方式，不触交易）。
//! 非 Windows（WSL 构建/对表）返回与 Python 无 pywin32 时相同的 500 契约。

use serde_json::Value;

/// 返回 (status, body)；Windows 分支见下方 cfg。
pub fn open_in_yuanhang(code: &str, mode: &str) -> (u16, Vec<u8>, &'static str) {
    let mode = {
        let m = mode.trim().to_lowercase();
        if ["max", "full", "restore", "keep"].contains(&m.as_str()) {
            m
        } else {
            "max".to_string()
        }
    };
    #[cfg(windows)]
    {
        imp::open(code, &mode)
    }
    #[cfg(not(windows))]
    {
        let _ = (code, mode);
        // Python 在缺 pywin32 时抛 ImportError → FastAPI 500 text/plain
        (500, b"Internal Server Error".to_vec(), "text/plain; charset=utf-8")
    }
}

pub fn json_cell(v: &Value) -> (u16, Vec<u8>, &'static str) {
    (200, serde_json::to_vec(v).unwrap_or_else(|_| b"{}".to_vec()),
     "application/json")
}

#[cfg(windows)]
mod imp {
    type Hwnd = isize;
    type Bool = i32;
    type Uint = u32;

    const GWL_STYLE: i32 = -16;
    const WS_CAPTION: i64 = 0x00C00000;
    const WS_THICKFRAME: i64 = 0x00040000;
    const WS_MINIMIZEBOX: i64 = 0x00020000;
    const WS_MAXIMIZEBOX: i64 = 0x00010000;
    const WS_SYSMENU: i64 = 0x00080000;

    const SW_MAXIMIZE: i32 = 3;
    const SW_RESTORE: i32 = 9;
    const SWP_NOZORDER: Uint = 0x0004;
    const SWP_NOMOVE: Uint = 0x0002;
    const SWP_NOSIZE: Uint = 0x0001;
    const SWP_SHOWWINDOW: Uint = 0x0040;
    const SM_CXSCREEN: i32 = 0;
    const SM_CYSCREEN: i32 = 1;
    const KEYEVENTF_KEYUP: Uint = 0x0002;
    const VK_MENU: u8 = 0x12;
    const VK_RETURN: u8 = 0x0D;

    #[repr(C)]
    struct Rect { left: i32, top: i32, right: i32, bottom: i32 }

    #[link(name = "user32")]
    extern "system" {
        fn EnumWindows(cb: extern "system" fn(Hwnd, isize) -> Bool, lparam: isize) -> Bool;
        fn IsWindowVisible(hwnd: Hwnd) -> Bool;
        fn GetWindowTextW(hwnd: Hwnd, buf: *mut u16, max: i32) -> i32;
        fn GetWindowTextLengthW(hwnd: Hwnd) -> i32;
        fn GetWindowRect(hwnd: Hwnd, rect: *mut Rect) -> Bool;
        fn SetWindowPos(hwnd: Hwnd, after: Hwnd, x: i32, y: i32, cx: i32, cy: i32, flags: Uint) -> Bool;
        fn ShowWindow(hwnd: Hwnd, cmd: i32) -> Bool;
        fn IsIconic(hwnd: Hwnd) -> Bool;
        fn SetForegroundWindow(hwnd: Hwnd) -> Bool;
        fn GetForegroundWindow() -> Hwnd;
        fn GetWindowLongPtrW(hwnd: Hwnd, index: i32) -> isize;
        fn SetWindowLongPtrW(hwnd: Hwnd, index: i32, value: isize) -> isize;
        fn GetSystemMetrics(index: i32) -> i32;
        fn keybd_event(vk: u8, scan: u8, flags: Uint, extra: usize);
    }

    thread_local! {
        static FULLSCREEN_SAVE: std::cell::RefCell<std::collections::HashMap<Hwnd, isize>> =
            std::cell::RefCell::new(std::collections::HashMap::new());
    }

    fn sleep_ms(ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }

    extern "system" fn enum_cb(hwnd: Hwnd, lparam: isize) -> Bool {
        unsafe {
            if IsWindowVisible(hwnd) != 0 {
                let len = GetWindowTextLengthW(hwnd);
                if len > 0 {
                    let mut buf = vec![0u16; (len + 1) as usize];
                    let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), len + 1);
                    let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
                    if title.contains("同花顺远航版") {
                        let found = &mut *(lparam as *mut Vec<Hwnd>);
                        found.push(hwnd);
                    }
                }
            }
        }
        1
    }

    fn find_window() -> Option<Hwnd> {
        let mut found: Vec<Hwnd> = Vec::new();
        unsafe {
            EnumWindows(enum_cb, &mut found as *mut Vec<Hwnd> as isize);
        }
        found.first().copied()
    }

    fn get_style(hwnd: Hwnd) -> isize {
        unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) }
    }

    fn set_style(hwnd: Hwnd, style: isize) {
        unsafe {
            SetWindowLongPtrW(hwnd, GWL_STYLE, style);
        }
    }

    fn set_fullscreen(hwnd: Hwnd, on: bool) {
        let mask = (WS_CAPTION | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_SYSMENU)
            as isize;
        if on {
            let style = get_style(hwnd);
            FULLSCREEN_SAVE.with(|m| {
                m.borrow_mut().entry(hwnd).or_insert(style);
            });
            let new = style & !mask;
            if new != style {
                set_style(hwnd, new);
            }
            let (cx, cy) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
            unsafe {
                SetWindowPos(hwnd, 0, 0, 0, cx, cy, SWP_NOZORDER | SWP_SHOWWINDOW);
            }
        } else {
            let orig = FULLSCREEN_SAVE.with(|m| m.borrow_mut().remove(&hwnd));
            match orig {
                Some(style) => {
                    set_style(hwnd, style);
                    unsafe {
                        SetWindowPos(hwnd, 0, 0, 0, 0, 0,
                                     SWP_NOSIZE | SWP_NOMOVE | SWP_NOZORDER | SWP_SHOWWINDOW);
                    }
                }
                None => unsafe {
                    ShowWindow(hwnd, SW_RESTORE);
                },
            }
        }
        sleep_ms(300);
    }

    fn apply_window_mode(hwnd: Hwnd, mode: &str) {
        match mode {
            "full" => set_fullscreen(hwnd, true),
            "restore" => set_fullscreen(hwnd, false),
            "max" => {
                let saved = FULLSCREEN_SAVE.with(|m| m.borrow().contains_key(&hwnd));
                if saved {
                    set_fullscreen(hwnd, false);
                }
                unsafe {
                    ShowWindow(hwnd, SW_MAXIMIZE);
                }
                sleep_ms(300);
            }
            _ => {}
        }
    }

    fn ensure_window_visible(hwnd: Hwnd) {
        unsafe {
            let (cx, cy) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
            let mut r = Rect { left: 0, top: 0, right: 0, bottom: 0 };
            if GetWindowRect(hwnd, &mut r) == 0 {
                return;
            }
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            let margin = 80;
            let intersects = r.left + margin < cx && r.right - margin > 0
                && r.top + margin < cy && r.bottom - margin > 0;
            if !intersects {
                let new_x = ((cx - w) / 2).max(0);
                let new_y = ((cy - h) / 2).max(0);
                SetWindowPos(hwnd, 0, new_x, new_y, w, h, SWP_SHOWWINDOW);
                sleep_ms(400);
            }
        }
    }

    pub fn open(code: &str, mode: &str) -> (u16, Vec<u8>, &'static str) {
        let code = code.trim().to_string();
        let hwnd = match find_window() {
            Some(h) => h,
            None => {
                return super::json_cell(&serde_json::json!({
                    "ok": false,
                    "error": "未检测到同花顺远航版主窗口, 请先打开远航版主界面"
                }));
            }
        };
        let result: Result<serde_json::Value, String> = (|| {
            ensure_window_visible(hwnd);
            unsafe {
                if IsIconic(hwnd) != 0 {
                    ShowWindow(hwnd, SW_RESTORE);
                    sleep_ms(200);
                }
            }
            apply_window_mode(hwnd, mode);
            unsafe {
                keybd_event(VK_MENU, 0, 0, 0);
                keybd_event(VK_MENU, 0, KEYEVENTF_KEYUP, 0);
                SetForegroundWindow(hwnd);
            }
            sleep_ms(350);
            if unsafe { GetForegroundWindow() } != hwnd {
                return Ok(serde_json::json!({
                    "ok": false,
                    "error": "无法切换到远航版前台窗口, 请手动点一下远航版再试"
                }));
            }
            if code.is_empty() {
                return Ok(serde_json::json!({
                    "ok": true,
                    "msg": format!("已调整远航版窗口(mode={mode})"),
                    "mode": mode
                }));
            }
            if !(code.chars().all(|c| c.is_ascii_digit()) && code.len() == 6) {
                return Ok(serde_json::json!({"ok": false, "error": "需要6位数字股票代码"}));
            }
            for ch in code.chars() {
                let vk = 0x30 + (ch as u8 - b'0');
                unsafe {
                    keybd_event(vk, 0, 0, 0);
                }
                sleep_ms(40);
                unsafe {
                    keybd_event(vk, 0, KEYEVENTF_KEYUP, 0);
                }
                sleep_ms(40);
            }
            sleep_ms(100);
            unsafe {
                keybd_event(VK_RETURN, 0, 0, 0);
            }
            sleep_ms(50);
            unsafe {
                keybd_event(VK_RETURN, 0, KEYEVENTF_KEYUP, 0);
            }
            Ok(serde_json::json!({
                "ok": true,
                "msg": format!("已向远航版发送 {code}, 请查看窗口"),
                "code": code, "mode": mode
            }))
        })();
        match result {
            Ok(v) => super::json_cell(&v),
            Err(e) => super::json_cell(&serde_json::json!({"ok": false, "error": format!("唤起失败: {e}")})),
        }
    }
}
