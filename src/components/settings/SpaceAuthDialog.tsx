/**
 * 协作空间登录/注册弹窗（设置「多人协作」区按钮触发）：
 * 登录与注册共用一套表单骨架，注册多一项可选昵称与密码确认。
 * need-login 引导（spaceDirectoryStore.loginPrompt）存在时预填服务器地址；
 * 携带重试条目时提示登录后自动进入目标空间，提交成功即清除引导并关闭弹窗。
 * 分层：只调 spaceAuthStore / spaceDirectoryStore / appStore 的动作，不直调 service。
 */
import { useState } from "react";
import { Loader2, X } from "lucide-react";
import {
  useSpaceDirectoryStore,
  type SpaceEnterEntry,
} from "@/stores/spaceDirectoryStore";
import { useSpaceAuthStore } from "@/stores/spaceAuthStore";
import { useAppStore } from "@/stores/appStore";

export type SpaceAuthMode = "login" | "register";

interface Props {
  mode: SpaceAuthMode;
  onClose: () => void;
}

const inputStyle = {
  color: "var(--text-primary)",
  background: "var(--input-bg)",
  border: "1px solid var(--input-border)",
} as const;

export function SpaceAuthDialog({ mode, onClose }: Props) {
  const busy = useSpaceAuthStore((s) => s.busy);
  const loginPrompt = useSpaceDirectoryStore((s) => s.loginPrompt);
  const clearLoginPrompt = useSpaceDirectoryStore((s) => s.clearLoginPrompt);

  const [serverUrl, setServerUrl] = useState(loginPrompt?.serverUrl ?? "");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [error, setError] = useState<string | null>(null);

  const submit = async () => {
    const addr = serverUrl.trim();
    setError(null);
    if (!addr || !username.trim() || !password) {
      setError("服务器地址、用户名与密码不能为空");
      return;
    }
    if (mode === "register" && password !== confirmPassword) {
      setError("两次输入的密码不一致");
      return;
    }
    try {
      const store = useSpaceAuthStore.getState();
      const entry: SpaceEnterEntry | undefined = loginPrompt?.retry;
      const name = displayName.trim() || undefined;
      if (mode === "login") {
        await store.login(addr, username.trim(), password, undefined, name);
      } else {
        await store.register(addr, username.trim(), password, undefined, name);
      }
      clearLoginPrompt();
      if (entry) {
        void useAppStore.getState().selectSpace(entry);
      }
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <div
      className="fixed inset-0 z-[200] flex items-center justify-center"
      style={{ background: "rgba(0,0,0,0.5)" }}
      onClick={onClose}
    >
      <div
        role="dialog"
        aria-label={mode === "login" ? "登录协作服务器" : "注册协作空间账号"}
        className="w-[380px] rounded-lg border shadow-xl"
        style={{ background: "var(--bg-secondary)", borderColor: "var(--border)" }}
        onClick={(e) => e.stopPropagation()}
      >
        <header className="px-4 py-3 border-b flex items-center justify-between" style={{ borderColor: "var(--border)" }}>
          <h3 className="text-sm font-medium" style={{ color: "var(--text-primary)" }}>
            {mode === "login" ? "登录协作服务器" : "注册协作空间账号"}
          </h3>
          <button onClick={onClose} style={{ color: "var(--text-muted)" }} className="hover:opacity-80">
            <X size={14} />
          </button>
        </header>

        <div className="p-4 flex flex-col gap-2.5">
          {loginPrompt?.retry && (
            <div
              className="flex items-center gap-2 text-xs px-2 py-1.5 rounded"
              style={{ background: "color-mix(in srgb, var(--accent) 12%, transparent)", color: "var(--text-primary)" }}
            >
              <span className="flex-1">登录后将继续打开协作空间 {loginPrompt.retry.name}</span>
              <button onClick={clearLoginPrompt} title="取消引导" style={{ color: "var(--text-muted)" }}>
                <X size={12} />
              </button>
            </div>
          )}
          <input
            value={serverUrl}
            onChange={(e) => setServerUrl(e.target.value)}
            placeholder="服务器地址（如 http://192.168.1.10:11224）"
            aria-label="服务器地址"
            autoFocus
            className="text-sm rounded px-2 py-1 outline-none"
            style={inputStyle}
          />
          <input
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            placeholder="用户名"
            aria-label="用户名"
            className="text-sm rounded px-2 py-1 outline-none"
            style={inputStyle}
          />
          <input
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            placeholder="密码"
            aria-label="密码"
            className="text-sm rounded px-2 py-1 outline-none"
            style={inputStyle}
          />
          {mode === "register" && (
            <input
              type="password"
              value={confirmPassword}
              onChange={(e) => setConfirmPassword(e.target.value)}
              placeholder="确认密码"
              aria-label="确认密码"
              className="text-sm rounded px-2 py-1 outline-none"
              style={inputStyle}
            />
          )}
          {mode === "register" && (
            <input
              value={displayName}
              onChange={(e) => setDisplayName(e.target.value)}
              placeholder="昵称（可选）"
              aria-label="昵称"
              className="text-sm rounded px-2 py-1 outline-none"
              style={inputStyle}
            />
          )}
          {error && (
            <span className="text-xs" style={{ color: "#f87171" }}>
              {error}
            </span>
          )}
          <div className="flex items-center gap-2 pt-1">
            <button
              onClick={() => void submit()}
              disabled={busy}
              className="px-3 py-1.5 text-xs rounded hover:opacity-80 disabled:opacity-50"
              style={{ background: "var(--accent)", color: "var(--accent-fg)" }}
            >
              {mode === "login" ? "登录" : "注册"}
            </button>
            <button
              onClick={onClose}
              disabled={busy}
              className="px-3 py-1.5 text-xs rounded border hover:opacity-80 disabled:opacity-50"
              style={{ borderColor: "var(--border)", color: "var(--text-secondary)" }}
            >
              取消
            </button>
            {busy && <Loader2 size={13} className="animate-spin" style={{ color: "var(--text-muted)" }} />}
          </div>
        </div>
      </div>
    </div>
  );
}
