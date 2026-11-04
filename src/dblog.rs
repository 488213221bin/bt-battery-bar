//! Minimal file logger for debugging (writes to %LOCALAPPDATA%\BtBatteryBar\debug.log).
//!
//! 日志在**发布版里也一直开启**（用于真机排障），因此必须有尺寸上限：长期运行
//! 的机器上设备报文/自愈事件会持续追加，无上限会无限增长。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

/// 单个日志文件的上限；超过后轮转成 `debug.log.1`（只保留最近一份历史）。
const MAX_LOG_BYTES: u64 = 1 << 20; // 1 MiB
/// 每 N 次写入检查一次尺寸，避免每次都多一次 stat。
const SIZE_CHECK_EVERY: u32 = 64;

static WRITES: AtomicU32 = AtomicU32::new(0);

pub fn path() -> PathBuf {
    let dir = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    dir.join("BtBatteryBar").join("debug.log")
}

pub fn log(msg: &str) {
    let p = path();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // 注意：这里刻意不用 `is_multiple_of()`（clippy 会建议），它需要 Rust 1.87+，
    // 而本项目声明的 MSRV 是 1.85。
    #[allow(clippy::manual_is_multiple_of)]
    let check_size = WRITES.fetch_add(1, Ordering::Relaxed) % SIZE_CHECK_EVERY == 0;
    if check_size {
        rotate_if_too_big(&p);
    }
    let mut f = match OpenOptions::new().create(true).append(true).open(p) {
        Ok(f) => f,
        Err(_) => return,
    };
    let _ = writeln!(f, "{} {}", chronoish(), msg);
}

/// 超过上限就把当前日志改名为 `debug.log.1`（覆盖上一份历史），下次写入自动新建。
fn rotate_if_too_big(p: &std::path::Path) {
    let Ok(meta) = std::fs::metadata(p) else {
        return;
    };
    if meta.len() <= MAX_LOG_BYTES {
        return;
    }
    let old = p.with_extension("log.1");
    let _ = std::fs::remove_file(&old);
    let _ = std::fs::rename(p, &old);
}

fn chronoish() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

// ── 运行期标记：区分「正常退出 / panic / 被外部杀掉」──────────────────
// 启动时写 running.lock，正常退出时删掉。下次启动若发现它还在，说明上一次
// 是非正常结束（panic abort 或被 TerminateProcess），日志里留一条证据。

fn marker_path() -> PathBuf {
    path().with_file_name("running.lock")
}

/// 启动时调用：判断上次是否非正常结束，写标记，记录 pid。
pub fn note_start() {
    let m = marker_path();
    match std::fs::read_to_string(&m) {
        Ok(prev) => log(&format!(
            "run: previous run ended WITHOUT clean exit ({})",
            prev.trim()
        )),
        Err(_) => log("run: previous run ended cleanly (or first run)"),
    }
    let _ = std::fs::write(
        &m,
        format!("pid={} started={}", std::process::id(), chronoish()),
    );
    log(&format!("run: start pid={}", std::process::id()));
}

/// 正常退出时调用：**先写日志再删标记**，删失败也不至于丢信息。
pub fn note_clean_exit(reason: &str) {
    log(&format!("run: exit (clean, reason={reason})"));
    let _ = std::fs::remove_file(marker_path());
}

/// 安装 panic hook。`panic = "abort"` 会让进程以 0xc0000409 静默终止，
/// 没有这条记录就完全查不到崩溃原因（事故：2026-11-03 23:13）。
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "?".to_string());
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "非字符串 panic 负载".to_string());
        log(&format!("PANIC {msg} @ {loc}"));
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_target_sits_next_to_the_log() {
        let p = PathBuf::from(r"C:\x\BtBatteryBar\debug.log");
        let rotated = p.with_extension("log.1");
        assert_eq!(
            rotated.file_name().and_then(|f| f.to_str()),
            Some("debug.log.1")
        );
        assert_eq!(rotated.parent(), p.parent());
    }
}
