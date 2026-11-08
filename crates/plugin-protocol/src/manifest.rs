//! 插件清单（`atelyx_manifest.toml`）：插件自述的身份、入口、兼容区间与权限声明，宿主装载前据此校验。
//! 本模块只含纯结构与序列化；入口存在性、协议版本一致性等需要文件系统的校验归宿主运行时。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 清单文件名（固定放在插件目录根部）。
pub const MANIFEST_FILE: &str = "atelyx_manifest.toml";

/// 清单格式版本。字段增删改即升版，旧版清单按「格式不识别」拒收。
pub const SCHEMA_VERSION: &str = "1.0.0";

/// 唯一的扩展类型：本期只承载 add-on（theme / asset-library 无对应文件格式）。
pub const TYPE_ADD_ON: &str = "add-on";

/// 支持的平台标签。
pub const PLATFORMS: [&str; 2] = ["windows-x64", "linux-x64"];

/// 插件清单。未知键拒收：拼错的键会静默丢掉声明，宁可报错。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// 清单格式版本，必须等于 [`SCHEMA_VERSION`]。
    pub schema_version: String,
    /// 反向域名 id（如 `com.atelyx.echo`），发布后不可变；同时是插件目录名与隔离命名空间。
    pub id: String,
    pub name: String,
    /// 一句话说明（安装页与详情页展示）。
    pub tagline: String,
    pub version: String,
    /// 扩展类型，只收 `add-on`。
    #[serde(rename = "type")]
    pub kind: String,
    /// 分类标签（安装页筛选用），可缺省。
    #[serde(default)]
    pub tags: Vec<String>,
    /// 维护者自述（宿主不校验形态）。
    pub maintainer: String,
    /// SPDX 标识列表（如 `SPDX:Apache-2.0`）。
    pub license: Vec<String>,
    #[serde(default)]
    pub copyright: Vec<String>,
    #[serde(default)]
    pub website: Option<String>,
    /// 支持的平台标签；缺省 = 全平台。
    #[serde(default)]
    pub platforms: Vec<String>,
    /// 最低宿主版本（语义化版本）；低于它即拒绝装载。
    pub host_version_min: String,
    /// 最高宿主版本（含）；缺省 = 不限。
    #[serde(default)]
    pub host_version_max: Option<String>,
    /// 协议版本，必须等于宿主的 [`crate::API_VERSION`]。
    pub api_version: u32,
    /// 生效的插件只有一份实例（多窗口共享同一宿主运行时单例）。
    #[serde(default)]
    pub singleton: bool,
    /// 切换仓库容器（本地 ↔ 空间 / 换仓库）时保持挂载；false = 宿主重载插件。
    #[serde(default)]
    pub keep_mounted_on_vault_switch: bool,
    /// 权限表：键 = 能力面，值 = 给用户看的原因。用于安装公示、详情展示与审计归类，
    /// 不拦截调用——插件与宿主同地址空间，这份声明是知情权而非沙箱边界。
    #[serde(default)]
    pub permissions: BTreeMap<PermissionKey, String>,
    pub entry: Entry,
    #[serde(default)]
    pub build: BuildSpec,
    /// 领域文档类型注册（扩展名 → 承载贡献）。
    #[serde(default)]
    pub doc_type_registrations: Vec<DocTypeRegistration>,
}

/// 按平台的插件入口（相对插件目录的路径）。缺失 = 该平台不支持。
/// 入口是原生动态库：宿主按 C ABI 装载到本进程，插件与宿主同地址空间。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux: Option<String>,
}

impl Entry {
    /// 当前平台的动态库相对路径；缺失即本平台不支持装载。
    pub fn for_current_platform(&self) -> Option<&str> {
        #[cfg(target_os = "windows")]
        {
            self.windows.as_deref()
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.linux.as_deref()
        }
    }
}

/// 打包设置（只被 `extension build` 读取，宿主装载时不用）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildSpec {
    /// 要打进包的文件路径（相对清单目录）；缺省 = 整目录。
    #[serde(default)]
    pub paths: Vec<String>,
    /// 打包时排除的路径模式。
    #[serde(default)]
    pub paths_exclude_pattern: Vec<String>,
}

/// 权限键：一键对应一组能力面，键集合即安装公示的粒度。增删随协议升版。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionKey {
    /// `http`。
    Network,
    /// `fs` / `vault` / `table` / 编辑选区 / 活动统计——凡读写仓库内容都算。
    Files,
    Clipboard,
    /// 自起子进程。
    Process,
    /// 插件窗口。
    Window,
    Tray,
    /// 全局快捷键。
    Shortcut,
    Notification,
    Collab,
    Ai,
    /// 系统文件 / 目录对话框。
    Dialog,
    /// 插件私有数据。
    State,
}

impl PermissionKey {
    /// 展示文案（安装确认与详情页的权限行）。
    pub fn label(self) -> &'static str {
        match self {
            PermissionKey::Network => "网络",
            PermissionKey::Files => "文件",
            PermissionKey::Clipboard => "剪贴板",
            PermissionKey::Process => "进程",
            PermissionKey::Window => "窗口",
            PermissionKey::Tray => "托盘",
            PermissionKey::Shortcut => "快捷键",
            PermissionKey::Notification => "通知",
            PermissionKey::Collab => "协作",
            PermissionKey::Ai => "AI",
            PermissionKey::Dialog => "对话框",
            PermissionKey::State => "数据",
        }
    }
}

/// 领域文档类型注册：扩展名、展示标签、图标名与承载文档内容的贡献 id。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocTypeRegistration {
    /// 不带点的扩展名（如 `board`）。
    pub extension: String,
    pub label: String,
    /// 宿主内置形状名（未知名回落拼图）。
    pub icon: String,
    /// 承载该文档实例的贡献 id（插件须同时以 `view.register` 注册同名贡献，
    /// `slot` 为面板）；打开该扩展名文件时宿主分配 surfaceId 并通知插件。
    pub surface: String,
}

/// 纯结构校验：id 形态与目录安全、必填字段非空、格式版本与类型、平台标签、权限原因。
/// 平台入口存在性与协议版本一致性依赖文件系统与宿主上下文，由宿主运行时校验。
impl Manifest {
    pub fn validate_shape(&self) -> Result<(), String> {
        validate_id(&self.id)?;
        if self.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "清单格式版本不识别：{}（要求 {SCHEMA_VERSION}）",
                self.schema_version
            ));
        }
        if self.kind != TYPE_ADD_ON {
            return Err(format!("扩展类型不支持：{}（只收 {TYPE_ADD_ON}）", self.kind));
        }
        for (name, value) in [
            ("name", &self.name),
            ("tagline", &self.tagline),
            ("version", &self.version),
            ("maintainer", &self.maintainer),
        ] {
            if value.trim().is_empty() {
                return Err(format!("{name} 不能为空"));
            }
        }
        if self.license.iter().all(|item| item.trim().is_empty()) {
            return Err("license 不能为空".into());
        }
        for platform in &self.platforms {
            if !PLATFORMS.contains(&platform.as_str()) {
                return Err(format!(
                    "平台标签不识别：{platform}（可选 {}）",
                    PLATFORMS.join(" / ")
                ));
            }
        }
        if self.api_version == 0 {
            return Err("api_version 不能为空或 0".into());
        }
        if parse_semver(&self.host_version_min).is_none() {
            return Err(format!(
                "host_version_min 非法（须为点分数字版本，如 0.4 或 0.4.0）：{}",
                self.host_version_min
            ));
        }
        if let Some(max) = &self.host_version_max {
            if parse_semver(max).is_none() {
                return Err(format!(
                    "host_version_max 非法（须为点分数字版本，如 0.9 或 0.9.0）：{max}"
                ));
            }
        }
        for (key, reason) in &self.permissions {
            if reason.trim().is_empty() {
                return Err(format!("权限 {} 缺原因（值须写清为什么要这项权限）", key.label()));
            }
        }
        for reg in &self.doc_type_registrations {
            if reg.extension.trim().is_empty() || reg.extension.contains('.') {
                return Err(format!("文档类型扩展名非法：{}", reg.extension));
            }
            if reg.surface.trim().is_empty() {
                return Err(format!("文档类型 {} 缺承载贡献 id（surface）", reg.extension));
            }
        }
        Ok(())
    }

    /// 本清单是否声明支持当前平台（缺省 = 全平台）。
    pub fn supports_current_platform(&self) -> bool {
        if self.platforms.is_empty() {
            return true;
        }
        let current = current_platform();
        self.platforms.iter().any(|platform| platform == current)
    }

    /// 是否声明了某权限（审计的 `declared` 判定）。
    pub fn declares(&self, key: PermissionKey) -> bool {
        self.permissions.contains_key(&key)
    }

    /// 权限表展开为「键, 原因」，按键序（安装确认与详情页按此顺序展示）。
    pub fn permission_list(&self) -> Vec<(PermissionKey, String)> {
        self.permissions
            .iter()
            .map(|(key, reason)| (*key, reason.clone()))
            .collect()
    }
}

/// 当前平台的清单标签（与 [`PLATFORMS`] 同口径）。
pub fn current_platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows-x64"
    } else if cfg!(target_os = "linux") {
        "linux-x64"
    } else {
        "unknown"
    }
}

/// 宽松语义化版本解析：`x` / `x.y` / `x.y.z`（缺段补 0），可带 `-` 预发布后缀（忽略）。
/// 返回 `None` = 非法形态。宿主与插件两侧共用同一口径做版本比较。
pub fn parse_semver(version: &str) -> Option<(u32, u32, u32)> {
    let core = version.trim().split('-').next().unwrap_or("");
    if core.is_empty() {
        return None;
    }
    let mut parts = [0u32; 3];
    let mut count = 0;
    for (index, segment) in core.split('.').enumerate() {
        if index >= 3 {
            return None;
        }
        if segment.is_empty() || !segment.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        parts[index] = segment.parse().ok()?;
        count += 1;
    }
    (count >= 1).then_some((parts[0], parts[1], parts[2]))
}

/// 反向域名 id：至少两段、每段为字母数字或 `-` 且不以 `-` 起止；同时用作目录名，
/// 故禁止路径分隔符与点开头（避免与宿主保留的隐藏目录混淆）。
fn validate_id(id: &str) -> Result<(), String> {
    if id.starts_with('.') {
        return Err(format!("id 不得以点开头：{id}"));
    }
    if id.contains(['/', '\\']) {
        return Err(format!("id 不得含路径分隔符：{id}"));
    }
    let labels: Vec<&str> = id.split('.').collect();
    if labels.len() < 2 {
        return Err(format!("id 须为反向域名（至少两段）：{id}"));
    }
    for label in labels {
        let valid = !label.is_empty()
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if !valid {
            return Err(format!("id 段非法：{label}（{id}）"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        Manifest {
            schema_version: SCHEMA_VERSION.into(),
            id: "com.atelyx.echo".into(),
            name: "Echo".into(),
            tagline: "示例".into(),
            version: "0.1.0".into(),
            kind: TYPE_ADD_ON.into(),
            tags: Vec::new(),
            maintainer: "Example".into(),
            license: vec!["SPDX:Apache-2.0".into()],
            copyright: Vec::new(),
            website: None,
            platforms: Vec::new(),
            host_version_min: "0.4.0".into(),
            host_version_max: None,
            api_version: 1,
            singleton: false,
            keep_mounted_on_vault_switch: false,
            permissions: BTreeMap::new(),
            entry: Entry { windows: Some("echo.dll".into()), linux: Some("libecho.so".into()) },
            build: BuildSpec::default(),
            doc_type_registrations: Vec::new(),
        }
    }

    #[test]
    fn shape_accepts_valid_manifest() {
        sample().validate_shape().expect("合法清单应通过");
    }

    #[test]
    fn shape_rejects_bad_ids() {
        for bad in
            ["echo", "com.", ".com.x", "com..x", "com.at el yx", "com.a/b", "com.-a", "com.a-"]
        {
            let mut m = sample();
            m.id = bad.into();
            assert!(m.validate_shape().is_err(), "应拒绝 id：{bad}");
        }
    }

    #[test]
    fn shape_rejects_empty_required_fields() {
        let mutators: [fn(&mut Manifest); 4] = [
            |m| m.name = " ".into(),
            |m| m.tagline = "".into(),
            |m| m.version = String::new(),
            |m| m.maintainer = "  ".into(),
        ];
        for mutate in mutators {
            let mut m = sample();
            mutate(&mut m);
            assert!(m.validate_shape().is_err());
        }
        let mut m = sample();
        m.license = vec![" ".into()];
        assert!(m.validate_shape().is_err(), "license 不得为空项");
        let mut m = sample();
        m.api_version = 0;
        assert!(m.validate_shape().is_err(), "api_version 不得为 0");
    }

    #[test]
    fn shape_rejects_unknown_schema_type_and_platform() {
        let mut m = sample();
        m.schema_version = "2.0.0".into();
        assert!(m.validate_shape().is_err(), "格式版本不识别应拒");
        let mut m = sample();
        m.kind = "theme".into();
        assert!(m.validate_shape().is_err(), "非 add-on 应拒");
        let mut m = sample();
        m.platforms = vec!["macos-arm64".into()];
        assert!(m.validate_shape().is_err(), "未知平台标签应拒");
    }

    /// 权限原因必填：空原因等于把公示机制作废。
    #[test]
    fn permission_reason_must_not_be_empty() {
        let mut m = sample();
        m.permissions.insert(PermissionKey::Network, "  ".into());
        let err = m.validate_shape().expect_err("空原因应拒");
        assert!(err.contains("网络"), "{err}");
        m.permissions.insert(PermissionKey::Network, "拉取素材".into());
        m.validate_shape().expect("有原因即通过");
        assert!(m.declares(PermissionKey::Network));
        assert!(!m.declares(PermissionKey::Files));
    }

    /// TOML 形态：表结构、平台入口映射、未知键拒收。
    #[test]
    fn toml_roundtrip_and_unknown_keys_rejected() {
        let mut m = sample();
        m.permissions.insert(PermissionKey::Files, "导入导出".into());
        let text = toml::to_string(&m).unwrap();
        assert!(text.contains("[permissions]"), "{text}");
        assert!(text.contains("files = \"导入导出\""), "{text}");
        assert!(text.contains("[entry]"), "{text}");
        let again: Manifest = toml::from_str(&text).unwrap();
        assert_eq!(again, m);

        let misspelled = text.replace("host_version_min", "hostVersionMin");
        let err = toml::from_str::<Manifest>(&misspelled).expect_err("拼错的键应拒收");
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    /// 入口按平台取：只声明 Windows 时本平台（Linux）不可装载。
    #[test]
    fn entry_resolves_per_platform() {
        let m = sample();
        assert_eq!(
            m.entry.for_current_platform(),
            Some(if cfg!(windows) { "echo.dll" } else { "libecho.so" })
        );
        let windows_only = Entry {
            windows: Some("echo.dll".into()),
            linux: None,
        };
        assert_eq!(
            windows_only.for_current_platform(),
            cfg!(windows).then_some("echo.dll")
        );
    }

    #[test]
    fn platform_filter_honors_empty_as_all() {
        let mut m = sample();
        assert!(m.supports_current_platform(), "缺省 = 全平台");
        m.platforms = vec![current_platform().to_string()];
        assert!(m.supports_current_platform());
        m.platforms = vec!["linux-x64".into()];
        assert_eq!(m.supports_current_platform(), !cfg!(windows));
    }
}

