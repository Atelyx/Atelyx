//! 插件宿主运行时：发现 / 装载 / RPC 泵 / 权限仲裁 / 生命周期 / 安装卸载。
//! 运行时为进程内单例（壳持 `Arc`，多窗口共享）；插件事件经 [`PluginRuntime::subscribe_events`]
//! 的单一广播通道进入壳的 update。进程创建与清理一律经 [`crate::plugin_process`]。

mod capability;
mod install;
mod instance;
mod layout;
mod manifest;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use atelyx_plugin_protocol::manifest::Manifest;

use crate::plugin_process::PluginProcessHost;

use instance::Instance;

/// 协议类型再导出：壳只依赖 `atelyx-core`，不直接依赖协议 crate。
pub use atelyx_plugin_protocol::manifest::Permission;
pub use atelyx_plugin_protocol::method::NotificationLevel;

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

/// 运行时 → 壳的事件（单一通道）。
#[derive(Clone, Debug)]
pub enum PluginEvent {
    StatusChanged { id: String, status: PluginStatus },
    Notification { id: String, title: String, body: Option<String>, level: NotificationLevel },
    PermissionDenied { id: String, method: String, detail: String },
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

/// 插件宿主运行时。
pub struct PluginRuntime {
    root: PathBuf,
    process_host: Arc<PluginProcessHost>,
    events: tokio::sync::broadcast::Sender<PluginEvent>,
    discovered: Mutex<HashMap<String, Discovered>>,
    instances: Mutex<HashMap<String, Arc<Instance>>>,
}

impl PluginRuntime {
    /// 建立运行时（`root` = 应用数据目录下 `plugins/`）。顺带清理遗留的安装临时目录。
    pub fn new(root: PathBuf) -> Arc<Self> {
        layout::sweep_temp(&root);
        let (events, _) = tokio::sync::broadcast::channel(256);
        Arc::new(Self {
            root,
            process_host: Arc::new(PluginProcessHost::new()),
            events,
            discovered: Mutex::new(HashMap::new()),
            instances: Mutex::new(HashMap::new()),
        })
    }

    /// 订阅运行时事件；壳把它接成订阅流（内层用广播，重复订阅 / 订阅重建都安全）。
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<PluginEvent> {
        self.events.subscribe()
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
        self.process_host.shutdown_children();
    }

    /// 退避重启回调（由实例的崩溃线程调用）。
    pub(crate) fn restart_if_current(self: &Arc<Self>, id: &str, generation: u64) {
        let instance = self.instances.lock().unwrap().get(id).cloned();
        if let Some(instance) = instance {
            let _ = instance.start_if_current(generation);
        }
    }

    fn after_install(self: &Arc<Self>, id: &str) -> Result<String, String> {
        self.scan();
        // 更新已启用的插件：先停旧进程再启新版本（否则旧进程继续跑，新内容不生效）
        let existing = self.instances.lock().unwrap().get(id).cloned();
        if let Some(instance) = existing {
            instance.stop();
        }
        self.start_by_id(id)?;
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
            self.events.clone(),
        );
        instance.attach_runtime(self);
        instances.insert(manifest.id.clone(), instance.clone());
        instance
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
}
