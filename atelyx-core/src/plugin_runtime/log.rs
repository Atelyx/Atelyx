//! 插件日志：每插件一个文件，写插件 stderr、协议违规与生命周期失败。
//! 正式构建是 GUI 子系统，插件 stderr 没有控制台可去，这里是插件输出唯一的可查落点。

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 单个日志文件的体积上限；写满即轮转一份 `.1`（只留一代，避免无限增长）。
const MAX_BYTES: u64 = 2 * 1024 * 1024;
/// 详情页读取的尾部上限。
pub(crate) const TAIL_BYTES: u64 = 64 * 1024;
/// 单行展示上限：插件可能输出超长单行（整段 JSON、堆栈），截断避免撑爆界面。
const MAX_LINE_CHARS: usize = 400;

/// 一个插件的日志写入器。stderr 读线程与状态机线程共用，写入按行串行。
pub(crate) struct LogSink {
    path: PathBuf,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// 已打开的写端；`None` = 尚未打开或上次写失败待重试。
    file: Option<File>,
    /// 当前文件已写字节数（打开时按磁盘大小起算）。
    written: u64,
    /// 写失败只提示一次：日志写不进去不能反过来刷屏宿主 stderr。
    reported: bool,
}

impl LogSink {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path, state: Mutex::new(State::default()) }
    }

    /// 关闭写端。卸载前必须调用：Windows 上打开着的文件会让整目录删除失败。
    pub(crate) fn close(&self) {
        self.state.lock().unwrap().file = None;
    }

    /// 追加一行（自动补本地时间戳与换行）。尾部空白由写端归一，插件原样输出里的换行按行拆开。
    pub(crate) fn line(&self, text: &str) {
        let text = text.trim_end();
        let line = format!("{} {text}\n", stamp(now_ms()));
        let mut state = self.state.lock().unwrap();
        if state.written + line.len() as u64 > MAX_BYTES {
            rotate(&self.path);
            *state = State::default();
        }
        if state.file.is_none() {
            state.file = open(&self.path);
            state.written = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        }
        let Some(file) = state.file.as_mut() else {
            report(&mut state, &self.path, "打开失败");
            return;
        };
        if let Err(e) = file.write_all(line.as_bytes()) {
            state.file = None;
            report(&mut state, &self.path, &format!("写入失败：{e}"));
            return;
        }
        state.written += line.len() as u64;
    }
}

fn report(state: &mut State, path: &Path, what: &str) {
    if state.reported {
        return;
    }
    state.reported = true;
    eprintln!("[plugin-log] {} {what}", path.display());
}

fn open(path: &Path) -> Option<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    OpenOptions::new().create(true).append(true).open(path).ok()
}

/// 轮转：当前文件改名 `.1`（覆盖上一代），失败也继续写新文件——日志写不进去不影响插件运行。
fn rotate(path: &Path) {
    let backup = path.with_file_name(format!("{}.1", super::layout::LOG_FILE));
    let _ = std::fs::remove_file(&backup);
    let _ = std::fs::rename(path, &backup);
}

/// 读日志末尾（最多 [`TAIL_BYTES`]，从完整行起算，超长单行截断）。无日志 = 空串。
pub(crate) fn tail(path: &Path) -> String {
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(TAIL_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut raw = Vec::new();
    if file.read_to_end(&mut raw).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&raw);
    // 从中间截取时首行可能是半行：丢掉它（该行上半截已不可读，展示半行只会误导）
    let text = if start == 0 {
        text.as_ref()
    } else {
        text.split_once('\n').map(|(_, rest)| rest).unwrap_or_default()
    };
    text.lines()
        .map(truncate_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate_line(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_CHARS {
        return line.to_string();
    }
    let mut out: String = line.chars().take(MAX_LINE_CHARS).collect();
    out.push('…');
    out
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 本地时间戳 `MM-DD HH:MM:SS`（与历史面板同一时区口径）。
fn stamp(now_ms: i64) -> String {
    let shifted = now_ms + crate::history::local_offset_ms();
    let secs = shifted.rem_euclid(86_400_000) / 1000;
    let (_, month, day) = crate::frontmatter::civil_from_days(shifted.div_euclid(86_400_000));
    format!(
        "{month:02}-{day:02} {:02}:{:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::test_support::TempDir;

    #[test]
    fn writes_lines_with_stamp_and_reads_tail() {
        let tmp = TempDir::new("plugin-log");
        let path = tmp.join("logs").join("runtime.log");
        let sink = LogSink::new(path.clone());
        sink.line("[stderr] 第一行\n");
        sink.line("第二行");
        let text = tail(&path);
        assert!(text.contains("[stderr] 第一行"), "{text}");
        assert!(text.contains("第二行"), "{text}");
        // 空行不写、无日志文件读取为空串
        assert_eq!(tail(&tmp.join("missing.log")), "");
    }

    #[test]
    fn rotates_when_full_and_keeps_one_generation() {
        let tmp = TempDir::new("plugin-log-rotate");
        let path = tmp.join("runtime.log");
        let sink = LogSink::new(path.clone());
        let chunk = "x".repeat(MAX_LINE_CHARS);
        // 写满一轮：轮转后旧内容进 .1，当前文件只剩最近一段
        for _ in 0..((MAX_BYTES / 410) + 10) {
            sink.line(&chunk);
        }
        let backup = path.with_file_name("runtime.log.1");
        assert!(backup.is_file(), "写满后应轮转出 .1");
        assert!(std::fs::metadata(&path).unwrap().len() < MAX_BYTES);
    }

    #[test]
    fn tail_truncates_overlong_lines() {
        let tmp = TempDir::new("plugin-log-long");
        let path = tmp.join("runtime.log");
        let sink = LogSink::new(path.clone());
        sink.line(&"y".repeat(MAX_LINE_CHARS + 100));
        let text = tail(&path);
        assert!(text.ends_with('…'), "超长行应截断：{text}");
        // 时间戳前缀计入行宽：整行（含省略号）等于展示上限 + 1
        assert_eq!(text.chars().count(), MAX_LINE_CHARS + 1);
    }
}
