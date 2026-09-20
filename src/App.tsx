import { lazy, Suspense, useEffect, useRef, useState } from "react";
import { ReactFlowProvider } from "@xyflow/react";
import { useAppStore } from "@/stores/appStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { useVaultStore } from "@/stores/vaultStore";
import { usePluginStore } from "@/stores/pluginStore";
import { PanelWindowRoot } from "@/components/layout/PanelWindowRoot";
import { ErrorBoundary } from "@/components/common/ErrorBoundary";
import { LoadingScreen } from "@/components/common/LoadingScreen";
import { NotificationHost } from "@/components/common/NotificationHost";
import { useAppearance } from "@/hooks/useAppearance";
import { getCurrentWindowLabel } from "@/services/window";
import { layoutReconcile } from "@/services/layout";
import { PANEL_LABEL_PREFIX, usePanelStore } from "@/stores/panelStore";

// 页面 lazy 分割：主包不含 CodeMirror/KaTeX/高亮语言包等重库，LoadingScreen 更快出现。
// ReactFlowProvider 留在 App 层（页面组件自身的 useReactFlow hooks 需要它在组件外；
// React Flow 因 canvasStore 依赖本就在主包，不额外增加首屏体积）。
const ProjectWorkspacePage = lazy(async () => {
  const mod = await import("@/pages/ProjectWorkspacePage");
  return { default: mod.ProjectWorkspacePage };
});

/** 窗口形态应用串行队列：多次触发（含 boot 末尾）时按序执行，队列 promise 因内层 catch 永不 reject。 */
let windowShapeQueue: Promise<void> = Promise.resolve();
function applyWindowShape(): Promise<void> {
  const apply = useAppStore.getState().applyWorkspaceWindow;
  windowShapeQueue = windowShapeQueue.then(() => apply().catch(() => {}));
  return windowShapeQueue;
}

/** 插件应用页面承载（全页接管；缺注册 = 占位 + 返回按钮；插件崩溃不影响 App）。 */
function PluginPageMount({ pageId }: { pageId: string }) {
  const close = useAppStore((s) => s.closePluginPage);
  usePluginStore((s) => s.uiRevision);
  const reg = usePluginStore.getState().pluginAppPage(pageId);
  if (!reg) {
    return (
      <div
        className="h-full w-full flex flex-col items-center justify-center gap-3"
        style={{ background: "var(--bg-primary)", color: "var(--text-secondary)" }}
      >
        <span className="text-sm">插件页面「{pageId}」已停用或卸载</span>
        <button
          className="px-3 py-1.5 rounded text-xs"
          style={{ background: "var(--accent)", color: "var(--accent-fg)" }}
          onClick={close}
        >
          返回工作区
        </button>
      </div>
    );
  }
  const Comp = reg.component;
  return (
    <ErrorBoundary>
      <div className="relative h-full w-full">
        <button
          className="absolute top-3 right-3 z-50 px-3 py-1.5 rounded text-xs border"
          style={{ background: "var(--bg-secondary)", borderColor: "var(--border)", color: "var(--text-secondary)" }}
          onClick={close}
        >
          退出插件页面
        </button>
        <Comp />
      </div>
    </ErrorBoundary>
  );
}

/** 主窗口应用主体（工作区 + booting 流程）。 */
function MainWorkspaceApp() {
  const pluginPage = useAppStore((s) => s.pluginPage);
  const vaultRoot = useAppStore((s) => s.vaultRoot);
  const init = useAppStore((s) => s.init);
  const selectSpace = useAppStore((s) => s.selectSpace);
  const loadSettings = useSettingsStore((s) => s.load);
  useAppearance();

  /** 初始化未完成前渲染加载屏（Logo + 扫光条 + 当前加载项步骤清单），完成后渲染工作区。
   *  全屏加载仅 boot；面板内进仓/切仓库不整屏替换——仓库树在目标行显示加载动画，
   *  切换全程工作区保持可见（「全部加载完再进入」门控仍由 selectVault 内部顺序保证）。 */
  const [booting, setBooting] = useState(true);

  // boot 是单实例一次性初始化：React StrictMode（dev）会把 effect 双跑（setup→cleanup→setup），
  // 不守卫会重复执行 init/selectVault/插件加载，加载步骤清单随之逐条重复上报。
  // ref 在 StrictMode 的模拟卸载/重挂间保持同一实例（仅真卸载重挂/HMR 换组件才复位），守卫安全。
  const bootedRef = useRef(false);

  // 窗口形态：boot 末尾统一应用一次（可调整 + 最小尺寸 = 默认；静默降级，串行队列）。
  useEffect(() => {
    applyWindowShape();
  }, [booting]);

  // 窗口关闭守卫：先 flush 全部 pending 改动再真正关窗，防 debounce 窗口内丢数据（幂等注册）
  useEffect(() => {
    useAppStore.getState().installCloseGuard();
  }, []);

  // 仓库文件监听：激活仓库期间订阅；无激活仓库（树区空态）时无 root 可监听，保持停止。
  // 订阅副作用归 vaultStore（分层：组件不直连 service），store 内幂等
  useEffect(() => {
    useVaultStore.getState().startFileWatcher(vaultRoot !== null);
  }, [vaultRoot]);

  // 应用挂载：init 读取最近仓库，loadSettings 加载应用级外观配置，
  // selectVault 进入仓库后由 loadVaultConfig 填充仓库级配置（AI 供应商/搜索源 + keychain key）。
  // 记住上次所在仓库：init 返回非 null 时直接进入；无仓库时进工作区空态（树区引导）。
  // 加载会话：beginLoad 开启全屏加载屏 + 步骤清单，init/selectVault 内部逐步上报，finally 收尾。
  useEffect(() => {
    if (bootedRef.current) return;
    bootedRef.current = true;
    let settled = false;
    const app = useAppStore.getState();
    app.beginLoad();
    // 兜底：初始化 IPC 异常挂起（如读取配置卡死）时强制结束加载屏落到工作区空态，防永久卡加载屏
    const fallback = setTimeout(() => {
      if (settled) return;
      app.endLoad();
      void applyWindowShape().then(() => setBooting(false));
    }, 5000);
    void (async () => {
      const autoEnter = await init();
      app.reportLoad("加载应用设置");
      await loadSettings();
      // 面板运行时初始化（协作连接改由 panelStore.syncCollabHost 按视图归属驱动）
      app.reportLoad("初始化窗口与面板");
      await usePanelStore.getState().initMain();
      if (autoEnter?.kind === "local") {
        // 进仓门控在 selectVault 内：全部加载完成（含插件）后才进入仓库
        await useAppStore.getState().selectVault(autoEnter.root);
      } else if (autoEnter?.kind === "space") {
        // 最近条目是协作空间：会话无效时 selectSpace 返回 need-login，静默停留未激活态
        await selectSpace(autoEnter.entry);
      } else {
        // 无最近仓库（进工作区空态）：selectVault 不会执行，这里补一次插件加载——
        // 否则 app 级插件（如主题）不生效。
        await usePluginStore.getState().load().catch(() => {});
      }
      // 撕裂窗口恢复：进仓库后由 Rust 调和补建持久化撕裂窗口的 OS 窗口；撕裂窗口自行
      // bootstrap 拉布局快照 + 订阅 layout-broadcast 广播渲染
      app.reportLoad("还原布局窗口");
      await layoutReconcile();
      // 自动更新（应用级，global.json）：开启时启动静默检查一次，失败静默跳过。
      // 走 store 包装（runAutoUpdate 内部先 flush 全部 pending 改动再检查安装，重启不丢数据；
      // 协作连接收尾不随 flush 执行，见 appStore.flushAllPending 注释）
      if (useAppStore.getState().autoUpdate) {
        void useAppStore.getState().runAutoUpdate();
      }
    })().finally(async () => {
      settled = true;
      clearTimeout(fallback);
      // 等窗口形态应用完成再结束加载屏：工作区渲染时窗口已是最终大小，防跳变
      await applyWindowShape();
      app.endLoad();
      setBooting(false);
    });
  }, [init, selectSpace, loadSettings]);

  return (
    <Suspense fallback={<LoadingScreen />}>
      {booting ? (
        <LoadingScreen />
      ) : pluginPage ? (
        <PluginPageMount pageId={pluginPage} />
      ) : (
        <ProjectWorkspacePage />
      )}
    </Suspense>
  );
}

/** 应用入口：按窗口 label 分流——主窗口走完整启动流程；撕裂窗口只渲染单面板。
 * 撕裂窗口不执行 init/selectVault/自动更新等主窗口专属逻辑（面板角色由 panelStore 管理）。
 * label 读取失败（IPC/init 脚本异常）时降级为主窗口角色并打日志，绝不白屏。 */
export default function App() {
  const [isPanel] = useState(() => {
    try {
      return getCurrentWindowLabel().startsWith(PANEL_LABEL_PREFIX);
    } catch (e) {
      console.error("读取窗口 label 失败", e);
      return false;
    }
  });

  // 全局屏蔽浏览器默认右键菜单（两窗口角色都需要）
  useEffect(() => {
    const suppress = (e: MouseEvent) => e.preventDefault();
    document.addEventListener("contextmenu", suppress);
    return () => document.removeEventListener("contextmenu", suppress);
  }, []);

  return (
    <ReactFlowProvider>
      {/* 错误边界：渲染崩溃显示错误面板（可读可关窗），不白屏；通知宿主也在边界内（其渲染异常不越界白屏） */}
      <ErrorBoundary>
        {isPanel ? <PanelWindowRoot /> : <MainWorkspaceApp />}
        {/* 应用内通知宿主（每个窗口各挂一个；插件经 ctx.notification 触达） */}
        <NotificationHost />
      </ErrorBoundary>
    </ReactFlowProvider>
  );
}
