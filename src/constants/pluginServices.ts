/**
 * 插件面服务展示元数据：服务名 → 标签 + 敏感标记。
 * 管理页「披露 vs 实际」审计对照展示用（declares 声明侧与审计实际侧共用同一词汇表）。
 */
export const PLUGIN_SERVICE_LABELS: Record<string, string> = {
  state: "插件自持状态",
  storage: "插件键值存储",
  http: "联网请求",
  notification: "应用内通知",
  app: "宿主信息",
  shell: "执行外部程序",
  vault: "仓库文件读写",
  dialog: "系统对话框",
  clipboard: "剪贴板读写",
  window: "窗口控制",
  ai: "AI 会话与工具",
  collab: "协作在线状态",
  canvas: "画布数据",
  table: "表格数据",
  note: "当前笔记读写",
  chat: "AI 会话面板",
  history: "领域历史读与回滚",
  layout: "工作区布局读与操作",
  uiState: "应用级 UI 状态读",
  slots: "插件 UI 注册",
};

/** 纳入审计/披露的服务面名单（唯一清单：审计侧据此判定「访问了哪个服务」，与展示标签同源）。 */
export const PLUGIN_SERVICE_NAMES: readonly string[] = Object.keys(PLUGIN_SERVICE_LABELS);

/** 敏感服务（审计/披露 UI「敏感」高亮）。 */
export const PLUGIN_SERVICE_SENSITIVE: ReadonlySet<string> = new Set(["shell", "clipboard", "http"]);
