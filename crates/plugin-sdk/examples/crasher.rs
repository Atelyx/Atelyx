//! 端到端夹具（同进程 cdylib）：会话一建立就 panic——宿主侧应看到「插件会话 panic」，
//! 并按崩溃退避最终熔断。不作为功能样例。

struct Crasher;

#[async_trait::async_trait]
impl atelyx_plugin_sdk::PluginHandler for Crasher {
    async fn on_ready(&self, _ctx: &atelyx_plugin_sdk::Context) {
        panic!("夹具故意 panic");
    }
}

atelyx_plugin_sdk::export_in_process_plugin!(Crasher);
