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
const VaultSelectPage = lazy(async () => {
  const mod = await import("@/pages/VaultSelectPage");
  return { default: mod.VaultSelectPage };
});
const ProjectWorkspacePage = lazy(async () => {
  const mod = await import("@/pages/ProjectWorkspacePage");
  return { default: mod.ProjectWorkspacePage };
});

/** 窗口形态应用串行队列：view 切换（含 booting 结束的首应用）多次触发时按序执行，
 * 最终形态 = 最后一次调用的视图（防并发乱序把工作区窗口锁成启动页形态）。
 * 队列 promise 因内层 catch 永不 reject。 */
let windowShapeQueue: Promise<void> = Promise.resolve();
function applyWindowShape(): Promise<void> {
  const s = useAppStore.getState();
  const apply = s.view === "vaultSelect" ? s.applyStartupWindow : s.applyWorkspaceWindow;
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

/** 主窗口应用主体（启动页 + 工作区 + booting 流程）。 */
function MainWorkspaceApp() {
  const view = useAppStore((s) => s.view);
  const pluginPage = useAppStore((s) => s.pluginPage);
  const entryLoading = useAppStore((s) => s.entryLoading);
  const init = useAppStore((s) => s.init);
  const loadSettings = useSettingsStore((s) => s.load);
  useAppearance();

  /** 初始化/进仓未完成前渲染加载屏（Logo + 扫光条 + 当前加载项步骤清单），
   *  完成后按 view 渲染启动页/工作区。booting 覆盖首帧与 boot 全程，entryLoading
   *  覆盖 selectVault（启动页点击进仓/工作区内切仓库）全程——门控「全部加载完再进入仓库」。 */
  const [booting, setBooting] = useState(true);

  // boot 是单实例一次性初始化：React StrictMode（dev）会把 effect 双跑（setup→cleanup→setup），
  // 不守卫会重复执行 init/selectVault/插件加载，加载步骤清单随之逐条重复上报。
  // ref 在 StrictMode 的模拟卸载/重挂间保持同一实例（仅真卸载重挂/HMR 换组件才复位），守卫安全。
  const bootedRef = useRef(false);

  // 窗口形态随视图切换：启动页固定 960×640 不可调整，工作区恢复可调整（静默降级，串行队列）。
  // 窗口恒以启动页尺寸创建（tauri.conf.json），加载屏期间即小窗；booting 期间 view 变化触发的
  // applyWorkspaceWindow 幂等（窗口小于默认时才放大），booting 结束再由 applyWindowShape
  // 统一按最终视图应用一次，进入工作区全程只有一次放大。
  useEffect(() => {
    applyWindowShape();
  }, [view]);

  // 窗口关闭守卫：先 flush 全部 pending 改动再真正关窗，防 debounce 窗口内丢数据（幂等注册）
  useEffect(() => {
    useAppStore.getState().installCloseGuard();
  }, []);

  // 仓库文件监听：进仓库后全程订阅（工作区），vaultSelect 页未开仓库 watcher 未启动，订阅无意义。
  // 订阅副作用归 vaultStore（分层：组件不直连 service），view 切换时启停（store 内幂等）
  useEffect(() => {
    useVaultStore.getState().startFileWatcher(view !== "vaultSelect");
  }, [view]);

  // 应用挂载：init 登记最近仓库（首启建默认仓库），loadSettings 加载应用级外观配置，
  // selectVault 进入仓库后由 loadVaultConfig 填充仓库级配置（AI 供应商/搜索源 + keychain key）。
  // 记住上次所在仓库：init 返回非 null 时跳过启动页直接进入。
  // 加载会话：beginLoad 开启全屏加载屏 + 步骤清单，init/selectVault 内部逐步上报，finally 收尾。
  useEffect(() => {
    if (bootedRef.current) return;
    bootedRef.current = true;
    // 预载工作区页面 chunk（lazy 在首次渲染才触发加载，booting 期间先编译/加载，切换无感）
    void import("@/pages/ProjectWorkspacePage").catch(() => {});
    let settled = false;
    const app = useAppStore.getState();
    app.beginLoad();
    // 兜底：初始化 IPC 异常挂起（如读取配置卡死）时强制结束加载屏落到启动页，防永久卡加载屏
    const fallback = setTimeout(() => {
      if (settled) return;
      app.endLoad();
      void applyWindowShape().then(() => setBooting(false));
    }, 5000);
    void (async () => {
      const autoEnterRoot = await init();
      app.reportLoad("加载应用设置");
      await loadSettings();
      // 面板运行时初始化（协作连接改由 panelStore.syncCollabHost 按视图归属驱动）
      app.reportLoad("初始化窗口与面板");
      await usePanelStore.getState().initMain();
      if (autoEnterRoot) {
        // 进仓门控在 selectVault 内：全部加载完成（含插件）后才切工作区视图
        await useAppStore.getState().selectVault(autoEnterRoot);
      } else {
        // 首启/无最近仓库（停在启动页）：selectVault 不会执行，这里补一次插件加载——
        // 否则 app 级插件（如主题）在启动页不生效，且与 backToVaultSelect 路径行为不一致。
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
  }, [init, loadSettings]);

  return (
    <Suspense fallback={<LoadingScreen />}>
      {booting || entryLoading ? (
        <LoadingScreen />
      ) : view === "workspace" ? (
        pluginPage ? (
          <PluginPageMount pageId={pluginPage} />
        ) : (
          <ProjectWorkspacePage />
        )
      ) : (
        <VaultSelectPage />
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
