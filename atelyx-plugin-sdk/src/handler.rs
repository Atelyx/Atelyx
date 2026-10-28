//! 宿主通知的插件侧处理面：一个 trait 承接全部 `host:` 通知，未知通知落到 stderr（失败可见）。

use serde_json::Value;

use crate::context::Context;
use crate::protocol::event::PublishParams;
use crate::protocol::frame::{FrameInputParams, FrameTickParams};
use crate::protocol::view::ViewEvent;
use crate::protocol::method::{
    CanvasFrameParams, CanvasResizedParams, ChatEventParams, CollabMessageParams, DocCloseParams,
    DocOpenParams, ProcessEventParams, ShortcutTriggeredParams, SurfaceAttachedParams,
    SurfaceDetachedParams, ToolCallParams, TrayClickParams, ViewEventParams, WindowClosedParams,
    WindowIdParams,
};

/// 宿主通知处理面。全部 `host:` 通知按方法名分派；实现只需覆盖关心的项。
/// 回调按通知到达顺序逐个执行：读线程只入队，单个 worker 顺序消费，因此同一份状态不必
/// 自己加锁；代价是单个回调卡住会挡住后续通知（弹对话框 / 长请求这类阻塞调用要么容忍
/// 整体暂停，要么在回调里另起任务自己驱动）。回调里仍可再经 [`Context`] 调宿主——响应
/// 配对在读线程上，与回调不会互等。
/// 顺序约定：`on_ready` 之前的宿主通知可能先于它执行——需要初始化状态的回调必须容忍
/// 「尚未 ready」的情形。
#[async_trait::async_trait]
pub trait PluginHandler: Send + Sync + 'static {
    /// 握手完成后调用一次：提交初始界面树、声明订阅都应在这里做。
    async fn on_ready(&self, ctx: &Context);

    async fn on_view_event(&self, _ctx: &Context, _params: ViewEventParams) {}
    /// 面重新进入布局（面板被添加 / 插件重启后重发）。已挂构建器时 SDK 在回调前强制重发树；
    /// 画布清单等附加内容在此补交。
    async fn on_surface_attached(&self, _ctx: &Context, _params: SurfaceAttachedParams) {}
    async fn on_surface_detached(&self, _ctx: &Context, _params: SurfaceDetachedParams) {}
    async fn on_host_event(&self, _ctx: &Context, _params: PublishParams) {}
    async fn on_process_event(&self, _ctx: &Context, _params: ProcessEventParams) {}
    async fn on_shortcut(&self, _ctx: &Context, _params: ShortcutTriggeredParams) {}
    async fn on_doc_open(&self, _ctx: &Context, _params: DocOpenParams) {}
    async fn on_doc_close(&self, _ctx: &Context, _params: DocCloseParams) {}
    async fn on_window_closed(&self, _ctx: &Context, _params: WindowClosedParams) {}
    /// 窗口因失焦被收起（`host:window.hidden`）：需要再唤起时经 [`Context::window_show`] 重新显示。
    async fn on_window_hidden(&self, _ctx: &Context, _params: WindowIdParams) {}
    /// 画布节点的像素尺寸变化（`host:canvas.resized`）：按新尺寸对齐绘制坐标与滚动范围。
    async fn on_canvas_resized(&self, _ctx: &Context, _params: CanvasResizedParams) {}
    /// 画布帧拍（`host:canvas.frame`）：节点声明了 `fps` 才有，收到后提交这一帧的绘制内容。
    /// 帧拍不触发界面树自动提交（逐帧重建树是纯浪费），画布内容经 `submit_canvas` / `patch_canvas` 更新。
    async fn on_canvas_frame(&self, _ctx: &Context, _params: CanvasFrameParams) {}
    /// 用户点击了本插件的某个托盘条目（`host:tray.click`）。
    async fn on_tray_click(&self, _ctx: &Context, _params: TrayClickParams) {}
    /// 同空间的对等插件发来消息（`host:collab.message`）。
    async fn on_collab_message(&self, _ctx: &Context, _params: CollabMessageParams) {}
    /// 对话事件流（`host:ai.chat.event`）：快照节流拍 + 终态 + 命名结果。
    async fn on_chat_event(&self, _ctx: &Context, _params: ChatEventParams) {}
    /// 整面帧的原始输入（`host:frame.input`）：指针 / 滚轮 / 键盘 / 输入法 / 焦点 / 尺寸。
    /// 帧输入不触发界面树自动提交——状态变化后由插件自行 `submit_frame`。
    async fn on_frame_input(&self, _ctx: &Context, _params: FrameInputParams) {}
    /// 整面帧的帧拍（`host:frame.tick`）：帧声明了 `fps` 才有，两拍之间应提交新内容。
    async fn on_frame_tick(&self, _ctx: &Context, _params: FrameTickParams) {}

    /// 工具调用（`host:ai.toolCall`）：须用同一 `callId` 经 [`Context::tool_result`] 回报结果。
    /// 默认回报「未处理」错误，宿主不必等到调用超时。
    async fn on_tool_call(&self, ctx: &Context, params: ToolCallParams) {
        let _ = ctx
            .tool_result(&params.call_id, "插件未处理工具调用", true)
            .await;
    }

    /// 未知通知：默认写 stderr（宿主把插件 stderr 归入诊断输出）。
    async fn on_unknown_notification(&self, _ctx: &Context, method: &str, params: Value) {
        eprintln!("插件收到未知的宿主通知：{method}：{params}");
    }
}

/// 节点事件：处理完自动重建并提交该面，插件不必在回调末尾手动 `notify`。
async fn view_event<H: PluginHandler + ?Sized>(handler: &H, ctx: &Context, params: ViewEventParams) {
    let surface_id = params.surface_id.clone();
    let bulk = is_high_rate(&params.event);
    handler.on_view_event(ctx, params).await;
    if !bulk {
        auto_submit(ctx, &surface_id).await;
    }
}

/// 逐帧到达的事件（滚动位置、画布指针）不触发整树重建：它们几乎不改树结构，
/// 而拖动 / 滚动期间按帧到达，逐帧重建与全树比较是纯浪费。需要重发时插件自己调 `notify`。
fn is_high_rate(event: &ViewEvent) -> bool {
    matches!(event, ViewEvent::Scroll { .. } | ViewEvent::Canvas { .. })
}

/// 画布尺寸变化：处理完同样自动提交（重排界面树是这类回调的常见需求；尺寸变化频率低）。
async fn canvas_resized<H: PluginHandler + ?Sized>(handler: &H, ctx: &Context, params: CanvasResizedParams) {
    let surface_id = params.surface_id.clone();
    handler.on_canvas_resized(ctx, params).await;
    auto_submit(ctx, &surface_id).await;
}

/// 事件处理返回后按托管构建器重建并提交该面：与上次提交相同则不发（去重由托管层做）。
/// 面没挂构建器（插件只提交过静态树）时静默跳过——没有构建器就没有「自动渲染」可言。
/// 失败只落 stderr（宿主把插件 stderr 归入 `runtime.log`），原因随错误文本带出。
async fn auto_submit(ctx: &Context, surface_id: &str) {
    if ctx.ui_slot(surface_id).is_none() {
        return;
    }
    if let Err(e) = ctx.ui().notify(surface_id).await {
        eprintln!("面 {surface_id} 自动提交失败：{e}");
    }
}

/// 面重新进入布局：宿主可能已丢掉该面的树，先按托管构建器强制重发（未挂载则跳过），
/// 插件回调里的附加提交（画布清单）才有所依托。
async fn on_attached<H: PluginHandler + ?Sized>(
    handler: &H,
    ctx: &Context,
    params: SurfaceAttachedParams,
) {
    ctx.ui().resubmit(&params.surface_id).await;
    let surface_id = params.surface_id.clone();
    handler.on_surface_attached(ctx, params).await;
    // 回调里初始化的状态同样要上屏（重发过的树与之相同则去重不发）
    auto_submit(ctx, &surface_id).await;
}

/// 按方法名分派一条宿主通知。params 解析失败按未知通知处理并注明原因（不静默丢弃）。
pub(crate) async fn dispatch<H: PluginHandler + ?Sized>(
    handler: &H,
    ctx: &Context,
    method: &str,
    params: Value,
) {
    use crate::protocol::event;
    use crate::protocol::method as m;

    macro_rules! typed {
        ($ty:ty, $call:expr) => {
            match serde_json::from_value::<$ty>(params) {
                Ok(p) => $call(p).await,
                Err(e) => {
                    let note = format!("params 不可解析：{e}");
                    handler.on_unknown_notification(ctx, method, Value::String(note)).await
                }
            }
        };
    }

    match method {
        m::view::HOST_EVENT => typed!(ViewEventParams, |p| view_event(handler, ctx, p)),
        m::view::HOST_ATTACHED => typed!(SurfaceAttachedParams, |p| on_attached(handler, ctx, p)),
        m::view::HOST_DETACHED => typed!(SurfaceDetachedParams, |p| handler.on_surface_detached(ctx, p)),
        event::HOST_PUBLISH => typed!(PublishParams, |p| handler.on_host_event(ctx, p)),
        m::process::HOST_EVENT => typed!(ProcessEventParams, |p| handler.on_process_event(ctx, p)),
        m::shortcut::HOST_TRIGGERED => typed!(ShortcutTriggeredParams, |p| handler.on_shortcut(ctx, p)),
        m::doc::HOST_OPEN => typed!(DocOpenParams, |p| handler.on_doc_open(ctx, p)),
        m::doc::HOST_CLOSE => typed!(DocCloseParams, |p| handler.on_doc_close(ctx, p)),
        m::window::HOST_CLOSED => typed!(WindowClosedParams, |p| handler.on_window_closed(ctx, p)),
        m::window::HOST_HIDDEN => typed!(WindowIdParams, |p| handler.on_window_hidden(ctx, p)),
        m::canvas::HOST_RESIZED => typed!(CanvasResizedParams, |p| canvas_resized(handler, ctx, p)),
        m::canvas::HOST_FRAME => typed!(CanvasFrameParams, |p| handler.on_canvas_frame(ctx, p)),
        m::frame::HOST_INPUT => typed!(FrameInputParams, |p| handler.on_frame_input(ctx, p)),
        m::frame::HOST_TICK => typed!(FrameTickParams, |p| handler.on_frame_tick(ctx, p)),
        m::tray::HOST_CLICK => typed!(TrayClickParams, |p| handler.on_tray_click(ctx, p)),
        m::collab::HOST_MESSAGE => typed!(CollabMessageParams, |p| handler.on_collab_message(ctx, p)),
        m::ai::HOST_CHAT_EVENT => typed!(ChatEventParams, |p| handler.on_chat_event(ctx, p)),
        m::ai::HOST_TOOL_CALL => typed!(ToolCallParams, |p| handler.on_tool_call(ctx, p)),
        other => handler.on_unknown_notification(ctx, other, params).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use tokio::sync::mpsc;

    use crate::protocol::envelope::Envelope;
    use crate::protocol::view::ViewEvent;
    use atelyx_view::view_builders::text;

    /// 事件回调里什么都不做：验证自动提交替插件把树重发出去。
    struct Passive;

    #[async_trait::async_trait]
    impl PluginHandler for Passive {
        async fn on_ready(&self, _ctx: &Context) {}
        async fn on_view_event(&self, _ctx: &Context, _params: ViewEventParams) {}
    }

    async fn next_request(rx: &mut mpsc::Receiver<Envelope>) -> (u64, String, Value) {
        let envelope = rx.try_recv().expect("应已发出请求");
        let Envelope::Request(request) = envelope else {
            panic!("插件只发请求");
        };
        (request.id, request.method, request.params)
    }

    fn reply(ctx: &Context, id: u64) {
        ctx.remove_pending(id)
            .expect("应有在途调用")
            .send(Ok(serde_json::json!({ "applied": true })))
            .expect("调用方应仍在等");
    }

    #[tokio::test]
    async fn view_event_auto_submits_the_surface() {
        let (tx, mut rx) = mpsc::channel(8);
        let ctx = Context::new(tx);
        ctx.ui().mount("panel", || text("root", "同")).await;

        let params = serde_json::json!({
            "surfaceId": "panel",
            "nodeId": "root",
            "event": ViewEvent::Click,
        });
        let ctx_for_task = ctx.clone();
        let task = tokio::spawn(async move {
            dispatch(&Passive, &ctx_for_task, crate::protocol::method::view::HOST_EVENT, params).await;
        });
        tokio::task::yield_now().await;

        let (id, method, params) = next_request(&mut rx).await;
        assert_eq!(method, crate::protocol::method::view::SUBMIT, "事件回调后应自动提交该面");
        assert_eq!(params["surfaceId"], serde_json::json!("panel"));
        reply(&ctx, id);
        task.await.unwrap();

        // 状态没变：再收到同一事件时不重复上行（托管层按树去重）
        let params = serde_json::json!({
            "surfaceId": "panel",
            "nodeId": "root",
            "event": ViewEvent::Click,
        });
        let ctx_for_task = ctx.clone();
        let task = tokio::spawn(async move {
            dispatch(&Passive, &ctx_for_task, crate::protocol::method::view::HOST_EVENT, params).await;
        });
        task.await.unwrap();
        assert!(rx.try_recv().is_err(), "树未变则不再上行");
    }

    /// 回调里改的状态要能上屏：自动提交必须读到回调之后的状态。
    struct Counter(std::sync::Arc<std::sync::atomic::AtomicU32>);

    #[async_trait::async_trait]
    impl PluginHandler for Counter {
        async fn on_ready(&self, _ctx: &Context) {}
        async fn on_view_event(&self, _ctx: &Context, _params: ViewEventParams) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn handler_state_change_reaches_the_host() {
        let count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let build_count = count.clone();
        let (tx, mut rx) = mpsc::channel(8);
        let ctx = Context::new(tx);
        ctx.ui()
            .mount("panel", move || {
                text("root", format!("v{}", build_count.load(std::sync::atomic::Ordering::SeqCst)))
            })
            .await;

        // 回调递增的计数就是构建器读的那个：树内容能证明「读到了回调之后的状态」
        let handler = Counter(count.clone());
        let params = serde_json::json!({
            "surfaceId": "panel",
            "nodeId": "root",
            "event": ViewEvent::Click,
        });
        let ctx_for_task = ctx.clone();
        let task = tokio::spawn(async move {
            dispatch(&handler, &ctx_for_task, crate::protocol::method::view::HOST_EVENT, params).await;
        });
        tokio::task::yield_now().await;

        let (id, method, params) = next_request(&mut rx).await;
        assert_eq!(method, crate::protocol::method::view::SUBMIT);
        assert_eq!(params["tree"]["content"], serde_json::json!("v1"), "构建器读的是回调之后的状态");
        reply(&ctx, id);
        task.await.unwrap();
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1, "回调应被调用一次");
    }

    /// 逐帧到达的事件不重建整树（要重发由插件自己 notify）。
    #[tokio::test]
    async fn high_rate_events_do_not_rebuild_the_tree() {
        let (tx, mut rx) = mpsc::channel(8);
        let ctx = Context::new(tx);
        ctx.ui().mount("panel", || text("root", "同")).await;
        for event in [
            ViewEvent::Scroll { offset: 12.0, max_offset: 200.0 },
            ViewEvent::Canvas {
                pointer: crate::protocol::view::CanvasPointer::Move { x: 1.0, y: 2.0 },
            },
        ] {
            let params = serde_json::json!({
                "surfaceId": "panel",
                "nodeId": "root",
                "event": event,
            });
            dispatch(&Passive, &ctx, crate::protocol::method::view::HOST_EVENT, params).await;
        }
        assert!(rx.try_recv().is_err(), "滚动与画布指针不上行整树");
    }

    /// 帧拍只走画布回调，不触发界面树提交（逐帧重建树是纯浪费）。
    struct FrameWatcher(std::sync::atomic::AtomicUsize);

    #[async_trait::async_trait]
    impl PluginHandler for FrameWatcher {
        async fn on_ready(&self, _ctx: &Context) {}
        async fn on_canvas_frame(&self, _ctx: &Context, params: CanvasFrameParams) {
            assert_eq!(params.frame, 3);
            assert_eq!(params.node_id, "board");
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn canvas_frame_maps_to_its_own_callback() {
        let (tx, mut rx) = mpsc::channel(8);
        let ctx = Context::new(tx);
        ctx.ui().mount("panel", || text("root", "同")).await;
        let watcher = FrameWatcher(std::sync::atomic::AtomicUsize::new(0));
        dispatch(
            &watcher,
            &ctx,
            crate::protocol::method::canvas::HOST_FRAME,
            serde_json::json!({ "surfaceId": "panel", "nodeId": "board", "frame": 3, "deltaMs": 16 }),
        )
        .await;
        assert_eq!(watcher.0.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(rx.try_recv().is_err(), "帧拍不应触发界面树提交");
    }

    /// 整面帧的原始输入只走自己的回调，不触发界面树提交（帧内容由插件显式提交）。
    struct FrameInputWatcher(std::sync::Mutex<Vec<String>>);

    #[async_trait::async_trait]
    impl PluginHandler for FrameInputWatcher {
        async fn on_ready(&self, _ctx: &Context) {}
        async fn on_frame_input(&self, _ctx: &Context, params: crate::protocol::frame::FrameInputParams) {
            assert_eq!(params.surface_id, "panel");
            let crate::protocol::frame::FrameInput::PointerDown { x, hit, .. } = params.input else {
                panic!("应为指针按下");
            };
            self.0.lock().unwrap().push(format!("{x}:{}", hit.unwrap_or_default()));
        }
    }

    #[tokio::test]
    async fn frame_input_maps_to_its_own_callback() {
        let (tx, mut rx) = mpsc::channel(8);
        let ctx = Context::new(tx);
        ctx.ui().mount("panel", || text("root", "同")).await;
        let watcher = FrameInputWatcher(std::sync::Mutex::new(Vec::new()));
        dispatch(
            &watcher,
            &ctx,
            crate::protocol::method::frame::HOST_INPUT,
            serde_json::json!({
                "surfaceId": "panel",
                "input": { "kind": "pointerDown", "x": 12.0, "y": 8.0, "button": "left", "hit": "inc" },
            }),
        )
        .await;
        assert_eq!(watcher.0.lock().unwrap().as_slice(), ["12:inc"]);
        assert!(rx.try_recv().is_err(), "帧输入不应触发界面树提交");
    }
}
