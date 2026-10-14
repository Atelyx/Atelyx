//! 插件宿主运行时：发现 / 装载 / RPC 泵 / 权限仲裁 / 生命周期 / 安装卸载。
//! 运行时为进程内单例（壳持 `Arc`，多窗口共享）；插件事件经 [`PluginRuntime::subscribe_events`]
//! 的单一广播通道进入壳的 update。进程创建与清理一律经 [`crate::plugin_process`]。

mod capability;
mod child_process;
mod fs;
mod http;
mod install;
mod instance;
mod layout;
mod manifest;
mod shortcut;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use atelyx_plugin_protocol::event;
use atelyx_plugin_protocol::manifest::Manifest;

use crate::plugin_process::PluginProcessHost;

use instance::Instance;

/// 协议类型再导出：壳只依赖 `atelyx-core`，不直接依赖协议 crate。
pub use atelyx_plugin_protocol::event::VaultSwitchPayload;
pub use atelyx_plugin_protocol::manifest::Permission;
pub use atelyx_plugin_protocol::method::{ContributionSlot, FileFilter, NotificationLevel, SurfaceKind};
pub use atelyx_plugin_protocol::view::{
    Align, Axis, ButtonVariant, ColorToken, EventKind, EventValue, ImageFit, ImageSource, MenuItem,
    NodeBody, TextWeight, ViewEvent, ViewNode,
};

/// 插件运行状态。
#[derive(Clone, Debug, PartialEq)]
pub enum PluginStatus {
    /// 未启用或已停用。
    Stopped,
    /// 进程在跑。
    Running,
    /// 崩溃后退避重启中。
    Restarting,
    /// 窗口内崩溃超限，已熔断（需手动重试）。
    CircuitBroken,
    /// 启动失败 / 握手失败 / 不可装载（附可读原因）。
    Failed(String),
}

/// 一条被拒的能力调用（审计记录）。
#[derive(Clone, Debug)]
pub struct AuditRecord {
    pub method: String,
    /// 调用携带的路径（无则空串）。
    pub detail: String,
    /// 记录时刻（unix 秒）。
    pub at: u64,
}

/// 运行时 → 壳的广播事件（状态 / 通知 / 拒调用 / 界面与贡献变化；允许落后丢帧）。
#[derive(Clone, Debug)]
pub enum PluginEvent {
    StatusChanged { id: String, status: PluginStatus },
    Notification { id: String, title: String, body: Option<String>, level: NotificationLevel },
    PermissionDenied { id: String, method: String, detail: String },
    /// 某插件提交了新的界面树（树本身经 [`PluginRuntime::surface_tree`] 取；丢帧不影响最终渲染）。
    ViewSubmitted { id: String },
    /// 某插件的贡献注册表变化（注册 / 撤销 / 随会话收尾清空）。
    ContributionsChanged { id: String },
}

/// 一个已注册的贡献点（菜单与标签渲染用）。
#[derive(Clone, Debug, PartialEq)]
pub struct Contribution {
    pub plugin_id: String,
    pub slot: ContributionSlot,
    /// 贡献 id（插件内唯一、不含 `:`），同时是该面挂载后的 `surfaceId`。
    pub id: String,
    pub label: String,
    /// 宿主图标形状表里的图标名；插件未声明时为 `None`（宿主回落拼图）。
    pub icon: Option<String>,
}

/// 对话框种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogKind {
    File,
    Directory,
}

/// 对话框请求（运行时 → 壳）：壳按此弹系统对话框，用户的选择经 [`PluginRuntime::resolve_dialog`]
/// 回填给等待中的插件调用。
#[derive(Clone, Debug)]
pub struct DialogRequest {
    pub kind: DialogKind,
    pub title: Option<String>,
    /// 文件对话框是否为「保存」形态（目录对话框恒 false）。
    pub save: bool,
    pub default_path: Option<String>,
    pub filters: Vec<FileFilter>,
}

/// 运行时 → 壳的服务请求。与 [`PluginEvent`] 广播分属两条通道：服务请求必须无损送达——丢一条
/// 会让插件的调用永久悬挂，故走无界通道而非可落后的广播。
#[derive(Clone, Debug)]
pub enum HostRequest {
    Dialog { plugin_id: String, request_id: u64, dialog: DialogRequest },
}

/// 插件列表项（设置页渲染用）。
#[derive(Clone, Debug)]
pub struct PluginSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api_version: u32,
    pub permissions: Vec<Permission>,
    pub singleton: bool,
    pub keep_mounted_on_vault_switch: bool,
    /// 是否启用（`.enabled/` 标记存在）。
    pub enabled: bool,
    pub status: PluginStatus,
    /// 不可装载的可读原因（None = 可装载）。
    pub load_error: Option<String>,
    /// 被拒的能力调用条数。
    pub denied: usize,
    /// 已接收的界面树提交次数。
    pub view_submits: u64,
}

/// 磁盘上发现的一个插件。
#[derive(Clone)]
struct Discovered {
    manifest: Result<Manifest, String>,
    dir: PathBuf,
    enabled: bool,
}

/// 对话框结果回传通道（用户选定值；取消 = `None`）。
type DialogReplyTx = std::sync::mpsc::Sender<Result<Option<String>, String>>;

/// 在途对话框等待项：发起插件 id + 结果回传通道。
type DialogWait = (String, DialogReplyTx);

/// 插件宿主运行时。
pub struct PluginRuntime {
    root: PathBuf,
    process_host: Arc<PluginProcessHost>,
    shortcuts: Arc<shortcut::Registrar>,
    events: tokio::sync::broadcast::Sender<PluginEvent>,
    host_requests: tokio::sync::mpsc::UnboundedSender<HostRequest>,
    /// 服务请求接收端（壳侧订阅流持有；见 [`PluginRuntime::recv_host_request`]）。
    host_requests_rx: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<HostRequest>>,
    /// 在途对话框：request_id → 等待项。插件进程的读线程在此等待壳回填结果。
    dialogs: Mutex<HashMap<u64, DialogWait>>,
    next_dialog_id: AtomicU64,
    /// 当前容器身份（见 [`PluginRuntime::on_vault_switch`]）：同一容器重复进入不动作，据此判别首次激活。
    container: Mutex<Option<String>>,
    discovered: Mutex<HashMap<String, Discovered>>,
    instances: Mutex<HashMap<String, Arc<Instance>>>,
}

impl PluginRuntime {
    /// 建立运行时（`root` = 应用数据目录下 `plugins/`）。顺带清理遗留的安装临时目录。
    pub fn new(root: PathBuf) -> Arc<Self> {
        layout::sweep_temp(&root);
        let (events, _) = tokio::sync::broadcast::channel(256);
        let (host_requests, host_requests_rx) = tokio::sync::mpsc::unbounded_channel();
        let runtime = Arc::new(Self {
            root,
            process_host: Arc::new(PluginProcessHost::new()),
            shortcuts: shortcut::Registrar::new(),
            events,
            host_requests,
            host_requests_rx: tokio::sync::Mutex::new(host_requests_rx),
            dialogs: Mutex::new(HashMap::new()),
            next_dialog_id: AtomicU64::new(1),
            container: Mutex::new(None),
            discovered: Mutex::new(HashMap::new()),
            instances: Mutex::new(HashMap::new()),
        });
        // 快捷键触发在 OS 消息线程上到达：经注册器的派发线程回查归属后转投对应插件
        let weak = Arc::downgrade(&runtime);
        runtime.shortcuts.set_sink(Arc::new(move |plugin_id, accelerator| {
            if let Some(runtime) = weak.upgrade() {
                runtime.notify_shortcut(&plugin_id, &accelerator);
            }
        }));
        runtime
    }

    /// 订阅运行时事件；壳把它接成订阅流（内层用广播，重复订阅 / 订阅重建都安全）。
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<PluginEvent> {
        self.events.subscribe()
    }

    /// 取一条待处理的服务请求（对话框等）；壳把它接成订阅流。返回 `None` = 运行时已销毁。
    /// 无界通道不丢帧：请求被落到这里之前就已在队列里，壳无需担心「收不到」。
    pub async fn recv_host_request(&self) -> Option<HostRequest> {
        self.host_requests_rx.lock().await.recv().await
    }

    /// 登记一条对话框请求并把请求交给壳，返回结果接收端。调用方在结果到达后自行回执调用方——
    /// 对话框由用户决定耗时，不得占住插件进程的输出读线程（否则该期间插件输出与退出上报都被拖住）。
    /// 插件停用 / 崩溃 / 宿主退出会经 [`PluginRuntime::fail_dialogs_of`] / [`PluginRuntime::shutdown`]
    /// 把在途请求以可读失败收场，不会永久悬挂。
    pub(crate) fn open_dialog(
        &self,
        plugin_id: &str,
        dialog: DialogRequest,
    ) -> Result<std::sync::mpsc::Receiver<Result<Option<String>, String>>, String> {
        let request_id = self.next_dialog_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = std::sync::mpsc::channel();
        self.dialogs
            .lock()
            .unwrap()
            .insert(request_id, (plugin_id.to_string(), tx));
        let request = HostRequest::Dialog {
            plugin_id: plugin_id.to_string(),
            request_id,
            dialog,
        };
        if self.host_requests.send(request).is_err() {
            self.dialogs.lock().unwrap().remove(&request_id);
            return Err("对话框不可用：宿主已退出".to_string());
        }
        Ok(rx)
    }

    /// 壳侧回填对话框结果（用户选定路径；取消 = `None`）。未登记的 id 是空操作
    /// （插件已停用 / 已崩溃时在途请求已由 [`PluginRuntime::fail_dialogs_of`] 收场）。
    pub fn resolve_dialog(&self, request_id: u64, path: Option<String>) {
        let waiter = self.dialogs.lock().unwrap().remove(&request_id);
        if let Some((_, tx)) = waiter {
            let _ = tx.send(Ok(path));
        }
    }

    /// 让某插件全部在途对话框以可读失败收场（插件停用 / 失败 / 崩溃时调用）。
    pub(crate) fn fail_dialogs_of(&self, plugin_id: &str, reason: &str) {
        let ids: Vec<u64> = {
            let dialogs = self.dialogs.lock().unwrap();
            dialogs
                .iter()
                .filter(|(_, (owner, _))| owner == plugin_id)
                .map(|(id, _)| *id)
                .collect()
        };
        for id in ids {
            let waiter = self.dialogs.lock().unwrap().remove(&id);
            if let Some((_, tx)) = waiter {
                let _ = tx.send(Err(reason.to_string()));
            }
        }
    }

    /// 让全部在途对话框以可读失败收场（宿主退出收尾；逐插件路径已先收一遍，此处防漏网）。
    fn fail_all_dialogs(&self, reason: &str) {
        let waiters: Vec<std::sync::mpsc::Sender<Result<Option<String>, String>>> = {
            let mut dialogs = self.dialogs.lock().unwrap();
            dialogs.drain().map(|(_, (_, tx))| tx).collect()
        };
        for tx in waiters {
            let _ = tx.send(Err(reason.to_string()));
        }
    }

    /// 扫描磁盘：刷新已安装清单与启用标记，并停掉磁盘上已消失的插件实例。
    pub fn scan(self: &Arc<Self>) {
        let mut discovered = HashMap::new();
        for (name, dir) in layout::discover(&self.root) {
            let manifest = manifest::read_manifest(&dir).and_then(|m| {
                if m.id == name {
                    Ok(m)
                } else {
                    Err(format!("目录名与 manifest id 不一致：目录 {name}，声明 {}", m.id))
                }
            });
            let enabled = layout::is_enabled(&self.root, &name);
            discovered.insert(name.clone(), Discovered { manifest, dir, enabled });
        }
        *self.discovered.lock().unwrap() = discovered;

        let stale: Vec<String> = {
            let discovered = self.discovered.lock().unwrap();
            let instances = self.instances.lock().unwrap();
            instances
                .keys()
                .filter(|id| !discovered.contains_key(id.as_str()))
                .cloned()
                .collect()
        };
        for id in stale {
            let instance = self.instances.lock().unwrap().remove(&id);
            if let Some(instance) = instance {
                instance.stop();
            }
        }
    }

    /// 启动全部已启用插件（启动期与扫描后调用）。
    pub fn start_enabled(self: &Arc<Self>) {
        let targets: Vec<(String, PathBuf, Manifest)> = {
            let discovered = self.discovered.lock().unwrap();
            discovered
                .iter()
                .filter(|(_, d)| d.enabled)
                .filter_map(|(id, d)| {
                    d.manifest.as_ref().ok().map(|m| (id.clone(), d.dir.clone(), m.clone()))
                })
                .collect()
        };
        for (id, dir, manifest) in targets {
            let instance = self.instance_for(&manifest, &dir);
            if let Err(e) = instance.start() {
                eprintln!("[plugin {id}] 启动失败：{e}");
            }
        }
    }

    /// 取某插件某个面的最新界面树（从未提交 / 已随会话收尾清除时为 `None`）。
    pub fn surface_tree(&self, plugin_id: &str, surface_id: &str) -> Option<ViewNode> {
        self.instances
            .lock()
            .unwrap()
            .get(plugin_id)
            .and_then(|i| i.surface_tree(surface_id))
    }

    /// 回发一条界面事件给插件。插件未在跑时静默丢弃（对端已不在）。
    pub fn send_view_event(&self, plugin_id: &str, surface_id: &str, node_id: &str, event: ViewEvent) {
        let instance = self.instances.lock().unwrap().get(plugin_id).cloned();
        if let Some(instance) = instance {
            instance.notify_view_event(surface_id, node_id, event);
        }
    }

    /// 告知插件某个面已挂载（贡献标签出现在布局里）。插件未在跑时静默丢弃。
    pub fn notify_surface_attached(&self, plugin_id: &str, surface_id: &str, kind: SurfaceKind) {
        let instance = self.instances.lock().unwrap().get(plugin_id).cloned();
        if let Some(instance) = instance {
            instance.notify_surface_attached(surface_id, kind);
        }
    }

    /// 告知插件某个面已移除（贡献标签不再出现在布局里；插件重启时宿主会重新 attached）。
    pub fn notify_surface_detached(&self, plugin_id: &str, surface_id: &str) {
        let instance = self.instances.lock().unwrap().get(plugin_id).cloned();
        if let Some(instance) = instance {
            instance.notify_surface_detached(surface_id);
        }
    }

    /// 全部已注册贡献（跨插件汇总，按插件 id + 贡献 id 排序；未运行插件无贡献）。
    pub fn contributions(&self) -> Vec<Contribution> {
        let instances: Vec<Arc<Instance>> =
            self.instances.lock().unwrap().values().cloned().collect();
        let mut all: Vec<Contribution> = instances.iter().flat_map(|i| i.contributions()).collect();
        all.sort_by(|a, b| (&a.plugin_id, &a.id).cmp(&(&b.plugin_id, &b.id)));
        all
    }

    /// 某插件的私有数据目录（`<插件目录>/data`）；未安装 = `None`。壳据此读插件自带资源（界面图片）。
    pub fn plugin_data_dir(&self, plugin_id: &str) -> Option<PathBuf> {
        self.discovered
            .lock()
            .unwrap()
            .contains_key(plugin_id)
            .then(|| layout::data_dir(&self.root, plugin_id))
    }

    /// 列表（按 id 排序）：已安装插件 + 启用态 + 运行状态 + 审计计数。
    pub fn list(&self) -> Vec<PluginSummary> {
        let discovered = self.discovered.lock().unwrap();
        let instances = self.instances.lock().unwrap();
        let mut ids: Vec<&String> = discovered.keys().collect();
        ids.sort();
        ids.into_iter()
            .map(|id| {
                let entry = &discovered[id];
                let instance = instances.get(id);
                let manifest = entry.manifest.as_ref().ok();
                PluginSummary {
                    id: id.clone(),
                    name: manifest.map(|m| m.name.clone()).unwrap_or_default(),
                    version: manifest.map(|m| m.version.clone()).unwrap_or_default(),
                    api_version: manifest.map(|m| m.api_version).unwrap_or(0),
                    permissions: manifest.map(|m| m.permissions.clone()).unwrap_or_default(),
                    singleton: manifest.is_some_and(|m| m.singleton),
                    keep_mounted_on_vault_switch: manifest.is_some_and(|m| m.keep_mounted_on_vault_switch),
                    enabled: entry.enabled,
                    status: instance.map(|i| i.status()).unwrap_or(PluginStatus::Stopped),
                    load_error: entry.manifest.as_ref().err().cloned(),
                    denied: instance.map(|i| i.denied_count()).unwrap_or(0),
                    view_submits: instance.map(|i| i.view_submits()).unwrap_or(0),
                }
            })
            .collect()
    }

    /// 启用 / 停用。启用时不可装载或启动失败即回滚启用态并返回原因。
    pub fn set_enabled(self: &Arc<Self>, id: &str, on: bool) -> Result<(), String> {
        if !on {
            layout::set_enabled(&self.root, id, false)?;
            let instance = self.instances.lock().unwrap().get(id).cloned();
            if let Some(instance) = instance {
                instance.stop();
            }
            self.mark_enabled(id, false);
            return Ok(());
        }
        let (manifest, dir) = self.resolve(id)?;
        layout::set_enabled(&self.root, id, true)?;
        let instance = self.instance_for(&manifest, &dir);
        if let Err(e) = instance.start() {
            let _ = layout::set_enabled(&self.root, id, false);
            return Err(e);
        }
        self.mark_enabled(id, true);
        Ok(())
    }

    /// 熔断后手动重试：清零崩溃计数、恢复启用标记后重启（重试即用户意图启用）。
    pub fn retry(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let (manifest, dir) = self.resolve(id)?;
        let instance = self.instance_for(&manifest, &dir);
        instance.reset_crash_history();
        layout::set_enabled(&self.root, id, true)?;
        self.mark_enabled(id, true);
        instance.start()
    }

    /// 从本地目录安装（安装即启用）。返回安装的插件 id。
    pub fn install_dir(self: &Arc<Self>, src: &Path) -> Result<String, String> {
        let id = install::install_from_dir(&self.root, src)?;
        self.after_install(&id)
    }

    /// 从 zip 安装（安装即启用）。返回安装的插件 id。
    pub fn install_zip(self: &Arc<Self>, zip_path: &Path) -> Result<String, String> {
        let id = install::install_from_zip(&self.root, zip_path)?;
        self.after_install(&id)
    }

    /// 卸载：先停插件，再删安装内容、私有数据与上一代。
    pub fn uninstall(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let instance = self.instances.lock().unwrap().remove(id);
        if let Some(instance) = instance {
            instance.stop();
        }
        install::uninstall(&self.root, id)?;
        self.scan();
        Ok(())
    }

    /// 某插件的被拒调用记录（详情页展示）。
    pub fn audit(&self, id: &str) -> Vec<AuditRecord> {
        self.instances
            .lock()
            .unwrap()
            .get(id)
            .map(|i| i.audit())
            .unwrap_or_default()
    }

    /// 应用退出：停全部插件并结束全部托管进程（含插件再起的子进程）。
    pub fn shutdown(&self) {
        let instances: Vec<Arc<Instance>> =
            self.instances.lock().unwrap().values().cloned().collect();
        for instance in instances {
            instance.stop();
        }
        self.fail_all_dialogs("宿主已退出");
        self.process_host.shutdown_children();
    }

    /// 退避重启回调（由实例的崩溃线程调用）。
    pub(crate) fn restart_if_current(self: &Arc<Self>, id: &str, generation: u64) {
        let instance = self.instances.lock().unwrap().get(id).cloned();
        if let Some(instance) = instance {
            let _ = instance.start_if_current(generation);
        }
    }

    /// 切仓库容器（本地仓库 ↔ 协作空间 / 换仓库）后的插件侧处置：
    /// 声明 `keepMountedOnVaultSwitch` 的插件保持挂载，订阅了 `vault:switch` 的收到新容器身份；
    /// 未声明的插件由宿主重载（停进程再起）——它拿的旧仓库上下文已失效。
    ///
    /// 两种情形不动作：容器未变（同一仓库被再次选中）；首次激活（启动自动进入）——那一刻插件刚由
    /// 宿主启动、本就没有旧容器上下文，重载纯属白起一次，而广播仍是保活插件得知容器身份的唯一来源。
    pub fn on_vault_switch(self: &Arc<Self>, payload: VaultSwitchPayload) {
        let identity = container_identity(&payload);
        let first_activation = {
            let mut container = self.container.lock().unwrap();
            if container.as_deref() == Some(identity.as_str()) {
                return;
            }
            let first = container.is_none();
            *container = Some(identity);
            first
        };
        let targets: Vec<Arc<Instance>> = self.instances.lock().unwrap().values().cloned().collect();
        for instance in targets {
            // 只有跑着的实例才持有旧容器上下文：未运行的重载无意义、也无从送达事件
            if instance.status() != PluginStatus::Running {
                continue;
            }
            if !instance.keep_mounted_on_vault_switch() {
                if first_activation {
                    continue;
                }
                instance.stop();
                if let Err(e) = instance.start() {
                    eprintln!("[plugin {}] 切仓库重载失败：{e}", instance.id);
                }
                continue;
            }
            if instance.subscribed_to(event::name::VAULT_SWITCH) {
                instance.notify_domain_event(event::name::VAULT_SWITCH, &payload);
            }
        }
    }

    /// 转投一条全局快捷键触发给登记它的插件。
    pub(crate) fn notify_shortcut(&self, plugin_id: &str, accelerator: &str) {
        let instance = self.instances.lock().unwrap().get(plugin_id).cloned();
        if let Some(instance) = instance {
            instance.notify_shortcut(accelerator);
        }
    }

    /// 安装 / 更新后的收尾：重扫清单。更新的若是曾启用的插件，先停旧进程再用新内容重启
    /// （否则旧进程继续跑、新内容不生效）；**新装的插件不自动启用**，由用户在设置页启用。
    fn after_install(self: &Arc<Self>, id: &str) -> Result<String, String> {
        let was_enabled = layout::is_enabled(&self.root, id);
        self.scan();
        if let Some(instance) = self.instances.lock().unwrap().get(id).cloned() {
            instance.stop();
        }
        if was_enabled {
            self.start_by_id(id)?;
        }
        Ok(id.to_string())
    }

    fn start_by_id(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let (manifest, dir) = self.resolve(id)?;
        let instance = self.instance_for(&manifest, &dir);
        instance.start()
    }

    /// 解析某已安装插件的 manifest 与目录（未安装 / 不可装载都以可读原因拒绝）。
    fn resolve(&self, id: &str) -> Result<(Manifest, PathBuf), String> {
        let discovered = self
            .discovered
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| format!("插件未安装：{id}"))?;
        let manifest = discovered
            .manifest
            .clone()
            .map_err(|e| format!("不可启用：{e}"))?;
        Ok((manifest, discovered.dir))
    }

    fn mark_enabled(&self, id: &str, enabled: bool) {
        if let Some(entry) = self.discovered.lock().unwrap().get_mut(id) {
            entry.enabled = enabled;
        }
    }

    fn instance_for(self: &Arc<Self>, manifest: &Manifest, dir: &Path) -> Arc<Instance> {
        let mut instances = self.instances.lock().unwrap();
        if let Some(existing) = instances.get(&manifest.id) {
            return existing.clone();
        }
        let instance = Instance::new(
            manifest.clone(),
            dir.to_path_buf(),
            layout::data_dir(&self.root, &manifest.id),
            self.process_host.clone(),
            self.shortcuts.clone(),
            self.events.clone(),
        );
        instance.attach_runtime(self);
        instances.insert(manifest.id.clone(), instance.clone());
        instance
    }
}

/// 容器身份的规范化串（本地仓库与协作空间分别加前缀，两者取值域互不重叠）。
fn container_identity(payload: &VaultSwitchPayload) -> String {
    match payload {
        VaultSwitchPayload::Local { root } => format!("local:{root}"),
        VaultSwitchPayload::Space { space_id } => format!("space:{space_id}"),
    }
}

impl Drop for PluginRuntime {
    fn drop(&mut self) {
        // 运行时销毁即应用退出：停全部插件并结束整棵托管进程树（Unix 无作业对象兜底，必须显式收尾）。
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::test_support::TempDir;
    use atelyx_plugin_protocol::manifest::MANIFEST_FILE;

    /// 造一个可扫描到的插件目录（合法 manifest + 平台入口占位文件）。
    fn seed_plugin(root: &Path, id: &str) {
        let dir = layout::plugin_dir(root, id);
        std::fs::create_dir_all(&dir).unwrap();
        let entry = if cfg!(windows) { "app.exe" } else { "app" };
        std::fs::write(
            dir.join(MANIFEST_FILE),
            format!(
                r#"{{"id":"{id}","name":"Demo","version":"1.0.0","apiVersion":1,"entry":{{"windows":"app.exe","linux":"app"}},"permissions":[]}}"#
            ),
        )
        .unwrap();
        std::fs::write(dir.join(entry), b"stub").unwrap();
    }

    #[test]
    fn scan_and_list_reflect_enabled_marks() {
        let tmp = TempDir::new("runtime-scan");
        let root: &Path = &tmp;
        seed_plugin(root, "com.example.one");
        seed_plugin(root, "com.example.two");
        let runtime = PluginRuntime::new(root.to_path_buf());
        runtime.scan();
        let listed = runtime.list();
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().all(|p| !p.enabled && p.load_error.is_none()));
        // 置启用标记后重扫：状态随之变化
        let _ = layout::set_enabled(root, "com.example.one", true);
        runtime.scan();
        let one = runtime
            .list()
            .into_iter()
            .find(|p| p.id == "com.example.one")
            .unwrap();
        assert!(one.enabled);
        assert!(!runtime.list().iter().find(|p| p.id == "com.example.two").unwrap().enabled);
    }

    #[test]
    fn invalid_manifest_surfaces_load_error() {
        let tmp = TempDir::new("runtime-bad");
        let root: &Path = &tmp;
        std::fs::create_dir_all(layout::plugin_dir(root, "com.example.bad")).unwrap();
        std::fs::write(layout::plugin_dir(root, "com.example.bad").join(MANIFEST_FILE), "{}").unwrap();
        let runtime = PluginRuntime::new(root.to_path_buf());
        runtime.scan();
        let listed = runtime.list();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].load_error.is_some(), "非法 manifest 应以可读原因呈现");
    }

    #[test]
    fn disable_removes_marker_and_rejects_uninstalled_enable() {
        let tmp = TempDir::new("runtime-disable");
        let root: &Path = &tmp;
        seed_plugin(root, "com.example.one");
        let runtime = PluginRuntime::new(root.to_path_buf());
        let _ = layout::set_enabled(root, "com.example.one", true);
        runtime.scan();
        runtime.set_enabled("com.example.one", false).unwrap();
        assert!(!layout::is_enabled(root, "com.example.one"));
        // 未安装的 id 不可启用（可读错误）
        assert!(runtime.set_enabled("com.example.none", true).is_err());
        // 未安装的 id 卸载幂等成功
        assert!(runtime.uninstall("com.example.none").is_ok());
    }

    fn dialog(kind: DialogKind) -> DialogRequest {
        DialogRequest { kind, title: None, save: false, default_path: None, filters: Vec::new() }
    }

    #[test]
    fn dialog_request_is_resolved_by_the_shell() {
        let tmp = TempDir::new("runtime-dialog");
        let root: &Path = &tmp;
        let runtime = PluginRuntime::new(root.to_path_buf());
        let rx = runtime.open_dialog("com.a.one", dialog(DialogKind::File)).unwrap();
        let request_id = *runtime.dialogs.lock().unwrap().keys().next().unwrap();
        runtime.resolve_dialog(request_id, Some("/models/m.gguf".into()));
        assert_eq!(rx.recv().unwrap().unwrap().as_deref(), Some("/models/m.gguf"));
        // 登记随结果落地而摘除；重复回填是空操作
        assert!(runtime.dialogs.lock().unwrap().is_empty());
        runtime.resolve_dialog(request_id, None);
    }

    #[test]
    fn pending_dialogs_fail_with_the_plugin_and_are_per_plugin() {
        let tmp = TempDir::new("runtime-dialog-fail");
        let root: &Path = &tmp;
        let runtime = PluginRuntime::new(root.to_path_buf());
        let mine = runtime.open_dialog("com.a.one", dialog(DialogKind::Directory)).unwrap();
        let other = runtime.open_dialog("com.a.two", dialog(DialogKind::Directory)).unwrap();

        runtime.fail_dialogs_of("com.a.one", "插件已停止，对话框已取消");
        assert_eq!(mine.recv().unwrap().unwrap_err(), "插件已停止，对话框已取消");
        assert_eq!(runtime.dialogs.lock().unwrap().len(), 1, "别的插件的在途请求不受影响");

        runtime.shutdown();
        assert_eq!(other.recv().unwrap().unwrap_err(), "宿主已退出");
        assert!(runtime.dialogs.lock().unwrap().is_empty());
    }
}
