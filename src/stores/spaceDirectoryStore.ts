/**
 * 协作空间目录 store（空间列表 / 成员 / 邀请的 UI 数据编排）。
 *
 * 所有网络 I/O 经 `services/space/client`（组件不得直调 service）；最近空间条目的
 * 持久化真源在 global.json（appStore.recentSpaces 镜像），本 store 只在增删改名时
 * 经 `useAppStore.setState` 同步该镜像并落盘，不改动 appStore 的动作面。
 *
 * `loginPrompt`：need-login 引导状态——文件面板点空间条目无会话时置入（可携带登录后
 * 自动重试进入的空间条目），设置弹窗「多人协作」区读取预填，登录成功后自动重试。
 */

import { create } from "zustand";
import { createSpaceClient } from "@/services/space/client";
import { getToken } from "@/services/space/auth";
import { updateGlobalConfig } from "@/services/global";
import { useAppStore } from "@/stores/appStore";
import type {
  AcceptInviteResult,
  CreateInviteBody,
  InviteInfo,
  MemberInfo,
  SpaceSummary,
} from "@/services/space/client";

/** 登录成功后自动重试进入的空间条目（与 appStore.selectSpace 入参同形）。 */
export interface SpaceEnterEntry {
  serverUrl: string;
  spaceId: string;
  name: string;
}

/** need-login 引导：待登录服务器地址 + 可选的自动重试条目。 */
export interface SpaceLoginPrompt {
  serverUrl: string;
  retry?: SpaceEnterEntry;
}

interface SpaceDirectoryState {
  /** 已加载的空间列表（按服务器地址分组；未加载过的服务器无键）。 */
  spacesByServer: Record<string, SpaceSummary[]>;
  /** 各服务器空间列表加载中标记。 */
  loadingByServer: Record<string, boolean>;
  /** 各服务器空间列表加载错误（用户可见，重试即再拉）。 */
  errorByServer: Record<string, string | undefined>;
  /** need-login 引导（null = 无）。 */
  loginPrompt: SpaceLoginPrompt | null;
  /** 当前加载过的成员名册（成员面板/成员管理弹窗共用；按 serverUrl#spaceId 缓存键校验）。 */
  members: MemberInfo[];
  membersKey: string | null;
  membersLoading: boolean;
  membersError: string | null;

  /** 拉取某服务器当前账号可见的空间列表。 */
  loadServerSpaces: (serverUrl: string) => Promise<void>;
  /** 置 need-login 引导（文件面板 need-login 分支与设置区共用）。 */
  requestLogin: (prompt: SpaceLoginPrompt) => void;
  /** 清除引导（登录成功/用户关闭时）。 */
  clearLoginPrompt: () => void;
  /** 在某服务器创建空间；`rootPath` 传入服务器上已有文件夹的绝对路径时就地收编（不搬移文件）。
   *  成功后刷新列表并返回新空间摘要。
   *  只回 `{ spaceId, name }`：创建接口不返回 role/ownerUserId/createdAt（列表刷新后由 spacesByServer 提供完整字段），
   *  消费方（新增入口创建后直接进空间）也只用 id/name——不合成占位字段冒充完整摘要。 */
  createSpace: (serverUrl: string, name: string, rootPath?: string) => Promise<{ spaceId: string; name: string }>;
  /** 用邀请码加入某服务器的空间；成功后刷新列表。 */
  acceptInvite: (serverUrl: string, code: string) => Promise<AcceptInviteResult>;
  /** 重命名空间（服务端 + 最近条目镜像同步）。 */
  renameSpace: (serverUrl: string, spaceId: string, name: string) => Promise<void>;
  /** 拉取空间成员名册。 */
  loadMembers: (serverUrl: string, spaceId: string) => Promise<void>;
  /** 移除成员。 */
  removeMember: (serverUrl: string, spaceId: string, userId: string) => Promise<void>;
  /** 转让 owner 给指定成员。 */
  transferOwnership: (serverUrl: string, spaceId: string, toUserId: string) => Promise<void>;
  /** 生成邀请码。 */
  createInvite: (serverUrl: string, spaceId: string, body: CreateInviteBody) => Promise<InviteInfo>;
  /** 撤销邀请码。 */
  revokeInvite: (serverUrl: string, spaceId: string, code: string) => Promise<void>;
  /** 断开空间：仅移除本机最近条目（global.json + appStore 镜像），不影响服务端。 */
  forgetSpace: (serverUrl: string, spaceId: string) => Promise<void>;
}

function client(serverUrl: string) {
  return createSpaceClient(serverUrl, () => getToken(serverUrl));
}

/** 把最近空间条目镜像同步进 appStore 并落盘 global.json（失败不阻塞主流程，只记日志）。 */
async function syncRecentSpaces(spaces: ReturnType<typeof useAppStore.getState>["recentSpaces"]): Promise<void> {
  useAppStore.setState({ recentSpaces: spaces });
  try {
    await updateGlobalConfig({ spaces });
  } catch (e) {
    console.error("更新最近空间列表失败", e);
  }
}

export const useSpaceDirectoryStore = create<SpaceDirectoryState>((set, get) => ({
  spacesByServer: {},
  loadingByServer: {},
  errorByServer: {},
  loginPrompt: null,
  members: [],
  membersKey: null,
  membersLoading: false,
  membersError: null,

  loadServerSpaces: async (serverUrl) => {
    set((s) => ({ loadingByServer: { ...s.loadingByServer, [serverUrl]: true } }));
    try {
      const spaces = await client(serverUrl).spaces.list();
      set((s) => ({
        spacesByServer: { ...s.spacesByServer, [serverUrl]: spaces },
        errorByServer: { ...s.errorByServer, [serverUrl]: undefined },
      }));
    } catch (e) {
      console.error("加载空间列表失败", e);
      set((s) => ({
        errorByServer: {
          ...s.errorByServer,
          [serverUrl]: e instanceof Error ? e.message : String(e),
        },
      }));
    } finally {
      set((s) => ({ loadingByServer: { ...s.loadingByServer, [serverUrl]: false } }));
    }
  },

  requestLogin: (prompt) => set({ loginPrompt: prompt }),
  clearLoginPrompt: () => set({ loginPrompt: null }),

  createSpace: async (serverUrl, name, rootPath) => {
    const created = await client(serverUrl).spaces.create(
      rootPath ? { name, path: rootPath } : { name },
    );
    await get().loadServerSpaces(serverUrl);
    return { spaceId: created.spaceId, name: created.name };
  },

  acceptInvite: async (serverUrl, code) => {
    const result = await client(serverUrl).spaces.acceptInvite({ code });
    await get().loadServerSpaces(serverUrl);
    return result;
  },

  renameSpace: async (serverUrl, spaceId, name) => {
    await client(serverUrl).spaces.rename(spaceId, { name });
    await get().loadServerSpaces(serverUrl);
    // 激活中的空间改名：同步 appStore 显示名与最近条目镜像
    const identity = useAppStore.getState().vaultIdentity;
    if (identity?.kind === "space" && identity.serverUrl === serverUrl && identity.spaceId === spaceId) {
      useAppStore.setState({ vaultName: name });
    }
    const spaces = useAppStore
      .getState()
      .recentSpaces.map((e) =>
        e.serverUrl === serverUrl && e.spaceId === spaceId ? { ...e, name } : e,
      );
    await syncRecentSpaces(spaces);
  },

  loadMembers: async (serverUrl, spaceId) => {
    const key = `${serverUrl}#${spaceId}`;
    set({ membersLoading: true, membersError: null });
    try {
      const members = await client(serverUrl).spaces.listMembers(spaceId);
      set({ members, membersKey: key, membersLoading: false });
    } catch (e) {
      console.error("加载成员名册失败", e);
      set({
        membersError: e instanceof Error ? e.message : String(e),
        membersLoading: false,
      });
    }
  },

  removeMember: async (serverUrl, spaceId, userId) => {
    await client(serverUrl).spaces.removeMember(spaceId, userId);
    await get().loadMembers(serverUrl, spaceId);
  },

  transferOwnership: async (serverUrl, spaceId, toUserId) => {
    await client(serverUrl).spaces.transfer(spaceId, { toUserId });
    await get().loadMembers(serverUrl, spaceId);
  },

  createInvite: (serverUrl, spaceId, body) => client(serverUrl).spaces.createInvite(spaceId, body),

  revokeInvite: (serverUrl, spaceId, code) => client(serverUrl).spaces.revokeInvite(spaceId, code),

  forgetSpace: async (serverUrl, spaceId) => {
    const spaces = useAppStore
      .getState()
      .recentSpaces.filter((e) => !(e.serverUrl === serverUrl && e.spaceId === spaceId));
    await syncRecentSpaces(spaces);
  },
}));
