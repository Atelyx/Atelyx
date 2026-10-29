//! 能力总线：服务目录（谁提供哪个服务）与跨插件调用桥（宿主转发 + 在途等待）。
//!
//! 目录由 [`method::BUILTIN_SERVICES`] 建出内置条目，外部插件经 `service.register` 加入候选。
//! 单例型语义：同一服务同时只有一个活动提供者，内置实现是出厂默认的活动提供者；登记不等于生效，
//! 内置服务被外部登记时只成为候选，切换用 [`PluginRuntime::set_service_provider`]。
//!
//! 转发方向：宿主下发通知 `host:service.call`，提供者经请求 `service.result` 回报。等待与超时
//! 在独立运行时上的任务里完成——插件 stdout 读线程只做登记与下发，不被跨插件往返拖住。

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use atelyx_plugin_protocol::envelope::RpcError;
use atelyx_plugin_protocol::method;
use atelyx_plugin_protocol::service::{
    self, CallParams, ChangedParams, ListResult, RegisterParams, ResultParams, ServiceInfo,
    SERVICE_VERSION,
};
use serde_json::{json, Value};

use super::instance::Instance;
use super::{PluginEvent, PluginRuntime};

/// 跨插件调用的等待上限：提供者无响应按失败收场，调用方不会永久悬挂。
pub(crate) const SERVICE_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// 能力总线自身的命名空间：不是可登记的服务，也不进目录。
const BUS_NAMESPACE: &str = "service";

/// 不可替换的内置服务：字体与主题取自宿主 OS / 用户设置，外置后宿主自身外观会与设置脱节。
const NON_REPLACEABLE: &[&str] = &["query"];

/// 宿主保留方法：工具名册与结果回报由宿主记账，服务被外部顶替后也不能被提供者截走。
const HOST_OWNED: &[&str] =
    &[method::ai::REGISTER_TOOL, method::ai::UNREGISTER_TOOL, method::ai::TOOL_RESULT];

/// 活动提供者：内置实现（归属插件停用期间不可用）或某个外部插件。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Active {
    Builtin,
    Plugin(String),
}

/// 一个外部提供者的登记（插件 id + 契约版本 + 方法名）。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Provider {
    pub plugin_id: String,
    pub version: u32,
    pub methods: Vec<String>,
}

/// 目录条目：内置方法集合（`None` = 该服务没有内置实现）、归属插件、候选与当前活动提供者。
#[derive(Clone, Debug)]
struct Entry {
    /// 内置实现的归属插件；`None` = 只有外部提供者的服务。
    owner: Option<String>,
    builtin: Option<Vec<String>>,
    /// 归属插件是否启用：停用即内置实现退出目录，外部候选可继续提供服务。
    builtin_enabled: bool,
    candidates: Vec<Provider>,
    active: Active,
    replaceable: bool,
}

impl Entry {
    /// 内置实现当下是否可用（没有实现或归属插件被停用都算不可用）。
    fn builtin_available(&self) -> bool {
        self.builtin.is_some() && self.builtin_enabled
    }

    /// 目录里还有没有可用提供者。
    fn available(&self) -> bool {
        self.builtin_available() || !self.candidates.is_empty()
    }

    /// 内置实现被归属插件停用、又没有外部候选接手：服务仍在册但暂时没人能应答。
    fn blocked(&self) -> bool {
        self.builtin.is_some() && !self.builtin_available() && self.candidates.is_empty()
    }

    /// 摘掉一个候选。它正是活动提供者时按「内置优先，其次首个剩余候选」重选；
    /// 返回 `false` = 该条目已没有任何提供者（外部带进来的服务，条目应从目录移除）。
    fn remove_provider(&mut self, plugin_id: &str) -> bool {
        self.candidates.retain(|candidate| candidate.plugin_id != plugin_id);
        if matches!(&self.active, Active::Plugin(id) if id == plugin_id) {
            self.active = if self.builtin_available() {
                Active::Builtin
            } else if let Some(first) = self.candidates.first() {
                Active::Plugin(first.plugin_id.clone())
            } else if self.builtin.is_some() {
                // 归属插件停用中：保留内置登记，重新启用即可恢复
                Active::Builtin
            } else {
                return false;
            };
        }
        self.builtin.is_some() || !self.candidates.is_empty()
    }

    fn info(&self, name: &str) -> ServiceInfo {
        let (version, methods, provider) = match &self.active {
            Active::Builtin if self.builtin_available() => {
                (SERVICE_VERSION, self.builtin.clone().unwrap_or_default(), None)
            }
            Active::Builtin => (SERVICE_VERSION, Vec::new(), None),
            Active::Plugin(id) => {
                match self.candidates.iter().find(|candidate| &candidate.plugin_id == id) {
                    Some(candidate) => {
                        (candidate.version, candidate.methods.clone(), Some(id.clone()))
                    }
                    // 活动身份与候选表恒一致（登记时 INSERT、摘除时重选），走到这里只说明内部失配
                    None => (SERVICE_VERSION, Vec::new(), Some(id.clone())),
                }
            }
        };
        ServiceInfo {
            service: name.to_string(),
            version,
            methods,
            replaceable: self.replaceable,
            candidates: self
                .candidates
                .iter()
                .filter(|candidate| Some(&candidate.plugin_id) != provider.as_ref())
                .map(|candidate| candidate.plugin_id.clone())
                .collect(),
            provider,
        }
    }
}

/// 一次调用的落点：服务不在目录里 / 走内置实现 / 转给外部提供者。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Route {
    Missing,
    Builtin,
    Plugin(String),
}

/// 普通方法（非 `service.*`）的转发判定。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Forward {
    /// 本地内置实现（服务不在目录里也走这条：由分派报未知方法）。
    Local,
    /// 外部提供者没声明该方法。
    Undeclared,
    /// 内置实现随归属插件停用，且没有候选接手：方法仍在册但无人应答。
    Unavailable,
    Plugin(String),
}

/// 一次 `service.call` 的落点：内置实现按规范化方法名重走分派，外部提供者转发。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ServiceTarget {
    Builtin { wire: String, params: Value },
    Plugin { provider: String, service: String, method: String, params: Value },
}

/// 服务目录：服务名 → 条目（`BTreeMap` 保证 `list` 按服务名排序）。
pub(crate) struct ServiceDir {
    entries: BTreeMap<String, Entry>,
}

impl ServiceDir {
    /// 按内置能力目录建出出厂状态：每个内置服务由所属插件的内置实现承载。
    pub(crate) fn new() -> Self {
        let entries = method::BUILTIN_SERVICES
            .iter()
            .map(|(name, owner, methods)| {
                let methods: Vec<String> = methods
                    .iter()
                    .map(|full| method_segment(full).to_string())
                    .collect();
                let entry = Entry {
                    owner: Some((*owner).to_string()),
                    builtin: Some(methods),
                    builtin_enabled: true,
                    candidates: Vec::new(),
                    active: Active::Builtin,
                    replaceable: !NON_REPLACEABLE.contains(name),
                };
                ((*name).to_string(), entry)
            })
            .collect();
        Self { entries }
    }

    /// 切换内置插件启用态：它的内置实现随之进出目录；活动提供者恰好是内置实现时，
    /// 停用按「首个外部候选接任，否则暂不可用」处置。返回受影响的服务名（升序）。
    /// 平台插件的服务不可停用：宿主自身（设置页、笔记与画布）一直取用它们。
    pub(crate) fn set_owner_enabled(&mut self, owner: &str, enabled: bool) -> Vec<String> {
        if owner == method::PLATFORM_PLUGIN {
            return Vec::new();
        }
        let mut affected = Vec::new();
        for (name, entry) in &mut self.entries {
            if entry.owner.as_deref() != Some(owner) || entry.builtin_enabled == enabled {
                continue;
            }
            entry.builtin_enabled = enabled;
            if !enabled && matches!(entry.active, Active::Builtin) {
                if let Some(first) = entry.candidates.first() {
                    entry.active = Active::Plugin(first.plugin_id.clone());
                }
            }
            affected.push(name.clone());
        }
        affected
    }

    /// 登记一个外部提供者。已有内置实现的服务只成为候选（不静默顶替）；目录里没有的服务
    /// 首个提供者即活动提供者。不可替换的服务拒收（可读错误，不是沉默忽略）。
    pub(crate) fn register(
        &mut self,
        plugin_id: &str,
        params: &RegisterParams,
    ) -> Result<bool, String> {
        params.validate()?;
        if is_host_channel(&params.service) {
            return Err(format!(
                "`{}` 是宿主的通道命名空间，不是可替换的能力",
                params.service
            ));
        }
        let provider = Provider {
            plugin_id: plugin_id.to_string(),
            version: params.version.unwrap_or(SERVICE_VERSION),
            methods: params.methods.clone(),
        };
        match self.entries.get_mut(&params.service) {
            Some(entry) if !entry.replaceable => Err(format!(
                "服务 `{}` 的提供者不可替换：它取的是宿主 OS 与用户设置资源",
                params.service
            )),
            Some(entry) => {
                // 同一插件重复登记 = 原地更新声明；活动身份不变（登记不等于生效）
                match entry
                    .candidates
                    .iter_mut()
                    .find(|candidate| candidate.plugin_id == plugin_id)
                {
                    Some(existing) => *existing = provider,
                    None => entry.candidates.push(provider),
                }
                // 归属插件停用期间内置实现不在场：候选就是唯一提供者，登记即接任
                if matches!(entry.active, Active::Builtin) && !entry.builtin_available() {
                    if let Some(first) = entry.candidates.first().map(|c| c.plugin_id.clone()) {
                        entry.active = Active::Plugin(first);
                        return Ok(true);
                    }
                }
                // 候选变化不算活动提供者变化：订阅方不必重建
                Ok(false)
            }
            None => {
                self.entries.insert(
                    params.service.clone(),
                    Entry {
                        owner: None,
                        builtin: None,
                        builtin_enabled: true,
                        candidates: vec![provider],
                        active: Active::Plugin(plugin_id.to_string()),
                        replaceable: true,
                    },
                );
                Ok(true)
            }
        }
    }

    /// 撤销本插件对某个服务的登记；活动提供者撤销即按回落规则重选，没人接手则条目移除。
    pub(crate) fn unregister(&mut self, plugin_id: &str, service_name: &str) -> Result<bool, String> {
        let mut entry = self
            .entries
            .remove(service_name)
            .ok_or_else(|| format!("服务未登记：{service_name}"))?;
        if !entry.candidates.iter().any(|candidate| candidate.plugin_id == plugin_id) {
            self.entries.insert(service_name.to_string(), entry);
            return Err(format!("本插件未登记服务：{service_name}"));
        }
        let active = matches!(&entry.active, Active::Plugin(id) if id == plugin_id);
        if entry.remove_provider(plugin_id) {
            self.entries.insert(service_name.to_string(), entry);
        }
        Ok(active)
    }

    /// 切换活动提供者：`None` = 回落内置（没有内置实现即拒绝）。指定插件须是候选之一。
    pub(crate) fn set_active(
        &mut self,
        service_name: &str,
        provider: Option<&str>,
    ) -> Result<bool, String> {
        let entry = self
            .entries
            .get_mut(service_name)
            .ok_or_else(|| format!("未知服务：{service_name}"))?;
        match provider {
            Some(plugin_id) => {
                if !entry.replaceable {
                    return Err(format!(
                        "服务 `{service_name}` 的提供者不可替换：它取的是宿主 OS 与用户设置资源"
                    ));
                }
                if !entry
                    .candidates
                    .iter()
                    .any(|candidate| candidate.plugin_id == plugin_id)
                {
                    return Err(format!("插件 {plugin_id} 不是服务 {service_name} 的候选提供者"));
                }
                entry.active = Active::Plugin(plugin_id.to_string());
            }
            None => {
                if !entry.builtin_available() {
                    return Err(if entry.builtin.is_some() {
                        format!("服务 {service_name} 的内置实现所属插件已停用，不能回落")
                    } else {
                        format!("服务 {service_name} 没有内置实现，不能回落")
                    });
                }
                entry.active = Active::Builtin;
            }
        }
        Ok(true)
    }

    /// 摘掉某插件的全部候选与活动身份（停用 / 崩溃 / 卸载）。返回受影响的服务名（升序）。
    pub(crate) fn drop_plugin(&mut self, plugin_id: &str) -> Vec<String> {
        let names: Vec<String> = self.entries.keys().cloned().collect();
        let mut affected = Vec::new();
        for name in names {
            let mut entry = self.entries.remove(&name).expect("键取自目录快照");
            let involved = matches!(&entry.active, Active::Plugin(id) if id == plugin_id)
                || entry.candidates.iter().any(|candidate| candidate.plugin_id == plugin_id);
            if !involved {
                self.entries.insert(name, entry);
                continue;
            }
            if entry.remove_provider(plugin_id) {
                self.entries.insert(name.clone(), entry);
            }
            affected.push(name);
        }
        affected
    }

    pub(crate) fn route(&self, service_name: &str) -> Route {
        match self.entries.get(service_name) {
            Some(entry) if entry.available() => match &entry.active {
                Active::Builtin if entry.builtin_available() => Route::Builtin,
                Active::Builtin => Route::Missing,
                Active::Plugin(id) => Route::Plugin(id.clone()),
            },
            _ => Route::Missing,
        }
    }

    /// 服务是否处于「在册但暂不可用」：归属插件停用，外部候选也没接手。
    pub(crate) fn blocked(&self, service_name: &str) -> bool {
        self.entries.get(service_name).is_some_and(Entry::blocked)
    }

    /// 内置实现是否声明了该方法（`false` = 该服务没有内置实现或没有这个方法）。
    pub(crate) fn builtin_has(&self, service_name: &str, method_name: &str) -> bool {
        self.entries
            .get(service_name)
            .and_then(|entry| entry.builtin.as_ref())
            .is_some_and(|methods| methods.iter().any(|name| name == method_name))
    }

    /// 外部提供者是否声明了该方法（未登记 = `false`）。
    pub(crate) fn provider_declares(
        &self,
        provider: &str,
        service_name: &str,
        method_name: &str,
    ) -> bool {
        self.entries
            .get(service_name)
            .and_then(|entry| {
                entry
                    .candidates
                    .iter()
                    .find(|candidate| candidate.plugin_id == provider)
            })
            .is_some_and(|candidate| candidate.methods.iter().any(|name| name == method_name))
    }

    /// 某服务当前状态的对外形状（活动提供者变化时用于广播）。
    pub(crate) fn changed(&self, service_name: &str) -> ChangedParams {
        match self.entries.get(service_name) {
            None => ChangedParams {
                service: service_name.to_string(),
                provider: None,
                available: false,
            },
            Some(entry) => ChangedParams {
                service: service_name.to_string(),
                provider: match &entry.active {
                    Active::Builtin => None,
                    Active::Plugin(id) if entry.available() => Some(id.clone()),
                    Active::Plugin(_) => None,
                },
                available: entry.available(),
            },
        }
    }

    pub(crate) fn list(&self) -> ListResult {
        ListResult {
            services: self
                .entries
                .iter()
                .filter(|(_, entry)| entry.available())
                .map(|(name, entry)| entry.info(name))
                .collect(),
        }
    }
}

/// 宿主通道命名空间：界面提交、装配声明与事件通道由宿主独占，不允许被登记成可替换能力。
/// 放它们进来等于让插件接管 `view.register` 这类入口，插件会把自己从界面上摘掉。
const HOST_CHANNELS: &[&str] = &["service", "view", "frame", "layout", "event", "doc"];

/// 是否属于宿主通道命名空间。
fn is_host_channel(service_name: &str) -> bool {
    HOST_CHANNELS.contains(&service_name)
}

/// 全名按第一个 `.` 切出方法段（`vault.readText` → `readText`）。
fn method_segment(full: &str) -> &str {
    full.split_once('.').map_or(full, |(_, method)| method)
}

/// 普通方法名按第一个 `.` 切出（服务名, 方法名）。
fn split_service(method_name: &str) -> Option<(&str, &str)> {
    method_name.split_once('.')
}

fn service_error(code: &str, detail: impl AsRef<str>) -> RpcError {
    RpcError {
        code: RpcError::FAILED,
        message: format!("{code}：{}", detail.as_ref()),
        data: Some(json!({ "reason": code })),
    }
}

type ServiceReplyTx = tokio::sync::oneshot::Sender<Result<Value, String>>;

/// 在途服务调用：提供者与调用方身份（任一方停用都定点失败）+ 回传通道。
pub(super) struct ServiceCallWait {
    pub(super) provider: String,
    pub(super) caller: String,
    pub(super) tx: ServiceReplyTx,
}

impl PluginRuntime {
    pub(crate) fn register_service(
        &self,
        plugin_id: &str,
        params: RegisterParams,
    ) -> Result<(), String> {
        let changed = {
            let mut dir = self.services.lock().unwrap();
            dir.register(plugin_id, &params)?
        };
        // 目录内容变了（哪怕只是多了候选），壳侧注册表都要刷新；插件只被告知活动提供者变化
        self.announce_service_dir(plugin_id);
        if changed {
            self.broadcast_service_changed(std::slice::from_ref(&params.service));
        }
        Ok(())
    }

    pub(crate) fn unregister_service(
        &self,
        plugin_id: &str,
        service_name: &str,
    ) -> Result<(), String> {
        let changed = {
            let mut dir = self.services.lock().unwrap();
            dir.unregister(plugin_id, service_name)?
        };
        self.announce_service_dir(plugin_id);
        if changed {
            self.broadcast_service_changed(&[service_name.to_string()]);
        }
        Ok(())
    }

    /// 服务当前是否在目录里（内置实现所属插件停用且无外部候选接手时为 `false`）。
    pub fn service_available(&self, service_name: &str) -> bool {
        !matches!(self.services.lock().unwrap().route(service_name), Route::Missing)
    }

    /// 服务当前是否由内置实现承载（外部提供者接管或服务不可用时为 `false`）。
    /// 壳内同步渲染只能用内置实现：外部提供者是跨进程异步往返，赶不上一帧。
    pub fn service_is_builtin(&self, service_name: &str) -> bool {
        matches!(self.services.lock().unwrap().route(service_name), Route::Builtin)
    }

    /// 在册服务快照（按服务名排序）。
    pub fn service_list(&self) -> ListResult {
        self.services.lock().unwrap().list()
    }

    /// 切换某服务的活动提供者：`None` = 回落内置实现。这是设置页切换提供者的入口。
    pub fn set_service_provider(
        &self,
        service_name: &str,
        provider: Option<&str>,
    ) -> Result<(), String> {
        let changed = {
            let mut dir = self.services.lock().unwrap();
            dir.set_active(service_name, provider)?
        };
        self.announce_service_dir(provider.unwrap_or_default());
        if changed {
            self.broadcast_service_changed(&[service_name.to_string()]);
        }
        Ok(())
    }

    /// 切换内置插件启用态：它名下的服务随之进出能力总线。壳的内置插件开关走这里。
    /// 返回受影响的服务名。
    pub fn set_builtin_owner_enabled(&self, owner: &str, enabled: bool) -> Vec<String> {
        let affected = self.services.lock().unwrap().set_owner_enabled(owner, enabled);
        self.announce_service_dir(owner);
        self.broadcast_service_changed(&affected);
        affected
    }

    /// 摘掉某插件的全部服务登记（停用 / 崩溃 / 卸载），返回受影响的服务名。
    pub(crate) fn drop_plugin_services(&self, plugin_id: &str) -> Vec<String> {
        let affected = self.services.lock().unwrap().drop_plugin(plugin_id);
        self.announce_service_dir(plugin_id);
        self.broadcast_service_changed(&affected);
        affected
    }

    /// 告诉壳服务目录变了（壳据此重建注册表的服务区）。插件侧仍只收活动提供者变化。
    fn announce_service_dir(&self, plugin_id: &str) {
        let _ = self
            .events
            .send(PluginEvent::ServiceChanged { id: plugin_id.to_string() });
    }

    /// 解析一次 `service.call`：先按目录校验服务与活动提供者的方法声明，再给出落点。
    pub(crate) fn service_target(&self, call: CallParams) -> Result<ServiceTarget, RpcError> {
        call.validate().map_err(RpcError::failed)?;
        if call.service == BUS_NAMESPACE {
            return Err(service_error(
                service::error::UNKNOWN_SERVICE,
                "`service` 是能力总线自身的命名空间，不对外提供",
            ));
        }
        let dir = self.services.lock().unwrap();
        match dir.route(&call.service) {
            Route::Missing if dir.blocked(&call.service) => {
                Err(Self::unavailable_service(&call.service))
            }
            Route::Missing => Err(service_error(
                service::error::UNKNOWN_SERVICE,
                format!("服务未登记：{}", call.service),
            )),
            Route::Builtin => {
                if !dir.builtin_has(&call.service, &call.method) {
                    return Err(service_error(
                        service::error::UNKNOWN_METHOD,
                        format!("{} 没有内置方法 {}", call.service, call.method),
                    ));
                }
                Ok(ServiceTarget::Builtin { wire: call.wire_name(), params: call.params })
            }
            Route::Plugin(provider) => {
                if !dir.provider_declares(&provider, &call.service, &call.method) {
                    return Err(service_error(
                        service::error::UNKNOWN_METHOD,
                        format!("提供者 {provider} 未声明 {}.{}", call.service, call.method),
                    ));
                }
                Ok(ServiceTarget::Plugin {
                    provider,
                    service: call.service,
                    method: call.method,
                    params: call.params,
                })
            }
        }
    }

    /// 普通方法的转发判定：服务段的活动提供者是外部插件时转给它；宿主保留方法恒走本地。
    pub(crate) fn forward_to(&self, service_name: &str, method_name: &str) -> Forward {
        let wire = format!("{service_name}.{method_name}");
        if HOST_OWNED.contains(&wire.as_str()) {
            return Forward::Local;
        }
        let dir = self.services.lock().unwrap();
        match dir.route(service_name) {
            Route::Plugin(provider) => {
                if dir.provider_declares(&provider, service_name, method_name) {
                    Forward::Plugin(provider)
                } else {
                    Forward::Undeclared
                }
            }
            // 归属插件停用时不能落到本地实现：那里的分派还认这些方法
            Route::Missing if dir.blocked(service_name) => Forward::Unavailable,
            _ => Forward::Local,
        }
    }

    /// 不可分派的服务方法（活动提供者没声明该方法）的可读错误。
    pub(crate) fn undeclared_service_method(service_name: &str, method_name: &str) -> RpcError {
        service_error(
            service::error::UNKNOWN_METHOD,
            format!("{service_name} 未提供方法 {method_name}"),
        )
    }

    /// 服务在册但暂无人应答（归属插件已停用）的可读错误。
    pub(crate) fn unavailable_service(service_name: &str) -> RpcError {
        service_error(
            service::error::UNKNOWN_SERVICE,
            format!("服务 {service_name} 暂不可用：其归属插件已停用"),
        )
    }

    /// 把一次调用转给外部提供者：登记在途条目并下发 `host:service.call`，等待与超时在后台任务
    /// 里完成，结果作为调用方原始请求的应答写回。
    pub(crate) fn forward_service_call(
        self: &Arc<Self>,
        caller: &Arc<Instance>,
        request_id: u64,
        provider: &str,
        service_name: &str,
        method_name: &str,
        params: Value,
    ) {
        let (call_id, rx, provider_instance) = match self.register_service_call(caller, provider) {
            Ok(opened) => opened,
            Err(reason) => {
                caller.respond(request_id, None, Some(service_error(
                    service::error::PROVIDER_UNAVAILABLE,
                    reason,
                )));
                return;
            }
        };
        let runtime = self.clone();
        let caller_token = caller.run_token();
        let timeout_call_id = call_id.clone();
        let caller = caller.clone();
        let provider_id = provider.to_string();
        self.service_handle().spawn(async move {
            let outcome: Result<Value, RpcError> = match tokio::time::timeout(SERVICE_CALL_TIMEOUT, rx)
                .await
            {
                Ok(Ok(Ok(value))) => Ok(value),
                Ok(Ok(Err(reason))) if reason == service::error::PROVIDER_UNAVAILABLE => Err(
                    service_error(
                        service::error::PROVIDER_UNAVAILABLE,
                        format!("提供者 {provider_id} 已下线"),
                    ),
                ),
                // 提供者自报的业务错误：原样回给调用方（可读原因已由提供者给出）
                Ok(Ok(Err(reason))) => Err(RpcError::failed(reason)),
                Ok(Err(_)) => Err(service_error(
                    service::error::PROVIDER_UNAVAILABLE,
                    format!("提供者 {provider_id} 已下线"),
                )),
                Err(_) => {
                    runtime.service_calls.lock().unwrap().remove(&timeout_call_id);
                    Err(service_error(
                        service::error::TIMEOUT,
                        format!(
                            "提供者 {provider_id} 在 {} 秒内未回报结果",
                            SERVICE_CALL_TIMEOUT.as_secs()
                        ),
                    ))
                }
            };
            // 迟到的结果只写给发起它的那一次会话：同 id 插件重启后令牌已变，写回会串会话
            match outcome {
                Ok(value) => caller.respond_if_current(caller_token, request_id, Some(value), None),
                Err(error) => caller.respond_if_current(caller_token, request_id, None, Some(error)),
            }
        });
        // 下发放在会话闸门之外：写插件 stdin 在管道满时会阻塞，闸门内阻塞会让「停用该插件」
        // （收尾先取同一把闸门）连 UI 线程一起卡住，超时任务也等不到启动
        provider_instance.notify_service_call(&call_id, service_name, method_name, params);
    }

    /// 登记一条在途调用（只登记，不下发）：闸门保证提供者正在收尾时不再领新调用。
    /// 返回 `callId`、应答通道与提供者实例（下发由调用方在闸门外写对方 stdin）。
    fn register_service_call(
        &self,
        caller: &Arc<Instance>,
        provider: &str,
    ) -> Result<(String, tokio::sync::oneshot::Receiver<Result<Value, String>>, Arc<Instance>), String>
    {
        let instance = self
            .instances
            .lock()
            .unwrap()
            .get(provider)
            .cloned()
            .ok_or_else(|| format!("提供者 {provider} 未运行"))?;
        let call_id = format!(
            "{provider}-{}",
            self.next_service_call_id.fetch_add(1, Ordering::SeqCst)
        );
        let (tx, rx) = tokio::sync::oneshot::channel();
        let wait =
            ServiceCallWait { provider: provider.to_string(), caller: caller.id.clone(), tx };
        instance.with_active_session(|| {
            self.service_calls.lock().unwrap().insert(call_id.clone(), wait);
            Ok(())
        })?;
        Ok((call_id, rx, instance))
    }

    /// 提供者回报一次调用的结果。`callId` 不在册（超时 / 提供者已下线后迟到）或归属不符即回
    /// 可读错误——提供者据此区分「已送达」与「迟到被丢弃」。
    pub(crate) fn resolve_service_call(
        &self,
        plugin_id: &str,
        params: ResultParams,
    ) -> Result<(), String> {
        let waiter = {
            let mut calls = self.service_calls.lock().unwrap();
            match calls.get(&params.call_id) {
                Some(wait) if wait.provider == plugin_id => calls.remove(&params.call_id),
                Some(_) => return Err(format!("调用不属于本插件：{}", params.call_id)),
                None => {
                    return Err(format!(
                        "调用已收场或不存在：{}（迟到结果不会送达调用方）",
                        params.call_id
                    ))
                }
            }
        };
        let outcome = match (params.result, params.error) {
            (Some(value), None) => Ok(value),
            (_, Some(error)) => Err(error),
            (None, None) => Err("提供者既未给出 result 也未给出 error".to_string()),
        };
        let Some(wait) = waiter else {
            return Err(format!("调用已收场：{}", params.call_id));
        };
        wait.tx
            .send(outcome)
            .map_err(|_| format!("调用已收场：{}（结果未送达）", params.call_id))
    }

    /// 让某插件相关的在途调用立即以可读失败收场：它作为提供者（对端在等）或作为调用方
    /// （它在等对端）停用 / 崩溃 / 卸载，两个方向都要收场，不能等超时。
    pub(crate) fn fail_service_calls_of(&self, plugin_id: &str) {
        let waiters: Vec<ServiceReplyTx> = {
            let mut calls = self.service_calls.lock().unwrap();
            let ids: Vec<String> = calls
                .iter()
                .filter(|(_, wait)| wait.provider == plugin_id || wait.caller == plugin_id)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| calls.remove(&id).map(|wait| wait.tx))
                .collect()
        };
        for tx in waiters {
            let _ = tx.send(Err(service::error::PROVIDER_UNAVAILABLE.to_string()));
        }
    }

    /// 让全部在途服务调用以可读失败收场（宿主退出收尾）。
    pub(super) fn fail_all_service_calls(&self, reason: &str) {
        let waiters: Vec<ServiceReplyTx> = {
            let mut calls = self.service_calls.lock().unwrap();
            calls.drain().map(|(_, wait)| wait.tx).collect()
        };
        for tx in waiters {
            let _ = tx.send(Err(reason.to_string()));
        }
    }

    /// 活动提供者变化广播给全部运行中插件（进程不在跑的实例由实例侧静默丢弃）。
    fn broadcast_service_changed(&self, services: &[String]) {
        if services.is_empty() {
            return;
        }
        let changed: Vec<ChangedParams> = {
            let dir = self.services.lock().unwrap();
            services.iter().map(|name| dir.changed(name)).collect()
        };
        let instances: Vec<Arc<Instance>> =
            self.instances.lock().unwrap().values().cloned().collect();
        for params in &changed {
            for instance in &instances {
                instance.notify_service_changed(params);
            }
        }
    }

    /// 跨插件调用的执行器：首次转发时建立一条独立运行时，此后进程内复用。
    /// 转发发生在插件 stdout 读线程上，那里没有 tokio 上下文可借，故自建一条并让一个后台线程
    /// 常驻驱动它（`block_on(pending)` 只负责跑定时器与转发任务）。
    fn service_handle(&self) -> tokio::runtime::Handle {
        self.service_runtime
            .get_or_init(|| {
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::Builder::new()
                    .name("atelyx-service-call".into())
                    .spawn(move || {
                        let runtime = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("建立跨插件调用运行时失败");
                        tx.send(runtime.handle().clone()).expect("转发执行器句柄未被接收");
                        runtime.block_on(std::future::pending::<()>());
                    })
                    .expect("建立跨插件调用线程失败");
                rx.recv().expect("转发执行器线程未就绪")
            })
            .clone()
    }

    /// 普通方法名 → (服务段, 方法段)。`service.*` 与无前缀方法（握手 / 订阅）不是服务调用。
    pub(crate) fn split_service(method_name: &str) -> Option<(&str, &str)> {
        split_service(method_name).filter(|(service_name, _)| *service_name != BUS_NAMESPACE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(service_name: &str, methods: &[&str]) -> RegisterParams {
        RegisterParams {
            service: service_name.to_string(),
            version: None,
            methods: methods.iter().map(|name| (*name).to_string()).collect(),
        }
    }

    fn info<'a>(list: &'a ListResult, service_name: &str) -> &'a ServiceInfo {
        list.services
            .iter()
            .find(|entry| entry.service == service_name)
            .unwrap_or_else(|| panic!("目录里应有服务 {service_name}"))
    }

    #[test]
    fn builtin_catalog_splits_at_the_first_dot() {
        let dir = ServiceDir::new();
        assert!(!dir.entries.is_empty(), "内置目录不能为空");
        for (service_name, _, methods) in method::BUILTIN_SERVICES {
            assert_eq!(dir.route(service_name), Route::Builtin, "{service_name} 出厂即内置");
            for full in *methods {
                let (prefix, tail) = full.split_once('.').expect("内置方法名带服务前缀");
                assert_eq!(prefix, *service_name, "{full} 的服务段应与其目录条目一致");
                assert!(dir.builtin_has(service_name, tail), "{full} 的方法段应能命中原名");
            }
        }
        // 总线自身的命名空间不进目录
        assert_eq!(dir.route(BUS_NAMESPACE), Route::Missing);
        // 列表按服务名排序，且内置条目的方法名是切分后的方法段
        let list = dir.list();
        let mut sorted: Vec<&str> = list.services.iter().map(|s| s.service.as_str()).collect();
        let names = sorted.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        assert!(info(&list, "vault").methods.contains(&"readText".to_string()));
        assert!(info(&list, "markdown").methods.contains(&"parse".to_string()));
        assert_eq!(info(&list, "vault").provider, None);
        assert!(!info(&list, "query").replaceable, "字体与主题不外置");
        assert!(info(&list, "vault").replaceable);
    }

    #[test]
    fn external_registration_is_only_a_candidate() {
        let mut dir = ServiceDir::new();
        dir.register("com.a", &params("markdown", &["parse"])).unwrap();
        assert_eq!(dir.route("markdown"), Route::Builtin, "内置服务不得被静默顶替");
        assert_eq!(info(&dir.list(), "markdown").candidates, vec!["com.a"]);
        assert!(dir.provider_declares("com.a", "markdown", "parse"));
        assert!(!dir.provider_declares("com.a", "markdown", "render"));

        dir.set_active("markdown", Some("com.a")).unwrap();
        assert_eq!(dir.route("markdown"), Route::Plugin("com.a".into()));
        let listed = dir.list();
        assert_eq!(info(&listed, "markdown").provider.as_deref(), Some("com.a"));
        assert!(info(&listed, "markdown").candidates.is_empty());

        dir.set_active("markdown", None).unwrap();
        assert_eq!(dir.route("markdown"), Route::Builtin);
        assert!(dir.set_active("markdown", Some("com.b")).is_err(), "非候选不得当活动提供者");
    }

    #[test]
    fn new_services_belong_to_their_first_provider() {
        let mut dir = ServiceDir::new();
        dir.register("com.a", &params("demo", &["echo"])).unwrap();
        assert_eq!(dir.route("demo"), Route::Plugin("com.a".into()));
        dir.register("com.b", &params("demo", &["echo"])).unwrap();
        assert_eq!(dir.route("demo"), Route::Plugin("com.a".into()), "登记不改变活动提供者");
        assert_eq!(info(&dir.list(), "demo").candidates, vec!["com.b"]);

        assert_eq!(dir.drop_plugin("com.a"), vec!["demo".to_string()]);
        assert_eq!(dir.route("demo"), Route::Plugin("com.b".into()), "首个剩余候选接任");
        assert_eq!(dir.drop_plugin("com.b"), vec!["demo".to_string()]);
        assert_eq!(dir.route("demo"), Route::Missing, "无人提供即从目录消失");
    }

    #[test]
    fn dropping_the_active_provider_falls_back_to_builtin() {
        let mut dir = ServiceDir::new();
        dir.register("com.a", &params("vault", &["readText"])).unwrap();
        dir.set_active("vault", Some("com.a")).unwrap();
        assert_eq!(dir.drop_plugin("com.a"), vec!["vault".to_string()]);
        assert_eq!(dir.route("vault"), Route::Builtin, "活动提供者下线回落内置");
        assert!(info(&dir.list(), "vault").candidates.is_empty());
        // 与下线插件无关的服务不受影响
        assert!(dir.drop_plugin("com.other").is_empty());
    }

    #[test]
    fn platform_services_refuse_external_registration() {
        let mut dir = ServiceDir::new();
        for service_name in NON_REPLACEABLE {
            let error = dir.register("com.a", &params(service_name, &["theme"])).unwrap_err();
            assert!(error.contains("不可替换"), "应可读拒绝：{error}");
            assert_eq!(dir.route(service_name), Route::Builtin);
            assert!(info(&dir.list(), service_name).candidates.is_empty());
        }
        assert!(dir.set_active("query", Some("com.a")).is_err());
        assert!(dir.register("com.a", &params(BUS_NAMESPACE, &["list"])).is_err());
    }

    #[test]
    fn unregister_requires_ownership_and_prunes_unserved_services() {
        let mut dir = ServiceDir::new();
        dir.register("com.a", &params("demo", &["echo"])).unwrap();
        assert!(dir.unregister("com.b", "demo").is_err(), "不是自己的登记");
        assert!(dir.unregister("com.a", "nope").is_err(), "未登记的服务");
        dir.unregister("com.a", "demo").unwrap();
        assert_eq!(dir.route("demo"), Route::Missing);
    }

    #[test]
    fn disabling_a_builtin_owner_takes_its_services_off_the_bus() {
        let mut dir = ServiceDir::new();
        assert_eq!(dir.route("markdown"), Route::Builtin);
        assert_eq!(
            dir.set_owner_enabled("atelyx.notes", false),
            vec!["editor".to_string(), "markdown".to_string()],
            "笔记插件的全部服务一并离开"
        );
        assert_eq!(dir.route("markdown"), Route::Missing, "归属插件停用后服务不可用");
        assert_eq!(dir.route("editor"), Route::Missing);
        assert!(dir.blocked("markdown"), "在册但暂不可用与「从未登记」要能区分");
        assert!(!dir.blocked("vault"), "平台插件的方法不受停用影响");
        assert!(dir.list().services.iter().all(|service| service.service != "markdown"));
        assert!(!dir.changed("markdown").available);

        assert_eq!(
            dir.set_owner_enabled("atelyx.notes", true),
            vec!["editor".to_string(), "markdown".to_string()]
        );
        assert_eq!(dir.route("markdown"), Route::Builtin, "重新启用即恢复");
        assert!(!dir.blocked("markdown"));
        assert!(dir.list().services.iter().any(|service| service.service == "markdown"));
    }

    #[test]
    fn the_platform_owner_is_never_disabled() {
        let mut dir = ServiceDir::new();
        assert!(dir.set_owner_enabled(method::PLATFORM_PLUGIN, false).is_empty());
        assert_eq!(dir.route("vault"), Route::Builtin, "宿主自救面不随停用离开");
        assert_eq!(dir.route("query"), Route::Builtin);
    }

    #[test]
    fn a_disabled_owner_yields_to_an_external_candidate() {
        let mut dir = ServiceDir::new();
        dir.register("com.a", &params("markdown", &["parse"])).unwrap();
        dir.set_owner_enabled("atelyx.notes", false);
        assert_eq!(dir.route("markdown"), Route::Plugin("com.a".into()), "外部候选接任");
        assert_eq!(info(&dir.list(), "markdown").provider.as_deref(), Some("com.a"));

        dir.drop_plugin("com.a");
        assert_eq!(dir.route("markdown"), Route::Missing, "候选也没了即不可用");
        dir.set_owner_enabled("atelyx.notes", true);
        assert_eq!(dir.route("markdown"), Route::Builtin, "内置登记留位，启用即恢复");
    }

    #[test]
    fn a_candidate_registered_after_the_owner_is_disabled_takes_over() {
        let mut dir = ServiceDir::new();
        dir.set_owner_enabled("atelyx.notes", false);
        assert!(dir.blocked("markdown"));
        assert!(
            dir.register("com.a", &params("markdown", &["parse"])).unwrap(),
            "内置实现不在场时登记即接任，要广播活动提供者变化"
        );
        assert_eq!(dir.route("markdown"), Route::Plugin("com.a".into()));
        assert!(!dir.blocked("markdown"));
        assert!(
            !dir.register("com.b", &params("markdown", &["parse"])).unwrap(),
            "已有提供者时后来者只是候选"
        );
        assert_eq!(dir.route("markdown"), Route::Plugin("com.a".into()));

        dir.unregister("com.a", "markdown").unwrap();
        assert_eq!(dir.route("markdown"), Route::Plugin("com.b".into()), "剩余候选接任");
        dir.unregister("com.b", "markdown").unwrap();
        assert_eq!(dir.route("markdown"), Route::Missing, "候选退出后回到暂不可用");
        assert!(dir.blocked("markdown"), "内置登记仍在册，重新启用即恢复");
    }
}
