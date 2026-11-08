//! 能力总线：服务目录（谁提供哪个服务）与跨插件调用桥（宿主转发 + 在途等待）。
//!
//! 目录由 [`method::BUILTIN_SERVICES`] 建出内置条目：内置服务归宿主，插件不能占用这些名字。
//! 插件经 `service.register` 只能登记**新**服务名：一个服务名同时只有一个提供者，后来的登记
//! 以可读错误拒绝——两个插件争同一个名字没有仲裁规则，先登记者拥有它。
//!
//! 转发方向：宿主下发通知 `host:service.call`，提供者经请求 `service.result` 回报。等待与超时
//! 在独立运行时上的任务里完成——插件读线程只做登记与下发，不被跨插件往返拖住。

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

/// 宿主保留方法：工具名册与结果回报由宿主记账，不能被外部服务实现截走。
const HOST_OWNED: &[&str] =
    &[method::ai::REGISTER_TOOL, method::ai::UNREGISTER_TOOL, method::ai::TOOL_RESULT];

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
    Plugin(String),
}

/// 一次 `service.call` 的落点：内置实现按规范化方法名重走分派，外部提供者转发。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ServiceTarget {
    Builtin { wire: String, params: Value },
    Plugin { provider: String, service: String, method: String, params: Value },
}

/// 一个外部提供者的登记（插件 id + 契约版本 + 方法名）。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Provider {
    pub plugin_id: String,
    pub version: u32,
    pub methods: Vec<String>,
}

/// 目录条目：内置方法集合（`None` = 由外部插件提供）或外部提供者，二者恰有其一。
#[derive(Clone, Debug)]
struct Entry {
    builtin: Option<Vec<String>>,
    provider: Option<Provider>,
}

impl Entry {
    fn info(&self, name: &str) -> ServiceInfo {
        match &self.provider {
            Some(provider) => ServiceInfo {
                service: name.to_string(),
                version: provider.version,
                methods: provider.methods.clone(),
                provider: Some(provider.plugin_id.clone()),
            },
            None => ServiceInfo {
                service: name.to_string(),
                version: SERVICE_VERSION,
                methods: self.builtin.clone().unwrap_or_default(),
                provider: None,
            },
        }
    }
}

/// 服务目录：服务名 → 条目（`BTreeMap` 保证 `list` 按服务名排序）。
pub(crate) struct ServiceDir {
    entries: BTreeMap<String, Entry>,
}

impl ServiceDir {
    /// 按内置能力目录建出出厂状态：内置服务全部归宿主。
    pub(crate) fn new() -> Self {
        let entries = method::BUILTIN_SERVICES
            .iter()
            .map(|(name, methods)| {
                let methods: Vec<String> = methods
                    .iter()
                    .map(|full| method_segment(full).to_string())
                    .collect();
                ((*name).to_string(), Entry { builtin: Some(methods), provider: None })
            })
            .collect();
        Self { entries }
    }

    /// 登记一个外部提供者。内置服务名与宿主通道命名空间拒收（可读错误，不是沉默忽略），
    /// 已被其他插件占用的服务名同样拒收。
    pub(crate) fn register(
        &mut self,
        plugin_id: &str,
        params: &RegisterParams,
    ) -> Result<(), String> {
        params.validate()?;
        if is_host_channel(&params.service) {
            return Err(format!(
                "`{}` 是宿主的通道命名空间，不是可登记的服务",
                params.service
            ));
        }
        if self.entries.contains_key(&params.service) {
            return Err(format!(
                "服务名已被占用：{}（内置能力与已登记服务不可被顶替）",
                params.service
            ));
        }
        self.entries.insert(
            params.service.clone(),
            Entry {
                builtin: None,
                provider: Some(Provider {
                    plugin_id: plugin_id.to_string(),
                    version: params.version.unwrap_or(SERVICE_VERSION),
                    methods: params.methods.clone(),
                }),
            },
        );
        Ok(())
    }

    /// 撤销本插件对某个服务的登记；不是自己的登记或没有这一条即拒收。
    pub(crate) fn unregister(&mut self, plugin_id: &str, service_name: &str) -> Result<(), String> {
        match self.entries.get(service_name) {
            Some(entry) if entry.provider.as_ref().is_some_and(|p| p.plugin_id == plugin_id) => {
                self.entries.remove(service_name);
                Ok(())
            }
            Some(_) => Err(format!("本插件未登记服务：{service_name}")),
            None => Err(format!("服务未登记：{service_name}")),
        }
    }

    /// 摘掉某插件的全部登记（停用 / 崩溃 / 卸载）。返回受影响的服务名（升序）。
    pub(crate) fn drop_plugin(&mut self, plugin_id: &str) -> Vec<String> {
        let names: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, entry)| {
                entry.provider.as_ref().is_some_and(|provider| provider.plugin_id == plugin_id)
            })
            .map(|(name, _)| name.clone())
            .collect();
        for name in &names {
            self.entries.remove(name);
        }
        names
    }

    pub(crate) fn route(&self, service_name: &str) -> Route {
        match self.entries.get(service_name) {
            Some(entry) => match &entry.provider {
                Some(provider) => Route::Plugin(provider.plugin_id.clone()),
                None => Route::Builtin,
            },
            None => Route::Missing,
        }
    }

    /// 内置实现是否声明了该方法（`false` = 该服务由外部提供或没有这个方法）。
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
            .and_then(|entry| entry.provider.as_ref())
            .is_some_and(|entry| {
                entry.plugin_id == provider && entry.methods.iter().any(|name| name == method_name)
            })
    }

    /// 某服务当前状态的对外形状（登记变化时用于广播）。
    pub(crate) fn changed(&self, service_name: &str) -> ChangedParams {
        match self.entries.get(service_name) {
            None => ChangedParams {
                service: service_name.to_string(),
                provider: None,
                available: false,
            },
            Some(entry) => ChangedParams {
                service: service_name.to_string(),
                provider: entry.provider.as_ref().map(|p| p.plugin_id.clone()),
                available: true,
            },
        }
    }

    pub(crate) fn list(&self) -> ListResult {
        ListResult {
            services: self
                .entries
                .iter()
                .map(|(name, entry)| entry.info(name))
                .collect(),
        }
    }
}
/// 宿主通道命名空间：界面提交、装配声明与事件通道由宿主独占，不允许被登记成服务。
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
        self.services.lock().unwrap().register(plugin_id, &params)?;
        // 目录内容变了，壳侧注册表要刷新；同时广播该服务的可用性变化
        self.announce_service_dir(plugin_id);
        self.broadcast_service_changed(std::slice::from_ref(&params.service));
        Ok(())
    }

    pub(crate) fn unregister_service(
        &self,
        plugin_id: &str,
        service_name: &str,
    ) -> Result<(), String> {
        self.services.lock().unwrap().unregister(plugin_id, service_name)?;
        self.announce_service_dir(plugin_id);
        self.broadcast_service_changed(&[service_name.to_string()]);
        Ok(())
    }

    /// 服务当前是否在目录里。
    pub fn service_available(&self, service_name: &str) -> bool {
        !matches!(self.services.lock().unwrap().route(service_name), Route::Missing)
    }

    /// 在册服务快照（按服务名排序）。
    pub fn service_list(&self) -> ListResult {
        self.services.lock().unwrap().list()
    }

    /// 摘掉某插件的全部服务登记（停用 / 崩溃 / 卸载），返回受影响的服务名。
    pub(crate) fn drop_plugin_services(&self, plugin_id: &str) -> Vec<String> {
        let affected = self.services.lock().unwrap().drop_plugin(plugin_id);
        if !affected.is_empty() {
            self.announce_service_dir(plugin_id);
            self.broadcast_service_changed(&affected);
        }
        affected
    }

    /// 告诉壳服务目录变了（壳据此重建注册表的服务区）。插件侧仍只收可用性变化。
    fn announce_service_dir(&self, plugin_id: &str) {
        let _ = self
            .events
            .send(PluginEvent::ServiceChanged { id: plugin_id.to_string() });
    }
    /// 解析一次 `service.call`：先按目录校验服务与提供者的方法声明，再给出落点。
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

    /// 普通方法的转发判定：服务段的提供者是外部插件时转给它；宿主保留方法恒走本地。
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
            _ => Forward::Local,
        }
    }

    /// 不可分派的服务方法（提供者没声明该方法）的可读错误。
    pub(crate) fn undeclared_service_method(service_name: &str, method_name: &str) -> RpcError {
        service_error(
            service::error::UNKNOWN_METHOD,
            format!("{service_name} 未提供方法 {method_name}"),
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

    /// 服务可用性变化广播给全部运行中插件（进程不在跑的实例由实例侧静默丢弃）。
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
    /// 转发发生在插件读线程上，那里没有 tokio 上下文可借，故自建一条并让一个后台线程
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
        for (service_name, methods) in method::BUILTIN_SERVICES {
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
        assert_eq!(info(&list, "vault").provider, None, "内置服务归宿主");
    }

    /// 内置服务是宿主 API：插件不能占用这些名字，只能登记新服务名。
    #[test]
    fn builtin_service_names_cannot_be_registered() {
        let mut dir = ServiceDir::new();
        for (service_name, _) in method::BUILTIN_SERVICES {
            let error = dir.register("com.a", &params(service_name, &["echo"])).unwrap_err();
            assert!(error.contains("已被占用"), "{service_name} 应被拒：{error}");
            assert_eq!(dir.route(service_name), Route::Builtin, "{service_name} 仍归宿主");
        }
    }

    #[test]
    fn new_services_belong_to_their_first_provider() {
        let mut dir = ServiceDir::new();
        dir.register("com.a", &params("demo", &["echo"])).unwrap();
        assert_eq!(dir.route("demo"), Route::Plugin("com.a".into()));
        assert!(dir.provider_declares("com.a", "demo", "echo"));
        assert!(!dir.provider_declares("com.a", "demo", "other"));
        assert_eq!(dir.drop_plugin("com.a"), vec!["demo".to_string()]);
        assert_eq!(dir.route("demo"), Route::Missing, "无人提供即从目录消失");
    }

    /// 同名服务已有提供者时后来者拒收：没有仲裁规则，先登记者拥有名字。
    #[test]
    fn a_taken_service_name_is_refused() {
        let mut dir = ServiceDir::new();
        dir.register("com.a", &params("demo", &["echo"])).unwrap();
        let error = dir.register("com.b", &params("demo", &["echo"])).unwrap_err();
        assert!(error.contains("已被占用"), "{error}");
        assert_eq!(dir.route("demo"), Route::Plugin("com.a".into()), "先登记者不变");
    }

    #[test]
    fn host_channels_are_not_registerable() {
        let mut dir = ServiceDir::new();
        for channel in HOST_CHANNELS {
            let error = dir.register("com.a", &params(channel, &["echo"])).unwrap_err();
            assert!(error.contains("通道命名空间"), "{channel} 应被拒：{error}");
        }
    }

    #[test]
    fn unregister_requires_ownership_and_prunes_unserved_services() {
        let mut dir = ServiceDir::new();
        dir.register("com.a", &params("demo", &["echo"])).unwrap();
        assert!(dir.unregister("com.b", "demo").is_err(), "不是自己的登记");
        assert!(dir.unregister("com.a", "nope").is_err(), "未登记的服务");
        assert!(dir.unregister("com.a", "vault").is_err(), "内置服务不是插件的登记");
        dir.unregister("com.a", "demo").unwrap();
        assert_eq!(dir.route("demo"), Route::Missing);
    }

    #[test]
    fn changed_reports_provider_and_availability() {
        let mut dir = ServiceDir::new();
        assert_eq!(dir.changed("vault").provider, None);
        assert!(dir.changed("vault").available);
        assert!(!dir.changed("demo").available, "未登记即不可用");
        dir.register("com.a", &params("demo", &["echo"])).unwrap();
        assert_eq!(dir.changed("demo").provider.as_deref(), Some("com.a"));
    }
}
