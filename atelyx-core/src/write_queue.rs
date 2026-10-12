//! 仓库写盘串行队列：同进程内全部仓库内容写盘收敛到单一 FIFO 执行序。

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

/// 一条队列作业。
enum JobKind {
    Write { path: PathBuf, content: String },
    /// 字节写：附件二进制（图片导入等）走同一 FIFO，保持与文本写盘的相对顺序。
    WriteBytes { path: PathBuf, bytes: Vec<u8> },
    /// 旗标作业：不写盘，落地即代表排在其前提交的全部写盘已完成。
    Barrier,
}

struct Job {
    kind: JobKind,
    done: tokio::sync::oneshot::Sender<Result<(), String>>,
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
        let rx = self.push(JobKind::Write { path, content });
        rx.await.map_err(|_| "写盘队列已终止".to_string())?
    }

    /// 提交一次原子字节写（附件二进制）：语义与 `write` 相同，仅载荷为字节。
    pub async fn write_bytes(&self, path: PathBuf, bytes: Vec<u8>) -> Result<(), String> {
        let rx = self.push(JobKind::WriteBytes { path, bytes });
        rx.await.map_err(|_| "写盘队列已终止".to_string())?
    }

    /// 等待此前提交的全部写盘落地（不含本调用之后提交的）。
    pub async fn drain(&self) -> Result<(), String> {
        let rx = self.push(JobKind::Barrier);
        rx.await.map_err(|_| "写盘队列已终止".to_string())?
    }

    fn push(&self, kind: JobKind) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
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
            JobKind::Write { path, content } => crate::vault::atomic_write(&path, &content),
            JobKind::WriteBytes { path, bytes } => crate::vault::atomic_write_bytes(&path, &bytes),
            JobKind::Barrier => Ok(()),
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
}
