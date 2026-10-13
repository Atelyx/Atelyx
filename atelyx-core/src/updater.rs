//! 自动更新核心：版本比较、GitHub Release 检查与安装包下载（续传 + sha256 校验）。

use std::cmp::Ordering;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::header::{ACCEPT, CONTENT_RANGE, RANGE, USER_AGENT};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::net_guard::{self, ClientTimeout, HostPolicy};

/// 更新源仓库的 Release API 与兜底发布页。
const RELEASE_API: &str = "https://api.github.com/repos/Atelyx/Atelyx/releases/latest";
const RELEASES_PAGE: &str = "https://github.com/Atelyx/Atelyx/releases";
/// 版本检查超时（秒）：不让启动检查悬挂。
const CHECK_TIMEOUT_SECS: u64 = 15;
/// Windows 安装包产物后缀；无匹配产物时调用方转开发布页。
const ASSET_SUFFIX: &str = "-setup.exe";
/// 下载的连接与单次读超时（秒）：不设总超时，防大包被整包限时误杀。
const DOWNLOAD_CONNECT_SECS: u64 = 15;
const DOWNLOAD_READ_SECS: u64 = 60;
/// 进度回调节流间隔：分块频率不可控，收尾必调一次保证终值可见。
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// 版本号比较：按 `.` 分段逐段解析为 u64 比较，非数字段按 0、缺段补 0。
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let mut sa = a.split('.');
    let mut sb = b.split('.');
    loop {
        match (sa.next(), sb.next()) {
            (None, None) => return Ordering::Equal,
            (seg_a, seg_b) => {
                let x = segment_value(seg_a);
                let y = segment_value(seg_b);
                if x != y {
                    return x.cmp(&y);
                }
            }
        }
    }
}

/// 版本段取值：空段/非数字段按 0。
fn segment_value(seg: Option<&str>) -> u64 {
    seg.and_then(|s| s.parse::<u64>().ok()).unwrap_or(0)
}

/// 单次 Release 检查结果。`asset_*` 无 `-setup.exe` 产物时为 None，调用方转开发布页；
/// `sha256` 为 GitHub `digest` 原文（`sha256:<hex>` 形态），下载校验时剥除前缀。
#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    pub version: String,
    pub html_url: String,
    pub asset_name: Option<String>,
    pub asset_url: Option<String>,
    pub sha256: Option<String>,
}

/// GitHub Release API 响应中用到的字段（其余忽略，缺失容错）。
#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: Option<String>,
    html_url: Option<String>,
    assets: Option<Vec<GitHubAsset>>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: Option<String>,
    browser_download_url: Option<String>,
    digest: Option<String>,
}

/// 检查 GitHub 最新 Release：网络失败、HTTP 非 2xx 或发布信息缺版本号即 Err。
/// tag 的 `v`/`V` 前缀剥除后作为 `version` 返回；`html_url` 缺失时兜底发布页。
pub async fn latest_release() -> Result<ReleaseInfo, String> {
    let checked = net_guard::ensure_public_http_url(RELEASE_API)?;
    let client = net_guard::client_for(HostPolicy::PublicOnly, ClientTimeout::Total(CHECK_TIMEOUT_SECS))?;
    let release: GitHubRelease = client
        .get(checked)
        .header(USER_AGENT, concat!("Atelyx/", env!("CARGO_PKG_VERSION")))
        .header(ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("检查更新失败：{e}"))?
        .error_for_status()
        .map_err(|e| format!("检查更新失败：{e}"))?
        .json()
        .await
        .map_err(|e| format!("检查更新失败：{e}"))?;

    let tag = release.tag_name.unwrap_or_default();
    let tag = tag.trim();
    let version = tag.strip_prefix(['v', 'V']).unwrap_or(tag);
    if version.is_empty() {
        return Err("检查更新失败：发布信息缺少版本号".to_string());
    }

    // 产物名与下载地址须齐备才可应用内下载，缺一视同无产物
    let matched = release
        .assets
        .unwrap_or_default()
        .into_iter()
        .find(|asset| {
            asset
                .name
                .as_deref()
                .is_some_and(|name| name.to_lowercase().ends_with(ASSET_SUFFIX))
        });
    let (asset_name, asset_url, sha256) = match matched {
        Some(asset) if asset.browser_download_url.is_some() => {
            (asset.name, asset.browser_download_url, asset.digest)
        }
        _ => (None, None, None),
    };
    Ok(ReleaseInfo {
        version: version.to_string(),
        html_url: release
            .html_url
            .filter(|url| !url.trim().is_empty())
            .unwrap_or_else(|| RELEASES_PAGE.to_string()),
        asset_name,
        asset_url,
        sha256,
    })
}

/// 下载安装包到 `dest_dir`，返回正式文件路径。
/// 断点续传：`.part` 已存在按其长度发 Range 请求，206 且起点与本地一致则追加、200 从头重写、
/// 416 且本地长度等于总长视为已完整；其余错误保留 `.part` 直接报错。
/// 取消：置位 `cancel` 后立即停止并返回 Err("已取消")，`.part` 恒为完整前缀可直接续传。
/// 校验：`expected_sha256` 非 None 时流式校验（接受 `sha256:` 前缀），不匹配删 `.part` 返错；
/// 服务端报了总长而未下满同样报错并保留 `.part`；通过后删同名旧文件再原子改名落位。
/// 并发防护（同目录同文件同时只跑一个下载）由调用方负责。
/// `on_progress(received, total)` 只计已写盘字节，按 100ms 节流且收尾必调，total 未知时为 0。
pub async fn download(
    dest_dir: &Path,
    url: &str,
    expected_sha256: Option<&str>,
    cancel: Arc<AtomicBool>,
    on_progress: Arc<dyn Fn(u64, u64) + Send + Sync>,
) -> Result<PathBuf, String> {
    let checked = net_guard::ensure_public_http_url(url)?;
    let client = net_guard::client_for(
        HostPolicy::PublicOnly,
        ClientTimeout::ConnectRead {
            connect: DOWNLOAD_CONNECT_SECS,
            read: DOWNLOAD_READ_SECS,
        },
    )?;
    download_via(client, checked, dest_dir, expected_sha256, cancel, on_progress).await
}

/// 下载主体：定写法（续传/重写/已完整）→ 落盘 → 长度与摘要校验 → 原子改名。
/// 客户端与已校验 URL 由入口定好；测试以本机地址直入，不重复挂公网守卫。
async fn download_via(
    client: reqwest::Client,
    url: reqwest::Url,
    dest_dir: &Path,
    expected_sha256: Option<&str>,
    cancel: Arc<AtomicBool>,
    on_progress: Arc<dyn Fn(u64, u64) + Send + Sync>,
) -> Result<PathBuf, String> {
    if cancel.load(Relaxed) {
        return Err("已取消".to_string());
    }
    let file_name = file_name_from_url(&url)?;
    std::fs::create_dir_all(dest_dir).map_err(|e| format!("创建下载目录失败：{e}"))?;
    let part = dest_dir.join(format!("{file_name}.part"));
    let target = dest_dir.join(&file_name);

    let (received, total) = match open_stream(&client, &url, &part).await? {
        Source::Complete { total } => {
            on_progress(total, total);
            (total, Some(total))
        }
        Source::Restart { mut resp, total } => {
            let mut file = open_part_file(&part, true)?;
            pump(&mut resp, &mut file, 0, total, &cancel, on_progress.as_ref()).await?
        }
        Source::Append { mut resp, offset, total } => {
            let mut file = open_part_file(&part, false)?;
            file
                .seek(SeekFrom::Start(offset))
                .map_err(|e| format!("定位临时文件失败：{e}"))?;
            pump(&mut resp, &mut file, offset, total, &cancel, on_progress.as_ref()).await?
        }
    };

    // 服务端报了总长就必须下满：截断的 `.part` 不能当成功，否则改名后是损坏的包
    if let Some(total) = total {
        if received < total {
            return Err(format!("下载不完整：已下载 {received} 字节，应为 {total} 字节"));
        }
    }

    if let Some(expected) = expected_sha256 {
        if let Err(e) = verify_sha256(&part, expected) {
            // 摘要不匹配 = 分片来源可疑：删掉避免下次续传拼出坏包
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
    }

    // 旧包删除失败不阻塞落位：rename 失败会如实报错
    let _ = std::fs::remove_file(&target);
    std::fs::rename(&part, &target).map_err(|e| format!("更新包落盘失败：{e}"))?;
    Ok(target)
}

/// 打开 `.part`：fresh = 截断重写，否则保留内容由调用方 seek 到续传起点。
fn open_part_file(part: &Path, fresh: bool) -> Result<std::fs::File, String> {
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(fresh)
        .open(part)
        .map_err(|e| format!("打开临时文件失败：{e}"))
}

/// 把响应体逐块写入文件，返回（累计已写盘字节, 总长）。
/// 只在写盘前查取消，落盘内容恒为完整前缀；取消返回 Err("已取消")。
async fn pump(
    resp: &mut reqwest::Response,
    file: &mut std::fs::File,
    offset: u64,
    total: Option<u64>,
    cancel: &AtomicBool,
    on_progress: &(dyn Fn(u64, u64) + Send + Sync),
) -> Result<(u64, Option<u64>), String> {
    let mut received = offset;
    let mut last_emit = Instant::now();
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("下载中断：{e}"))? {
        if cancel.load(Relaxed) {
            return Err("已取消".to_string());
        }
        file.write_all(&chunk).map_err(|e| format!("写入更新包失败：{e}"))?;
        received += chunk.len() as u64;
        if last_emit.elapsed() >= PROGRESS_INTERVAL {
            last_emit = Instant::now();
            on_progress(received, total.unwrap_or(0));
        }
    }
    file.flush().map_err(|e| format!("写入更新包失败：{e}"))?;
    on_progress(received, total.unwrap_or(0));
    Ok((received, total))
}

/// 文件名取 URL 路径尾段原样使用；为空或含路径语义（`..`）即拒，不做 URL 解码。
fn file_name_from_url(url: &reqwest::Url) -> Result<String, String> {
    let raw = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .unwrap_or("");
    let name = raw.trim();
    if name.is_empty() || name.contains(['\\', '/']) || name.contains("..") {
        return Err(format!("下载地址缺少合法文件名：{raw}"));
    }
    Ok(name.to_string())
}

/// 流式比对 sha256：期望值可带 `sha256:` 前缀（GitHub digest 形态），忽略大小写。
fn verify_sha256(path: &Path, expected: &str) -> Result<(), String> {
    let expected = expected.trim().trim_start_matches("sha256:").to_ascii_lowercase();
    let mut file = std::fs::File::open(path).map_err(|e| format!("读取更新包失败：{e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf).map_err(|e| format!("读取更新包失败：{e}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    let actual = hex::encode(hasher.finalize());
    if actual != expected {
        return Err(format!(
            "更新包校验失败：摘要不匹配（期望 {expected}，实际 {actual}）"
        ));
    }
    Ok(())
}

/// 续传决策：HTTP 状态与本地已落盘长度折成「从哪写起、总长多少」。
#[derive(Debug, PartialEq)]
enum Resume {
    /// 从 `offset` 继续追加。
    Append { offset: u64, total: Option<u64> },
    /// 从头重写（服务端忽略 Range，或本地分片不比远端短）。
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

/// 续传决策（纯函数）：206 起点必须与本地长度一致否则拒绝（拼出损坏文件）；416 且长度相等
/// 视为已完整，416 但长度不符丢弃重发；其余状态一律报错并保留 `.part`。
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
enum Source {
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

/// 发请求并按 `resume_plan` 定下写法；本地分片不被服务端接受（416 但长度不符）时
/// 丢弃重发一次（第二次不带 Range 即 200）。
async fn open_stream(
    client: &reqwest::Client,
    url: &reqwest::Url,
    part: &Path,
) -> Result<Source, String> {
    let mut existing = std::fs::metadata(part).map(|meta| meta.len()).unwrap_or(0);
    for _ in 0..2 {
        let mut request = client.get(url.clone());
        if existing > 0 {
            request = request.header(RANGE, format!("bytes={existing}-"));
        }
        let resp = request.send().await.map_err(|e| format!("下载请求失败：{e}"))?;
        let status = resp.status().as_u16();
        let content_range = resp
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        match resume_plan(status, existing, content_range.as_deref(), resp.content_length())? {
            Resume::Append { offset, total } => {
                return Ok(Source::Append {
                    resp,
                    offset,
                    total,
                });
            }
            Resume::Restart { total } => {
                if status == 416 {
                    drop(resp);
                    existing = 0;
                    continue;
                }
                return Ok(Source::Restart { resp, total });
            }
            Resume::Complete { total } => return Ok(Source::Complete { total }),
        }
    }
    Err("下载失败：服务端持续拒绝续传请求".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const FILE_NAME: &str = "Atelyx_1.0.0_x64-setup.exe";

    // ---------- compare_versions ----------

    /// 全分支：相等 / 大小 / 缺段补 0 / 非数字段按 0 / 数值比较非字典序。
    #[test]
    fn compare_versions_covers_all_branches() {
        assert_eq!(compare_versions("1.2.3", "1.2.3"), Ordering::Equal);
        assert_eq!(compare_versions("1.2.4", "1.2.3"), Ordering::Greater);
        assert_eq!(compare_versions("1.2.3", "1.2.4"), Ordering::Less);
        // 缺段补 0（双向）
        assert_eq!(compare_versions("1.2", "1.2.0.0"), Ordering::Equal);
        assert_eq!(compare_versions("1.2.0.1", "1.2"), Ordering::Greater);
        assert_eq!(compare_versions("1.2", "1.2.1"), Ordering::Less);
        // 逐段按数值比较
        assert_eq!(compare_versions("1.10.0", "1.9.0"), Ordering::Greater);
        // 非数字段按 0
        assert_eq!(compare_versions("1.2.x", "1.2.0"), Ordering::Equal);
        assert_eq!(compare_versions("abc", "0"), Ordering::Equal);
        assert_eq!(compare_versions("1.2.3", "1.2.2.x"), Ordering::Greater);
    }

    // ---------- 续传决策纯函数 ----------

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
        // 206：起点与本地长度不一致即拒绝
        assert!(resume_plan(206, 100, Some("bytes 50-999/1000"), Some(950)).is_err());
        // 200：服务端忽略 Range，必须从头重写
        assert_eq!(
            resume_plan(200, 100, None, Some(1000)).unwrap(),
            Resume::Restart {
                total: Some(1000)
            }
        );
        // 416 且长度相等：本地分片已完整；长度不等：丢弃重来
        assert_eq!(
            resume_plan(416, 1000, Some("bytes */1000"), None).unwrap(),
            Resume::Complete {
                total: 1000
            }
        );
        assert_eq!(
            resume_plan(416, 1200, Some("bytes */1000"), None).unwrap(),
            Resume::Restart {
                total: None
            }
        );
        assert!(resume_plan(500, 0, None, None).is_err());
    }

    #[test]
    fn file_name_from_url_rejects_empty_and_traversal() {
        let ok = reqwest::Url::parse("https://example.com/dl/pkg/setup.exe?token=x").unwrap();
        assert_eq!(file_name_from_url(&ok).unwrap(), "setup.exe");
        let trailing_slash = reqwest::Url::parse("https://example.com/dl/").unwrap();
        assert!(file_name_from_url(&trailing_slash).is_err());
        let traversal = reqwest::Url::parse("https://example.com/dl/..%2Fetc").unwrap();
        assert!(file_name_from_url(&traversal).is_err());
    }

    // ---------- 本地 HTTP 服务 ----------

    /// 确定性测试数据。
    fn test_body(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 + 11) as u8).collect()
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    /// 测试自清临时目录。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("atelyx-updater-test-{}", nanoid::nanoid!(12)));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 进度收集器。
    fn progress_sink() -> (Arc<dyn Fn(u64, u64) + Send + Sync>, Arc<Mutex<Vec<(u64, u64)>>>) {
        let events: Arc<Mutex<Vec<(u64, u64)>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        (
            Arc::new(move |received, total| sink.lock().unwrap().push((received, total))),
            events,
        )
    }

    /// 最小 HTTP 服务：按 Range 返回 200 全量 / 206 分片 / 416，每连接一轮后断开；
    /// 每次请求的 Range 起点（无 Range 为 None）记录到 log。`slow_from` 使分片体超该长度时
    /// 先发前若干字节、停顿后再发余下，为中途取消留出窗口。
    struct TestServer {
        url: String,
        log: Arc<Mutex<Vec<Option<u64>>>>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.stop.store(true, Relaxed);
            if let Some(thread) = self.thread.take() {
                thread.join().ok();
            }
        }
    }

    fn spawn_server(body: Vec<u8>, slow_from: Option<(usize, Duration)>) -> TestServer {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let server_log = Arc::clone(&log);
        let server_stop = Arc::clone(&stop);
        // 服务端跑独立阻塞线程：不占异步运行时，测试结束经 stop + join 收尾
        let thread = std::thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            while !server_stop.load(Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => serve_conn(stream, &body, slow_from, &server_log),
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        TestServer {
            url: format!("http://{addr}/download/{FILE_NAME}"),
            log,
            stop,
            thread: Some(thread),
        }
    }

    /// 处理一个连接：读请求头 → 按 Range 决定响应 → 发完即断。
    fn serve_conn(
        mut stream: std::net::TcpStream,
        body: &[u8],
        slow_from: Option<(usize, Duration)>,
        log: &Mutex<Vec<Option<u64>>>,
    ) {
        stream.set_nonblocking(false).ok();
        stream.set_read_timeout(Some(Duration::from_millis(500))).ok();
        stream.set_write_timeout(Some(Duration::from_millis(500))).ok();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 2048];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
            // GET 请求无请求体，头终止符即请求末尾
            if buf.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&buf);
        let range_start = head.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if !name.trim().eq_ignore_ascii_case("range") {
                return None;
            }
            value
                .trim()
                .strip_prefix("bytes=")?
                .split('-')
                .next()?
                .parse::<u64>()
                .ok()
        });
        log.lock().unwrap().push(range_start);

        let (head_response, from) = match range_start {
            Some(start) if start as usize >= body.len() => (
                format!(
                    "HTTP/1.1 416 Requested Range Not Satisfiable\r\nContent-Range: bytes */{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    body.len()
                ),
                None,
            ),
            Some(start) => (
                format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                    start,
                    body.len() - 1,
                    body.len(),
                    body.len() - start as usize
                ),
                Some(start as usize),
            ),
            None => (
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                    body.len()
                ),
                Some(0),
            ),
        };
        stream.write_all(head_response.as_bytes()).ok();
        if let Some(from) = from {
            let remaining = &body[from..];
            match slow_from {
                Some((split, delay)) if remaining.len() > split => {
                    stream.write_all(&remaining[..split]).ok();
                    stream.flush().ok();
                    std::thread::sleep(delay);
                    stream.write_all(&remaining[split..]).ok();
                }
                _ => {
                    stream.write_all(remaining).ok();
                }
            }
        }
        stream.flush().ok();
    }

    /// 直入下载主体（绕开公网守卫——本机回环会被公网策略拒绝，守卫自身已另有测试覆盖）。
    async fn run_download(
        server: &TestServer,
        dest: &Path,
        sha: Option<&str>,
        cancel: Arc<AtomicBool>,
        on_progress: Arc<dyn Fn(u64, u64) + Send + Sync>,
    ) -> Result<PathBuf, String> {
        download_via(
            reqwest::Client::new(),
            reqwest::Url::parse(&server.url).unwrap(),
            dest,
            sha,
            cancel,
            on_progress,
        )
        .await
    }

    // ---------- 下载链路 ----------

    /// 全量下载 + sha256（带 `sha256:` 前缀形态）通过：落位正式文件、`.part` 消失、进度终值正确。
    #[tokio::test]
    async fn full_download_verifies_and_renames() {
        let body = test_body(256);
        let server = spawn_server(body.clone(), None);
        let dir = TempDir::new();
        let (on_progress, events) = progress_sink();
        let cancel = Arc::new(AtomicBool::new(false));

        let path = run_download(&server, &dir.0, Some(&format!("sha256:{}", sha256_hex(&body))), cancel, on_progress)
            .await
            .unwrap();

        assert_eq!(path, dir.0.join(FILE_NAME));
        assert_eq!(std::fs::read(&path).unwrap(), body);
        assert!(!dir.0.join(format!("{FILE_NAME}.part")).exists());
        assert_eq!(*events.lock().unwrap().last().unwrap(), (256, 256));
    }

    /// 续传：预置半个 `.part`，服务端按 Range 出 206 追加，最终文件完整。
    #[tokio::test]
    async fn resume_appends_from_partial_part() {
        let body = test_body(256);
        let half = body.len() / 2;
        let server = spawn_server(body.clone(), None);
        let dir = TempDir::new();
        std::fs::write(dir.0.join(format!("{FILE_NAME}.part")), &body[..half]).unwrap();

        let path = run_download(&server, &dir.0, None, Arc::new(AtomicBool::new(false)), progress_sink().0)
            .await
            .unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), body);
        assert!(!dir.0.join(format!("{FILE_NAME}.part")).exists());
        // 确为续传：服务端收到 Range: bytes={half}- 而非全量请求
        assert!(server.log.lock().unwrap().contains(&Some(half as u64)));
    }

    /// `.part` 已是完整文件：服务端 416 且总长一致 → 不再下载，直接校验落位。
    #[tokio::test]
    async fn complete_part_finalizes_without_download() {
        let body = test_body(64);
        let server = spawn_server(body.clone(), None);
        let dir = TempDir::new();
        std::fs::write(dir.0.join(format!("{FILE_NAME}.part")), &body).unwrap();

        let path = run_download(&server, &dir.0, None, Arc::new(AtomicBool::new(false)), progress_sink().0)
            .await
            .unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), body);
        // 唯一一次请求即 416（Range 起点 = 总长）
        assert_eq!(*server.log.lock().unwrap(), vec![Some(64)]);
        assert!(!dir.0.join(format!("{FILE_NAME}.part")).exists());
    }

    /// `.part` 比远端长（换包/损坏）：416 后丢弃重发，200 从头重写为完整文件。
    #[tokio::test]
    async fn oversized_part_restarts_from_scratch() {
        let body = test_body(64);
        let server = spawn_server(body.clone(), None);
        let dir = TempDir::new();
        let mut stale = body.clone();
        stale.extend_from_slice(b"stale-extra-bytes");
        std::fs::write(dir.0.join(format!("{FILE_NAME}.part")), &stale).unwrap();

        let path = run_download(&server, &dir.0, None, Arc::new(AtomicBool::new(false)), progress_sink().0)
            .await
            .unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), body);
        let stale_len = stale.len() as u64;
        let log = server.log.lock().unwrap();
        assert_eq!(log[0], Some(stale_len)); // Range: bytes={stale_len}- → 416
        assert_eq!(log[1], None); // 重发不带 Range → 200
        assert_eq!(log.len(), 2);
    }

    /// 摘要不匹配：报错且 `.part` 被删，不留可疑分片。
    #[tokio::test]
    async fn sha_mismatch_removes_part() {
        let server = spawn_server(test_body(64), None);
        let dir = TempDir::new();

        let error = run_download(&server, &dir.0, Some(&sha256_hex(&test_body(65))), Arc::new(AtomicBool::new(false)), progress_sink().0)
            .await
            .unwrap_err();

        assert!(error.contains("摘要不匹配"), "{error}");
        assert!(!dir.0.join(format!("{FILE_NAME}.part")).exists());
        assert!(!dir.0.join(FILE_NAME).exists());
    }

    /// 中途取消：报「已取消」，`.part` 保留为已写完整前缀。
    #[tokio::test]
    async fn cancel_mid_download_keeps_prefix() {
        let body = test_body(128);
        let split = 32;
        // 服务端发完前 32 字节停顿 500ms，留出取消窗口
        let server = spawn_server(body.clone(), Some((split, Duration::from_millis(500))));
        let dir = TempDir::new();
        let cancel = Arc::new(AtomicBool::new(false));
        let part = dir.0.join(format!("{FILE_NAME}.part"));
        let dest = dir.0.clone();
        let server_url = server.url.clone();
        let task_cancel = Arc::clone(&cancel);
        let task = tokio::spawn(async move {
            download_via(
                reqwest::Client::new(),
                reqwest::Url::parse(&server_url).unwrap(),
                &dest,
                None,
                task_cancel,
                progress_sink().0,
            )
            .await
        });

        // 等首个分片写盘后再取消，确保取消落在两次写盘之间
        let deadline = Instant::now() + Duration::from_secs(5);
        while std::fs::metadata(&part).map(|meta| meta.len()).unwrap_or(0) < split as u64 {
            assert!(Instant::now() < deadline, "等待首个分片写入超时");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        cancel.store(true, Relaxed);

        let error = task.await.unwrap().unwrap_err();
        assert_eq!(error, "已取消");
        assert_eq!(std::fs::read(&part).unwrap(), &body[..split]);
        assert!(!dir.0.join(FILE_NAME).exists());
    }
}
