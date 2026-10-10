# 设计：自定义设备名称（Custom Device Names）

- 日期：2026-10-10
- 状态：已批准（待实现）
- 关联：`src/settings.rs`、`src/app.rs`、新增 `src/rename.rs`

## 1. 背景与目标

BtBatteryBar 的任务栏迷你条按系统原名显示设备，长名字（如
「EDIFIER W820NB 双金标版」）挤占任务栏空间。本功能允许用户给每台设备设置
自定义显示名（别名），核心目的：**长名改短别名，省任务栏空间、一眼可读**。

### 已确认的需求决策

| 问题 | 决策 |
|---|---|
| 核心目的 | 长名改短别名（非区分同名设备、非统一命名规范） |
| 生效范围 | **仅迷你条 + 溢出面板**；右键菜单设备列表、托盘悬停提示、低电量提醒保持系统原名 |
| 改名入口 | 右键菜单 → 设备 → 「重命名设备 ▸」子菜单 → 小输入框（含「恢复默认」） |
| 设备范围 | 条上**所有**设备（蓝牙 GATT + 2.4G/HID）均可改名 |

### 非目标

- 不改系统设备名（只改本程序显示）。
- 不做设置窗口（项目是纯菜单式 UI，无设置窗）。
- 不做批量改名、导入导出。

## 2. 方案选型

选定 **方案 A：显示层注入**。

- **A（选定）**：`settings.json` 存地址→别名映射，别名只在两个显示收口点套用
  （`compute_layout()`、`panel_devices()`）。菜单/托盘/提醒读原始快照，天然保持原名。
- B（否决）：`rebuild_snapshot()` 里统一改名——菜单/托盘/提醒也会跟着变，与
  「仅条上生效」冲突，满足需求反而要反向还原，更绕。
- C（否决）：`Device` 增加 `display_name` 字段——`Device` 在 btreader/hid/demo/测试
  有 10+ 个构造点，样板最大，收益与 A 等同。

## 3. 数据模型（`src/settings.rs`）

`Settings` 新增字段，与 `known_devices` 分开存（`known_devices` 有 40 台淘汰逻辑，
别名不能跟着被淘汰）：

```rust
/// 地址（小写）→ 用户自定义显示名。键规范化为小写（地址匹配惯例为大小写不敏感）。
pub custom_names: HashMap<String, String>,   // #[serde(default)]，旧 settings.json 自动兼容
```

方法（全部可单元测试）：

- `custom_name(&self, address: &str) -> Option<&str>` — 查询（按小写键）。
- `set_custom_name(&mut self, address: &str, alias: &str) -> bool` —
  trim 后：**空串 = 删除别名**；**等于原名 = 删除别名**（原名从 `known_devices`
  查，查不到则不比较）；返回是否发生变更（变更才写盘）。
- `apply_display_names(&self, devs: &mut [Device])` — 别名套用的**唯一收口函数**：
  逐台按地址把 `d.name` 替换为别名；`custom_names` 为空时直接返回（零开销）。

## 4. 显示集成（`src/app.rs`，仅 2 处调用）

1. `compute_layout()`：`let mut all = self.visible_devices();` 之后短暂加锁调用
   `apply_display_names(&mut all)` → 测宽、绘制（`paint` 读 `cached_layout`）、
   `+n` 溢出计数全部用别名；别名改短后条**立即收窄**（走 `layout_and_resize`）。
2. `panel_devices()`：同样套用别名 → 溢出面板测行（`measure_panel`）、绘制用别名。

不受影响（保持系统原名）的读取方：

- 右键菜单设备列表 `rows_for_settings()`（读 `live_devices()` / `known_devices`）
- 托盘悬停 `Snapshot::status_summary()`
- 低电量提醒 `queue_low_alerts()`

去重保持在数据管线（`rebuild_snapshot` → `dedup_by_name`，按**真实名**分组），
别名在显示层 → 两台设备改成同名**不会**被合并为一行。

**排序决策**：条上排序保持现状（按原名、已连接优先），改名不改变设备在条上的
位置——位置稳定，改名不会让设备跳来跳去。

## 5. 菜单入口（`src/app.rs`）

- 位置：右键菜单 → 设备 → 新增子菜单 **「重命名设备 ▸」**，在现有设备列表之后、
  「添加 / 连接蓝牙设备」之前。
- 条目文案：已改名显示 `别名（原名）`；未改名显示原名；空列表显示灰字「暂无设备」。
  边界澄清：**现有**「显示/隐藏」设备列表（`row.label()`）继续只显示原名；
  `别名（原名）` 仅用于**新增的**重命名子菜单——行数据仍来自原名，别名由
  `custom_names` 查出后拼进条目文案，不改 `DeviceRow` 本身。
- 复用 `rows_for_settings()` 的行（含离线已知设备），上限 `MAX_MENU_DEVICES = 16`。
- 新 ID 段 `MID_RENAME_BASE = 3300`（`MID_DEV_BASE = 3100` + 16 = 3116，不冲突）。
- 地址存入新字段 `rename_addrs: Mutex<Vec<String>>`（仿现有 `settings_addrs` 模式；
  菜单是同步 `TrackPopupMenu`，选中后立即消费，无悬空引用）。

## 6. 重命名对话框（新文件 `src/rename.rs`，约 200 行）

项目零资源文件、全代码建窗；独立模块避免 2920 行的 `app.rs` 继续膨胀。

```rust
pub enum RenameResult { Set(String), Reset, Cancelled }
pub fn prompt(parent: HWND, dpi: i32, original: &str, current: Option<&str>) -> RenameResult
```

内部按项目惯例 `sc(v) = v * dpi / 96` 做整数换算（与 `Metrics::sc` 同式），
非法 DPI 视为 96；不用整数 `scale`，避免 150%（144 DPI）下精度丢失。

- 窗口类 `BtBatteryBarRename` 按需注册（重复注册视为已存在）。
- 布局（96 DPI 设计值经 `v * dpi / 96` 换算）：静态文本 `原名：XXX` + 单行输入框
  （`ES_AUTOHSCROLL`，预填当前别名；无别名则预填原名并全选）+ 按钮
  `确定`（默认，`BS_DEFPUSHBUTTON`）/ `取消` / `恢复默认`（无别名时 `EnableWindow(FALSE)`）。
- 字体：`GetStockObject(DEFAULT_GUI_FONT)` + `WM_SETFONT`。
- 消息循环：嵌套 `GetMessageW`（与 `TrackPopupMenu` 同一模式），`IsDialogMessageW`
  处理 Tab/Enter/Esc；对话框过程处理 `DM_GETDEFID` 返回 `IDOK` 使 Enter 生效；
  Esc 由 IsDialogMessage 映射为 `IDCANCEL`。窗口销毁即退出循环。
- **空输入 = 恢复默认**（与「空串删除别名」语义一致）。
- 纯 UI、不访问 settings：结果由 `App::on_command` 消费，设置逻辑留在可测试的
  `settings.rs`。
- `dpi` 由调用方传入（`GetDpiForWindow(GetAncestor(strip, GA_ROOT))`，失败视为 96），
  对话框不读条的私有 `Metrics`。

## 7. 保存与刷新流程

```
on_command(3300+i)
  → rename::prompt(GetAncestor(strip, GA_ROOT), dpi, 原名, 当前别名)
  → Cancelled：直接返回
  → settings.lock() → set_custom_name() → 有变化则 save()
  → self.mark_dirty(); self.layout_and_resize();   // 与改字号（MID_FONT_*）同一刷新路径
  → 下一 dirty 周期同时重绘溢出面板（process_dirty_or_reanchor 已覆盖）
```

原名来源：`live_devices()` 快照（始终是原名）或 `known_devices`（离线设备）。

## 8. 错误处理

- `custom_names` 为空时整条路径零额外开销（早退）。
- 地址在 `custom_names` 中但设备已不存在：查询不命中，无副作用；不做自动清理
  （别名数量级很小，且用户可能临时拔掉设备）。
- settings.json 解析失败仍走现有 `unwrap_or_default()` 回退（别名丢失但不崩溃，
  与其它设置字段一致）。
- 对话框创建失败（`CreateWindowExW`/`RegisterClassExW` 失败）：`prompt` 返回
  `Cancelled`，无副作用。
- 改名期间扫描线程可能在 `publish_*`：settings 用现有 `Mutex`，别名套用在显示层
  加锁读取，与现有 `filter_for_strip` 同等待遇，无新竞态。

## 9. 测试

`settings.rs` 单元测试（新增）：

1. `set_custom_name` 往返：设置 → 查询命中 → 重设 → 改回空删除。
2. 大小写不敏感：按 `AA:BB:...` 存、按 `aa:bb:...` 查命中（键小写化）。
3. trim：前后空白剥离；全空白 = 删除。
4. 等于原名 = 删除（原名在 `known_devices` 中时）。
5. serde 往返 + 旧 JSON（缺 `custom_names` 字段）解析为默认空。
6. `apply_display_names`：只改有别名的设备、其余原样；空映射零改动。
7. 去重不被别名影响：两台不同地址设备改同名后 `dedup_by_name`（先）+
   `apply_display_names`（后）仍为两行。
8. `rows_for_settings` 仍显示原名（菜单不受别名影响）。

验证：`cargo test`、`cargo build --release`；手动验证对话框交互（Enter/Esc/恢复
默认/空输入/条立即收窄/面板同步）。

## 10. 文档

- README：「📋 菜单项」表加「重命名设备」一行；「🔔 贴心功能」加一条自定义名称。
- 不改 STORE-LISTING / 版本号（是否随版本发布另行决定）。

## 11. 已知取舍

- 对话框为纯 Win32 UI，无法单元测试，靠手动验证；设置逻辑 100% 在 `settings.rs` 可测。
- 手改 settings.json 写入非法 `custom_names`（如值为非字符串）会导致整个
  settings 回退默认——与现有所有字段相同的既有行为，不额外加固。
