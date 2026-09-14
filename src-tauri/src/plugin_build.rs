//! 插件依赖获取与产物打包：把清单声明的 npm 依赖打成自包含单文件产物。
//!
//! 目标：开发者只声明标准依赖，其余由宿主在**安装/更新时**完成——解析、取件、校验、打包；
//! 运行时只求值产物，用户机器不需要 Node/pnpm，启动时不需要联网。
//!
//! 为什么只读 `package-lock.json` 而不做版本区间解析：锁文件已经把整棵树钉到具体版本与字节
//! （`resolved` + `integrity`），宿主照单取件即可——零 semver 解析、结果可复现（同一份锁在
//! 任何机器上得到同一份依赖），并且天然满足「当前版/上一版各自携带自己的锁定信息」：锁与产物
//! 都在插件目录内，随版本目录一起切换与回退，不需要额外状态字段。
//!
//! 产物 = `<插件根>/.atelyx-dist/entry.js`（ESM、浏览器 realm）。取件与打包都在安装事务的候选
//! 目录内完成，失败即整体失败、当前版本不受影响。
//!
//! 边界：产物跑在 WebView 里，所以只有**浏览器可用**的 npm 包能进来——依赖 Node 内置模块
//! （fs/child_process 等）、原生扩展（.node）、或无法静态解析的 `require(变量)` 的包会在打包
//! 阶段失败，并尽力指认到来源文件；这类需求走 `ctx.native.invoke` / `shell.exec` 逃生舱。
//!
//! 取件地址来自插件仓库提交的锁文件：安装即授权（与插件代码本身的信任级别一致），故只做
//! https 白名单，不额外叠加网络边界。

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use flate2::read::GzDecoder;
use serde_json::{Map, Value};
use sha2::{Digest, Sha512};
use tauri::{AppHandle, Manager};

use crate::commands::plugin::sanitize_archive_entry;

/// 产物目录名（插件根内）与产物入口（相对插件根）。
pub const BUILD_DIR: &str = ".atelyx-dist";
pub const BUILD_ENTRY: &str = ".atelyx-dist/entry.js";

/// 单个 tarball 下载体积上限。
const MAX_TARBALL_BYTES: u64 = 128 * 1024 * 1024;
/// tar 内单条目解压上限。
const MAX_TAR_ENTRY_BYTES: u64 = 32 * 1024 * 1024;
/// 单个 tarball 解压总量上限。
const MAX_TAR_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
/// 单个 tarball 条目数上限。
const MAX_TAR_ENTRY_COUNT: usize = 10_000;
/// 一次安装取件的依赖包数量上限（生产依赖闭包）。
const MAX_DEP_PACKAGES: usize = 2_000;

/// 打包产物的体积上限：产物把整棵依赖内联进一个文件，比手写入口大得多。
/// 读入口侧共用同一常量——安装期就按它拒掉，避免「装成功、运行时才发现读不出来」。
pub const MAX_BUILT_ENTRY_BYTES: u64 = 32 * 1024 * 1024;

/// 打包器单次执行挂死宽限（不是产物体积/时长上限，只兜网络黑洞与病态依赖图）。
const ESBUILD_HANG_GUARD: Duration = Duration::from_secs(10 * 60);

/// 一次安装取件的整体墙钟上限（同样只兜黑洞与病态依赖图，不是正常耗时的预算）。
const DEP_FETCH_HANG_GUARD: Duration = Duration::from_secs(30 * 60);

/// 宿主提供的 React 全局接成 `react` 包形状的注入模块。
///
/// 为什么不让依赖自带一份 React：宿主与插件同 realm、同一棵渲染树，两份 React 实例会让组件
/// 调用 hooks 直接失败。导出名逐条列出也是刻意的——依赖用到清单外的导出时，打包阶段就以
/// 「找不到导出」失败（安装期可见），而不是拖到运行时才炸。
const REACT_SHIM: &str = r#"const R = globalThis.React;
if (!R) throw new Error("宿主 React 未就绪，插件依赖无法初始化");
export default R;
export const Fragment = R.Fragment;
export const StrictMode = R.StrictMode;
export const Suspense = R.Suspense;
export const Children = R.Children;
export const Component = R.Component;
export const PureComponent = R.PureComponent;
export const version = R.version;
export const isValidElement = R.isValidElement;
export const createElement = (...a) => R.createElement(...a);
export const cloneElement = (...a) => R.cloneElement(...a);
export const createRef = (...a) => R.createRef(...a);
export const createContext = (...a) => R.createContext(...a);
export const forwardRef = (...a) => R.forwardRef(...a);
export const memo = (...a) => R.memo(...a);
export const lazy = (...a) => R.lazy(...a);
export const startTransition = (...a) => R.startTransition(...a);
export const useState = (...a) => R.useState(...a);
export const useEffect = (...a) => R.useEffect(...a);
export const useLayoutEffect = (...a) => R.useLayoutEffect(...a);
export const useInsertionEffect = (...a) => R.useInsertionEffect(...a);
export const useMemo = (...a) => R.useMemo(...a);
export const useCallback = (...a) => R.useCallback(...a);
export const useRef = (...a) => R.useRef(...a);
export const useReducer = (...a) => R.useReducer(...a);
export const useContext = (...a) => R.useContext(...a);
export const useImperativeHandle = (...a) => R.useImperativeHandle(...a);
export const useDebugValue = (...a) => R.useDebugValue(...a);
export const useId = (...a) => R.useId(...a);
export const useSyncExternalStore = (...a) => R.useSyncExternalStore(...a);
export const useTransition = (...a) => R.useTransition(...a);
export const useDeferredValue = (...a) => R.useDeferredValue(...a);
"#;

/// 自动 JSX 运行时（`react/jsx-runtime`）的注入模块：依赖里用自动运行时编译的代码走这里，
/// 同样落到宿主的那一份 React 上。
const JSX_RUNTIME_SHIM: &str = r#"const R = globalThis.React;
if (!R) throw new Error("宿主 React 未就绪，插件依赖无法初始化");
const element = (type, props, key) =>
  key === undefined ? R.createElement(type, props) : R.createElement(type, Object.assign({ key }, props));
export const Fragment = R.Fragment;
export const jsx = element;
export const jsxs = element;
export const jsxDEV = element;
"#;

/// 注入模块在产物目录内的落点（相对插件根；打包完即删）。
const SHIM_DIR: &str = ".atelyx-dist/_shims";

/// 打包器在资源目录内的相对位置（与 `tauri.conf.json` 的 `bundle.resources` 一致）。
const ESBUILD_RESOURCE_DIR: &str = "resources/esbuild";

/// esbuild 在无法静态解析 `require` 时会写入这段运行时兜底（我们随应用分发的版本固定）。
/// 判定要求整句两段都命中：只认半句的话，插件源码或依赖里出现同名词（文案、测试夹具）会被误判
/// 成产物有问题而拒绝安装。
const DYNAMIC_REQUIRE_MARKER: &str = "Dynamic require of \"";
const DYNAMIC_REQUIRE_SUFFIX: &str = "is not supported";

/// 插件是否已有打包产物（入口指向依据：产物存在即用产物，否则回落到清单 main）。
pub fn built_entry(plugin_root: &Path) -> Option<&'static str> {
    plugin_root.join(BUILD_ENTRY).is_file().then_some(BUILD_ENTRY)
}

/// 依据清单准备自包含产物。
///
/// - 未要求打包（既没声明依赖、也没显式开启）→ 清掉可能残留的产物并返回（保证「入口是清单的
///   函数」：去掉依赖或关掉开关的版本不会继续跑上一版产物）。
/// - `discard_dependencies` = 打包后删除解压出来的依赖：安装到插件目录的常规路径为 true；
///   本地目录来源是开发者的实时引用，其 node_modules 归开发者，故传 false。
pub async fn prepare_artifact(
    app: &AppHandle,
    plugin_root: &Path,
    manifest: &Value,
    discard_dependencies: bool,
) -> Result<(), String> {
    let deps = runtime_dependencies(manifest);
    let bundle_requested = manifest["atelyx"]["bundle"].as_bool() == Some(true);
    let build_dir = plugin_root.join(BUILD_DIR);

    if deps.is_empty() && !bundle_requested {
        remove_build_dir(&build_dir)?;
        return Ok(());
    }

    let entry = manifest["main"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or("清单未声明入口（main），无法打包")?;
    // 入口限定在插件根内（与运行时读入口同口径），拒绝符号链接段。
    let entry_path = crate::commands::plugin::safe_plugin_path(plugin_root, entry)?;

    // 每次从干净目录重建：产物内容不参与增量判断，避免上一版残留混进新产物。
    remove_build_dir(&build_dir)?;
    std::fs::create_dir_all(&build_dir).map_err(|e| format!("创建产物目录失败：{e}"))?;

    if !deps.is_empty() {
        let cache = cache_dir(app)?;
        let client = http_client()?;
        // 逐块空闲超时挡不住「服务端持续慢速送字节」，故整轮取件另设墙钟上限（与 git 同一口径）。
        let fetched = tokio::time::timeout(
            DEP_FETCH_HANG_GUARD,
            materialize_dependencies(&cache, &client, plugin_root, &deps),
        )
        .await;
        match fetched {
            Ok(result) => result?,
            Err(_) => return Err("获取依赖超时（网络不可达或依赖过多），已中止".into()),
        }
    }
    let bundle_result = run_esbuild(app, plugin_root, &entry_path).await;
    if discard_dependencies && !deps.is_empty() {
        // 产物已自包含，解压出来的依赖不再需要；删不掉不算安装失败，但要留痕。
        if let Err(e) = std::fs::remove_dir_all(plugin_root.join("node_modules")) {
            eprintln!("[plugin] 清理依赖目录失败（不影响使用）：{e}");
        }
    }
    bundle_result
}

/// 清单声明的运行时依赖（`dependencies`；devDependencies / peerDependencies / optionalDependencies
/// 都不参与——前两者不是运行所需，后者大量是平台专用包，强行纳入会把安装失败率推高）。
fn runtime_dependencies(manifest: &Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(obj) = manifest.get("dependencies").and_then(|d| d.as_object()) {
        for (name, range) in obj {
            if name.trim().is_empty() {
                continue;
            }
            if let Some(value) = range.as_str() {
                out.insert(name.clone(), value.to_string());
            }
        }
    }
    out
}

/// 删除产物目录；不存在即无操作，删不掉则报错（残留产物会被当成有效入口）。
fn remove_build_dir(build_dir: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(build_dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("清理旧产物失败：{e}")),
    }
}

// ===== 锁定信息解析 =====

/// 读 `package-lock.json` 并取出 `packages` 映射（npm 7+ 的 lockfileVersion 2/3 形态）。
fn read_lock_packages(plugin_root: &Path) -> Result<Map<String, Value>, String> {
    let path = crate::commands::plugin::safe_plugin_path(plugin_root, "package-lock.json")
        .map_err(|e| format!("{e}（声明了 dependencies 必须提交 package-lock.json）"))?;
    let raw = std::fs::read_to_string(&path).map_err(|e| {
        format!("读取 package-lock.json 失败（声明了 dependencies 必须提交该文件）：{e}")
    })?;
    let lock: Value = serde_json::from_str(&raw)
        .map_err(|e| format!("package-lock.json 不是合法 JSON：{e}"))?;
    lock.get("packages")
        .and_then(|p| p.as_object())
        .cloned()
        .ok_or("package-lock.json 缺少 packages 段（需 npm 7+ 生成的锁文件）".to_string())
}

/// 生产依赖闭包：从清单的 `dependencies` 出发，按锁文件里的实际落盘路径逐层解析。
/// 返回 (锁键, 包条目) 列表，键即 `node_modules` 下的相对路径。
fn resolve_closure(
    packages: &Map<String, Value>,
    root_deps: &BTreeMap<String, String>,
) -> Result<Vec<(String, Value)>, String> {
    let mut queue: VecDeque<(String, String)> = VecDeque::new();
    for name in root_deps.keys() {
        queue.push_back((name.clone(), String::new()));
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<(String, Value)> = Vec::new();
    while let Some((name, from)) = queue.pop_front() {
        let key = resolve_package_key(packages, &from, &name).ok_or_else(|| {
            if from.is_empty() {
                format!("package-lock.json 缺少依赖 {name}")
            } else {
                format!("package-lock.json 缺少依赖 {name}（被 {from} 依赖）")
            }
        })?;
        if !seen.insert(key.clone()) {
            continue;
        }
        if out.len() >= MAX_DEP_PACKAGES {
            return Err(format!("依赖数量超过上限（{MAX_DEP_PACKAGES} 个）"));
        }
        let entry = packages.get(&key).cloned().unwrap_or(Value::Null);
        let mut children: Vec<String> = entry
            .get("dependencies")
            .and_then(|d| d.as_object())
            .map(|d| d.keys().cloned().collect())
            .unwrap_or_default();
        // 固定顺序：同一份锁在任何机器上得到同一套取件与日志顺序。
        children.sort();
        for child in children {
            queue.push_back((child, key.clone()));
        }
        out.push((key, entry));
    }
    Ok(out)
}

/// 按 npm 的解析规则定位依赖的落盘键：从引用方所在目录逐层向上找 `node_modules/<name>`。
/// 键形如 `node_modules/a/node_modules/b`，空串 = 插件根。
fn resolve_package_key(packages: &Map<String, Value>, from: &str, name: &str) -> Option<String> {
    let mut base = from.to_string();
    loop {
        let candidate = if base.is_empty() {
            format!("node_modules/{name}")
        } else {
            format!("{base}/node_modules/{name}")
        };
        if packages.contains_key(&candidate) {
            return Some(candidate);
        }
        if base.is_empty() {
            return None;
        }
        // 上跳一层：剥掉末尾的 `/node_modules/<包名>`；根层带 `node_modules/` 前缀则回到根。
        match base.rfind("/node_modules/") {
            Some(idx) => base.truncate(idx),
            None => base.clear(),
        }
    }
}

/// 锁键 → 落盘相对路径。锁键来自不可信的插件仓库，逐段校验后才允许拼路径。
fn dependency_target_path(key: &str) -> Result<PathBuf, String> {
    if key.contains('\\') {
        return Err(format!("依赖路径非法：{key}"));
    }
    let mut parts = key.split('/');
    if parts.next() != Some("node_modules") {
        return Err(format!("依赖路径非法：{key}"));
    }
    let mut out = PathBuf::from("node_modules");
    let mut segments = 0usize;
    for seg in parts {
        if seg.is_empty() || seg == "." || seg == ".." || seg.contains(':') {
            return Err(format!("依赖路径非法：{key}"));
        }
        out.push(seg);
        segments += 1;
    }
    if segments == 0 {
        return Err(format!("依赖路径非法：{key}"));
    }
    Ok(out)
}

/// 解析 `integrity` 取 sha512 摘要（多段时取 sha512 那一段；非 sha512 一律拒绝）。
fn integrity_digest(integrity: &str) -> Result<Vec<u8>, String> {
    let token = integrity
        .split_whitespace()
        .find(|t| t.starts_with("sha512-"))
        .ok_or_else(|| format!("依赖锁定信息缺少 sha512 摘要：{integrity}"))?;
    let encoded = &token["sha512-".len()..];
    let digest = decode_base64(encoded)?;
    // sha512 = 64 字节：长度不对说明锁定信息本身畸形，在此拦下（否则会一路走到「校验不符」，
    // 报出的原因与真实原因无关，误导排查）。
    if digest.len() != 64 {
        return Err(format!("依赖锁定信息的 sha512 摘要长度异常：{integrity}"));
    }
    Ok(digest)
}

/// base64 解码（容忍有无 padding 两种写法）。
fn decode_base64(encoded: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(encoded))
        .map_err(|e| format!("摘要不是合法 base64：{e}"))
}

/// 字节数组的小写十六进制表示（用作缓存文件名，内容寻址）。
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

// ===== 取件与解压 =====

/// HTTP 客户端：逐块空闲超时（大 tarball 在慢链路上不会被中途 abort），体积上限在读取路径上。
fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .read_timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| format!("创建网络客户端失败：{e}"))
}

/// 依赖缓存目录（内容寻址，按包名无关的摘要寻址：同一份锁定在任何插件间复用）。
fn cache_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("plugin-cache");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建依赖缓存目录失败：{e}"))?;
    sweep_stale_temp(&dir);
    Ok(dir)
}

/// 清理下载中途被打断留下的临时文件（`.tmp-*`）：它们不在内容寻址的命名空间里，
/// 没有别的路径会回收，不清就会随每次中断堆积（单个上限可达 tarball 体积上限）。
/// 判龄用 mtime——这些文件不经改名产生，mtime 即创建时刻。
fn sweep_stale_temp(dir: &Path) {
    const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with(".tmp-") {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if now.duration_since(modified).is_ok_and(|age| age >= MAX_AGE) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// 取件整轮：读锁 → 逐个依赖「缓存命中校验 / 下载校验」→ 解压到 node_modules 对应路径。
async fn materialize_dependencies(
    cache: &Path,
    client: &reqwest::Client,
    plugin_root: &Path,
    deps: &BTreeMap<String, String>,
) -> Result<(), String> {
    let packages = read_lock_packages(plugin_root)?;
    let selected = resolve_closure(&packages, deps)?;
    for (key, entry) in selected {
        let resolved = entry
            .get("resolved")
            .and_then(|r| r.as_str())
            .ok_or_else(|| format!("依赖 {key} 缺少 resolved（file:/link:/git 来源不受支持）"))?;
        if !resolved.starts_with("https://") {
            return Err(format!(
                "依赖 {key} 的来源不受支持（仅支持 registry 的 https 下载）：{resolved}"
            ));
        }
        let integrity = entry
            .get("integrity")
            .and_then(|i| i.as_str())
            .ok_or_else(|| format!("依赖 {key} 缺少 integrity 校验信息"))?;
        let digest = integrity_digest(integrity)
            .map_err(|e| format!("依赖 {key} 的锁定信息不可用：{e}"))?;
        let cached = cache.join(format!("{}.tgz", hex(&digest)));
        fetch_to_cache(client, resolved, &cached, &digest)
            .await
            .map_err(|e| format!("获取依赖 {key} 失败：{e}"))?;
        extract_tgz_safe(&cached, plugin_root, &key)?;
    }
    Ok(())
}

/// 下载到缓存并按摘要校验。缓存命中也要校验：磁盘内容不可信（写坏/被替换）时不校验就等于
/// 把错误内容打进产物，而重下一个 tarball 的代价远小于一次错产物。
async fn fetch_to_cache(
    client: &reqwest::Client,
    url: &str,
    cache_path: &Path,
    digest: &[u8],
) -> Result<(), String> {
    if cache_path.is_file() {
        if sha512_file(cache_path).is_ok_and(|d| d == digest) {
            return Ok(());
        }
        let _ = std::fs::remove_file(cache_path);
    }
    let dir = cache_path.parent().ok_or("依赖缓存路径异常")?;
    let temp = dir.join(format!(".tmp-{}", nanoid::nanoid!()));
    let download = download_to(client, url, &temp, digest).await;
    if let Err(e) = download {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    // 先写临时文件再改名：半个文件不会被后续安装当成缓存命中。
    std::fs::rename(&temp, cache_path).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        format!("写入依赖缓存失败：{e}")
    })
}

/// 流式下载到文件，边下边算 sha512；下满校验不符即失败（不留文件）。
async fn download_to(
    client: &reqwest::Client,
    url: &str,
    temp: &Path,
    digest: &[u8],
) -> Result<(), String> {
    let mut resp = client
        .get(url)
        .header("User-Agent", "atelyx")
        .send()
        .await
        .map_err(|e| format!("下载失败：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("下载失败（HTTP {}）", resp.status()));
    }
    let mut file = std::fs::File::create(temp).map_err(|e| e.to_string())?;
    let mut hasher = Sha512::new();
    let mut total: u64 = 0;
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("下载失败：{e}"))? {
        total += chunk.len() as u64;
        if total > MAX_TARBALL_BYTES {
            return Err("依赖包超过体积上限".into());
        }
        hasher.update(&chunk);
        file.write_all(&chunk).map_err(|e| e.to_string())?;
    }
    if hasher.finalize().as_slice() != digest {
        return Err("依赖包校验失败（内容与锁定摘要不符）".into());
    }
    Ok(())
}

/// 计算文件的 sha512 摘要。
fn sha512_file(path: &Path) -> Result<Vec<u8>, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha512::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_vec())
}

/// 解压 npm tarball 到插件根下的依赖路径。条目须在 `package/` 之下（registry 打包约定），
/// 只接受普通文件与目录——链接类条目会让解压结果指向依赖目录之外，一律拒绝。
///
/// 落盘目的地按插件内路径逐段拒绝符号链接：git 来源的插件可以把 `node_modules` 提交成指向
/// 插件根之外的链接，跟随它写文件就是在插件根外落盘（与运行时读入口同一把守卫）。
fn extract_tgz_safe(tgz: &Path, plugin_root: &Path, key: &str) -> Result<(), String> {
    dependency_target_path(key)?;
    let dest = crate::commands::plugin::safe_plugin_path(plugin_root, key)?;
    let file = std::fs::File::open(tgz).map_err(|e| format!("打开依赖包失败：{e}"))?;
    let mut archive = tar::Archive::new(GzDecoder::new(std::io::BufReader::new(file)));
    std::fs::create_dir_all(&dest).map_err(|e| format!("创建依赖目录失败：{e}"))?;
    let entries = archive.entries().map_err(|e| format!("读取依赖包失败：{e}"))?;
    let mut count = 0usize;
    let mut total: u64 = 0;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("读取依赖包失败：{e}"))?;
        count += 1;
        if count > MAX_TAR_ENTRY_COUNT {
            return Err("依赖包条目过多".into());
        }
        let raw = entry
            .path()
            .map_err(|e| format!("读取依赖包条目名失败：{e}"))?
            .to_string_lossy()
            .into_owned();
        let stripped = raw.strip_prefix("package/").ok_or_else(|| {
            format!("依赖包条目不在 package/ 目录下：{raw}")
        })?;
        if stripped.is_empty() {
            continue;
        }
        let clean = sanitize_archive_entry(stripped)?;
        let entry_path = dest.join(&clean);
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            std::fs::create_dir_all(&entry_path).map_err(|e| e.to_string())?;
            continue;
        }
        if !kind.is_file() {
            return Err(format!("依赖包含不支持的条目类型：{raw}"));
        }
        let declared = entry.header().size().unwrap_or(0);
        if declared > MAX_TAR_ENTRY_BYTES {
            return Err(format!("依赖包条目过大：{raw}"));
        }
        if let Some(parent) = entry_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut writer = std::fs::File::create(&entry_path).map_err(|e| e.to_string())?;
        // 头里声明的长度不可信，按实际写出字节设硬上限。
        let copied =
            std::io::copy(&mut entry.by_ref().take(MAX_TAR_ENTRY_BYTES + 1), &mut writer)
                .map_err(|e| e.to_string())?;
        if copied > MAX_TAR_ENTRY_BYTES || total + copied > MAX_TAR_TOTAL_BYTES {
            return Err(format!("依赖包条目过大：{raw}"));
        }
        total += copied;
    }
    Ok(())
}

// ===== 打包 =====

/// 定位打包器：随应用分发的资源（`bundle.resources` → 资源目录）优先，调试构建下回退到源码
/// 目录（`tauri dev` 未把资源放到二进制旁时仍可用）。
fn resolve_esbuild_binary(app: &AppHandle) -> Result<PathBuf, String> {
    let name = if cfg!(windows) { "esbuild.exe" } else { "esbuild" };
    let mut tried: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = app.path().resource_dir() {
        let candidate = dir.join(ESBUILD_RESOURCE_DIR).join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
        tried.push(candidate);
    }
    if cfg!(debug_assertions) {
        let candidate = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(ESBUILD_RESOURCE_DIR)
            .join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
        tried.push(candidate);
    }
    Err(format!(
        "打包器不可用（未随应用分发）：已查找 {}；请在项目根执行 pnpm install 后重新构建",
        tried
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(" / ")
    ))
}

/// 调打包器把入口打成自包含 ESM 产物。
///
/// 产物面向浏览器 realm（`--platform=browser`），因此依赖里对 Node 内置模块/原生扩展的引用
/// 会在这一步直接报错——错误信息本身指明文件与行列，正是我们要的「安装期失败并指名到包」。
async fn run_esbuild(app: &AppHandle, plugin_root: &Path, entry_path: &Path) -> Result<(), String> {
    let binary = resolve_esbuild_binary(app)?;
    run_esbuild_with(&binary, plugin_root, entry_path).await
}

/// 打包实现（打包器路径由调用方给出：生产走资源目录，测试用源码目录下同步好的那一份）。
async fn run_esbuild_with(
    binary: &Path,
    plugin_root: &Path,
    entry_path: &Path,
) -> Result<(), String> {
    let shim_root = plugin_root.join(SHIM_DIR);
    std::fs::create_dir_all(&shim_root).map_err(|e| format!("创建注入模块目录失败：{e}"))?;
    std::fs::write(shim_root.join("react.js"), REACT_SHIM)
        .map_err(|e| format!("写入注入模块失败：{e}"))?;
    std::fs::write(shim_root.join("jsx-runtime.js"), JSX_RUNTIME_SHIM)
        .map_err(|e| format!("写入注入模块失败：{e}"))?;
    // 打包器的工作目录 = 插件根，故别名与产物路径都用插件根下的相对形式。
    let shim_rel = SHIM_DIR.replace('\\', "/");

    let mut cmd = tokio::process::Command::new(binary);
    cmd.current_dir(plugin_root);
    cmd.arg("--bundle")
        .arg("--format=esm")
        .arg("--platform=browser")
        .arg("--target=es2020")
        .arg("--jsx=transform")
        .arg("--charset=utf8")
        .arg("--color=false")
        .arg("--log-level=warning")
        .arg("--log-limit=20")
        .arg(format!("--alias:react=./{shim_rel}/react.js"))
        .arg(format!("--alias:react/jsx-runtime=./{shim_rel}/jsx-runtime.js"))
        .arg(format!("--alias:react/jsx-dev-runtime=./{shim_rel}/jsx-runtime.js"))
        .arg(format!("--outfile={BUILD_ENTRY}"))
        .arg(entry_path);
    cmd.kill_on_drop(true);

    let output = match tokio::time::timeout(ESBUILD_HANG_GUARD, cmd.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            let _ = std::fs::remove_dir_all(&shim_root);
            return Err(format!("执行打包器失败：{e}"));
        }
        Err(_) => {
            let _ = std::fs::remove_dir_all(&shim_root);
            return Err("打包超时（依赖图过大或打包器卡住），已中止".into());
        }
    };
    let _ = std::fs::remove_dir_all(&shim_root);
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if message.is_empty() {
            "打包失败（打包器未给出原因）".to_string()
        } else {
            format!("打包失败：{message}")
        });
    }
    let warnings = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !warnings.is_empty() {
        // 告警不阻断安装（如体积提示），但要留痕供排查。
        eprintln!("[plugin] 打包告警：{warnings}");
    }
    let artifact_path = plugin_root.join(BUILD_ENTRY);
    let size = std::fs::metadata(&artifact_path)
        .map_err(|e| format!("读取打包产物失败：{e}"))?
        .len();
    if size > MAX_BUILT_ENTRY_BYTES {
        return Err(format!(
            "打包产物过大（{size} 字节，上限 {MAX_BUILT_ENTRY_BYTES} 字节）：请减少依赖或改用更轻的替代"
        ));
    }
    let artifact = std::fs::read(&artifact_path).map_err(|e| format!("读取打包产物失败：{e}"))?;
    if let Some(reason) = unresolved_dynamic_require(&artifact) {
        return Err(reason);
    }
    Ok(())
}

/// 产物里是否残留打包器无法静态解析的 `require`，并尽量回指到来源文件。
///
/// 为什么按产物文本判定：打包器对无法内联的 `require` 不报错也不告警，只写一段运行时兜底
/// （见 `DYNAMIC_REQUIRE_MARKER`）——在 WebView 里被调用到那行时必然抛错。兜底自带模块注释
/// （`// <路径>`），足以把这行回指到具体文件，正是「安装期失败并指名到包」需要的信息。
fn unresolved_dynamic_require(artifact: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(artifact);
    if !(text.contains(DYNAMIC_REQUIRE_MARKER) && text.contains(DYNAMIC_REQUIRE_SUFFIX)) {
        return None;
    }
    let origin = text
        .lines()
        .enumerate()
        .find(|(_, line)| is_dynamic_require_call(line))
        .and_then(|(index, _)| {
            let before: Vec<&str> = text.lines().take(index).collect();
            module_banner_before(&before)
        });
    let suffix = "请改用 import，或把该部分移出插件产物";
    Some(match origin {
        Some(origin) => {
            format!("入口或其依赖中存在无法静态解析的 require（打包器无法内联）：{origin}；{suffix}")
        }
        None => format!("入口或其依赖中存在无法静态解析的 require（打包器无法内联）：{suffix}"),
    })
}

/// 是否是兜底 `require` 的**调用**行：只排除它的两种定义形态（`var __require = …` 与
/// `function __require() {`），其余带调用的行都算命中。
fn is_dynamic_require_call(line: &str) -> bool {
    line.contains("__require")
        && line.contains('(')
        && !line.contains("var __require")
        && !line.contains("function __require")
}

/// 命中行之前最近的一条模块注释（打包器为每个内联模块写的 `// <路径>`），取不到返回 None。
fn module_banner_before(lines: &[&str]) -> Option<String> {
    for line in lines.iter().rev() {
        let Some(rest) = line.strip_prefix("// ") else {
            continue;
        };
        if looks_like_module_path(rest) {
            return Some(rest.to_string());
        }
    }
    None
}

/// 模块注释里的文本是否像一个内联模块的路径：不带空白（自然语言注释通常带空格，中文注释不带，
/// 故还要按扩展名收口），且以脚本类扩展名收尾。
fn looks_like_module_path(text: &str) -> bool {
    const EXTENSIONS: [&str; 9] = [".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs", ".mts", ".cts", ".json"];
    !text.is_empty()
        && !text.contains(char::is_whitespace)
        && EXTENSIONS.iter().any(|ext| text.ends_with(ext))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::test_support::TempDir;
    use serde_json::json;

    fn lock_with(packages: Value) -> Map<String, Value> {
        packages.as_object().cloned().unwrap()
    }

    fn deps_map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, range)| (name.to_string(), range.to_string()))
            .collect()
    }

    /// 生成一个最小 tar.gz（条目名为 `name`，内容为 `content`）。
    fn make_tgz(path: &Path, entries: &[(&str, &str)]) {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        let file = std::fs::File::create(path).unwrap();
        let encoder = GzEncoder::new(file, Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for (name, content) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, content.as_bytes()).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
    }

    /// 直接写 tar 头，用于构造 Builder 的路径校验会挡下的病态条目（穿越路径、链接）。
    fn make_raw_tgz(path: &Path, name: &[u8], content: &[u8], link: Option<&str>) {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        let file = std::fs::File::create(path).unwrap();
        let mut builder = tar::Builder::new(GzEncoder::new(file, Compression::default()));
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        if let Some(link) = link {
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            let bytes = link.as_bytes();
            header.as_gnu_mut().unwrap().linkname[..bytes.len()].copy_from_slice(bytes);
        }
        header.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name);
        header.set_cksum();
        builder.append(&header, content).unwrap();
        builder.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn runtime_dependencies_reads_only_dependencies() {
        let manifest = json!({
            "dependencies": { "nanoid": "^5.0.0" },
            "devDependencies": { "typescript": "^5.0.0" },
            "optionalDependencies": { "fsevents": "^2.0.0" },
            "peerDependencies": { "react": "^18.0.0" }
        });
        let deps = runtime_dependencies(&manifest);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps.get("nanoid").map(String::as_str), Some("^5.0.0"));
    }

    #[test]
    fn resolve_closure_walks_production_closure_only() {
        let packages = lock_with(json!({
            "": { "dependencies": { "a": "^1.0.0" } },
            "node_modules/a": { "version": "1.0.0", "dependencies": { "b": "^1.0.0" }, "resolved": "https://x/a.tgz", "integrity": "sha512-AA==" },
            "node_modules/b": { "version": "1.0.0", "resolved": "https://x/b.tgz", "integrity": "sha512-BB==" },
            "node_modules/dev-only": { "version": "1.0.0", "dev": true }
        }));
        let selected = resolve_closure(&packages, &deps_map(&[("a", "^1.0.0")])).unwrap();
        let keys: Vec<&str> = selected.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["node_modules/a", "node_modules/b"]);
    }

    #[test]
    fn resolve_closure_prefers_nearest_nested_copy() {
        let packages = lock_with(json!({
            "": { "dependencies": { "a": "^1.0.0", "c": "^1.0.0" } },
            "node_modules/a": { "version": "1.0.0", "dependencies": { "c": "^2.0.0" } },
            "node_modules/a/node_modules/c": { "version": "2.0.0" },
            "node_modules/c": { "version": "1.0.0" }
        }));
        let selected = resolve_closure(&packages, &deps_map(&[("a", "^1.0.0"), ("c", "^1.0.0")])).unwrap();
        let keys: Vec<&str> = selected.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec!["node_modules/a", "node_modules/c", "node_modules/a/node_modules/c"]
        );
    }

    #[test]
    fn resolve_closure_reports_missing_dependency() {
        let packages = lock_with(json!({ "" : { "dependencies": { "a": "^1.0.0" } } }));
        let err = resolve_closure(&packages, &deps_map(&[("a", "^1.0.0")])).unwrap_err();
        assert!(err.contains("package-lock.json 缺少依赖 a"), "{err}");
    }

    #[test]
    fn dependency_target_path_rejects_traversal() {
        assert!(dependency_target_path("node_modules/@scope/pkg").is_ok());
        assert!(dependency_target_path("node_modules/a/node_modules/b").is_ok());
        for bad in [
            "../evil",
            "node_modules/../../evil",
            "node_modules/a/../b",
            "/node_modules/a",
            "node_modules/",
            "src/index.js",
            "node_modules/a\\b",
        ] {
            assert!(dependency_target_path(bad).is_err(), "应拒绝：{bad}");
        }
    }

    #[test]
    fn integrity_digest_requires_sha512() {
        // "abc" 的 sha512
        let ok = integrity_digest("sha512-3a81oZNherrMQXNJriBBMRLm+k6JqX6iCp7u5ktV05ohkpkqJ0/BqDa6PCOj/uu9RU1EI2Q86A4qmslPpUyknw==");
        assert_eq!(ok.unwrap().len(), 64);
        // 多段时取 sha512 段
        assert!(integrity_digest("sha1-abc sha512-3a81oZNherrMQXNJriBBMRLm+k6JqX6iCp7u5ktV05ohkpkqJ0/BqDa6PCOj/uu9RU1EI2Q86A4qmslPpUyknw==").is_ok());
        assert!(integrity_digest("sha1-abc").unwrap_err().contains("sha512"));
        // 前缀对但摘要畸形（空/长度不对）同样拒绝，且原因是锁定信息而非「校验不符」
        assert!(integrity_digest("sha512-").unwrap_err().contains("长度异常"));
        assert!(integrity_digest("sha512-YWJj").unwrap_err().contains("长度异常"));
    }

    #[test]
    fn extract_tgz_safe_strips_package_prefix_and_writes_files() {
        let tmp = TempDir::new("plugin-build-extract");
        let tgz = tmp.join("pkg.tgz");
        make_tgz(&tgz, &[("package/dist/index.js", "export default 1;"), ("package/", "")]);
        extract_tgz_safe(&tgz, &tmp, "node_modules/pkg").unwrap();
        let written = std::fs::read_to_string(tmp.join("node_modules/pkg/dist/index.js")).unwrap();
        assert_eq!(written, "export default 1;");
    }

    #[test]
    fn extract_tgz_safe_rejects_traversal_and_links() {
        let tmp = TempDir::new("plugin-build-escape");
        let tgz = tmp.join("evil.tgz");
        make_raw_tgz(&tgz, b"package/../../evil.js", b"x", None);
        let err = extract_tgz_safe(&tgz, &tmp, "node_modules/evil").unwrap_err();
        assert!(err.contains(".."), "{err}");

        // 符号链接条目：指向依赖目录之外，必须拒绝
        let link_tgz = tmp.join("link.tgz");
        make_raw_tgz(&link_tgz, b"package/link", b"", Some("/etc/passwd"));
        let err = extract_tgz_safe(&link_tgz, &tmp, "node_modules/link").unwrap_err();
        assert!(err.contains("不支持的条目类型"), "{err}");
    }

    #[test]
    fn extract_tgz_safe_requires_package_prefix() {
        let tmp = TempDir::new("plugin-build-prefix");
        let tgz = tmp.join("flat.tgz");
        make_tgz(&tgz, &[("index.js", "x")]);
        let err = extract_tgz_safe(&tgz, &tmp, "node_modules/flat").unwrap_err();
        assert!(err.contains("package/"), "{err}");
    }

    /// 建一个指向 `target` 的目录链接：Windows 用 junction（无需管理员权限），其余平台用符号链接。
    /// 返回是否成功——环境不支持时用例跳过（那是环境能力，不是被测行为）。
    #[cfg(windows)]
    fn link_dir(target: &Path, link: &Path) -> bool {
        junction::create(target, link).is_ok()
    }

    #[cfg(not(windows))]
    fn link_dir(target: &Path, link: &Path) -> bool {
        std::os::unix::fs::symlink(target, link).is_ok()
    }

    /// 插件把 `node_modules` 提交成指向插件根之外的链接时，解压不得跟随它落盘。
    #[test]
    fn extract_tgz_safe_refuses_to_follow_directory_link() {
        let tmp = TempDir::new("plugin-build-symlink");
        let outside = TempDir::new("plugin-build-symlink-outside");
        let root = tmp.join("plugin");
        std::fs::create_dir_all(&root).unwrap();
        if !link_dir(&outside, &root.join("node_modules")) {
            eprintln!("跳过：本机不能创建目录链接");
            return;
        }
        let tgz = tmp.join("pkg.tgz");
        make_tgz(&tgz, &[("package/index.js", "x")]);
        let err = extract_tgz_safe(&tgz, &root, "node_modules/pkg").unwrap_err();
        assert!(err.contains("符号链接"), "{err}");
        assert!(!outside.join("pkg").exists(), "不得写到链接目标目录");
    }

    #[test]
    fn unresolved_dynamic_require_detects_and_attributes_fallback() {
        let clean = b"var x = 1; export { x };";
        assert!(unresolved_dynamic_require(clean).is_none());

        // 打包器兜底 + 模块注释：应回指到出问题的那个文件
        let dirty = concat!(
            "var __require = (x) => { throw Error('Dynamic require of \"' + x + '\" is not supported'); };\n",
            "\n",
            "// node_modules/dyn-dep/lib/index.js\n",
            "var y = __require(n);\n"
        );
        let message = unresolved_dynamic_require(dirty.as_bytes()).unwrap();
        assert!(message.contains("node_modules/dyn-dep/lib/index.js"), "{message}");

        // 找不到模块注释时退回通用文案，不把源码里的自然语言注释当路径
        let anonymous = concat!(
            "var __require = (x) => { throw Error('Dynamic require of \"' + x + '\" is not supported'); };\n",
            "// 这里是插件的说明注释\n",
            "var y = __require(n);\n"
        );
        let message = unresolved_dynamic_require(anonymous.as_bytes()).unwrap();
        assert!(!message.contains("说明注释"), "{message}");

        // 只有半句（插件源码/依赖里的同名词）不算：误判会直接拒掉能正常安装的插件
        let lookalike = br#"var a = "Dynamic require of \"x\""; export { a };"#;
        assert!(unresolved_dynamic_require(lookalike).is_none());
    }

    #[test]
    fn built_entry_requires_existing_artifact() {
        let tmp = TempDir::new("plugin-build-entry");
        assert!(built_entry(&tmp).is_none());
        std::fs::create_dir_all(tmp.join(BUILD_DIR)).unwrap();
        assert!(built_entry(&tmp).is_none());
        std::fs::write(tmp.join(BUILD_ENTRY), "export default {};").unwrap();
        assert_eq!(built_entry(&tmp), Some(BUILD_ENTRY));
    }

    #[test]
    fn read_lock_packages_requires_packages_map() {
        let tmp = TempDir::new("plugin-build-lock");
        std::fs::write(tmp.join("package-lock.json"), r#"{ "lockfileVersion": 1 }"#).unwrap();
        let err = read_lock_packages(&tmp).unwrap_err();
        assert!(err.contains("packages"), "{err}");

        std::fs::write(tmp.join("package-lock.json"), "{ not json").unwrap();
        assert!(read_lock_packages(&tmp).unwrap_err().contains("合法 JSON"));

        std::fs::remove_file(tmp.join("package-lock.json")).unwrap();
        let err = read_lock_packages(&tmp).unwrap_err();
        assert!(err.contains("package-lock.json"), "{err}");
    }

    /// 缓存命中路径：磁盘内容与锁定摘要一致即直接复用（不发请求），不一致即丢弃重取。
    #[test]
    fn fetch_to_cache_reuses_verified_entry_and_discards_mismatch() {
        let tmp = TempDir::new("plugin-build-cache");
        let cache = tmp.join("pkg.tgz");
        std::fs::write(&cache, b"payload").unwrap();
        let digest = sha512_file(&cache).unwrap();
        let client = http_client().unwrap();
        let unreachable = "http://127.0.0.1:1/unreachable.tgz";
        // 命中且校验通过：URL 不可达也成功 = 没有发起请求
        tauri::async_runtime::block_on(fetch_to_cache(&client, unreachable, &cache, &digest)).unwrap();
        assert!(cache.is_file(), "命中时缓存文件应保持");

        // 摘要不符：丢弃缓存并尝试重新下载（此处必然失败，但缓存必须已被清掉）
        let other = TempDir::new("plugin-build-cache-other");
        std::fs::write(other.join("p.tgz"), b"other").unwrap();
        let wrong = sha512_file(&other.join("p.tgz")).unwrap();
        let result = tauri::async_runtime::block_on(fetch_to_cache(&client, unreachable, &cache, &wrong));
        assert!(result.is_err());
        assert!(!cache.exists(), "摘要不符的缓存必须被删除");
    }

    /// 本机同步好的打包器（`pnpm tauri:build` 前会放进源码目录）；缺失则跳过用例。
    fn local_esbuild_binary() -> Option<PathBuf> {
        let name = if cfg!(windows) { "esbuild.exe" } else { "esbuild" };
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(ESBUILD_RESOURCE_DIR)
            .join(name);
        path.is_file().then_some(path)
    }

    fn write_fixture(root: &Path, files: &[(&str, &str)]) {
        for (rel, content) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
    }

    fn bundle(root: &Path, entry: &str) -> Result<String, String> {
        let binary = local_esbuild_binary().ok_or("未找到打包器")?;
        let entry_path = crate::commands::plugin::safe_plugin_path(root, entry).unwrap();
        tauri::async_runtime::block_on(run_esbuild_with(&binary, root, &entry_path))?;
        Ok(std::fs::read_to_string(root.join(BUILD_ENTRY)).unwrap())
    }

    #[test]
    fn run_esbuild_inlines_relative_imports_and_local_dependency() {
        if local_esbuild_binary().is_none() {
            eprintln!("跳过：未找到打包器（先执行 node scripts/sync-esbuild.mjs）");
            return;
        }
        let tmp = TempDir::new("plugin-build-bundle");
        write_fixture(
            &tmp,
            &[
                (
                    "src/index.ts",
                    "import { greet } from \"./helper\";\nimport { tag } from \"tiny-dep\";\nexport default { apply: () => greet(tag) };\n",
                ),
                ("src/helper.ts", "export const greet = (s: string) => `hi ${s}`;\n"),
                (
                    "node_modules/tiny-dep/package.json",
                    r#"{ "name": "tiny-dep", "version": "1.0.0", "main": "index.js" }"#,
                ),
                (
                    "node_modules/tiny-dep/index.js",
                    "module.exports = { tag: \"from-tiny-dep\" };\n",
                ),
            ],
        );
        let artifact = bundle(&tmp, "src/index.ts").unwrap();
        assert!(artifact.contains("from-tiny-dep"), "依赖内容应内联进产物");
        assert!(artifact.contains("hi "), "相对导入应内联进产物");
        assert!(!artifact.contains("from \"tiny-dep\""), "不应残留裸包引用");
        assert!(!tmp.join(SHIM_DIR).exists(), "注入模块目录应在打包后清理");
    }

    #[test]
    fn run_esbuild_aliases_react_to_host_global() {
        if local_esbuild_binary().is_none() {
            eprintln!("跳过：未找到打包器（先执行 node scripts/sync-esbuild.mjs）");
            return;
        }
        let tmp = TempDir::new("plugin-build-react");
        write_fixture(
            &tmp,
            &[
                (
                    "src/index.tsx",
                    "import React, { useState } from \"react\";\nexport default { apply: () => React.createElement(\"div\", null, useState) };\n",
                ),
                ("src/auto.tsx", "export default { apply: () => <span /> };\n"),
            ],
        );
        let artifact = bundle(&tmp, "src/index.tsx").unwrap();
        assert!(artifact.contains("globalThis.React"), "react 应落到宿主全局");
        assert!(!artifact.contains("from \"react\""), "不应残留 react 裸包引用");
        // JSX 走经典转译，引用的就是宿主提供的 React 全局（与未打包入口同语义）。
        let jsx_artifact = bundle(&tmp, "src/auto.tsx").unwrap();
        assert!(jsx_artifact.contains("React.createElement"), "JSX 应转译为 createElement");
    }

    #[test]
    fn run_esbuild_fails_on_node_builtin_with_named_specifier() {
        if local_esbuild_binary().is_none() {
            eprintln!("跳过：未找到打包器（先执行 node scripts/sync-esbuild.mjs）");
            return;
        }
        let tmp = TempDir::new("plugin-build-nodebuiltin");
        write_fixture(
            &tmp,
            &[("src/index.ts", "import { readFileSync } from \"node:fs\";\nexport default { apply: () => readFileSync };\n")],
        );
        let err = bundle(&tmp, "src/index.ts").unwrap_err();
        assert!(err.contains("node:fs"), "错误应指名到具体模块：{err}");
        assert!(err.contains("src/index.ts"), "错误应指明文件：{err}");
    }

    /// nanoid@5.1.5 的锁定信息（registry 上的已发布版本，字节固定）。
    const NANOID_LOCK: &str = r#"{
  "name": "com.example.dep-demo",
  "version": "1.0.0",
  "lockfileVersion": 3,
  "requires": true,
  "packages": {
    "": {
      "name": "com.example.dep-demo",
      "version": "1.0.0",
      "dependencies": { "nanoid": "^5.1.5" }
    },
    "node_modules/nanoid": {
      "version": "5.1.5",
      "resolved": "https://registry.npmjs.org/nanoid/-/nanoid-5.1.5.tgz",
      "integrity": "sha512-Ir/+ZpE9fDsNH0hQ3C68uyThDXzYcim2EqcZ8zn8Chtt1iylPT9xXJB0kPCnqzgcEGikO9RxSrh63MsmVCU7Fw=="
    }
  }
}"#;

    /// 端到端（默认不跑，手动执行看真实链路）：
    /// `cargo test --manifest-path src-tauri/Cargo.toml -- --ignored`
    #[test]
    #[ignore = "需要网络：真实拉取 registry 依赖并打包"]
    fn end_to_end_fetches_verifies_extracts_and_bundles_registry_dependency() {
        if local_esbuild_binary().is_none() {
            eprintln!("跳过：未找到打包器（先执行 node scripts/sync-esbuild.mjs）");
            return;
        }
        let tmp = TempDir::new("plugin-build-e2e");
        write_fixture(
            &tmp,
            &[
                (
                    "package.json",
                    r#"{ "name": "com.example.dep-demo", "version": "1.0.0", "main": "src/index.ts", "dependencies": { "nanoid": "^5.1.5" }, "atelyx": { "type": "tool" } }"#,
                ),
                ("package-lock.json", NANOID_LOCK),
                (
                    "src/index.ts",
                    "import { nanoid } from \"nanoid\";\nexport default { apply: () => nanoid() };\n",
                ),
            ],
        );
        let manifest: Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.join("package.json")).unwrap(),
        )
        .unwrap();
        let cache = tmp.join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let client = http_client().unwrap();
        tauri::async_runtime::block_on(materialize_dependencies(
            &cache,
            &client,
            &tmp,
            &runtime_dependencies(&manifest),
        ))
        .unwrap();
        assert!(tmp.join("node_modules/nanoid/package.json").is_file(), "依赖应解压到锁定路径");

        // 第二次取件走缓存命中（不发请求也应成功）。
        tauri::async_runtime::block_on(materialize_dependencies(
            &cache,
            &client,
            &tmp,
            &runtime_dependencies(&manifest),
        ))
        .unwrap();

        let artifact = bundle(&tmp, "src/index.ts").unwrap();
        assert!(!artifact.contains("from \"nanoid\""), "不应残留裸包引用");
        assert!(artifact.contains("export"), "产物应是 ESM");
    }
}
