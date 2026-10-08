/**
 * 仓库切换门（两阶段切换协议）：主窗口切换仓库前，协调全部撕裂窗口落盘在途写入并整窗禁写，
 * 防撕裂窗口的 debounce 写盘打进已切换 root 的新仓库（跨仓库污染）。
 * 协议流程与时序契约归协议帧、runVaultSwitchGate 与 handlePrepare 的归属注释。
 */
import { emit, emitTo, listen } from "@tauri-apps/api/event";
import { getAllWebviews } from "@tauri-apps/api/webview";
import { getCurrentWindowLabel } from "@/services/window";

/** 主窗口 label（撕裂窗口 ack 经 emitTo 定向上行）。 */
const HOST_LABEL = "main";
/** 撕裂窗口 label 前缀（与 panelStore/Rust `PANEL_LABEL_PREFIX` 对齐；服务层不反向 import store）。 */
const PANEL_LABEL_PREFIX = "panel-";
const PREPARE_EVENT = "vault-switch-prepare";
const ACK_EVENT = "vault-switch-ack";
const ABORT_EVENT = "vault-switch-abort";
/** 等待期间的存活名单复查间隔（ms）：窗口关闭 → Rust 条目移除 → 快照不再含它 → 停止等待。 */
const GATE_POLL_MS = 200;

// ---------- 协议帧 ----------

/** 主窗口 → 全部窗口：请求进入切换准备（落盘 + 禁写）。 */
export interface VaultSwitchPrepare {
  kind: "prepare";
  switchId: string;
}

/** 撕裂窗口 → 主窗口：本窗口 flush 结果。 */
export interface VaultSwitchAck {
  kind: "ack";
  switchId: string;
  /** 上报窗口 id（不含 label 前缀）。 */
  from: string;
  ok: boolean;
  /** ok = false 时的失败原因（ack 必达：失败也要上行，主窗口据此中止切换）。 */
  error?: string;
}

/** 主窗口 → 全部窗口：撤销本次切换门（撕裂窗口清遮罩；已消费该 switchId 的窗口忽略）。 */
export interface VaultSwitchAbort {
  kind: "abort";
  switchId: string;
}

// ---------- 撕裂窗口面 ----------

/** 切换门状态（撕裂窗口）：preparing = 已 flush 待切换；switching = 新上下文加载中。
 *  非空时面板遮罩禁写（flush 后到新上下文应用前，任何写入都会打进已切换的新仓库）。 */
export interface VaultSwitchGateState {
  phase: "preparing" | "switching";
  switchId: string;
}

/** 撕裂窗口面依赖（panelStore 注入；模块不反向 import store）。 */
export interface VaultSwitchGatePanelDeps {
  /** 本窗口全部领域挂起写入落盘（失败抛错 = ack 携带失败，主窗口据此中止切换）。 */
  flush: () => Promise<void>;
  /** 门状态变化（遮罩渲染驱动）。 */
  onGateState: (state: VaultSwitchGateState | null) => void;
}

let panelDeps: VaultSwitchGatePanelDeps | null = null;
let panelGate: VaultSwitchGateState | null = null;
/** 本窗口 label（安装时捕获；窗口 label 终生不变，事件回调不读动态值）。 */
let panelSelfLabel = HOST_LABEL;
/** 在途 flush 的 switchId（重发帧幂等守卫：在途不叠加，其完成时自带 ack）。 */
let flushInFlightFor: string | null = null;
let panelInstalled = false;

function setPanelGate(state: VaultSwitchGateState | null): void {
  panelGate = state;
  panelDeps?.onGateState(state);
}

/**
 * 安装撕裂窗口面（幂等；依赖每次更新）。仅撕裂窗口调用（主窗口是 prepare 发送方，
 * 即便误装也按 label 忽略 prepare）。注册完成后 prepare 才可达——initPanel 接线先于
 * bootstrap，窗口存在期内不会被漏过。
 */
export async function installVaultSwitchGatePanel(deps: VaultSwitchGatePanelDeps): Promise<void> {
  panelDeps = deps;
  panelSelfLabel = currentLabel();
  if (panelInstalled) return;
  panelInstalled = true;
  await listen<VaultSwitchPrepare>(PREPARE_EVENT, (e) => {
    void handlePrepare(e.payload);
  });
  await listen<VaultSwitchAbort>(ABORT_EVENT, (e) => {
    handleAbort(e.payload);
  });
}

function currentLabel(): string {
  try {
    return getCurrentWindowLabel();
  } catch {
    return HOST_LABEL;
  }
}

async function handlePrepare(frame: VaultSwitchPrepare): Promise<void> {
  if (!panelDeps) return;
  // 主窗口不消费自己发出的 prepare
  if (panelSelfLabel === HOST_LABEL) return;
  // 同一门的重发帧幂等：在途 flush 不叠加（其完成时自带 ack）；已完成的重跑走脏门控
  // （无挂起即空转）并补 ack（防上行丢失），失败的借此重试（磁盘可能已恢复）
  if (flushInFlightFor === frame.switchId) return;
  // 新 switchId = 新一轮切换：覆盖旧门（主窗口切换串行，正常不重叠；防御错位帧）。
  // 覆盖前不撤旧遮罩——旧门 flush 已完成，遮罩连续保持到新门消费完毕。
  setPanelGate({ phase: "preparing", switchId: frame.switchId });
  flushInFlightFor = frame.switchId;
  try {
    await panelDeps.flush();
  } catch (e) {
    flushInFlightFor = null;
    const error = e instanceof Error ? e.message : String(e);
    console.error("切换门 flush 失败，ack 失败待主窗口处置", e);
    // 保持遮罩：flush 失败时写状态不明，禁写最安全；主窗口 abort（或下一轮 prepare）才撤销
    await sendAck({ kind: "ack", switchId: frame.switchId, from: panelWindowId(), ok: false, error });
    return;
  }
  flushInFlightFor = null;
  await sendAck({ kind: "ack", switchId: frame.switchId, from: panelWindowId(), ok: true });
}

/** ack 上行（事件线异常只记日志：ack 丢失会让主窗口无限等待，但本地 IPC 之外无重试面）。 */
async function sendAck(ack: VaultSwitchAck): Promise<void> {
  try {
    await emitTo(HOST_LABEL, ACK_EVENT, ack);
  } catch (e) {
    console.error("上行切换门 ack 失败", e);
  }
}

function handleAbort(frame: VaultSwitchAbort): void {
  // 只清 preparing：switching = 新上下文已应用、加载链在途，链 finally 必然收尾撤销。
  // 若 abort 清了 switching，会出现「遮罩已撤、身份已切、数据未载」的中间态暴露。
  if (panelGate?.switchId !== frame.switchId || panelGate.phase !== "preparing") return;
  setPanelGate(null);
}

/** 本窗口 id（label 去掉 panel- 前缀；与 panelStore.windowId 同口径）。 */
function panelWindowId(): string {
  return panelSelfLabel.startsWith("panel-")
    ? panelSelfLabel.slice("panel-".length)
    : panelSelfLabel;
}

/** 当前门状态（panelStore 在应用上下文广播时据此判定走切换门路径还是普通基线路径）。 */
export function currentVaultSwitchGate(): VaultSwitchGateState | null {
  return panelGate;
}

/** 门推进到 switching（撕裂窗口开始应用新仓库上下文；遮罩保持到加载链完成）。 */
export function markVaultSwitchApplying(): void {
  if (panelGate?.phase === "preparing") {
    setPanelGate({ ...panelGate, phase: "switching" });
  }
}

/** 门收尾（撕裂窗口新上下文加载链完成；switchId 不匹配 = 陈旧链收尾，忽略）。 */
export function completeVaultSwitchGate(switchId: string): void {
  if (panelGate?.switchId === switchId) setPanelGate(null);
}

// ---------- 主窗口面 ----------

/** 切换门运行结果。 */
export interface VaultSwitchGateResult {
  /** false = 存在 ack 失败的窗口（已广播 abort）；调用方必须中止切换并提示。 */
  ok: boolean;
  failures: Array<{ windowId: string; error: string }>;
  /** 本次门 id（调用方在收尾 finally 据此广播 abort 撤遮罩）。 */
  switchId: string;
}

/** 主窗口面选项（queryAliveWindows/pollMs 供测试注入替身；缺省枚举活的撕裂 webview）。 */
export interface VaultSwitchGateRunOptions {
  /** 存活撕裂窗口 id 查询。 */
  queryAliveWindows?: () => Promise<string[]>;
  /** 存活名单复查间隔（ms）。 */
  pollMs?: number;
}

/** 存活撕裂窗口 = 活的 webview，而非布局模型条目：boot 调和（进仓库后）才补建 OS 窗口，
 *  启动自动进仓时模型里已有持久化条目而窗口不存在，按模型等待会让启动死锁。 */
async function defaultAliveWindows(): Promise<string[]> {
  const webviews = await getAllWebviews();
  return webviews
    .map((w) => w.label)
    .filter((label) => label.startsWith(PANEL_LABEL_PREFIX))
    .map((label) => label.slice(PANEL_LABEL_PREFIX.length));
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

/**
 * 运行切换门：广播 prepare 并无限等待全部存活撕裂窗口的 ack。等待期间周期复查存活名单
 * （已消失的窗口从等待集剔除——关窗守卫自身 flush，无在途写入；查询失败按上一轮名单继续）
 * 并周期重发 prepare（boot 中的窗口接好监听前会漏帧，重发保证最终可达；对已 gated 的窗口
 * 幂等——flush 走脏门控、重复 ack 无害）。任一 ack 失败 → 广播 abort + 返回失败清单。
 * 活窗口枚举失败 = 无法枚举窗口，降级放行（事件线/IPC 不可达时随后的 open_vault 也会失败，
 * 切换中止于同一处；不阻塞在门上）。
 */
export async function runVaultSwitchGate(
  opts?: VaultSwitchGateRunOptions,
): Promise<VaultSwitchGateResult> {
  const switchId = crypto.randomUUID();
  const queryAlive = opts?.queryAliveWindows ?? defaultAliveWindows;
  let alive: string[];
  try {
    alive = await queryAlive();
  } catch (e) {
    console.error("切换门查询撕裂窗口列表失败，跳过等待直接切换", e);
    return { ok: true, failures: [], switchId };
  }
  if (alive.length === 0) return { ok: true, failures: [], switchId };

  const acks = new Map<string, VaultSwitchAck>();
  let unlisten: () => void;
  try {
    unlisten = await listen<VaultSwitchAck>(ACK_EVENT, (e) => {
      const f = e.payload;
      if (f.kind === "ack" && f.switchId === switchId) acks.set(f.from, f);
    });
  } catch (e) {
    // 事件线不可达 = 无法协调也无人应答：降级放行（同活窗口枚举失败）
    console.error("切换门订阅 ack 失败，跳过等待直接切换", e);
    return { ok: true, failures: [], switchId };
  }
  try {
    const pollMs = opts?.pollMs ?? GATE_POLL_MS;
    // 无限等待（作者裁决）：卡死窗口由用户手动关闭解除（webview 消失即剔除），不超时强切
    while (true) {
      try {
        alive = await queryAlive();
      } catch {
        // 复查失败按上一轮名单继续等
      }
      if (alive.every((id) => acks.has(id))) break;
      try {
        await emit(PREPARE_EVENT, { kind: "prepare", switchId } satisfies VaultSwitchPrepare);
      } catch (e) {
        // 重发失败下一轮再试；首轮发出前的订阅已就位，漏帧只发生在 boot 中窗口一侧
        console.error("重发切换门 prepare 失败", e);
      }
      await sleep(pollMs);
    }
  } finally {
    unlisten();
  }
  const failures = [...acks.values()]
    .filter((a) => !a.ok)
    .map((a) => ({ windowId: a.from, error: a.error ?? "撕裂窗口落盘失败" }));
  if (failures.length > 0) {
    await broadcastAbort(switchId);
    return { ok: false, failures, switchId };
  }
  return { ok: true, failures: [], switchId };
}

/** 广播 abort（fire-and-forget 安全网：事件线异常只记日志，调用方不得因此产生未处理拒绝）。 */
async function broadcastAbort(switchId: string): Promise<void> {
  try {
    await emit(ABORT_EVENT, { kind: "abort", switchId } satisfies VaultSwitchAbort);
  } catch (e) {
    console.error("广播切换门 abort 失败", e);
  }
}

/** 广播 abort（主窗口收尾安全网：撤销本次门在撕裂窗口侧的遮罩；已消费的窗口按 id 忽略）。 */
export async function abortVaultSwitchGate(switchId: string): Promise<void> {
  await broadcastAbort(switchId);
}
