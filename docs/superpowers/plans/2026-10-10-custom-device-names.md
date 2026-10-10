# 自定义设备名称（Custom Device Names）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给条上每台设备设置自定义显示名（别名），别名只在任务栏迷你条与溢出面板生效，菜单/托盘提示/低电量提醒保持系统原名。

**Architecture:** 方案 A（显示层注入）——`settings.json` 新增 `custom_names`（地址小写 → 别名）；别名只在 `compute_layout()` 与 `panel_devices()` 两个显示收口点套用；入口为右键菜单 → 设备 → 「重命名设备 ▸」→ 纯代码创建的 Win32 小对话框（嵌套消息循环）。详见 `docs/superpowers/specs/2026-10-10-custom-device-names-design.md`。

**Tech Stack:** Rust edition 2024 / windows-rs 0.58（Win32 用户端 API，无 .rc 资源）/ serde_json / cargo test。

---

## 与 spec 的唯一差异（已评审）

spec §5 写的是 `rename_addrs: Mutex<Vec<String>>`（只存地址）。本计划改为
`rename_items: Mutex<Vec<(String, String)>>`（地址 + 系统原名）：菜单构建时
`rows_for_settings()` 的行**已经是正确原名**（在线设备来自快照、离线设备来自
known_devices），顺手存下来可避免 on_command 里二次查找，且与 §7「原名来源」
语义完全一致。其余与 spec 相同。

## 文件结构

| 文件 | 动作 | 职责 |
|---|---|---|
| `src/settings.rs` | 修改 | `custom_names` 字段、`custom_name`/`set_custom_name`/`apply_display_names` 方法 + 全部单测 |
| `src/app.rs` | 修改 | 条/面板显示接入（Task 3）、菜单子菜单（Task 4）、命令分发（Task 6） |
| `src/rename.rs` | **新建** | 重命名对话框：纯 UI，不访问 settings，返回 `RenameResult` |
| `src/main.rs` | 修改 | 加 `mod rename;` |
| `README.md` | 修改 | 菜单项表 + 特性列表各加一条 |

锁安全前提（Task 3/6 均依赖，已核实）：`compute_layout()` 与 `panel_devices()`
的所有调用方都不持有 `settings` 锁（`layout_and_resize` 里 `desired_placement`
的锁在 `compute_layout` 返回**之后**才取；`paint_panel` 的 `low_alert` 是
let 语句临时量，语句结束即释放，第 1901 行才调 `panel_devices`）。std Mutex
不可重入，新增加锁点必须保持「不嵌套」——Task 6 特别要求**弹窗前释放锁**。

---

### Task 1: `custom_names` 数据字段与 set/get（TDD）

**Files:**
- Modify: `src/settings.rs`（struct 字段、`Default`、`impl Settings` 方法）
- Test: `src/settings.rs` 内 `mod tests`

- [ ] **Step 1.1: 写三个失败测试**

追加到 `src/settings.rs` 的 `mod tests`（文件末尾 `}` 之前）：

```rust
    #[test]
    fn custom_name_set_get_remove_case_insensitive() {
        let mut s = Settings::default();
        assert_eq!(s.custom_name("AA:BB:CC:DD:EE:FF"), None);
        // 地址大小写不敏感（内部键存小写）
        assert!(s.set_custom_name("AA:BB:CC:DD:EE:FF", "耳机"));
        assert_eq!(s.custom_name("aa:bb:cc:dd:ee:ff"), Some("耳机"));
        // 同名再设一次 → 无变更
        assert!(!s.set_custom_name("aa:bb:cc:dd:ee:ff", "耳机"));
        // 空串 = 删除别名
        assert!(s.set_custom_name("AA:BB:CC:DD:EE:FF", ""));
        assert!(s.custom_names.is_empty());
    }

    #[test]
    fn custom_name_trims_whitespace() {
        let mut s = Settings::default();
        assert!(s.set_custom_name("hid:m", "  鼠标  "));
        assert_eq!(s.custom_name("hid:m"), Some("鼠标"));
        // trim 后为空 = 删除
        assert!(s.set_custom_name("hid:m", "   "));
        assert_eq!(s.custom_name("hid:m"), None);
    }

    #[test]
    fn custom_name_equal_to_original_removes_alias() {
        let mut s = Settings::default();
        s.known_devices.push(KnownDevice {
            address: "hid:m".into(),
            name: "MX Mouse".into(),
            kind: "Mouse".into(),
        });
        // 已有别名 → 改回原名 = 删除别名（算变更）
        assert!(s.set_custom_name("hid:m", "临时"));
        assert!(s.set_custom_name("hid:m", "MX Mouse"));
        assert_eq!(s.custom_name("hid:m"), None);
        // 本就没有别名时输入原名 → 无变更
        assert!(!s.set_custom_name("hid:m", "MX Mouse"));
        // 原名未知（不在 known_devices）→ 不比较，正常保存
        assert!(s.set_custom_name("hid:unknown", "MX Mouse"));
        assert_eq!(s.custom_name("hid:unknown"), Some("MX Mouse"));
    }
```

- [ ] **Step 1.2: 运行测试，确认按预期失败**

Run: `cargo test settings::`
Expected: **编译失败**，含类似错误：
- `error[E0560]: struct Settings has no field named custom_names`
- `error[E0599]: no method named custom_name found for struct Settings`

- [ ] **Step 1.3: 实现字段与方法**

`src/settings.rs` 顶部导入（第 3 行 `use std::path::PathBuf;` 之后加）：

```rust
use std::collections::HashMap;
```

`Settings` struct 内（`pub text_pct: u32,` 之后、第 61 行 `}` 之前）加字段：

```rust
    /// 地址（小写）→ 用户自定义显示名（别名）。与 known_devices 分开存：
    /// known_devices 有 40 台淘汰逻辑，用户手动改的名字不能跟着被淘汰。
    /// 旧 settings.json 缺此字段时由 #[serde(default)] 落到空表。
    pub custom_names: HashMap<String, String>,
```

`impl Default for Settings` 内（`text_pct: DEFAULT_TEXT_PCT,` 之后加）：

```rust
            custom_names: HashMap::new(),
```

`impl Settings` 内（`set_shown` 方法之后加）：

```rust
    /// 查询设备的自定义显示名（地址大小写不敏感，键存为小写）。
    pub fn custom_name(&self, address: &str) -> Option<&str> {
        if self.custom_names.is_empty() {
            return None;
        }
        self.custom_names
            .get(&address.to_lowercase())
            .map(|s| s.as_str())
    }

    /// 设置自定义显示名。trim 后：空串 = 删除别名；与系统原名相同 = 删除别名
    /// （原名查 known_devices，查不到则不比较）。返回是否发生变更（变更才写盘）。
    pub fn set_custom_name(&mut self, address: &str, alias: &str) -> bool {
        let key = address.to_lowercase();
        let alias = alias.trim();
        let original = self
            .known_devices
            .iter()
            .find(|k| k.address.eq_ignore_ascii_case(address))
            .map(|k| k.name.clone());
        if alias.is_empty() || original.as_deref() == Some(alias) {
            return self.custom_names.remove(&key).is_some();
        }
        let old = self.custom_names.get(&key).cloned();
        if old.as_deref() == Some(alias) {
            return false;
        }
        self.custom_names.insert(key, alias.to_string());
        true
    }
```

- [ ] **Step 1.4: 运行测试，确认通过**

Run: `cargo test settings::`
Expected: `test result: ok. 12 passed; 0 failed`（原 9 个 + 新 3 个）

- [ ] **Step 1.5: 提交**

```bash
git add src/settings.rs
git commit -m "Add custom_names setting: per-device display aliases (set/get + tests)"
```

---

### Task 2: `apply_display_names` 与显示范围保护测试（TDD）

**Files:**
- Modify: `src/settings.rs`（`apply_display_names` 方法）
- Test: `src/settings.rs` 内 `mod tests`

- [ ] **Step 2.1: 写五个失败测试**

追加到 `src/settings.rs` 的 `mod tests`：

```rust
    #[test]
    fn apply_display_names_only_rewrites_aliased_devices() {
        let mut s = Settings::default();
        let mut devs = vec![
            dev("EDIFIER W820NB", "bt:h", false, None),
            dev("MX Mouse", "hid:m", true, Some(80)),
        ];
        s.set_custom_name("bt:h", "耳机");
        s.apply_display_names(&mut devs);
        assert_eq!(devs[0].name, "耳机");
        // 没有别名的设备原样保留
        assert_eq!(devs[1].name, "MX Mouse");
    }

    #[test]
    fn apply_display_names_noop_when_empty() {
        let s = Settings::default();
        let mut devs = vec![dev("Mouse", "hid:m", true, None)];
        s.apply_display_names(&mut devs);
        assert_eq!(devs[0].name, "Mouse");
    }

    /// 关键回归：两台不同设备改成同一个别名后仍是两行 ——
    /// 去重（dedup_by_name）按真实名在数据管线分组，别名只在显示层套用。
    #[test]
    fn same_alias_does_not_merge_devices() {
        use crate::model::dedup_by_name;
        let mut s = Settings::default();
        let mut devs = vec![
            dev("MouseA", "hid:a", true, Some(50)),
            dev("MouseB", "hid:b", true, Some(60)),
        ];
        devs = dedup_by_name(devs);
        assert_eq!(devs.len(), 2);
        s.set_custom_name("hid:a", "鼠标");
        s.set_custom_name("hid:b", "鼠标");
        s.apply_display_names(&mut devs);
        assert_eq!(devs.len(), 2);
        assert!(devs.iter().all(|d| d.name == "鼠标"));
    }

    /// 菜单行（显示/隐藏列表、重命名子菜单的行数据）必须保持系统原名。
    #[test]
    fn menu_rows_keep_original_name_despite_alias() {
        let mut s = Settings::default();
        let live = [dev("EDIFIER W820NB", "bt:h", true, Some(55))];
        s.set_custom_name("bt:h", "耳机");
        let rows = s.rows_for_settings(&live);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "EDIFIER W820NB");
        assert_eq!(rows[0].label(), "EDIFIER W820NB    55%  已连接");
    }

    #[test]
    fn custom_names_serde_roundtrip_and_legacy_json() {
        let mut s = Settings::default();
        s.set_custom_name("bt:h", "耳机");
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.custom_name("BT:H"), Some("耳机"));
        // 旧 settings.json 缺 custom_names 字段 → 默认空表
        let legacy: Settings = serde_json::from_str(r#"{"poll_seconds":30}"#).unwrap();
        assert!(legacy.custom_names.is_empty());
    }
```

- [ ] **Step 2.2: 运行测试，确认按预期失败**

Run: `cargo test settings::`
Expected: **编译失败**，`error[E0599]: no method named apply_display_names found for struct Settings`

- [ ] **Step 2.3: 实现 `apply_display_names`**

`src/settings.rs` 的 `impl Settings` 内（`set_custom_name` 之后加）：

```rust
    /// 把自定义名套用到显示列表（**条与溢出面板的唯一收口点**）。
    /// 菜单、托盘悬停、低电量提醒不走这里，继续显示系统原名。
    pub fn apply_display_names(&self, devs: &mut [Device]) {
        if self.custom_names.is_empty() {
            return;
        }
        for d in devs.iter_mut() {
            if let Some(a) = self.custom_name(&d.address) {
                d.name = a.to_string();
            }
        }
    }
```

- [ ] **Step 2.4: 运行测试，确认通过**

Run: `cargo test settings::`
Expected: `test result: ok. 17 passed; 0 failed`（9 原有 + 3 + 5）

Run: `cargo test model::`
Expected: `test result: ok. 2 passed; 0 failed`（去重逻辑无回归）

- [ ] **Step 2.5: 提交**

```bash
git add src/settings.rs
git commit -m "Add Settings::apply_display_names and scope-protection tests"
```

---

### Task 3: 条与溢出面板接入别名（仅 2 处）

**Files:**
- Modify: `src/app.rs`（`compute_layout`、`panel_devices`）

- [ ] **Step 3.1: `compute_layout()` 套用别名**

`src/app.rs` 约 1450 行，把：

```rust
            let m = self.metrics();
            let dc = GetDC(HWND::default());
            let all = self.visible_devices();
```

替换为：

```rust
            let m = self.metrics();
            let dc = GetDC(HWND::default());
            let mut all = self.visible_devices();
            // 别名只在显示层生效：条与溢出面板用别名；菜单/托盘提示/低电量
            // 提醒读原始快照，保持系统原名（spec §4）。
            {
                let cfg = self.settings.lock().unwrap();
                cfg.apply_display_names(&mut all);
            }
```

（`all` 后续用于测宽与入列，改为 `mut`；锁在块尾释放，`desired_placement`
之后再取锁不会嵌套。）

- [ ] **Step 3.2: `panel_devices()` 套用别名**

`src/app.rs` 约 1694 行，把：

```rust
    fn panel_devices(&self) -> Vec<Device> {
        self.visible_devices()
    }
```

替换为：

```rust
    fn panel_devices(&self) -> Vec<Device> {
        let mut devs = self.visible_devices();
        // 与 compute_layout 同一收口：溢出面板也显示别名。
        let cfg = self.settings.lock().unwrap();
        cfg.apply_display_names(&mut devs);
        devs
    }
```

- [ ] **Step 3.3: 锁嵌套自查**

Run: `grep -n "settings.lock()" src/app.rs`
Expected: 调用点清单与以下结论一致——
- `layout_and_resize`（1369 行 `compute_layout` → 1372 行 `desired_placement` 才锁）顺序不嵌套；
- `paint_panel` 中 `let low_alert = self.settings.lock().unwrap().low_battery_alert;` 是 let 临时量，**语句结束即释放**，1901 行 `panel_devices()` 才加锁，不嵌套；
- `size_and_paint_panel` → `panel_devices()` 前无锁；
- `visible_devices()` 自带的锁在其返回时已释放，与新增块顺序不嵌套。

- [ ] **Step 3.4: 编译 + 全量测试**

Run: `cargo build`
Expected: `Finished dev [unoptimized + debuginfo] target(s)`，无 error

Run: `cargo test`
Expected: 全部 `test result: ok`，**0 failed**

- [ ] **Step 3.5: 提交**

```bash
git add src/app.rs
git commit -m "Apply custom names on strip and overflow panel only"
```

---

### Task 4: 「重命名设备 ▸」子菜单

**Files:**
- Modify: `src/app.rs`（菜单 ID 常量、`App` 字段、`App::new` 初始化、`show_menu`）

- [ ] **Step 4.1: 新增菜单 ID 常量**

`src/app.rs` 第 130 行 `const MID_DEV_BASE: usize = 3100;` 之后加：

```rust
/// 「重命名设备」子菜单条目 id 基址（避开 MID_DEV_BASE..+MAX_MENU_DEVICES=3116）。
const MID_RENAME_BASE: usize = 3300;
```

- [ ] **Step 4.2: 新增 `App` 字段并在 `new()` 初始化**

`src/app.rs` 第 205 行 `pub settings_addrs: Mutex<Vec<String>>,` 之后加：

```rust
    /// 「重命名设备」子菜单条目：(地址, 系统原名)。与 settings_addrs 同理：
    /// 菜单是同步 TrackPopupMenu，返回后 on_command 立即消费。
    rename_items: Mutex<Vec<(String, String)>>,
```

`App::new()` 中第 308 行 `settings_addrs: Mutex::new(Vec::new()),` 之后加：

```rust
            rename_items: Mutex::new(Vec::new()),
```

- [ ] **Step 4.3: `show_menu()` 插入子菜单**

`src/app.rs` `show_menu` 内，把：

```rust
            *self.settings_addrs.lock().unwrap() = addrs;
            let _ = AppendMenuW(m_dev, MF_SEPARATOR, 0, PCWSTR::null());
            append_item(m_dev, MID_BT_ADD, w!("添加 / 连接蓝牙设备"), false);
            append_item(m_dev, MID_BT_DEVICES, w!("已连接的设备"), false);
```

替换为：

```rust
            *self.settings_addrs.lock().unwrap() = addrs;
            let _ = AppendMenuW(m_dev, MF_SEPARATOR, 0, PCWSTR::null());
            // 「重命名设备」子菜单：条目显示当前生效名（有别名 → 别名（原名）），
            // 行数据（地址 + 系统原名）存入 rename_items 供 on_command 使用。
            let Ok(m_rename) = CreatePopupMenu() else { return };
            let mut rename_items: Vec<(String, String)> = Vec::new();
            if rows.is_empty() {
                append_flags(
                    m_rename,
                    MENU_ITEM_FLAGS(MF_STRING.0 | MF_GRAYED.0),
                    0,
                    "暂无设备",
                    &mut keep,
                );
            } else {
                for (i, row) in rows.iter().take(MAX_MENU_DEVICES).enumerate() {
                    let label = match cfg.custom_name(&row.address) {
                        Some(alias) => format!("{}（{}）", alias, row.name),
                        None => row.name.clone(),
                    };
                    append_flags(
                        m_rename,
                        MENU_ITEM_FLAGS(MF_STRING.0),
                        MID_RENAME_BASE + i,
                        &label,
                        &mut keep,
                    );
                    rename_items.push((row.address.clone(), row.name.clone()));
                }
            }
            *self.rename_items.lock().unwrap() = rename_items;
            append_popup(m_dev, m_rename, w!("重命名设备"));
            let _ = AppendMenuW(m_dev, MF_SEPARATOR, 0, PCWSTR::null());
            append_item(m_dev, MID_BT_ADD, w!("添加 / 连接蓝牙设备"), false);
            append_item(m_dev, MID_BT_DEVICES, w!("已连接的设备"), false);
```

（`cfg` 是 833 行克隆的 Settings，`rows`/`keep` 均已存在；`append_flags`
签名 `(HMENU, MENU_ITEM_FLAGS, usize, &str, &mut Vec<Vec<u16>>)`。）

- [ ] **Step 4.4: 编译 + 测试**

Run: `cargo build`
Expected: 无 error（此时子菜单项还没有处理分支，点选不动作，Task 6 接上）

Run: `cargo test`
Expected: 0 failed

- [ ] **Step 4.5: 提交**

```bash
git add src/app.rs
git commit -m "Add Rename device submenu to the Devices menu"
```

---

### Task 5: 重命名对话框模块 `src/rename.rs`

**Files:**
- Create: `src/rename.rs`
- Modify: `src/main.rs`（`mod rename;`）

- [ ] **Step 5.1: 注册模块**

`src/main.rs` 第 22 行 `mod model;` 之后加：

```rust
mod rename;
```

- [ ] **Step 5.2: 新建 `src/rename.rs`（完整文件）**

```rust
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
    GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, GWLP_USERDATA, HICON, IDC_ARROW,
    IDCANCEL, IDOK, IsDialogMessageW, IsWindow, LoadCursorW, MSG, MoveWindow, PostQuitMessage,
    RegisterClassExW, SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowTextW,
    ShowWindow, SPI_GETWORKAREA, SW_SHOWNORMAL, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    SystemParametersInfoW, TranslateMessage, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_DESTROY,
    WM_ERASEBKGND, WM_NCCREATE, WM_QUIT, WM_SETFONT, WM_SIZE, WS_CAPTION, WS_CHILD,
    WS_EX_CLIENTEDGE, WS_EX_CONTROLPARENT, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
    WNDCLASSEXW, WNDCLASS_STYLES, WINDOW_EX_STYLE, WINDOW_STYLE,
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
        let _ = EnableWindow(st.reset_btn, BOOL(false));
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
    let _ = MoveWindow(st.label_hwnd, s(PAD), s(PAD), inner_w, s(LABEL_H), BOOL(true));
    let _ = MoveWindow(
        st.edit,
        s(PAD),
        s(PAD + LABEL_H + LABEL_GAP),
        inner_w,
        s(EDIT_H),
        BOOL(true),
    );
    let by = ch - s(PAD) - s(BTN_H);
    let _ = MoveWindow(st.reset_btn, s(PAD), by, s(BTN_W_WIDE), s(BTN_H), BOOL(true));
    let cx = cw - s(PAD) - s(BTN_W);
    let _ = MoveWindow(st.cancel_btn, cx, by, s(BTN_W), s(BTN_H), BOOL(true));
    let ox = cx - s(BTN_GAP) - s(BTN_W);
    let _ = MoveWindow(st.ok_btn, ox, by, s(BTN_W), s(BTN_H), BOOL(true));
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
        let _ = AdjustWindowRectExForDpi(&mut rc, style, BOOL(false), ex, dpi as u32);
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
                    PostQuitMessage(msg.wparam.0 as i32);
                }
                break;
            }
            if !IsDialogMessageW(hwnd, &msg).as_bool() {
                TranslateMessage(&msg);
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
```

- [ ] **Step 5.3: 编译**

Run: `cargo build`
Expected: 无 error。若报错，对照已核实的 windows-rs 0.58 符号归属：
`ES_AUTOHSCROLL/WM_SETFONT/IDOK/IDCANCEL/DM_GETDEFID/BN_CLICKED` → `WindowsAndMessaging`；
`EM_SETSEL` → `UI::Controls`；`SetFocus/EnableWindow` → `UI::Input::KeyboardAndMouse`；
`AdjustWindowRectExForDpi` → `UI::HiDpi`；`DEFAULT_GUI_FONT/GetStockObject` → `Graphics::Gdi`。

Run: `cargo test`
Expected: 0 failed

- [ ] **Step 5.4: 提交**

```bash
git add src/rename.rs src/main.rs
git commit -m "Add rename dialog window (code-created Win32, nested message loop)"
```

---

### Task 6: 命令接线（菜单 → 对话框 → 保存 → 刷新）

**Files:**
- Modify: `src/app.rs`（`on_command`）

- [ ] **Step 6.1: 插入 `on_command` 分支**

`src/app.rs` `on_command` 中，把（`MID_DEV_BASE` 分支的**结尾** + 其后的兜底臂；
`self.mark_dirty(); }\n _ => {}` 这一组合在全文件唯一，1089 行的 `_ => {}`
缩进 12 格、2071 行的为 20 格，不会误匹配）：

```rust
                self.mark_dirty();
            }
            _ => {}
```

替换为（新分支插在设备显示/隐藏分支**之后**、兜底臂**之前**）：

```rust
            id if (MID_RENAME_BASE..MID_RENAME_BASE + MAX_MENU_DEVICES).contains(&id) => {
                let idx = id - MID_RENAME_BASE;
                let item = self.rename_items.lock().unwrap().get(idx).cloned();
                let Some((addr, original)) = item else {
                    return;
                };
                // 读取必须在弹窗前完成并释放锁：对话框跑嵌套消息循环，期间
                // 定时器会经 paint / process_dirty 再次锁 settings，
                // std Mutex 不可重入 —— 持锁弹窗必死锁。
                let current = self
                    .settings
                    .lock()
                    .unwrap()
                    .custom_name(&addr)
                    .map(|s| s.to_string());
                let owner = GetAncestor(self.hwnd(), GA_ROOT);
                let dpi = unsafe { GetDpiForWindow(owner) };
                let alias = match crate::rename::prompt(
                    owner,
                    dpi as i32,
                    &original,
                    current.as_deref(),
                ) {
                    crate::rename::RenameResult::Cancelled => return,
                    crate::rename::RenameResult::Set(name) => name,
                    crate::rename::RenameResult::Reset => String::new(),
                };
                let changed = {
                    let mut cfg = self.settings.lock().unwrap();
                    let changed = cfg.set_custom_name(&addr, &alias);
                    if changed {
                        cfg.save();
                    }
                    changed
                };
                if changed {
                    // 与改字号（MID_FONT_*）同一刷新路径：立即重算条宽，
                    // 下一 dirty tick 重绘条与溢出面板。
                    self.mark_dirty();
                    self.layout_and_resize();
                }
            }
            _ => {}
```

（`GetAncestor`/`GA_ROOT`/`GetDpiForWindow` 均已在 app.rs 导入；`mark_dirty +
layout_and_resize` 与 `MID_FONT_*` 分支完全同构。）

- [ ] **Step 6.2: 编译 + 测试**

Run: `cargo build`
Expected: 无 error

Run: `cargo test`
Expected: 0 failed

- [ ] **Step 6.3: 提交**

```bash
git add src/app.rs
git commit -m "Wire rename command: prompt, save alias, relayout strip"
```

---

### Task 7: README 文档与终验

**Files:**
- Modify: `README.md`（特性列表、菜单项表）

- [ ] **Step 7.1: README 特性列表加一条**

`README.md` 「### 🔔 贴心功能」小节，把：

```markdown
- **低电量提醒**：电量 ≤ 20% 时托盘气泡提醒（同一设备每 30 分钟最多一次）
```

替换为：

```markdown
- **低电量提醒**：电量 ≤ 20% 时托盘气泡提醒（同一设备每 30 分钟最多一次）
- **自定义设备名**：给条上任意设备起短别名（「EDIFIER W820NB 双金标版」→「耳机」），长名不再挤占任务栏；只改本程序显示，不修改系统设备名
```

- [ ] **Step 7.2: README 菜单项表加一行**

`README.md` 「## 📋 菜单项」表中，把：

```markdown
| 低电量提醒 | 电量 ≤ 20% 时托盘气泡提醒（同一设备每 30 分钟最多一次） |
```

替换为：

```markdown
| 低电量提醒 | 电量 ≤ 20% 时托盘气泡提醒（同一设备每 30 分钟最多一次） |
| 重命名设备 | 设备 → 重命名设备：给任意设备起短别名（仅条上/溢出面板生效；留空或「恢复默认」还原系统原名） |
```

- [ ] **Step 7.3: 全量回归**

Run: `cargo test`
Expected: 全部 `test result: ok`，**0 failed**（settings 模块 17 个为本功能新增）

Run: `cargo build --release`
Expected: `Finished release [optimized] target(s)`，产出 `target\release\bt-battery-bar.exe`

- [ ] **Step 7.4: 手动验证清单（demo 模式）**

> 先退出托盘里正在运行的 BtBatteryBar（右键 → 退出），否则单实例保护会直接返回。

Run: `cargo run -- demo`

1. 右键条 → 设备 → 「重命名设备 ▸」：条目显示各设备**系统原名**；
2. 点一台 → 对话框弹出、预填原名且**全文选中**；输入「耳机」回车 → 条上立即显示「耳机」，条宽随名字变化；
3. 再开菜单：该条目变为 `耳机（原名）`；**托盘悬停提示仍显示原名**；
4. 键盘：Enter = 确定，Esc = 取消，Tab 在控件间循环；无别名时「恢复默认」按钮为禁用态；
5. 输入空串回车 / 点「恢复默认」 → 条上恢复系统原名；
6. 检查 `%LOCALAPPDATA%\BtBatteryBar\settings.json`：出现 `"custom_names": { ... }`；重启程序别名仍在。

- [ ] **Step 7.5: 提交**

```bash
git add README.md
git commit -m "Document custom device names in README"
```

---

## 验收标准（对齐 spec §9）

- [ ] `cargo test` 0 failed，settings 新增 17 项测试全绿
- [ ] `cargo build --release` 成功
- [ ] 别名只出现在条与溢出面板；菜单行、托盘提示、低电量提醒保持系统原名
- [ ] 两台设备改成同名仍是两行（`same_alias_does_not_merge_devices` + 手动）
- [ ] 改名后条宽立即重算；配置持久化、重启保留
- [ ] 旧 `settings.json`（无 `custom_names` 字段）正常解析
