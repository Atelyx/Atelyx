/**
 * 协作空间登录/注册弹窗（设置「多人协作」区触发）：共用表单骨架，注册多可选昵称与密码确认。
 * need-login 引导存在时预填服务器地址；携带重试条目时提交成功即自动进入目标空间并清除引导。
 * 只调 spaceAuthStore / spaceDirectoryStore / appStore 的动作，不直调 service。
 */
import { useState } from "react";
import { X } from "lucide-react";
import {
  useSpaceDirectoryStore,
  type SpaceEnterEntry,
} from "@/stores/spaceDirectoryStore";
import { DialogFrame } from "@/components/common/DialogFrame";
import { Z_LAYERS } from "@/constants/zLayers";
import { Spinner } from "@/components/common/primitives";
import { Input } from "@/components/common/Input";
import { IconButton } from "@/components/common/Button";
import { useSpaceAuthStore } from "@/stores/spaceAuthStore";
import { useAppStore } from "@/stores/appStore";
import { useBackHandler } from "@/hooks/useBackHandler";

export type SpaceAuthMode = "login" | "register";

interface Props {
  mode: SpaceAuthMode;
  onClose: () => void;
}

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

  // 返回键先关登录弹窗
  useBackHandler(true, () => {
    onClose();
    return true;
  });

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
    <DialogFrame
      onClose={onClose}
      z={Z_LAYERS.dialog}
      panelClassName="w-[380px] max-w-[calc(100vw-2rem)]"
      ariaLabel={mode === "login" ? "登录协作服务器" : "注册协作空间账号"}
    >
        <header className="px-4 py-3 border-b flex items-center justify-between" style={{ borderColor: "var(--border)" }}>
          <h3 className="text-sm font-medium" style={{ color: "var(--text-primary)" }}>
            {mode === "login" ? "登录协作服务器" : "注册协作空间账号"}
          </h3>
          <IconButton icon={<X size={14} />} label="关闭" onClick={onClose} variant="subtle" size="sm" />
        </header>

        <div className="p-4 flex flex-col gap-2.5">
          {loginPrompt?.retry && (
            <div
              className="flex items-center gap-2 text-xs px-2 py-1.5 rounded"
              style={{ background: "var(--accent-soft)", color: "var(--text-primary)" }}
            >
              <span className="flex-1">登录后将继续打开协作空间 {loginPrompt.retry.name}</span>
              <IconButton
                icon={<X size={12} />}
                label="取消引导"
                onClick={clearLoginPrompt}
                variant="subtle"
                size="xs"
              />
            </div>
          )}
          <Input
            value={serverUrl}
            onChange={(e) => setServerUrl(e.target.value)}
            placeholder="服务器地址（如 http://192.168.1.10:11224）"
            aria-label="服务器地址"
            autoFocus
          />
          <Input
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            placeholder="用户名"
            aria-label="用户名"
          />
          <Input
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            placeholder="密码"
            aria-label="密码"
          />
          {mode === "register" && (
            <Input
              type="password"
              value={confirmPassword}
              onChange={(e) => setConfirmPassword(e.target.value)}
              placeholder="确认密码"
              aria-label="确认密码"
            />
          )}
          {mode === "register" && (
            <Input
              value={displayName}
              onChange={(e) => setDisplayName(e.target.value)}
              placeholder="昵称（可选）"
              aria-label="昵称"
            />
          )}
          {error && (
            <span className="text-xs" style={{ color: "var(--danger)" }}>
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
            {busy && <Spinner size={13} />}
          </div>
        </div>
    </DialogFrame>
  );
}
