/**
 * 通用历史记录（画布/笔记/表格共用，每文件隐藏侧文件，审计/回滚轴）。
 *
 * 磁盘仍以源文件（.atlx/.md/.atb）为真源；历史是独立的审计轴，存
 * `.atelyx/history/<kind>/<最小编码名>.json`（note 保持顶层路径；随仓库走、跨设备一致；
 * `.atelyx` 隐藏目录被文件树与 watcher 排除，写历史不触发回波）。
 * 每个版本存**全文快照**（回滚即时可靠）+ 相对上一版本的 diff 摘要（展示「改了啥」；
 * 笔记默认行级，画布/表格经 `summarize` 回调生成实体级人话摘要），
 * 版本粒度 = 落盘存档点 + 外部写入 + 手动回滚 + Agent 工具写入（连贯编辑合并为一个存档点，不逐键）。
 *
 * 多人协作：默认作者 = 当前用户协作昵称（`setHistoryAuthor` 注入）；主作者可被
 * `authorOverride` 覆盖——协作远端合入署发送端协作者、Agent 工具写文件署「AI Agent（操作人）」；
 * 并发存档点可带 `coAuthors`（窗口内全部参与者，多人署名）。合并仅限**同一作者 id**连续编辑，
 * 防版本串身份/串版（不同作者/不同操作人拆版）。
 *
 * 防膨胀：留存默认全留，可配 `maxVersions` 剪枝（保留最近 N 版）；diff 摘要而非整存两份正文。
 * 并发安全：读改写整文件（同事写同一侧文件为罕见边界，后写者胜，容忍偶发丢版本）。
 */
import { deleteVaultFile, readVaultFile, writeVaultFile } from "@/services/vault/aiFiles";
import type { AgentHistoryReadResult } from "@/types";

/** 历史作用的文件类型（决定侧文件路径与扩展名识别）。 */
export type HistoryKind = "note" | "canvas" | "table";

/** 历史 version 的作者身份。 */
export interface HistoryAuthor {
  /** 稳定标识（设备名 / 固定 agent id）。 */
  id: string;
  /** 显示名（协作昵称，缺省回落设备名）。 */
  name: string;
  /** 设备名。 */
  device: string;
}

/** 版本行为枚举（UI 展示 + 回滚决策；历史侧文件中的旧值由展示层 `?? action` 兜底显示原值）。 */
export type HistoryAction = "edit" | "restore";

/** 一个历史版本。 */
export interface HistoryVersion {
  seq: number;
  ts: number;
  author: HistoryAuthor;
  action: HistoryAction;
  /** 该版本全文快照（LF；回滚目标）。 */
  content: string;
  /** 相对上一版本的改动摘要（展示用；纯函数 diff 现算补全，见 diffSummary）。 */
  summary?: string;
  /** 协作并发存档点内的其他参与作者（主 author 之外；多协作者同时编辑合并进同一版本时记录，按操作人署名）。 */
  coAuthors?: HistoryAuthor[];
  /** 补充说明（外部导入/回滚/Agent 写入时的备注）。 */
  note?: string;
}

/** 侧文件 schema。 */
interface HistoryFile {
  versions: HistoryVersion[];
}

/**
 * 历史侧文件字节预算（写盘前剪枝：超限丢最旧保最新）。与 Rust `read_vault_file`
 * 的 5MB 整读上限对齐：历史文件一旦膨胀超过该上限，`loadHistory` 读失败返回空数组、
 * 随后 `recordHistoryVersion` 会以「仅新版本」整文件覆盖写回 → 全部旧版本一次性静默
 * 清空（数据丢失）。故写入前必须把序列化体积压到预算内，保证永不触及读上限。
 * 用 UTF-8 字节数（TextEncoder）而非 JS 字符串长度——Rust 侧按字节判定，中文内容
 * 两者不等长。
 */
const HISTORY_BYTE_BUDGET = 5_000_000;

/** 剪枝：序列化体积超预算时从最旧开始丢，仅剩 1 版也超预算时保留 1 版兜底（尽力而为）。 */
function pruneVersions(versions: HistoryVersion[], budget: number): HistoryVersion[] {
  let next = versions;
  const size = () =>
    new TextEncoder().encode(JSON.stringify({ versions: next })).length;
  while (next.length > 1 && size() > budget) {
    next = next.slice(1);
  }
  return next;
}

/** AI Agent 工具写文件的固定身份（协作历史里的「Agent 协作」条目）。 */
export const AGENT_AUTHOR: HistoryAuthor = {
  id: "ai-agent",
  name: "AI Agent",
  device: "AI",
};

/** 侧文件根（隐藏目录，随仓库走）。 */
const HISTORY_DIR = ".atelyx/history";

/**
 * 侧文件名编码（最小百分号转义）：仅转义文件系统非法字符（`% / \ : * ? " < > |`）、
 * 控制符与 DEL（UTF-8 %XX，大写十六进制），中文等合法字符保留原样。
 * 不能用 encodeURIComponent——它把每个 CJK 字符膨胀成 9 字符（`项`→`%E9%A1%B9`），
 * 中文长路径的侧文件名会超出 NAS/SMB 服务端 ~260 字符路径上限（实测客户端 `\\?\UNC\`
 * 扩展前缀无效，服务端仍拒绝写入），导致长名笔记的历史永远写不进/读不出。最小编码下
 * 侧文件路径长度 ≈ 笔记自身路径 + 常数 overhead——笔记可访问则侧文件必可访问。
 * 编码须与 Rust `vault.rs` 的 `percent_encode` 保持同一字符集（重命名迁移两端各自计算），
 * 解码复用 Rust `percent_decode`（`%` 本身也被转义，`%XX` 序列无歧义）。
 */
function encodeSideName(file: string): string {
  let out = "";
  for (const ch of file) {
    const code = ch.codePointAt(0)!;
    out +=
      code < 0x20 || code === 0x7f || "%/\\:*?\"<>|".includes(ch)
        ? "%" +
          code.toString(16).toUpperCase().padStart(2, "0")
        : ch;
  }
  return out;
}

/** 旧版侧文件名（encodeURIComponent 全量转义）：存量侧文件的读取/迁移来源。 */
function legacyHistoryPathFor(kind: HistoryKind, file: string): string {
  const encoded = encodeURIComponent(file);
  return kind === "note"
    ? `${HISTORY_DIR}/${encoded}.json`
    : `${HISTORY_DIR}/${kind}/${encoded}.json`;
}

/** 侧文件相对路径：note 顶层，canvas/table 按 kind 分目录；文件名为最小编码（见 encodeSideName）。 */
export function historyPathFor(kind: HistoryKind, file: string): string {
  const encoded = encodeSideName(file);
  return kind === "note"
    ? `${HISTORY_DIR}/${encoded}.json`
    : `${HISTORY_DIR}/${kind}/${encoded}.json`;
}

let myAuthor: HistoryAuthor = { id: "", name: "", device: "" };
/** 设置当前作者（进入仓库/身份变化时由上层注入；未设置回落设备名）。 */
export function setHistoryAuthor(author: HistoryAuthor): void {
  myAuthor = author;
}

/**
 * 读取侧文件版本列表；缺失/损坏 → 空数组（历史尽力而为，不阻塞编辑）。
 * 新编码名缺失时回退读旧 encodeURIComponent 名（存量侧文件），实现读侧兼容。
 */
export async function loadHistory(
  kind: HistoryKind,
  file: string,
): Promise<HistoryVersion[]> {
  return (await loadVersions(kind, file)).versions;
}

/** 双路径读取：新编码名优先，缺失回落旧编码存量名；fromLegacy 供迁移收尾判定。 */
async function loadVersions(
  kind: HistoryKind,
  file: string,
): Promise<{ versions: HistoryVersion[]; fromLegacy: boolean }> {
  try {
    const raw = await readVaultFile(historyPathFor(kind, file));
    const parsed = JSON.parse(raw) as HistoryFile;
    return { versions: Array.isArray(parsed.versions) ? parsed.versions : [], fromLegacy: false };
  } catch {
    // 新名缺失/损坏：回落旧编码存量侧文件
  }
  try {
    const raw = await readVaultFile(legacyHistoryPathFor(kind, file));
    const parsed = JSON.parse(raw) as HistoryFile;
    return { versions: Array.isArray(parsed.versions) ? parsed.versions : [], fromLegacy: true };
  } catch {
    return { versions: [], fromLegacy: false };
  }
}

/**
 * 存量侧文件迁移：确保该文件的历史位于新编码名下——新名缺失且旧名存在时把版本迁移过去；
 * 无论来源，最后清理旧编码侧文件（防新旧并存致仓库历史聚合重复计数；删除失败静默）。
 * 重命名/移动迁移（Rust remap_sideloads 只按新编码名查找）依赖侧文件已处于新名下——
 * 迁移必须先于 remap 完成；幂等，任一步失败静默（下次再试）。
 */
export async function migrateHistoryFile(
  kind: HistoryKind,
  file: string,
): Promise<void> {
  const { versions, fromLegacy } = await loadVersions(kind, file);
  if (fromLegacy) {
    try {
      await writeVaultFile(
        historyPathFor(kind, file),
        JSON.stringify({ versions: pruneVersions(versions, HISTORY_BYTE_BUDGET) }),
      );
    } catch {
      // 迁移写入失败：旧文件仍在，下次再试
      return;
    }
  }
  // 纯 ASCII 文件名下 encodeURIComponent 与最小编码输出相同（新旧路径一致），不存在
  // 「旧文件」——删除即删真实历史；仅路径不同（含 CJK/非法字符）时才有旧文件可清
  if (legacyHistoryPathFor(kind, file) === historyPathFor(kind, file)) return;
  try {
    await deleteVaultFile(legacyHistoryPathFor(kind, file));
  } catch {
    // 无旧文件或删除失败：静默
  }
}

/** 行级 diff：返回相对上一版本的改动行摘要文本（如 `+3 −1 行`；未变 → `未改动`）。 */
function diffSummary(prev: string, next: string): string {
  if (prev === next) return "未改动";
  const a = prev.split("\n");
  const b = next.split("\n");
  let added = 0;
  let removed = 0;
  // 简单行补齐（myers 太贵）：分别统计多出/缺失的行，足够展示「加了 N 行减了 M 行」
  const common = new Set(prev === "" ? [] : a);
  for (const line of b) if (line !== "" && !common.has(line)) added++;
  const commonB = new Set(b);
  for (const line of a) if (line !== "" && !commonB.has(line)) removed++;
  // 空行不计数；实际增删行以字数估算兜底（避免全空文件显示 0）
  if (added === 0 && removed === 0) {
    const lenDiff = next.length - prev.length;
    return lenDiff >= 0 ? `+${lenDiff} 字` : `${lenDiff} 字`;
  }
  return `${added ? `+${added} 行 ` : ""}${removed ? `−${removed} 行` : ""}`.trim();
}

/** 合并协作作者集合（按 id 去重）：coalesce 滑动更新时多存档点的参与者并集，防丢前序协作者。 */
function mergeCoAuthors(
  a: HistoryAuthor[] | undefined,
  b: HistoryAuthor[] | undefined,
): HistoryAuthor[] | undefined {
  if (!b || b.length === 0) return a;
  if (!a || a.length === 0) return b;
  const seen = new Set(a.map((x) => x.id));
  const merged = [...a];
  for (const x of b) {
    if (!seen.has(x.id)) {
      seen.add(x.id);
      merged.push(x);
    }
  }
  return merged;
}

/**
 * 追加一个历史版本（版本边界）。内容与上一版本相同的 no-op 直接跳过（防重复存档点）。
 * maxVersions = 0 表示全留（默认）；>0 时保留最近 N 版（剪枝，防无限膨胀）。
 * coalesceEditMs > 0 时：上一版本为连续编辑（action="edit"）且**作者相同**且距今 < 该值 →
 * 就地滑动更新该版本（不新增 seq，保证「稳定存档点 + 最新可回滚到当下」，避免每次 500ms
 * 防抖都加一版）。外部/回滚/Agent 等显式边界不受节流影响（作者不同也不合并，防身份串版）。
 */
export async function recordHistoryVersion(
  kind: HistoryKind,
  file: string,
  opts: {
    content: string;
    action: HistoryAction;
    note?: string;
    maxVersions?: number;
    coalesceEditMs?: number;
    /** 侧文件字节预算（缺省 HISTORY_BYTE_BUDGET；测试注入小值验证剪枝）。 */
    byteBudget?: number;
    /** 作者覆盖（Agent 工具写入等非当前用户来源）。 */
    authorOverride?: HistoryAuthor;
    /** 协作并发存档点内的其他参与作者（主 author 之外；多协作者同时编辑合并进同一版本时记录）。 */
    coAuthors?: HistoryAuthor[];
    /** 版本摘要生成（缺省 = 行级 diff）；prev = 上一版本全文（首版为空串）。表格/画布传实体级摘要。 */
    summarize?: (prev: string, next: string) => string;
  },
): Promise<void> {
  const { versions, fromLegacy } = await loadVersions(kind, file);
  const last = versions[versions.length - 1];
  if (last && last.content === opts.content && last.action === opts.action) {
    // 内容 no-op：但读自旧编码存量时仍迁移到新名（防重命名迁移/仓库聚合在旧名上失联）
    if (fromLegacy) await migrateHistoryFile(kind, file);
    return;
  }
  const author = opts.authorOverride ?? myAuthor;
  const summaryOf = (prev: string, next: string): string =>
    opts.summarize ? opts.summarize(prev, next) : diffSummary(prev, next);
  const coalesce =
    opts.coalesceEditMs &&
    opts.action === "edit" &&
    last?.action === "edit" &&
    last.author.id === author.id &&
    Date.now() - last.ts < opts.coalesceEditMs;
  // 连续编辑节流：滑动更新上一版本快照（seq 不变），回滚仍可到最新
  if (coalesce) {
    const prev = versions[versions.length - 2]?.content ?? "";
    last!.content = opts.content;
    last!.ts = Date.now();
    last!.author = author;
    last!.coAuthors = mergeCoAuthors(last!.coAuthors, opts.coAuthors);
    last!.summary = summaryOf(prev, opts.content);
    if (opts.note) last!.note = opts.note;
    // 写盘前同样过字节预算：coalesce 滑动更新也可能把总量推过预算（大笔记贴近上限时
    // 单次内容增长即触发），否则下次整读失败 → 非 coalesce 路径「仅新版本」覆盖写回清空历史
    const pruned = pruneVersions(versions, opts.byteBudget ?? HISTORY_BYTE_BUDGET);
    try {
      await writeVaultFile(historyPathFor(kind, file), JSON.stringify({ versions: pruned }));
      // 读自旧编码存量 → 新文件已写出，删除旧文件（防仓库历史聚合重复计数；缺失报错被吞）
      if (fromLegacy) await deleteVaultFile(legacyHistoryPathFor(kind, file)).catch(() => {});
    } catch {
      // 历史写入失败（只读/权限）不应阻塞编辑：静默降级
    }
    return;
  }
  const prevContent = last?.content ?? "";
  const version: HistoryVersion = {
    seq: last ? last.seq + 1 : 1,
    ts: Date.now(),
    author,
    action: opts.action,
    content: opts.content,
    summary: summaryOf(prevContent, opts.content),
    ...(opts.coAuthors && opts.coAuthors.length ? { coAuthors: opts.coAuthors } : {}),
    ...(opts.note ? { note: opts.note } : {}),
  };
  let next = [...versions, version];
  const max = opts.maxVersions ?? 0;
  if (max > 0 && next.length > max) {
    next = next.slice(next.length - max);
  }
  // 字节预算剪枝（与版本数上限叠加，优先保最新）：防历史文件膨胀触及读上限被整份清空
  next = pruneVersions(next, opts.byteBudget ?? HISTORY_BYTE_BUDGET);
  try {
    await writeVaultFile(historyPathFor(kind, file), JSON.stringify({ versions: next }));
    // 读自旧编码存量 → 新文件已写出，删除旧文件（缺失报错被吞；防聚合重复计数）
    if (fromLegacy) await deleteVaultFile(legacyHistoryPathFor(kind, file)).catch(() => {});
  } catch {
    // 历史写入失败（只读/权限）不应阻塞编辑：静默降级
  }
}

/** 取指定 seq 版本的全文快照；不存在返回 null（回滚目标）。 */
export function versionContentAt(versions: HistoryVersion[], seq: number): string | null {
  return versions.find((v) => v.seq === seq)?.content ?? null;
}

/** 按文件扩展名识别历史 kind（.md → note、.atlx → canvas、.atb → table；其余 null）。 */
function historyKindOfFile(file: string): HistoryKind | null {
  if (/\.md$/i.test(file)) return "note";
  if (/\.atlx$/i.test(file)) return "canvas";
  if (/\.atb$/i.test(file)) return "table";
  return null;
}

/**
 * Agent 工具写文件后的历史记录（write_file/edit_file 落地回调；store 层调用）。
 * content 缺省时读回磁盘（edit_file 无新内容）。跳过 `.atelyx/` 隐藏目录（历史自身
 * 侧文件写入、配置等不记历史，防递归/噪声）。记录失败静默降级。
 */
export async function recordAgentFileWrite(
  file: string,
  content?: string,
): Promise<void> {
  if (file.startsWith(".atelyx/")) return;
  const kind = historyKindOfFile(file);
  if (!kind) return;
  let snapshot = content;
  if (snapshot === undefined) {
    try {
      snapshot = await readVaultFile(file);
    } catch {
      return;
    }
  }
  // 操作人 = 当前登录用户（Agent 任务由其发起）：历史须能追溯到责任人，署名「AI Agent（操作人）」。
  // id 带操作人标识——60s coalesce 只合并同一操作人的连续 Agent 写入，不同操作人不串版（防前序操作人丢失）。
  const operator = myAuthor.name || myAuthor.device || "用户";
  const operatorId = myAuthor.id || "unknown";
  await recordHistoryVersion(kind, file, {
    content: snapshot,
    action: "edit",
    authorOverride: {
      id: `${AGENT_AUTHOR.id}:${operatorId}`,
      name: `AI Agent（${operator}）`,
      device: AGENT_AUTHOR.device,
    },
    note: `AI 工具写入/修改（由 ${operator} 操作）`,
    coalesceEditMs: 60_000,
  });
}

/** read_history 工具单次内联返回的版本摘要上限（只带摘要不带全文；防止撑爆上下文）。 */
const HISTORY_LIST_MAX = 50;

/**
 * AI read_history 工具后端：读某仓库文件的历史（按扩展名识别 kind，内部直读
 * `.atelyx/history/` 侧文件——隐藏屏蔽的刻意豁免）。未传 version 返回版本摘要列表
 * （不带全文，防撑爆上下文）；传 version 返回该版全文快照（模型 write_file 写回即恢复）。
 * 无历史/未知类型显式 ok=false。
 */
export async function readHistoryForAgent(
  file: string,
  opts?: { version?: number },
): Promise<AgentHistoryReadResult> {
  const kind = historyKindOfFile(file);
  if (!kind) {
    const base = file.split("/").pop() ?? file;
    return { ok: false, summary: `该文件 ${base} 无历史记录（仅 .md/.atlx/.atb 有）` };
  }
  const versions = await loadHistory(kind, file);
  if (versions.length === 0) return { ok: false, summary: "无历史记录" };
  if (opts?.version !== undefined) {
    const content = versionContentAt(versions, opts.version);
    if (content === null) {
      return { ok: false, summary: `版本 v${opts.version} 不存在（共 ${versions.length} 版）` };
    }
    return { ok: true, summary: `v${opts.version} 快照（${[...content].length} 字符）`, content };
  }
  const list = versions.slice(-HISTORY_LIST_MAX).map((v) => ({
    seq: v.seq,
    ts: v.ts,
    authorName: v.author.name,
    action: v.action,
    ...(v.summary ? { summary: v.summary } : {}),
    ...(v.note ? { note: v.note } : {}),
  }));
  return { ok: true, summary: `共 ${versions.length} 个版本`, versions: list };
}
