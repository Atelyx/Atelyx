//! Atelyx 插件协议：宿主与插件之间的对外契约。
//! 握手校验契约版本；信封按 id 配对请求与回执；绘制指令、输入事件、能力方法与事件
//! 均为封闭枚举，manifest 的 `apiVersion` 与握手版本都取同一个 [`API_VERSION`]。

pub mod chat;
pub mod envelope;
pub mod event;
pub mod frame;
pub mod handshake;
pub mod layout;
pub mod manifest;
pub mod method;
pub mod paint;
pub mod service;

/// 契约版本：取 [`atelyx_plugin_abi::CONTRACT_VERSION`] 这同一个值，宿主与插件的 manifest、
/// 握手、函数表都按它严格相等校验。协议冻结为稳定契约前不递增本号。
pub const API_VERSION: u32 = atelyx_plugin_abi::CONTRACT_VERSION;

/// 方法名的方向约定：`host:` 前缀归宿主保留，只有宿主→插件方向可调用；
/// 插件→宿主只可调用无前缀方法。方向错用即协议违规。
pub fn is_host_method(method: &str) -> bool {
    method.starts_with("host:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_version_is_the_abi_contract_version() {
        assert_eq!(API_VERSION, atelyx_plugin_abi::CONTRACT_VERSION);
    }
}
