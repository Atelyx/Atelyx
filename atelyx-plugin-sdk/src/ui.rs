//! 界面树托管：插件为每个面挂一个「状态 → 树」构建器，状态变化后按面通知构建并提交。
//! 构建结果与上次提交相同则不再上行；面重新进入布局时由连接层强制重发。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::protocol::envelope::RpcError;
use crate::protocol::view::ViewNode;
use crate::Context;

pub(crate) type Registry = Mutex<HashMap<String, Arc<Slot>>>;

pub(crate) struct Slot {
    build: Mutex<Box<dyn Fn() -> ViewNode + Send + Sync>>,
    /// 面级门：构建与提交串行在此；`Some` = 上次成功提交的树。
    last: tokio::sync::Mutex<Option<ViewNode>>,
}

/// 界面树托管句柄（经 [`Context::ui`] 取得）：每个面各自挂构建器，按面通知提交。
pub struct Ui<'a> {
    ctx: &'a Context,
}

impl<'a> Ui<'a> {
    pub(crate) fn new(ctx: &'a Context) -> Self {
        Self { ctx }
    }

    /// 为一个面挂上构建器；同一面重复挂载即替换，等该面在途提交收场后才换上新构建器。
    /// 构建器在 [`Ui::notify`] 持面级门时调用：须同步读取状态（异步来源先落到同步可读的缓存），
    /// 且不得再调 `mount` / `notify`——同一面的门不可重入，会互等。
    pub async fn mount<F>(&self, surface_id: &str, build: F)
    where
        F: Fn() -> ViewNode + Send + Sync + 'static,
    {
        if let Some(slot) = self.ctx.ui_slot(surface_id) {
            let mut last = slot.last.lock().await;
            *slot.build.lock().unwrap() = Box::new(build);
            *last = None;
        } else {
            let slot = Slot {
                build: Mutex::new(Box::new(build)),
                last: tokio::sync::Mutex::new(None),
            };
            self.ctx
                .ui_registry()
                .lock()
                .unwrap()
                .insert(surface_id.into(), Arc::new(slot));
        }
    }

    /// 构建并提交该面的树：与上次成功提交的树相同则不发，返回 `false`。
    /// 同一面的并发通知在门内逐个构建与提交，后到的构建读到最新状态，最后提交的树即最终状态。
    pub async fn notify(&self, surface_id: &str) -> Result<bool, RpcError> {
        let slot = self.slot(surface_id)?;
        self.submit(surface_id, slot, false).await
    }

    /// 面重新进入布局：宿主可能已丢掉该面的树，跳过相同判定强制重发；未挂构建器时不动作。
    pub(crate) async fn resubmit(&self, surface_id: &str) {
        let Some(slot) = self.ctx.ui_slot(surface_id) else {
            return;
        };
        if let Err(e) = self.submit(surface_id, slot, true).await {
            eprintln!("重发面 {surface_id} 的界面失败：{e}");
        }
    }

    fn slot(&self, surface_id: &str) -> Result<Arc<Slot>, RpcError> {
        self.ctx
            .ui_slot(surface_id)
            .ok_or_else(|| RpcError::failed(format!("面 {surface_id} 未挂载界面构建器")))
    }

    async fn submit(&self, surface_id: &str, slot: Arc<Slot>, force: bool) -> Result<bool, RpcError> {
        let mut last = slot.last.lock().await;
        let tree = {
            let build = slot.build.lock().unwrap();
            build()
        };
        if !force && last.as_ref() == Some(&tree) {
            return Ok(false);
        }
        self.ctx.submit_view(surface_id, tree.clone()).await?;
        *last = Some(tree);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use serde_json::{json, Value};
    use tokio::sync::mpsc;

    use super::*;
    use crate::protocol::envelope::{Envelope, RequestId};
    use atelyx_view::view_builders::text;

    fn test_context() -> (Context, mpsc::Receiver<Envelope>) {
        let (tx, rx) = mpsc::channel(8);
        (Context::new(tx), rx)
    }

    async fn next_request(rx: &mut mpsc::Receiver<Envelope>) -> (u64, Value) {
        let envelope = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("1s 内应发出请求")
            .expect("通道应存活");
        let Envelope::Request(request) = envelope else {
            panic!("插件只发请求");
        };
        let RequestId::Num(id) = request.id else {
            panic!("请求 id 应为数字");
        };
        (id, request.params)
    }

    fn reply(ctx: &Context, id: u64, result: Result<Value, RpcError>) {
        ctx.remove_pending(id)
            .expect("应有在途调用")
            .send(result)
            .expect("调用方应仍在等");
    }

    async fn answer_ok(ctx: &Context, rx: &mut mpsc::Receiver<Envelope>) -> Value {
        let (id, params) = next_request(rx).await;
        reply(ctx, id, Ok(json!({ "applied": true })));
        params
    }

    fn notify_in_background(ctx: &Context) -> tokio::task::JoinHandle<Result<bool, RpcError>> {
        let ctx = ctx.clone();
        tokio::spawn(async move { ctx.ui().notify("panel").await })
    }

    #[tokio::test]
    async fn notify_sends_then_dedups_identical_tree() {
        let (ctx, mut rx) = test_context();
        ctx.ui().mount("panel", || text("root", "同")).await;

        let task = notify_in_background(&ctx);
        let params = answer_ok(&ctx, &mut rx).await;
        assert_eq!(params["surfaceId"], json!("panel"));
        assert_eq!(params["tree"]["id"], json!("root"));
        assert!(task.await.unwrap().unwrap());

        assert!(!ctx.ui().notify("panel").await.unwrap());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn notify_resends_after_state_change() {
        let state = Arc::new(AtomicU32::new(0));
        let (ctx, mut rx) = test_context();
        let build_state = state.clone();
        ctx.ui()
            .mount("panel", move || text("root", format!("v{}", build_state.load(Ordering::SeqCst))))
            .await;

        let task = notify_in_background(&ctx);
        answer_ok(&ctx, &mut rx).await;
        assert!(task.await.unwrap().unwrap());

        state.store(1, Ordering::SeqCst);
        let task = notify_in_background(&ctx);
        let params = answer_ok(&ctx, &mut rx).await;
        assert_eq!(params["tree"]["content"], json!("v1"));
        assert!(task.await.unwrap().unwrap());
    }

    #[tokio::test]
    async fn resubmit_forces_identical_tree() {
        let (ctx, mut rx) = test_context();
        ctx.ui().mount("panel", || text("root", "同")).await;

        let task = notify_in_background(&ctx);
        answer_ok(&ctx, &mut rx).await;
        assert!(task.await.unwrap().unwrap());

        let ctx_for_task = ctx.clone();
        let task = tokio::spawn(async move { ctx_for_task.ui().resubmit("panel").await });
        let params = answer_ok(&ctx, &mut rx).await;
        assert_eq!(params["tree"]["content"], json!("同"));
        task.await.unwrap();
    }

    #[tokio::test]
    async fn resubmit_unmounted_surface_is_noop() {
        let (ctx, mut rx) = test_context();
        ctx.ui().resubmit("panel").await;
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn notify_unmounted_surface_errors() {
        let (ctx, _rx) = test_context();
        let err = ctx.ui().notify("panel").await.unwrap_err();
        assert!(err.message.contains("未挂载"), "{}", err.message);
    }

    #[tokio::test]
    async fn failed_submit_does_not_poison_cache() {
        let (ctx, mut rx) = test_context();
        ctx.ui().mount("panel", || text("root", "同")).await;

        let task = notify_in_background(&ctx);
        let (id, _) = next_request(&mut rx).await;
        reply(&ctx, id, Err(RpcError::failed("宿主拒绝")));
        assert!(task.await.unwrap().is_err());

        let task = notify_in_background(&ctx);
        answer_ok(&ctx, &mut rx).await;
        assert!(task.await.unwrap().unwrap());
    }

    #[tokio::test]
    async fn remount_replaces_builder() {
        let (ctx, mut rx) = test_context();
        ctx.ui().mount("panel", || text("root", "旧")).await;
        let task = notify_in_background(&ctx);
        answer_ok(&ctx, &mut rx).await;
        assert!(task.await.unwrap().unwrap());

        ctx.ui().mount("panel", || text("root", "新")).await;
        let task = notify_in_background(&ctx);
        let params = answer_ok(&ctx, &mut rx).await;
        assert_eq!(params["tree"]["content"], json!("新"));
        assert!(task.await.unwrap().unwrap());
    }

    #[tokio::test]
    async fn remount_waits_for_in_flight_submit() {
        let (ctx, mut rx) = test_context();
        ctx.ui().mount("panel", || text("root", "旧")).await;
        let old = notify_in_background(&ctx);
        let (id, params) = next_request(&mut rx).await;
        assert_eq!(params["tree"]["content"], json!("旧"));

        let ctx_for_task = ctx.clone();
        let remount = tokio::spawn(async move { ctx_for_task.ui().mount("panel", || text("root", "新")).await });
        tokio::task::yield_now().await;
        assert!(!remount.is_finished(), "在途提交未收场前不应完成重挂载");

        reply(&ctx, id, Ok(json!({ "applied": true })));
        assert!(old.await.unwrap().unwrap());
        remount.await.unwrap();

        let task = notify_in_background(&ctx);
        let params = answer_ok(&ctx, &mut rx).await;
        assert_eq!(params["tree"]["content"], json!("新"));
        assert!(task.await.unwrap().unwrap());
    }

    #[tokio::test]
    async fn concurrent_notify_builds_from_latest_state() {
        let state = Arc::new(AtomicU32::new(0));
        let (ctx, mut rx) = test_context();
        let build_state = state.clone();
        ctx.ui()
            .mount("panel", move || text("root", format!("v{}", build_state.load(Ordering::SeqCst))))
            .await;

        let first = notify_in_background(&ctx);
        let (id, params) = next_request(&mut rx).await;
        assert_eq!(params["tree"]["content"], json!("v0"));

        state.store(1, Ordering::SeqCst);
        let second = notify_in_background(&ctx);
        reply(&ctx, id, Ok(json!({ "applied": true })));
        assert!(first.await.unwrap().unwrap());

        let params = answer_ok(&ctx, &mut rx).await;
        assert_eq!(params["tree"]["content"], json!("v1"));
        assert!(second.await.unwrap().unwrap());
    }
}
