/**
 * 设置「多人协作」区的协作空间账号段：登录/注册入口按钮 + 已登录服务器与设备会话管理。
 * 表单在 SpaceAuthDialog 弹窗中呈现（登录与注册各自弹出）。
 *
 * need-login 引导：`spaceDirectoryStore.loginPrompt` 置入时显示引导条并自动弹出登录窗
 * （弹窗内预填服务器地址）；若携带重试条目，登录/注册成功后自动重试进入该协作空间。
 *
 * 分层：只调 spaceAuthStore / spaceDirectoryStore / appStore 的动作，不直调 service。
 */
import { useEffect, useState } from "react";
import { ChevronDown, ChevronRight, Loader2, LogOut, MonitorSmartphone, Server, X } from "lucide-react";
import { SettingCard } from "@/components/settings/SettingCard";
import { SpaceAuthDialog, type SpaceAuthMode } from "@/components/settings/SpaceAuthDialog";
import { useSpaceAuthStore } from "@/stores/spaceAuthStore";
import { useSpaceDirectoryStore } from "@/stores/spaceDirectoryStore";
import type { DeviceInfo } from "@/types";

/** 单个已登录服务器的设备会话列表（展开时拉取；踢下线后刷新）。 */
function DeviceList({ serverUrl }: { serverUrl: string }) {
  const [devices, setDevices] = useState<DeviceInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [revoking, setRevoking] = useState<string | null>(null);
  const listDevices = useSpaceAuthStore((s) => s.listDevices);
  const revokeDevice = useSpaceAuthStore((s) => s.revokeDevice);

  const refresh = () => {
    listDevices(serverUrl)
      .then(setDevices)
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
  };

  // 首次展开即拉取（存 null = 未加载；加载/失败后不再自动重拉）。
  // 副作用经 useEffect：渲染体内直接调 refresh() 会在并发渲染下重复发起请求
  useEffect(() => {
    if (devices === null && error === null) refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- 仅在首次展开（未加载且无错误）时拉取一次
  }, [serverUrl]);

  const revoke = (sessionId: string) => {
    setRevoking(sessionId);
    revokeDevice(serverUrl, sessionId)
      .then(refresh)
      .catch((e) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setRevoking(null));
  };

  return (
    <div className="mt-1.5 ml-4 space-y-1 border-l pl-3" style={{ borderColor: "var(--border)" }}>
      {error && (
        <div className="text-xs" style={{ color: "#f87171" }}>
          {error}
        </div>
      )}
      {devices?.length === 0 && (
        <div className="text-xs" style={{ color: "var(--text-muted)" }}>
          无设备会话
        </div>
      )}
      {devices?.map((d) => (
        <div key={d.id} className="flex items-center gap-2 text-xs">
          <MonitorSmartphone size={12} style={{ color: "var(--text-muted)" }} />
          <span className="truncate" style={{ color: "var(--text-primary)" }}>
            {d.deviceName}
            {d.current && <span className="ml-1" style={{ color: "var(--text-muted)" }}>（本机）</span>}
          </span>
          <button
            onClick={() => revoke(d.id)}
            disabled={revoking !== null}
            title="吊销该设备会话（下次使用需重新登录）"
            className="ml-auto flex-shrink-0 px-1.5 py-0.5 rounded hover:bg-[var(--hover)] disabled:opacity-40"
            style={{ color: "#f87171" }}
          >
            踢下线
          </button>
        </div>
      ))}
    </div>
  );
}

/** 已登录服务器列表（登出 + 设备会话管理）。 */
function ServerList() {
  const servers = useSpaceAuthStore((s) => s.servers);
  const logout = useSpaceAuthStore((s) => s.logout);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [loggingOut, setLoggingOut] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const toggleExpand = (serverUrl: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(serverUrl)) next.delete(serverUrl);
      else next.add(serverUrl);
      return next;
    });
  };

  const doLogout = async (serverUrl: string) => {
    setLoggingOut(serverUrl);
    setError(null);
    try {
      await logout(serverUrl);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoggingOut(null);
    }
  };

  if (servers.length === 0) return null;

  return (
    <div className="flex flex-col gap-1.5 w-[340px]">
      {error && (
        <span className="text-xs" style={{ color: "#f87171" }}>
          {error}
        </span>
      )}
      {servers.map((s) => (
        <div key={s.serverUrl} className="flex flex-col">
          <div className="flex items-center gap-2 text-xs">
            <button
              onClick={() => toggleExpand(s.serverUrl)}
              title="设备会话"
              style={{ color: "var(--text-muted)" }}
            >
              {expanded.has(s.serverUrl) ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
            </button>
            <Server size={12} style={{ color: "var(--text-muted)" }} />
            <span className="truncate flex-1" style={{ color: "var(--text-primary)" }} title={s.serverUrl}>
              {s.serverUrl}
            </span>
            <span className="flex-shrink-0" style={{ color: "var(--text-muted)" }}>
              {s.displayName || s.username}
            </span>
            <button
              onClick={() => void doLogout(s.serverUrl)}
              disabled={loggingOut !== null}
              title="退出该服务器登录"
              className="flex items-center gap-1 flex-shrink-0 px-1.5 py-0.5 rounded hover:bg-[var(--hover)] disabled:opacity-40"
              style={{ color: "#f87171" }}
            >
              {loggingOut === s.serverUrl ? (
                <Loader2 size={11} className="animate-spin" />
              ) : (
                <LogOut size={11} />
              )}
              登出
            </button>
          </div>
          {expanded.has(s.serverUrl) && <DeviceList serverUrl={s.serverUrl} />}
        </div>
      ))}
    </div>
  );
}

/** 协作空间账号段（设置「多人协作」区挂载）。 */
export function SpaceAccountSection() {
  const busy = useSpaceAuthStore((s) => s.busy);
  const loginPrompt = useSpaceDirectoryStore((s) => s.loginPrompt);
  const clearLoginPrompt = useSpaceDirectoryStore((s) => s.clearLoginPrompt);
  const [dialogMode, setDialogMode] = useState<SpaceAuthMode | null>(null);

  // need-login 引导出现时自动弹出登录窗（预填地址 + 重试条目在弹窗内呈现）；
  // 用户关闭弹窗后引导条仍在，可再点「登录」重开（依赖未变不重复触发）。
  useEffect(() => {
    if (loginPrompt) setDialogMode((cur) => cur ?? "login");
  }, [loginPrompt]);

  return (
    <SettingCard title="协作空间" description="登录协作服务器后可加入多人协作空间">
      <div className="flex flex-col gap-3">
        {loginPrompt && (
          <div
            className="flex items-center gap-2 text-xs px-2 py-1.5 rounded w-[340px]"
            style={{ background: "color-mix(in srgb, var(--accent) 12%, transparent)", color: "var(--text-primary)" }}
          >
            <span className="flex-1">登录后将继续打开协作空间 {loginPrompt.retry?.name ?? ""}</span>
            <button onClick={clearLoginPrompt} title="取消引导" style={{ color: "var(--text-muted)" }}>
              <X size={12} />
            </button>
          </div>
        )}
        <div className="flex items-center gap-2">
          <button
            onClick={() => setDialogMode("register")}
            disabled={busy}
            className="px-2.5 py-1 text-xs rounded border hover:opacity-80 disabled:opacity-50"
            style={{ borderColor: "var(--border)", color: "var(--text-secondary)" }}
          >
            注册
          </button>
          <button
            onClick={() => setDialogMode("login")}
            disabled={busy}
            className="px-2.5 py-1 text-xs rounded hover:opacity-80 disabled:opacity-50"
            style={{ background: "var(--accent)", color: "var(--accent-fg)" }}
          >
            登录
          </button>
          {busy && <Loader2 size={13} className="animate-spin" style={{ color: "var(--text-muted)" }} />}
        </div>
        <ServerList />
      </div>
      {dialogMode !== null && <SpaceAuthDialog mode={dialogMode} onClose={() => setDialogMode(null)} />}
    </SettingCard>
  );
}
