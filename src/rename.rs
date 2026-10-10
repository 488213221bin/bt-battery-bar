//! 重命名对话框：给单台设备设置自定义显示名（别名）。
//!
//! 项目没有 .rc 资源文件，窗口与控件全部在代码里创建；交互靠嵌套
//! `GetMessageW` 消息循环（与 `TrackPopupMenu` 同一模式），不用 DialogBox。
//!
//! 纯 UI 模块：不读写 settings，结果交回 `App::on_command` 处理。
#![allow(unsafe_op_in_unsafe_fn)]

use windows::Win32::Foundation::{COLORREF, BOOL, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DEFAULT_GUI_FONT, DeleteObject, FillRect, GetStockObject, HBRUSH, HDC,
    HGDIOBJ,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::EM_SETSEL;
use windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CREATESTRUCTW, CreateWindowExW, DefWindowProcW,
    DestroyWindow, DispatchMessageW, DM_GETDEFID, ES_AUTOHSCROLL, GetClientRect, GetMessageW,
    GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, GWLP_USERDATA, HICON, HMENU,
    IDC_ARROW, IDCANCEL, IDOK, IsDialogMessageW, IsWindow, LoadCursorW, MSG, MoveWindow,
    PostQuitMessage, RegisterClassExW, SendMessageW, SetForegroundWindow, SetWindowLongPtrW,
    SetWindowTextW, ShowWindow, SPI_GETWORKAREA, SW_SHOWNORMAL,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW, TranslateMessage, WM_CLOSE,
    WM_COMMAND, WM_CREATE, WM_DESTROY, WM_ERASEBKGND, WM_NCCREATE, WM_QUIT, WM_SETFONT, WM_SIZE,
    WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE, WS_EX_CONTROLPARENT, WS_OVERLAPPED, WS_SYSMENU,
    WS_TABSTOP, WS_VISIBLE, WNDCLASSEXW, WNDCLASS_STYLES, WINDOW_EX_STYLE, WINDOW_STYLE,
};
use windows::core::{PCWSTR, w};

/// 自定义按钮 ID（IDOK=1 / IDCANCEL=2 之外）。
const ID_RESET: u32 = 1001;
/// IsDialogMessage 应答默认按钮时返回的 DC_HASDEFID 标志（HIWORD）。
const DC_HASDEFID: u32 = 3;

/// 96 DPI 设计值（与 Metrics::sc 同一整数换算 (v*dpi+48)/96）。
const CLIENT_W: i32 = 380;
const CLIENT_H: i32 = 112;
const PAD: i32 = 12;
const LABEL_H: i32 = 18;
const LABEL_GAP: i32 = 8;
const EDIT_H: i32 = 24;
const BTN_H: i32 = 26;
const BTN_W: i32 = 76;
const BTN_W_WIDE: i32 = 96; // 恢复默认
const BTN_GAP: i32 = 8;
/// 标签里系统原名最多显示的字符数（超出加省略号）。
const MAX_ORIG_CHARS: usize = 60;

/// 对话框结果。
#[derive(Debug)]
pub enum RenameResult {
    /// 用户输入了新名字（已 trim、非空）。
    Set(String),
    /// 空输入或「恢复默认」：清除别名。
    Reset,
    /// 取消 / Esc / 关窗。
    Cancelled,
}

/// 对话框状态：经 GWLP_USERDATA 挂在窗口上，生命周期不超过 `prompt` 调用。
struct State {
    original: String,
    current: Option<String>,
    dpi: i32,
    label_hwnd: HWND,
    edit: HWND,
    ok_btn: HWND,
    cancel_btn: HWND,
    reset_btn: HWND,
    result: RenameResult,
}

impl State {
    fn sc(&self, v: i32) -> i32 {
        (v * self.dpi + 48) / 96
    }
}

fn ellipsis(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

unsafe fn state_of(hwnd: HWND) -> *mut State {
    GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State
}

fn window_text(hwnd: HWND) -> String {
    unsafe {
        let n = GetWindowTextLengthW(hwnd);
        let mut buf = vec![0u16; n.max(0) as usize + 1];
        let got = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..got.max(0) as usize])
    }
}

fn set_font(child: HWND, font: HGDIOBJ) {
    unsafe {
        let _ = SendMessageW(child, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
    }
}

/// 创建五个子控件并预填、禁用状态、排版一次。
unsafe fn create_children(hwnd: HWND, st: &mut State) {
    let font = GetStockObject(DEFAULT_GUI_FONT);
    let hinst = HINSTANCE(GetModuleHandleW(PCWSTR::null()).unwrap().0);
    let label: Vec<u16> = format!("原名：{}", ellipsis(&st.original, MAX_ORIG_CHARS))
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    st.label_hwnd = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("STATIC"),
        PCWSTR(label.as_ptr()),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0),
        0,
        0,
        0,
        0,
        hwnd,
        HMENU::default(),
        hinst,
        None,
    )
    .unwrap_or_default();

    st.edit = CreateWindowExW(
        WS_EX_CLIENTEDGE,
        w!("EDIT"),
        PCWSTR::null(),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | ES_AUTOHSCROLL as u32),
        0,
        0,
        0,
        0,
        hwnd,
        HMENU::default(),
        hinst,
        None,
    )
    .unwrap_or_default();

    st.ok_btn = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("BUTTON"),
        w!("确定"),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32),
        0,
        0,
        0,
        0,
        hwnd,
        HMENU(IDOK.0 as usize as _),
        hinst,
        None,
    )
    .unwrap_or_default();

    st.cancel_btn = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("BUTTON"),
        w!("取消"),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_PUSHBUTTON as u32),
        0,
        0,
        0,
        0,
        hwnd,
        HMENU(IDCANCEL.0 as usize as _),
        hinst,
        None,
    )
    .unwrap_or_default();

    st.reset_btn = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("BUTTON"),
        w!("恢复默认"),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_PUSHBUTTON as u32),
        0,
        0,
        0,
        0,
        hwnd,
        HMENU(ID_RESET as usize as _),
        hinst,
        None,
    )
    .unwrap_or_default();

    for child in [st.label_hwnd, st.edit, st.ok_btn, st.cancel_btn, st.reset_btn] {
        if !child.0.is_null() {
            set_font(child, font);
        }
    }

    // 预填：有别名填别名，否则填原名；「恢复默认」仅在已有别名时可用。
    let prefill = st.current.clone().unwrap_or_else(|| st.original.clone());
    let pf: Vec<u16> = prefill.encode_utf16().chain(std::iter::once(0)).collect();
    let _ = SetWindowTextW(st.edit, PCWSTR(pf.as_ptr()));
    if st.current.is_none() {
        let _ = EnableWindow(st.reset_btn, BOOL(0));
    }

    layout_children(hwnd);
}

/// 按当前客户区尺寸与 DPI 排布子控件（WM_CREATE / WM_SIZE 调用）。
unsafe fn layout_children(hwnd: HWND) {
    let st = state_of(hwnd);
    if st.is_null() {
        return;
    }
    let st = &*st;
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let cw = rc.right - rc.left;
    let ch = rc.bottom - rc.top;
    if cw <= 0 || ch <= 0 {
        return;
    }
    let s = |v: i32| st.sc(v);
    let inner_w = (cw - s(PAD) * 2).max(0);
    let _ = MoveWindow(st.label_hwnd, s(PAD), s(PAD), inner_w, s(LABEL_H), BOOL(1));
    let _ = MoveWindow(
        st.edit,
        s(PAD),
        s(PAD + LABEL_H + LABEL_GAP),
        inner_w,
        s(EDIT_H),
        BOOL(1),
    );
    let by = ch - s(PAD) - s(BTN_H);
    let _ = MoveWindow(st.reset_btn, s(PAD), by, s(BTN_W_WIDE), s(BTN_H), BOOL(1));
    let cx = cw - s(PAD) - s(BTN_W);
    let _ = MoveWindow(st.cancel_btn, cx, by, s(BTN_W), s(BTN_H), BOOL(1));
    let ox = cx - s(BTN_GAP) - s(BTN_W);
    let _ = MoveWindow(st.ok_btn, ox, by, s(BTN_W), s(BTN_H), BOOL(1));
}

unsafe extern "system" fn dlg_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            // 继续创建（DefWindowProc 对 WM_NCCREATE 返回 TRUE）。
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_CREATE => {
            let st = state_of(hwnd);
            if st.is_null() {
                return LRESULT(-1); // 中止窗口创建
            }
            create_children(hwnd, &mut *st);
            LRESULT(0)
        }
        WM_ERASEBKGND => {
            // 系统画刷为空（hbrBackground = NULL），自己填浅灰底。
            let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let br = CreateSolidBrush(COLORREF(0x00F2_F2F2));
            let _ = FillRect(hdc, &rc, br);
            let _ = DeleteObject(HGDIOBJ(br.0));
            LRESULT(1) // 已擦除，别让 DefWindowProc 再擦
        }
        WM_SIZE => {
            layout_children(hwnd);
            LRESULT(0)
        }
        WM_COMMAND => {
            let code = ((wparam.0 as u32) >> 16) & 0xFFFF;
            let id = (wparam.0 as u32) & 0xFFFF;
            if code == BN_CLICKED {
                let st = state_of(hwnd);
                if !st.is_null() {
                    match id {
                        x if x == IDOK.0 as u32 => {
                            let text = window_text((*st).edit);
                            (*st).result = if text.trim().is_empty() {
                                RenameResult::Reset // 空输入 = 恢复默认
                            } else {
                                RenameResult::Set(text.trim().to_string())
                            };
                            let _ = DestroyWindow(hwnd);
                        }
                        x if x == IDCANCEL.0 as u32 => {
                            (*st).result = RenameResult::Cancelled;
                            let _ = DestroyWindow(hwnd);
                        }
                        ID_RESET => {
                            (*st).result = RenameResult::Reset;
                            let _ = DestroyWindow(hwnd);
                        }
                        _ => {}
                    }
                }
            }
            LRESULT(0)
        }
        // IsDialogMessage 问默认按钮：应答 LOWORD=IDOK, HIWORD=DC_HASDEFID。
        // windows-rs 元数据 DM_GETDEFID=0x0400，传统头文件为 0x0402，两者都答，
        // Enter 在任一定义下都能触发「确定」。
        m if m == DM_GETDEFID || m == 0x0402 => {
            LRESULT(((DC_HASDEFID << 16) | IDOK.0 as u32) as isize)
        }
        WM_CLOSE => {
            // 关窗 = 取消（result 保持 Cancelled 初值）。
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 弹出重命名对话框并阻塞到关闭。`parent` 为拥有者窗口（条取
/// `GetAncestor(GA_ROOT)`），`dpi` 决定缩放（≤0 视为 96）。
pub fn prompt(parent: HWND, dpi: i32, original: &str, current: Option<&str>) -> RenameResult {
    let dpi = if dpi <= 0 { 96 } else { dpi };
    unsafe {
        let hinst = HINSTANCE(GetModuleHandleW(PCWSTR::null()).unwrap().0);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(dlg_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: HICON::default(),
            hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH::default(),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: w!("BtBatteryBarRename"),
            hIconSm: HICON::default(),
        };
        let _ = RegisterClassExW(&wc); // 重复注册（ERROR_CLASS_ALREADY_EXISTS）忽略

        let mut state = State {
            original: original.to_string(),
            current: current.map(|s| s.to_string()),
            dpi,
            label_hwnd: HWND::default(),
            edit: HWND::default(),
            ok_btn: HWND::default(),
            cancel_btn: HWND::default(),
            reset_btn: HWND::default(),
            result: RenameResult::Cancelled,
        };

        let style = WINDOW_STYLE(WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0);
        let ex = WINDOW_EX_STYLE(WS_EX_CONTROLPARENT.0);
        // 期望客户区 → 换算外框尺寸。
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: (CLIENT_W * dpi + 48) / 96,
            bottom: (CLIENT_H * dpi + 48) / 96,
        };
        let _ = AdjustWindowRectExForDpi(&mut rc, style, BOOL(0), ex, dpi as u32);
        let win_w = rc.right - rc.left;
        let win_h = rc.bottom - rc.top;

        // 屏幕工作区水平居中、垂直偏上三分之一。
        let mut wa = RECT::default();
        let _ = SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut wa as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
        let x = wa.left + ((wa.right - wa.left) - win_w) / 2;
        let y = wa.top + ((wa.bottom - wa.top) - win_h) / 3;

        let state_ptr: *mut State = &mut state;
        let hwnd = match CreateWindowExW(
            ex,
            w!("BtBatteryBarRename"),
            w!("重命名设备"),
            style,
            x,
            y,
            win_w,
            win_h,
            parent,
            HMENU::default(),
            hinst,
            Some(state_ptr as *const core::ffi::c_void),
        ) {
            Ok(h) => h,
            Err(_) => return RenameResult::Cancelled,
        };

        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus((*state_ptr).edit);
        // 全文选中：直接键入即可覆盖。
        let _ = SendMessageW((*state_ptr).edit, EM_SETSEL, WPARAM(0), LPARAM(-1));

        let mut msg = MSG::default();
        loop {
            let r = GetMessageW(&mut msg, HWND::default(), 0, 0).0;
            if r <= 0 {
                // WM_QUIT 被本循环消费掉了：转发回外层消息循环。
                if r == 0 && msg.message == WM_QUIT {
                    PostQuitMessage(msg.wParam.0 as i32);
                }
                break;
            }
            if !IsDialogMessageW(hwnd, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if !IsWindow(hwnd).as_bool() {
                break;
            }
        }
        if IsWindow(hwnd).as_bool() {
            let _ = DestroyWindow(hwnd);
        }
        // 窗口已销毁：GWLP_USERDATA 链上无人再用 state_ptr，直接读绑定返回。
        state.result
    }
}
