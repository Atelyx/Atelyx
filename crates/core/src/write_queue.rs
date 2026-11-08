//! 仓库写盘串行队列：同进程内全部仓库内容写盘收敛到单一 FIFO 执行序。

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

/// 一条队列作业。
enum JobKind {
    Write { path: PathBuf, content: String },
    /// 字节写：附件二进制（图片导入等）走同一 FIFO，保持与文本写盘的相对顺序。
    WriteBytes { path: PathBuf, bytes: Vec<u8> },
    /// 从既有文件原子落库（插件私有目录里暂存的大文件）；源文件由调用方持有，
    /// 本作业只读不删。
    WriteFromFile { path: PathBuf, source: PathBuf },
    /// 批量作业：闭包内自管多文件读改写（全库批量重建等），整体占一个执行序位，
    /// 内部多次写盘不与其它作业交错。
    Bulk(Box<dyn FnOnce() -> Result<(), String> + Send>),
    /// 旗标作业：不写盘，落地即代表排在其前提交的全部写盘已完成。
    Barrier,
}

struct Job {
    kind: JobKind,
    /// 回执：`Ok(created)` 的 `created` = 本次落盘前目标是否不存在（由执行序内判定）。
    done: tokio::sync::oneshot::Sender<Result<bool, String>>,
}

#[derive(Default)]
struct QueueInner {
    jobs: Mutex<VecDeque<Job>>,
    signal: Condvar,
}

fn poison<T>(err: PoisonError<MutexGuard<'_, T>>) -> MutexGuard<'_, T> {
    err.into_inner()
}

/// FIFO 写盘队列：`write` 按提交顺序逐个执行原子写并回执单次结果，`drain` 等此前提交全部落地。
/// 作业在提交时冻结绝对路径与内容，仓库切换不影响在途写盘目标。clone 共享同一执行序。
/// 「是否新建」由队列在执行序内判定（见 `write_reporting_created`），调用方不必预判磁盘状态。
#[derive(Clone)]
pub struct WriteQueue {
    inner: Arc<QueueInner>,
}

impl Default for WriteQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl WriteQueue {
    /// 创建队列并启动常驻工作线程（随进程退出结束）。
    pub fn new() -> Self {
        let queue = Self {
            inner: Arc::new(QueueInner::default()),
        };
        let inner = Arc::clone(&queue.inner);
        std::thread::Builder::new()
            .name("vault-write-queue".into())
            .spawn(move || worker(inner))
            .expect("启动写盘队列线程");
        queue
    }

    /// 提交一次原子写，完成后回执本次写盘结果；同路径并发提交按提交顺序执行（后写者胜）。
    pub async fn write(&self, path: PathBuf, content: String) -> Result<(), String> {
        self.write_reporting_created(path, content).await.map(|_| ())
    }

    /// 同 [`Self::write`]，额外回执目标在落盘前是否存在（`true` = 本次新建）。
    /// 存在性在队列执行序内判定：同一路径连发的写入按落地先后各自给出真假，
    /// 不受提交那一刻的磁盘状态与在途写盘影响。
    pub async fn write_reporting_created(
        &self,
        path: PathBuf,
        content: String,
    ) -> Result<bool, String> {
        let rx = self.push(JobKind::Write { path, content });
        rx.await.map_err(|_| "写盘队列已终止".to_string())?
    }

    /// 提交一次原子字节写（附件二进制）：语义与 `write` 相同，仅载荷为字节。
    pub async fn write_bytes(&self, path: PathBuf, bytes: Vec<u8>) -> Result<(), String> {
        self.write_bytes_reporting_created(path, bytes).await.map(|_| ())
    }

    /// 同 [`Self::write_bytes`]，额外回执是否新建（判定口径见 [`Self::write_reporting_created`]）。
    pub async fn write_bytes_reporting_created(
        &self,
        path: PathBuf,
        bytes: Vec<u8>,
    ) -> Result<bool, String> {
        let rx = self.push(JobKind::WriteBytes { path, bytes });
        rx.await.map_err(|_| "写盘队列已终止".to_string())?
    }

    /// 提交一次从既有文件的原子写（大文件导入）：语义与 `write_bytes` 相同，额外回执是否新建。
    pub async fn write_from_file(&self, path: PathBuf, source: PathBuf) -> Result<bool, String> {
        let rx = self.push(JobKind::WriteFromFile { path, source });
        rx.await.map_err(|_| "写盘队列已终止".to_string())?
    }

    /// 提交一次批量作业：闭包在队列工作线程内执行（内部多文件读改写整体串行，
    /// 不与其它写盘作业交错）；排在其前的写盘全部落地后才开始，回执闭包结果。
    /// 闭包 panic 不终止队列线程，回执 Err；仓库根等会话参数由调用方在提交时捕获，
    /// 执行期不重读当前会话（排队期间切仓不改变作业目标）。
    pub async fn bulk<T: Send + 'static>(
        &self,
        job: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let (value_tx, value_rx) = tokio::sync::oneshot::channel();
        let boxed: Box<dyn FnOnce() -> Result<(), String> + Send> = Box::new(move || {
            // panic 被拦截为 Err 回执：worker 线程常驻，闭包炸掉会拖死后续全部写盘
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job))
                .unwrap_or_else(|_| Err("内部写作业异常终止".to_string()));
            value_tx.send(outcome).map_err(|_| "批量作业回执接收端已关闭".to_string())
        });
        let rx = self.push(JobKind::Bulk(boxed));
        // 批量作业不参与「是否新建」判定，回执的 created 恒 false
        rx.await.map_err(|_| "写盘队列已终止".to_string())??;
        value_rx.await.map_err(|_| "写盘队列已终止".to_string())?
    }

    /// 等待此前提交的全部写盘落地（不含本调用之后提交的）。
    pub async fn drain(&self) -> Result<(), String> {
        let rx = self.push(JobKind::Barrier);
        rx.await.map_err(|_| "写盘队列已终止".to_string())?.map(|_| ())
    }

    fn push(&self, kind: JobKind) -> tokio::sync::oneshot::Receiver<Result<bool, String>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut jobs = self.inner.jobs.lock().unwrap_or_else(poison);
        jobs.push_back(Job { kind, done: tx });
        self.inner.signal.notify_one();
        rx
    }
}

fn worker(inner: Arc<QueueInner>) {
    loop {
        let job = {
            let mut jobs = inner.jobs.lock().unwrap_or_else(poison);
            loop {
                if let Some(job) = jobs.pop_front() {
                    break job;
                }
                jobs = inner.signal.wait(jobs).unwrap_or_else(poison);
            }
        };
        let result = match job.kind {
            // 存在性取执行序内的磁盘事实：排在前面的同路径作业已落地，此处看到的才是本次真实起点
            JobKind::Write { path, content } => {
                let existed = path.is_file();
                crate::vault::atomic_write(&path, &content).map(|()| !existed)
            }
            JobKind::WriteBytes { path, bytes } => {
                let existed = path.is_file();
                crate::vault::atomic_write_bytes(&path, &bytes).map(|()| !existed)
            }
            JobKind::WriteFromFile { path, source } => {
                let existed = path.is_file();
                crate::vault::atomic_write_from_file(&path, &source).map(|()| !existed)
            }
            JobKind::Bulk(job) => job().map(|()| false),
            JobKind::Barrier => Ok(false),
        };
        let _ = job.done.send(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn read(path: std::path::PathBuf) -> String {
        tokio::task::spawn_blocking(move || std::fs::read_to_string(path).unwrap_or_default())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn same_path_writes_execute_in_submit_order() {
        let tmp = crate::vault::test_support::TempDir::new("wq-fifo");
        let path = tmp.join("note.md");
        let queue = WriteQueue::new();
        queue.write(path.clone(), "first".into()).await.unwrap();
        queue.write(path.clone(), "second".into()).await.unwrap();
        // 串行执行下后写必胜；若两写并发完成顺序无保证，此处会间歇读到 "first"
        assert_eq!(read(path).await, "second");
    }

    #[tokio::test]
    async fn write_result_propagates_failure() {
        // 父路径是文件：create_dir_all 必败，错误原样回执给提交方
        let tmp = crate::vault::test_support::TempDir::new("wq-fail");
        let blocker = tmp.join("blocker");
        std::fs::write(&blocker, b"").unwrap();
        let queue = WriteQueue::new();
        let err = queue.write(blocker.join("child.md"), "x".into()).await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn bulk_job_runs_between_writes_in_order() {
        let tmp = crate::vault::test_support::TempDir::new("wq-bulk");
        let queue = WriteQueue::new();
        let p1 = tmp.join("a.md");
        let p2 = tmp.join("b.md");
        let p3 = tmp.join("c.md");
        queue.write(p1.clone(), "1".into()).await.unwrap();
        // 批量作业闭包内多文件读改写：整体串行，前序写已落地、后序写未开始
        let probe1 = p1.clone();
        let probe3 = p3.clone();
        let bulk_path = tmp.join("bulk.md");
        queue
            .bulk(move || {
                assert_eq!(std::fs::read_to_string(&probe1).unwrap(), "1");
                assert!(!probe3.exists());
                std::fs::write(&bulk_path, "bulk").unwrap();
                Ok(())
            })
            .await
            .unwrap();
        queue.write(p3.clone(), "3".into()).await.unwrap();
        assert_eq!(std::fs::read_to_string(p2).unwrap_or_default(), "");
        assert_eq!(std::fs::read_to_string(tmp.join("bulk.md")).unwrap(), "bulk");
        // 闭包错误原样回执
        let err: Result<(), String> = queue.bulk(|| Err("失败".into())).await;
        assert_eq!(err.unwrap_err(), "失败");
    }

    #[tokio::test]
    async fn bulk_job_panic_returns_err_and_queue_survives() {
        let tmp = crate::vault::test_support::TempDir::new("wq-panic");
        let queue = WriteQueue::new();
        // 闭包 panic 被拦截为 Err 回执，不杀死常驻 worker
        let err: Result<(), String> = queue.bulk(|| -> Result<(), String> { panic!("boom") }).await;
        assert_eq!(err.unwrap_err(), "内部写作业异常终止");
        // panic 后队列仍可用：后续写盘照常落地
        let path = tmp.join("after.md");
        queue.write(path.clone(), "ok".into()).await.unwrap();
        assert_eq!(read(path).await, "ok");
    }

    /// 从既有文件落库是复制：目标逐字节等于源文件，源文件原样保留（由调用方自行清理）。
    #[tokio::test]
    async fn write_from_file_copies_and_leaves_source() {
        let tmp = crate::vault::test_support::TempDir::new("wq-from-file");
        let source = tmp.join("staged.bin");
        let target = tmp.join("nested/target.bin");
        let bytes: Vec<u8> = (0..64 * 1024).map(|i| (i % 251) as u8).collect();
        std::fs::write(&source, &bytes).unwrap();
        let queue = WriteQueue::new();
        let created = queue
            .write_from_file(target.clone(), source.clone())
            .await
            .unwrap();
        assert!(created, "目标原先不存在");
        assert_eq!(std::fs::read(&target).unwrap(), bytes, "内容按源文件原样落库");
        assert_eq!(std::fs::read(&source).unwrap(), bytes, "源文件保持原样");
    }

    /// 源文件缺失：回执错误、目标保持原内容、临时文件不残留。
    #[tokio::test]
    async fn write_from_file_missing_source_keeps_target() {
        let tmp = crate::vault::test_support::TempDir::new("wq-from-file-missing");
        let target = tmp.join("target.bin");
        std::fs::write(&target, b"old").unwrap();
        let queue = WriteQueue::new();
        let err = queue
            .write_from_file(target.clone(), tmp.join("missing.part"))
            .await;
        assert!(err.is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"old", "失败不得改动目标");
        let leftover: Vec<_> = std::fs::read_dir(&*tmp)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftover.is_empty(), "失败路径必须清掉临时文件");
    }

    #[tokio::test]
    async fn drain_waits_until_prior_writes_land() {
        let tmp = crate::vault::test_support::TempDir::new("wq-drain");
        let queue = WriteQueue::new();
        for i in 0..8 {
            let path = tmp.join(&format!("{i}.md"));
            queue.write(path, format!("{i}")).await.unwrap();
        }
        queue.drain().await.unwrap();
        for i in 0..8 {
            assert_eq!(read(tmp.join(&format!("{i}.md"))).await, i.to_string());
        }
    }

    #[tokio::test]
    async fn byte_writes_execute_in_submit_order_with_text() {
        let tmp = crate::vault::test_support::TempDir::new("wq-bytes");
        let queue = WriteQueue::new();
        // 文本写与字节写混排：各自落地且 FIFO 相对顺序成立
        queue.write(tmp.join("a.md"), "文本".into()).await.unwrap();
        queue.write_bytes(tmp.join("img.bin"), vec![1, 2, 3]).await.unwrap();
        queue.drain().await.unwrap();
        assert_eq!(read(tmp.join("a.md")).await, "文本");
        let bytes = tokio::task::spawn_blocking(move || std::fs::read(tmp.join("img.bin")).unwrap())
            .await
            .unwrap();
        assert_eq!(bytes, vec![1, 2, 3]);
    }

    /// 「是否新建」按执行序判定：同一路径连发两次时首个为新建、其后为覆盖，
    /// 与提交那一刻的磁盘状态无关（提交时两次都看不到该文件）。
    #[tokio::test]
    async fn created_flag_follows_execution_order_not_submit_state() {
        let tmp = crate::vault::test_support::TempDir::new("wq-created");
        let target = tmp.join("nested/a.bin");
        let queue = WriteQueue::new();
        let (first, second) = tokio::join!(
            queue.write_reporting_created(target.clone(), "1".into()),
            queue.write_reporting_created(target.clone(), "2".into()),
        );
        assert!(first.unwrap(), "首个写入报告新建");
        assert!(!second.unwrap(), "排在其后的写入报告覆盖");
        assert_eq!(read(target).await, "2", "后写者胜");

        let bytes_created = queue
            .write_bytes_reporting_created(tmp.join("b.bin"), vec![0])
            .await
            .unwrap();
        assert!(bytes_created);
        let source = tmp.join("c.part");
        std::fs::write(&source, b"c").unwrap();
        let from_file_created = queue
            .write_from_file(tmp.join("c.bin"), source)
            .await
            .unwrap();
        assert!(from_file_created, "字节写与文件写同一判定口径");
    }
}
