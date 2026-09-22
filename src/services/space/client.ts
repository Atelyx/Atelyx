/**
 * 协作空间 HTTP 客户端（传输层，不含状态/重试）。
 *
 * 对应协作服务端 `collab-relay`（`/api/*`）：账号 / 空间 / 成员 / 邀请 / 内容。
 * 请求/响应字段名、大小写、错误体（`{"error": "..."}`）均按服务端如实镜像，不发明字段。
 * meta 端点由另一代理并行实现，形状以本文件契约为准；联调若有出入以后端为准调整。
 *
 * 规约：base = 规整后的 serverUrl（去尾斜杠）+ `/api`；带 `Authorization: Bearer`；
 * JSON 收发；30 秒超时；网络错误/超时/非 2xx 统一归一成 `SpaceApiError`（中文可定位，
 * 附 `status` 供上层分类，如 401 = 会话失效）。
 */

/** 请求/响应超时（毫秒）。 */
const REQUEST_TIMEOUT_MS = 30_000;

/**
 * 协作空间请求错误载体。
 * `status`：HTTP 状态码（0 = 未达服务端，网络/超时）；`code`：失败类别；
 * `serverMessage`：服务端返回的可读消息（网络/超时为空串）；`serverBody`：服务端
 * 错误体原始 JSON（网络/超时为 null，供 409 冲突等需要从体里取字段的端点用）。
 * 消息含「服务器地址 + 状态码 + 服务端错误消息」，便于定位。
 */
export class SpaceApiError extends Error {
  readonly status: number;
  readonly code: "network" | "timeout" | "http";
  readonly serverMessage: string;
  readonly serverBody: unknown;
  readonly url: string;

  constructor(opts: {
    message: string;
    status: number;
    code: "network" | "timeout" | "http";
    serverMessage: string;
    serverBody?: unknown;
    url: string;
  }) {
    super(opts.message);
    this.name = "SpaceApiError";
    this.status = opts.status;
    this.code = opts.code;
    this.serverMessage = opts.serverMessage;
    this.serverBody = opts.serverBody ?? null;
    this.url = opts.url;
  }
}

// ===== 类型（与服务端 JSON 形状一一对应）=====

import type {
  BacklinkRow,
  TagRow,
  GlobVaultResult,
  GrepVaultResult,
  CanvasPatch,
  DatedNote,
  TablePatch,
} from "@/types";
import type { DeviceInfo, InviteInfo } from "@/types/space";

// 设备会话与邀请码的服务端 JSON 形状落位 types/space（组件层引用需经 types 契约层）
export type { DeviceInfo, InviteInfo };

export interface AuthResponse {
  userId: string;
  username: string;
  displayName: string;
  token: string;
  sessionId: string;
}

export interface Credentials {
  username: string;
  password: string;
  displayName?: string;
  deviceName?: string;
}

export interface SpaceSummary {
  spaceId: string;
  name: string;
  role: string;
  ownerUserId: string;
  createdAt: number;
}

export interface CreateSpaceBody {
  name: string;
  path?: string;
}

export interface CreateSpaceResult {
  spaceId: string;
  name: string;
  role: string;
  rootPath?: string;
}

export interface MemberInfo {
  userId: string;
  username: string;
  displayName: string;
  role: string;
  joinedAt: number;
}

export interface CreateInviteBody {
  role: string;
  expiresInHours?: number;
  maxUses?: number;
}

export interface AcceptInviteResult {
  spaceId: string;
  name: string;
  role: string;
}

export interface SpaceMeta {
  values: Record<string, string>;
}

export interface MetaPatchBody {
  values: Record<string, string>;
}

export interface TreeNode {
  name: string;
  path: string;
  isDir: boolean;
  /** 文件 mtime unix 秒（展示/排序用；与乐观锁版本号无关）。 */
  updatedAt: number;
  children: TreeNode[];
}

export interface FileContent {
  content: string;
  /** 服务端内容版本号（乐观锁判据，与写/补丁返回值同源；不是文件 mtime）。 */
  updatedAt: number;
  /** base64 读回时服务端回显 `base64`。 */
  encoding?: "base64";
}

/** 读文件选项：`encoding: "base64"` = 原始字节按标准 base64 返回（附件/媒体等二进制内容必须走 base64，文本读会损坏字节）。 */
export interface ReadFileOptions {
  encoding?: "base64";
}

export interface WriteFileBody {
  path: string;
  content: string;
  /** 缺省 = content 按文本落盘；`base64` = content 为标准 base64，服务端解码后按字节落盘（限额按解码后字节计）。 */
  encoding?: "base64";
  /** 乐观并发基准：服务端内容版本更新则拒绝（409），不传 = 跳过冲突检查。 */
  baseUpdatedAt?: number;
}

export interface PatchCanvasBody {
  path: string;
  patch: CanvasPatch;
  baseUpdatedAt?: number;
}

export interface PatchTableBody {
  path: string;
  patch: TablePatch;
  baseUpdatedAt?: number;
  /** 跳过冲突检查强制覆盖（冲突条「保留本地并保存」用）。 */
  force?: boolean;
}

/** 补丁端点结果：200 → 成功（含写入后内容版本与实际路径）；409 → 冲突（不抛错，updatedAt = 服务端当前版本）。 */
export type SpacePatchResult =
  | { conflict: false; updatedAt: number; file: string }
  | { conflict: true; updatedAt?: number };

export interface RenameBody {
  oldPath: string;
  newPath: string;
}

export interface CopyBody {
  fromPath: string;
  toPath: string;
}

export interface CreateFolderBody {
  path: string;
}

export interface DeleteFolderBody {
  path: string;
  force?: boolean;
}

export interface DeleteFolderResult {
  needsConfirm?: boolean;
  deleted?: boolean;
  /** 递归条目数（含隐藏项；删除确认弹窗文案用）。 */
  itemCount?: number;
}

/** 媒体目录单层条目（`GET /media/list`，保留媒体目录枚举；树端点不含隐藏目录）。 */
export interface MediaListEntry {
  name: string;
  size: number;
}

/** 分组方法面（auth / spaces / meta / content）。 */
export interface SpaceClient {
  auth: {
    register(body: Credentials): Promise<AuthResponse>;
    login(body: Credentials): Promise<AuthResponse>;
    logout(): Promise<void>;
    listDevices(): Promise<DeviceInfo[]>;
    revokeDevice(sessionId: string): Promise<void>;
  };
  spaces: {
    list(): Promise<SpaceSummary[]>;
    create(body: CreateSpaceBody): Promise<CreateSpaceResult>;
    rename(spaceId: string, body: { name: string }): Promise<{ spaceId: string; name: string }>;
    transfer(spaceId: string, body: { toUserId: string }): Promise<{ spaceId: string; ownerUserId: string }>;
    listMembers(spaceId: string): Promise<MemberInfo[]>;
    removeMember(spaceId: string, userId: string): Promise<void>;
    createInvite(spaceId: string, body: CreateInviteBody): Promise<InviteInfo>;
    revokeInvite(spaceId: string, code: string): Promise<void>;
    acceptInvite(body: { code: string }): Promise<AcceptInviteResult>;
  };
  meta: {
    getSpaceMeta(spaceId: string): Promise<SpaceMeta>;
    patchSpaceMeta(spaceId: string, body: MetaPatchBody): Promise<unknown>;
    deleteSpaceMeta(spaceId: string, key: string): Promise<void>;
    getMyMeta(spaceId: string): Promise<SpaceMeta>;
    patchMyMeta(spaceId: string, body: MetaPatchBody): Promise<unknown>;
    deleteMyMeta(spaceId: string, key: string): Promise<void>;
  };
  content: {
    getTree(spaceId: string): Promise<TreeNode[]>;
    readFile(spaceId: string, path: string, opts?: ReadFileOptions): Promise<FileContent>;
    writeFile(spaceId: string, body: WriteFileBody): Promise<{ updatedAt: number }>;
    /** 单层枚举保留媒体目录（如 `.space-media/...`；隐藏目录不出现在树/索引端点）。 */
    mediaList(spaceId: string, path: string): Promise<{ entries: MediaListEntry[] }>;
    rename(spaceId: string, body: RenameBody): Promise<void>;
    copy(spaceId: string, body: CopyBody): Promise<void>;
    deleteFile(spaceId: string, path: string): Promise<void>;
    createFolder(spaceId: string, body: CreateFolderBody): Promise<{ path: string }>;
    deleteFolder(spaceId: string, body: DeleteFolderBody): Promise<DeleteFolderResult>;
    patchCanvas(spaceId: string, body: PatchCanvasBody): Promise<SpacePatchResult>;
    patchTable(spaceId: string, body: PatchTableBody): Promise<SpacePatchResult>;
    backlinks(spaceId: string, query: { noteName: string; noteFile: string }): Promise<BacklinkRow[]>;
    tags(spaceId: string): Promise<TagRow[]>;
    glob(spaceId: string, body: { pattern: string; path?: string }): Promise<GlobVaultResult>;
    grep(spaceId: string, body: { pattern: string; path?: string; include?: string }): Promise<GrepVaultResult>;
    /** 追加一个历史版本（服务端在同一路径串行锁内合并，多端并发不丢版本）。 */
    historyRecord(spaceId: string, body: HistoryRecordBody): Promise<unknown>;
    /** 聚合全空间历史（版本流 + 全量版本时间戳）。 */
    historyAggregate(spaceId: string): Promise<SpaceHistoryAggregate>;
    /** 扫描带日期笔记（frontmatter date/due；尊重团队排除文件夹）。 */
    datedNotes(spaceId: string): Promise<DatedNote[]>;
  };
}

/** 历史版本作者（与 `services/history` 的 HistoryAuthor 同形状）。 */
export interface HistoryRecordAuthor {
  id: string;
  name: string;
  device: string;
}

/** `POST /history/record` 请求体（judgement 判据由客户端表达，服务端不感知内容格式）。 */
export interface HistoryRecordBody {
  kind: "note" | "canvas" | "table";
  file: string;
  content: string;
  action: "edit" | "restore";
  author: HistoryRecordAuthor;
  summary?: string;
  note?: string;
  coAuthors?: HistoryRecordAuthor[];
  /** >0 时「上一版为同作者 edit 且在窗口内」就地滑动更新（版本粒度，不逐键）。 */
  coalesceEditMs?: number;
  /** >0 时保留最近 N 版。 */
  maxVersions?: number;
  /** 侧文件字节预算（超限从最旧剪枝）。 */
  byteBudget?: number;
}

/** 历史版本行（聚合返回，不含全文）。 */
export interface SpaceHistoryEntry {
  file: string;
  kind: string;
  ts: number;
  authorId: string;
  authorName: string;
  authorDevice: string;
  action: string;
  summary?: string;
  note?: string;
}

/** `GET /history/aggregate` 返回：版本流（ts 倒序、上限）+ 全量版本时间戳。 */
export interface SpaceHistoryAggregate {
  entries: SpaceHistoryEntry[];
  timestamps: number[];
}

/** 从非 2xx 响应体提取服务端错误消息（优先 `{"error":...}` / `{"message":...}`）与原始 JSON 体。 */
async function parseServerResponse(res: Response): Promise<{ message: string; body: unknown }> {
  const text = await res.text().catch(() => "");
  if (!text) return { message: res.statusText || `状态码 ${res.status}`, body: null };
  let body: unknown = null;
  try {
    body = JSON.parse(text);
  } catch {
    // 非 JSON：原文截断后直出
  }
  if (body && typeof body === "object") {
    const b = body as { error?: unknown; message?: unknown };
    if (typeof b.error === "string") return { message: b.error, body };
    if (typeof b.message === "string") return { message: b.message, body };
  }
  return { message: text.slice(0, 500), body };
}

export interface CreateSpaceClientOptions {
  /** 取当前 Bearer 令牌（每次鉴权请求前调用；登录/注册端点传空串即不携带）。 */
  getToken: () => Promise<string>;
}

/**
 * 创建某协作服务器的 HTTP 客户端。
 * @param serverUrl 服务器地址（如 `http://192.168.1.10:11224`），可带尾斜杠。
 * @param getToken 取令牌的函数；返回空串表示不携带 Authorization。
 */
export function createSpaceClient(serverUrl: string, getToken: () => Promise<string>): SpaceClient {
  const base = `${serverUrl.replace(/\/+$/, "")}/api`;

  async function request<T>(
    method: string,
    path: string,
    opts?: { auth?: boolean; query?: Record<string, string | number | boolean | undefined>; body?: unknown },
  ): Promise<T> {
    let url: URL;
    try {
      url = new URL(`${base}${path}`);
    } catch {
      throw new SpaceApiError({
        message: `协作服务器地址无效：${serverUrl}`,
        status: 0,
        code: "network",
        serverMessage: "",
        url: `${base}${path}`,
      });
    }
    if (opts?.query) {
      for (const [key, value] of Object.entries(opts.query)) {
        if (value !== undefined) url.searchParams.set(key, String(value));
      }
    }

    const headers: Record<string, string> = { "Content-Type": "application/json" };
    if (opts?.auth !== false) {
      const token = await getToken();
      if (token) headers["Authorization"] = `Bearer ${token}`;
    }

    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);
    let res: Response;
    try {
      res = await fetch(url.toString(), {
        method,
        headers,
        body: opts?.body !== undefined ? JSON.stringify(opts.body) : undefined,
        signal: controller.signal,
      });
    } catch (err) {
      // 本客户端唯一 abort 来源是内部超时定时器，故 AbortError 即超时
      if (err instanceof Error && err.name === "AbortError") {
        throw new SpaceApiError({
          message: `连接协作服务器 ${serverUrl} 超时（${REQUEST_TIMEOUT_MS / 1000} 秒）`,
          status: 0,
          code: "timeout",
          serverMessage: "",
          url: url.toString(),
        });
      }
      throw new SpaceApiError({
        message: `无法连接协作服务器 ${serverUrl}：${err instanceof Error ? err.message : String(err)}`,
        status: 0,
        code: "network",
        serverMessage: "",
        url: url.toString(),
      });
    } finally {
      clearTimeout(timer);
    }

    if (!res.ok) {
      const { message: serverMessage, body } = await parseServerResponse(res).catch(() => ({
        message: "",
        body: null,
      }));
      throw new SpaceApiError({
        message: `协作服务器 ${serverUrl} 返回 ${res.status}：${serverMessage}`,
        status: res.status,
        code: "http",
        serverMessage,
        serverBody: body,
        url: url.toString(),
      });
    }

    const text = await res.text().catch(() => "");
    if (!text) return {} as T;
    try {
      return JSON.parse(text) as T;
    } catch {
      // 2xx 但响应体非 JSON（反代错误页/截断响应等）：归一化为 SpaceApiError，
      // 不裸抛 SyntaxError——上层按 status/code 分类处理，裸异常会让调用方误判为网络故障
      throw new SpaceApiError({
        message: `协作服务器 ${serverUrl} 返回了无法解析的数据（${res.status}）`,
        status: res.status,
        code: "http",
        serverMessage: text.slice(0, 500),
        url: url.toString(),
      });
    }
  }

  /** 补丁端点通用：200 → 成功结果；409 → 冲突结果（不抛错，冲突由调用方按语义消费）；
   *  其余错误照常归一化抛 SpaceApiError（如 patch.id 不匹配 400）。 */
  async function patchRequest(
    spaceId: string,
    kind: "canvas" | "table",
    body: PatchCanvasBody | PatchTableBody,
  ): Promise<SpacePatchResult> {
    try {
      const res = await request<{ updatedAt: number; file: string }>(
        "POST",
        `/spaces/${spaceId}/patches/${kind}`,
        { body },
      );
      return { conflict: false, updatedAt: res.updatedAt, file: res.file };
    } catch (err) {
      if (err instanceof SpaceApiError && err.status === 409) {
        const b = err.serverBody as { updatedAt?: unknown } | null;
        const updatedAt = typeof b?.updatedAt === "number" ? b.updatedAt : undefined;
        return { conflict: true, ...(updatedAt !== undefined ? { updatedAt } : {}) };
      }
      throw err;
    }
  }

  return {
    auth: {
      register: (body) => request<AuthResponse>("POST", "/auth/register", { auth: false, body }),
      login: (body) => request<AuthResponse>("POST", "/auth/login", { auth: false, body }),
      logout: () => request<void>("POST", "/auth/logout"),
      listDevices: () => request<DeviceInfo[]>("GET", "/auth/devices"),
      revokeDevice: (sessionId) => request<void>("DELETE", `/auth/devices/${sessionId}`),
    },
    spaces: {
      list: () => request<SpaceSummary[]>("GET", "/spaces"),
      create: (body) => request<CreateSpaceResult>("POST", "/spaces", { body }),
      rename: (spaceId, body) => request<{ spaceId: string; name: string }>("PATCH", `/spaces/${spaceId}`, { body }),
      transfer: (spaceId, body) =>
        request<{ spaceId: string; ownerUserId: string }>("POST", `/spaces/${spaceId}/transfer`, { body }),
      listMembers: (spaceId) => request<MemberInfo[]>("GET", `/spaces/${spaceId}/members`),
      removeMember: (spaceId, userId) => request<void>("DELETE", `/spaces/${spaceId}/members/${userId}`),
      createInvite: (spaceId, body) => request<InviteInfo>("POST", `/spaces/${spaceId}/invites`, { body }),
      revokeInvite: (spaceId, code) => request<void>("DELETE", `/spaces/${spaceId}/invites/${code}`),
      acceptInvite: (body) => request<AcceptInviteResult>("POST", "/invites/accept", { body }),
    },
    meta: {
      getSpaceMeta: (spaceId) => request<SpaceMeta>("GET", `/spaces/${spaceId}/meta`),
      patchSpaceMeta: (spaceId, body) => request<unknown>("PATCH", `/spaces/${spaceId}/meta`, { body }),
      deleteSpaceMeta: (spaceId, key) =>
        request<void>("DELETE", `/spaces/${spaceId}/meta`, { query: { scope: "space", key } }),
      getMyMeta: (spaceId) => request<SpaceMeta>("GET", `/spaces/${spaceId}/meta/me`),
      patchMyMeta: (spaceId, body) => request<unknown>("PATCH", `/spaces/${spaceId}/meta/me`, { body }),
      deleteMyMeta: (spaceId, key) => request<void>("DELETE", `/spaces/${spaceId}/meta/me`, { query: { key } }),
    },
    content: {
      getTree: (spaceId) => request<TreeNode[]>("GET", `/spaces/${spaceId}/tree`),
      readFile: (spaceId, path, opts) =>
        request<FileContent>("GET", `/spaces/${spaceId}/file`, {
          query: { path, encoding: opts?.encoding },
        }),
      writeFile: (spaceId, body) => request<{ updatedAt: number }>("PUT", `/spaces/${spaceId}/file`, { body }),
      mediaList: (spaceId, path) =>
        request<{ entries: MediaListEntry[] }>("GET", `/spaces/${spaceId}/media/list`, { query: { path } }),
      rename: (spaceId, body) => request<void>("POST", `/spaces/${spaceId}/rename`, { body }),
      copy: (spaceId, body) => request<void>("POST", `/spaces/${spaceId}/copy`, { body }),
      deleteFile: (spaceId, path) => request<void>("DELETE", `/spaces/${spaceId}/file`, { query: { path } }),
      createFolder: (spaceId, body) => request<{ path: string }>("POST", `/spaces/${spaceId}/folder`, { body }),
      deleteFolder: (spaceId, body) => request<DeleteFolderResult>("DELETE", `/spaces/${spaceId}/folder`, { body }),
      patchCanvas: (spaceId, body) => patchRequest(spaceId, "canvas", body),
      patchTable: (spaceId, body) => patchRequest(spaceId, "table", body),
      backlinks: (spaceId, query) =>
        request<BacklinkRow[]>("GET", `/spaces/${spaceId}/backlinks`, {
          query: { noteName: query.noteName, noteFile: query.noteFile },
        }),
      tags: (spaceId) => request<TagRow[]>("GET", `/spaces/${spaceId}/tags`),
      glob: (spaceId, body) => request<GlobVaultResult>("POST", `/spaces/${spaceId}/glob`, { body }),
      grep: (spaceId, body) => request<GrepVaultResult>("POST", `/spaces/${spaceId}/grep`, { body }),
      historyRecord: (spaceId, body) =>
        request<unknown>("POST", `/spaces/${spaceId}/history/record`, { body }),
      historyAggregate: (spaceId) =>
        request<SpaceHistoryAggregate>("GET", `/spaces/${spaceId}/history/aggregate`),
      datedNotes: (spaceId) => request<DatedNote[]>("GET", `/spaces/${spaceId}/dated-notes`),
    },
  };
}
