//! 应用内更新：流式下载（进度/取消/断点续传/摘要校验）与拉起安装；只实现 Windows，安卓桥在 `commands/mobile.rs`。
//! 出网走公网策略（同 `commands/web.rs` 的 `fetch_web`）；安装命令只接受下载目录内的路径。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};

use crate::net_guard::{ensure_public_http_url, public_dns_resolver, redirect_policy};

/// 建立连接的超时：站点只连不答时尽早失败。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// 单次读的超时：给弱网余量，同时让停滞连接不会无限挂住（取消也因此能及时生效）。
/// 不能用整包超时——安装包动辄数十 MB，会误杀正常下载。
const READ_TIMEOUT: Duration = Duration::from_secs(60);
/// 进度推送间隔（前端据此更新，不必自行节流）。
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// 下载进度事件（与前端 `services/updater/index.ts` 的 `DownloadEvent` 逐字对齐）。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum DownloadEvent {
    /// 已拿到响应并定下续传起点。
    Started {
        total: Option<u64>,
        #[serde(rename = "resumingFrom")]
        resuming_from: u64,
    },
    /// 累计已写盘字节。
    Progress { received: u64, total: Option<u64> },
    /// 下载完成，正在校验摘要。
    Verifying,
}

/// 事件出口：命令侧包一层 Tauri `Channel`，测试侧收进内存。
type EventSink = Arc<dyn Fn(DownloadEvent) + Send + Sync + 'static>;

/// 在途下载的取消句柄与世代号（取消命令要够到在途任务，故托管在应用级）。
#[derive(Default)]
pub struct UpdateDownloadState {
    active: Mutex<Option<ActiveDownload>>,
    seq: AtomicU64,
}

struct ActiveDownload {
    id: u64,
    cancel: Arc<AtomicBool>,
}

impl UpdateDownloadState {
    /// 登记一次下载；已有在途任务即拒绝（两个任务会往同一个 `.part` 上追加）。
    fn begin(&self) -> Result<(u64, Arc<AtomicBool>), String> {
        let mut slot = self.active.lock().unwrap();
        if slot.is_some() {
            return Err("已有更新包正在下载".to_string());
        }
        let id = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let cancel = Arc::new(AtomicBool::new(false));
        *slot = Some(ActiveDownload {
            id,
            cancel: Arc::clone(&cancel),
        });
        Ok((id, cancel))
    }

    /// 解除登记；世代号不符 = 本次已被后续任务取代，不动新任务的取消标志。
    fn finish(&self, id: u64) {
        let mut slot = self.active.lock().unwrap();
        if slot.as_ref().is_some_and(|active| active.id == id) {
            *slot = None;
        }
    }
}

/// 下载更新包到应用数据目录的 `updates/`。
/// 返回 `Ok(None)` = 用户取消（`.part` 保留，再次调用即续传）；`Ok(Some(path))` = 校验通过。
#[tauri::command(async)]
pub async fn download_update_package(
    app: AppHandle,
    state: State<'_, UpdateDownloadState>,
    url: String,
    file_name: String,
    sha256: Option<String>,
    on_event: Channel<DownloadEvent>,
) -> Result<Option<String>, String> {
    let file_name = sanitize_file_name(&file_name)?;
    let parsed = ensure_public_http_url(&url)?;
    let dir = updates_dir(&app)?;
    let part = dir.join(format!("{file_name}.part"));
    let target = dir.join(&file_name);

    let sink: EventSink = Arc::new(move |event| {
        let _ = on_event.send(event);
    });

    let (id, cancel) = state.begin()?;
    let result = run_download(parsed, &part, &target, sha256.as_deref(), &sink, &cancel).await;
    state.finish(id);
    result.map(|path| path.map(|p| p.to_string_lossy().into_owned()))
}

/// 取消在途下载（只置标志，在途任务于下一块写盘前收尾并保留 `.part`）。
#[tauri::command]
pub fn cancel_update_download(state: State<'_, UpdateDownloadState>) -> Result<(), String> {
    let slot = state.active.lock().unwrap();
    match slot.as_ref() {
        Some(active) => {
            active.cancel.store(true, Ordering::Relaxed);
            Ok(())
        }
        None => Err("当前没有正在进行的下载".to_string()),
    }
}

/// 拉起已下载的安装包（只接受下载目录内的路径）。
/// Windows 上用 `ShellExecuteW` 启动后立即退出：它能按安装器 manifest 弹 UAC（perMachine 安装
/// 需要提权，`Command` 做不到），退出则让出文件占用供安装器覆盖。
#[tauri::command]
pub fn install_downloaded_update(app: AppHandle, path: String) -> Result<(), String> {
    let installer = resolve_downloaded_package(&app, &path)?;
    #[cfg(windows)]
    {
        launch_windows_installer(&installer)?;
        app.exit(0);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = installer;
        Err("当前平台不支持应用内安装更新".to_string())
    }
}

/// 校验待安装路径落在下载目录内（两边都规范化，防 `..` 与符号链接绕过）；安卓安装入口共用。
pub(crate) fn resolve_downloaded_package(app: &AppHandle, path: &str) -> Result<PathBuf, String> {
    let dir = dunce::canonicalize(updates_dir(app)?).map_err(|e| format!("更新目录不可用：{e}"))?;
    let candidate = dunce::canonicalize(path).map_err(|e| format!("安装包不可用：{e}"))?;
    if !candidate.starts_with(&dir) {
        return Err("只能安装更新目录内的安装包".to_string());
    }
    Ok(candidate)
}

/// 应用数据目录下的更新落盘目录（不存在则创建）。
fn updates_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("取应用数据目录失败：{e}"))?
        .join("updates");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建更新目录失败：{e}"))?;
    Ok(dir)
}

/// 只接受单段文件名，挡住分隔符与上跳（真正的路径守卫在 `resolve_downloaded_package`）。
fn sanitize_file_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("下载文件名不能为空".to_string());
    }
    if name.contains(['/', '\\']) || name.contains("..") {
        return Err(format!("下载文件名非法：{raw}"));
    }
    Ok(name.to_string())
}

/// 续传决策：HTTP 状态与本地已落盘长度折成「从哪写起、总长多少」。
#[derive(Debug, PartialEq)]
enum Resume {
    /// 从 `offset` 继续追加。
    Append { offset: u64, total: Option<u64> },
    /// 从头重写（服务端忽略了 Range，或本地分片不比远端短）。
    Restart { total: Option<u64> },
    /// 本地分片已是完整文件（服务端 416 且长度相等）。
    Complete { total: u64 },
}

/// 解析 `Content-Range`，返回 `(起始偏移, 总长)`；`bytes */123` 形态无起始偏移。
fn parse_content_range(value: &str) -> Option<(Option<u64>, Option<u64>)> {
    let rest = value.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let total = total.trim().parse::<u64>().ok();
    let range = range.trim();
    if range == "*" {
        return Some((None, total));
    }
    let (start, _end) = range.split_once('-')?;
    Some((Some(start.trim().parse::<u64>().ok()?), total))
}

/// 续传决策（纯函数，便于覆盖 206/200/416 三条分支）。
fn resume_plan(
    status: u16,
    existing: u64,
    content_range: Option<&str>,
    content_length: Option<u64>,
) -> Result<Resume, String> {
    match status {
        206 => {
            let (start, total) = content_range
                .and_then(parse_content_range)
                .ok_or_else(|| "服务端的分段响应缺少 Content-Range".to_string())?;
            let start = start.ok_or_else(|| "服务端的分段响应缺少起始偏移".to_string())?;
            if start != existing {
                return Err(format!(
                    "服务端返回的分段起点 {start} 与本地已下载长度 {existing} 不一致"
                ));
            }
            Ok(Resume::Append {
                offset: existing,
                total: total.or_else(|| content_length.map(|len| len + existing)),
            })
        }
        200 => Ok(Resume::Restart {
            total: content_length,
        }),
        416 => Ok(match content_range.and_then(parse_content_range).and_then(|(_, t)| t) {
            Some(total) if total == existing => Resume::Complete { total },
            _ => Resume::Restart { total: None },
        }),
        other => Err(format!("下载失败：HTTP {other}")),
    }
}

/// 已打开响应体的形态。
enum Stream {
    Append {
        resp: reqwest::Response,
        offset: u64,
        total: Option<u64>,
    },
    Restart {
        resp: reqwest::Response,
        total: Option<u64>,
    },
    Complete {
        total: u64,
    },
}

/// 发请求并按 `resume_plan` 定下写法；本地分片不被服务端接受（416）时丢弃重来一次。
async fn open_stream(
    client: &reqwest::Client,
    url: &reqwest::Url,
    part: &Path,
) -> Result<Stream, String> {
    let mut existing = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    for _ in 0..2 {
        let mut request = client.get(url.clone());
        if existing > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={existing}-"));
        }
        let resp = request
            .send()
            .await
            .map_err(|e| format!("下载请求失败：{e}"))?;
        let status = resp.status().as_u16();
        let content_range = resp
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        match resume_plan(status, existing, content_range.as_deref(), resp.content_length())? {
            Resume::Append { offset, total } => {
                return Ok(Stream::Append {
                    resp,
                    offset,
                    total,
                })
            }
            Resume::Restart { total } => {
                // 416 = 本地分片比远端长（换包/损坏）：丢弃后重发一次，第二次不带 Range 即 200
                if status == 416 {
                    drop(resp);
                    existing = 0;
                    continue;
                }
                return Ok(Stream::Restart { resp, total });
            }
            Resume::Complete { total } => return Ok(Stream::Complete { total }),
        }
    }
    Err("下载失败：服务端持续拒绝续传请求".to_string())
}

/// 响应体分块源：真实现走 reqwest，测试注入内存块流以表达「中途失败 / 取消 / 截断」。
trait ChunkSource {
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String>;
}

impl ChunkSource for reqwest::Response {
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
        self.chunk()
            .await
            .map(|chunk| chunk.map(|bytes| bytes.to_vec()))
            .map_err(|e| format!("下载中断：{e}"))
    }
}

/// 把响应体逐块写入落盘目标；返回累计字节数，取消时返回 `None`。
/// 只计已写盘字节、只在写盘前检查取消，落盘内容恒为完整前缀，`.part` 无需截断即可续传。
async fn pump<S, W>(
    source: &mut S,
    writer: &mut W,
    offset: u64,
    total: Option<u64>,
    sink: &EventSink,
    cancel: &AtomicBool,
) -> Result<Option<u64>, String>
where
    S: ChunkSource,
    W: AsyncWrite + Unpin,
{
    let mut received = offset;
    let mut last_emit = Instant::now();
    while let Some(chunk) = source.next_chunk().await? {
        if cancel.load(Ordering::Relaxed) {
            writer.flush().await.map_err(|e| format!("写入更新包失败：{e}"))?;
            return Ok(None);
        }
        writer
            .write_all(&chunk)
            .await
            .map_err(|e| format!("写入更新包失败：{e}"))?;
        received += chunk.len() as u64;
        if last_emit.elapsed() >= PROGRESS_INTERVAL {
            last_emit = Instant::now();
            sink(DownloadEvent::Progress { received, total });
        }
    }
    writer.flush().await.map_err(|e| format!("写入更新包失败：{e}"))?;
    sink(DownloadEvent::Progress { received, total });
    Ok(Some(received))
}

/// 流式比对 sha256；期望值可带 `sha256:` 前缀（GitHub Release API 的 `digest` 即此形态）。
async fn verify_sha256(path: &Path, expected: &str) -> Result<(), String> {
    let expected = expected.trim().trim_start_matches("sha256:").to_ascii_lowercase();
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("读取更新包失败：{e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buf)
            .await
            .map_err(|e| format!("读取更新包失败：{e}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected {
        return Err(format!("更新包校验失败：摘要不匹配（期望 {expected}，实际 {actual}）"));
    }
    Ok(())
}

/// 下载主体：定写法 → 落盘 → 长度校验 → 摘要校验 → 原子改名。
async fn run_download(
    url: reqwest::Url,
    part: &Path,
    target: &Path,
    sha256: Option<&str>,
    sink: &EventSink,
    cancel: &AtomicBool,
) -> Result<Option<PathBuf>, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .redirect(redirect_policy(ensure_public_http_url))
        .dns_resolver(public_dns_resolver())
        .build()
        .map_err(|e| format!("客户端初始化失败：{e}"))?;

    let stream = open_stream(&client, &url, part).await?;
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(part)
        .await
        .map_err(|e| format!("打开临时文件失败：{e}"))?;

    let (received, total) = match stream {
        Stream::Complete { total } => {
            sink(DownloadEvent::Started {
                total: Some(total),
                resuming_from: total,
            });
            (total, Some(total))
        }
        Stream::Restart { mut resp, total } => {
            file.set_len(0)
                .await
                .map_err(|e| format!("清空临时文件失败：{e}"))?;
            sink(DownloadEvent::Started {
                total,
                resuming_from: 0,
            });
            match pump(&mut resp, &mut file, 0, total, sink, cancel).await? {
                Some(received) => (received, total),
                None => return Ok(None),
            }
        }
        Stream::Append {
            mut resp,
            offset,
            total,
        } => {
            file.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(|e| format!("定位临时文件失败：{e}"))?;
            sink(DownloadEvent::Started {
                total,
                resuming_from: offset,
            });
            match pump(&mut resp, &mut file, offset, total, sink, cancel).await? {
                Some(received) => (received, total),
                None => return Ok(None),
            }
        }
    };
    drop(file);

    // 服务端报了总长就必须下满：截断的 `.part` 不能被当成功，否则改名后装出损坏的包
    if let Some(total) = total {
        if received < total {
            return Err(format!("下载不完整：已下载 {received} 字节，应为 {total} 字节"));
        }
    }

    if let Some(expected) = sha256 {
        sink(DownloadEvent::Verifying);
        if let Err(e) = verify_sha256(part, expected).await {
            let _ = tokio::fs::remove_file(part).await;
            return Err(e);
        }
    }

    tokio::fs::rename(part, target)
        .await
        .map_err(|e| format!("更新包落盘失败：{e}"))?;
    Ok(Some(target.to_path_buf()))
}

/// 以 passive 模式启动 NSIS 安装器：`/P /R`（passive + 装完重启）、`/UPDATE`、`/ARGS`。
#[cfg(windows)]
fn launch_windows_installer(installer: &Path) -> Result<(), String> {
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;

    use winapi::um::shellapi::ShellExecuteW;
    use winapi::um::winuser::SW_SHOW;

    let wide = |value: &std::ffi::OsStr| -> Vec<u16> { value.encode_wide().chain(once(0)).collect() };
    let operation = wide(std::ffi::OsStr::new("open"));
    let file = wide(installer.as_os_str());
    let parameters = wide(std::ffi::OsStr::new("/P /R /UPDATE /ARGS"));

    // ShellExecuteW 返回值 <= 32 为错误码（> 32 才是成功句柄）
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            parameters.as_ptr(),
            std::ptr::null(),
            SW_SHOW,
        )
    } as isize;
    if result <= 32 {
        return Err(format!("启动安装程序失败（错误码 {result}）"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[test]
    fn content_range_parses_range_and_total() {
        assert_eq!(parse_content_range("bytes 100-199/1000"), Some((Some(100), Some(1000))));
        assert_eq!(parse_content_range("bytes 0-0/0"), Some((Some(0), Some(0))));
        assert_eq!(parse_content_range("bytes */1000"), Some((None, Some(1000))));
        assert_eq!(parse_content_range("bytes 100-199/*"), Some((Some(100), None)));
        assert_eq!(parse_content_range("garbage"), None);
    }

    #[test]
    fn resume_plan_covers_partial_and_full_responses() {
        // 206：起点必须与本地长度一致，总长优先取 Content-Range
        assert_eq!(
            resume_plan(206, 100, Some("bytes 100-999/1000"), Some(900)).unwrap(),
            Resume::Append {
                offset: 100,
                total: Some(1000)
            }
        );
        // 206：总长为通配时用 Content-Length 换算
        assert_eq!(
            resume_plan(206, 100, Some("bytes 100-999/*"), Some(900)).unwrap(),
            Resume::Append {
                offset: 100,
                total: Some(1000)
            }
        );
        // 206：起点与本地长度不一致即拒绝（否则会拼出损坏文件）
        assert!(resume_plan(206, 100, Some("bytes 50-999/1000"), Some(950)).is_err());
        // 200：服务端忽略 Range，必须从头重写
        assert_eq!(
            resume_plan(200, 100, None, Some(1000)).unwrap(),
            Resume::Restart { total: Some(1000) }
        );
        // 416 且长度相等：本地分片已完整
        assert_eq!(
            resume_plan(416, 1000, Some("bytes */1000"), None).unwrap(),
            Resume::Complete { total: 1000 }
        );
        // 416 但长度不等：丢弃重来
        assert_eq!(
            resume_plan(416, 1200, Some("bytes */1000"), None).unwrap(),
            Resume::Restart { total: None }
        );
        assert!(resume_plan(500, 0, None, None).is_err());
    }

    #[test]
    fn sanitize_file_name_rejects_paths() {
        assert_eq!(sanitize_file_name(" Atelyx_0.5.8_x64-setup.exe ").unwrap(), "Atelyx_0.5.8_x64-setup.exe");
        assert!(sanitize_file_name("a/b.exe").is_err());
        assert!(sanitize_file_name("a\\b.exe").is_err());
        assert!(sanitize_file_name("..").is_err());
        assert!(sanitize_file_name("   ").is_err());
    }

    /// 内存块源：按序吐出预置块，用尽后返回 `None`；可指定第几次调用开始报错。
    struct FakeSource {
        chunks: VecDeque<Vec<u8>>,
        fail_after: Option<usize>,
        calls: usize,
    }

    impl ChunkSource for FakeSource {
        async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
            if let Some(limit) = self.fail_after {
                if self.calls >= limit {
                    return Err("下载中断：连接被重置".to_string());
                }
            }
            self.calls += 1;
            Ok(self.chunks.pop_front())
        }
    }

    fn collecting_sink() -> (EventSink, Arc<Mutex<Vec<DownloadEvent>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&events);
        let sink: EventSink = Arc::new(move |event| recorder.lock().unwrap().push(event));
        (sink, events)
    }

    #[tokio::test]
    async fn pump_writes_all_chunks_and_reports_final_progress() {
        let mut source = FakeSource {
            chunks: VecDeque::from(vec![b"abc".to_vec(), b"de".to_vec()]),
            fail_after: None,
            calls: 0,
        };
        let mut writer: Vec<u8> = Vec::new();
        let (sink, events) = collecting_sink();
        let cancel = AtomicBool::new(false);

        let received = pump(&mut source, &mut writer, 0, Some(5), &sink, &cancel)
            .await
            .unwrap();

        assert_eq!(received, Some(5));
        assert_eq!(writer, b"abcde");
        // 收尾必推一次终值，否则进度条停在最后一帧
        assert_eq!(
            events.lock().unwrap().last().cloned(),
            Some(DownloadEvent::Progress {
                received: 5,
                total: Some(5)
            })
        );
    }

    #[tokio::test]
    async fn pump_stops_before_writing_when_cancelled() {
        let mut source = FakeSource {
            chunks: VecDeque::from(vec![b"abc".to_vec(), b"de".to_vec()]),
            fail_after: None,
            calls: 0,
        };
        let mut writer: Vec<u8> = Vec::new();
        let (sink, _events) = collecting_sink();
        let cancel = AtomicBool::new(true);

        let received = pump(&mut source, &mut writer, 0, Some(5), &sink, &cancel)
            .await
            .unwrap();

        // 取消时不落任何新字节，落盘内容始终是完整前缀，`.part` 可直接续传
        assert_eq!(received, None);
        assert!(writer.is_empty());
    }

    #[tokio::test]
    async fn pump_surfaces_network_failure() {
        let mut source = FakeSource {
            chunks: VecDeque::from(vec![b"abc".to_vec(), b"de".to_vec()]),
            fail_after: Some(1),
            calls: 0,
        };
        let mut writer: Vec<u8> = Vec::new();
        let (sink, _events) = collecting_sink();
        let cancel = AtomicBool::new(false);

        let error = pump(&mut source, &mut writer, 0, Some(5), &sink, &cancel)
            .await
            .unwrap_err();

        assert!(error.contains("下载中断"), "{error}");
        // 失败前已写下的字节保留（`.part` 供续传）
        assert_eq!(writer, b"abc");
    }

    #[test]
    fn download_state_rejects_concurrent_tasks() {
        let state = UpdateDownloadState::default();
        let (id, cancel) = state.begin().unwrap();
        assert!(state.begin().is_err());

        state.finish(id + 1); // 世代号不符：不动在途任务的标志
        assert!(!cancel.load(Ordering::Relaxed));
        assert!(state.begin().is_err());

        state.finish(id);
        assert!(state.begin().is_ok());
    }

    #[test]
    fn cancel_only_touches_active_download() {
        let state = UpdateDownloadState::default();
        assert!(state.active.lock().unwrap().is_none());
        let (_id, cancel) = state.begin().unwrap();
        {
            let slot = state.active.lock().unwrap();
            slot.as_ref().unwrap().cancel.store(true, Ordering::Relaxed);
        }
        assert!(cancel.load(Ordering::Relaxed));
    }
}
