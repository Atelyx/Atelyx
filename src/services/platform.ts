/**
 * 内核能力层：按运行平台声明内核级能力有无，调用点按能力分支（禁止散落平台字符串判断）。
 *
 * 能力收口标准：能不能靠「组合层不挂某个插件」实现取舍？能的走组合层（默认组合按平台裁剪），
 * 不能的（不属于任何插件：多窗口、窗口控制、目录选择器、凭据存储、自动更新、进程执行）才进本层。
 * 缺失能力必须显式可见（禁用态或提示），调用点不得静默失效。
 *
 * 平台判定经 WebView UA（安卓 WebView 的 UA 固含 Android 标识；桌面两端均无），
 * 同步可得，供启动路径在首次渲染前分支。能力表在应用运行期内恒定。
 */

/** 内核级能力表（布尔 = 该端是否提供）。 */
export interface PlatformCapabilities {
  /** 多窗口与撕裂窗口（桌面专有叠加能力）。 */
  multiWindow: boolean;
  /** 窗口控制（最小化/最大化/自绘标题栏/全屏/关闭守卫）。 */
  windowControls: boolean;
  /** 目录选择器（桌面 = 系统原生文件夹选择器；移动端无系统选择器，改用自研目录浏览 UI）。 */
  directoryPicker: boolean;
  /** 凭据存储（桌面 = OS keychain；移动端 = 系统级安全存储，接口一致）。 */
  credentialStorage: boolean;
  /** 应用内自动更新（移动端无 updater，改为提示下载）。 */
  autoUpdate: boolean;
  /** 进程执行与插件依赖打包（移动端不存在，缺失显式可见）。 */
  processExecution: boolean;
  /** 插件外部目录授权（清单声明仓库外目录并逐目录批准；移动端不提供）。 */
  pluginExternalDirs: boolean;
  /** 磁盘文件监听（两端均无：外部改动在打开/重读时感知）。 */
  fileWatching: boolean;
}

/** 当前是否运行在安卓 WebView。 */
export function isAndroidPlatform(): boolean {
  return typeof navigator !== "undefined" && /android/i.test(navigator.userAgent);
}

/** 当前平台的能力表。 */
export function platformCapabilities(): PlatformCapabilities {
  if (isAndroidPlatform()) {
    return {
      multiWindow: false,
      windowControls: false,
      directoryPicker: false,
      credentialStorage: true,
      autoUpdate: false,
      processExecution: false,
      pluginExternalDirs: false,
      fileWatching: false,
    };
  }
  return {
    multiWindow: true,
    windowControls: true,
    directoryPicker: true,
    credentialStorage: true,
    autoUpdate: true,
    processExecution: true,
    pluginExternalDirs: true,
    fileWatching: false,
  };
}
