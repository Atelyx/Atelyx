/**
 * Agent 配置类型（仓库级，落盘 `.atelyx/agents.json`）。
 * Agent = 可复用的对话预设：名称 + 系统提示词（引用提示词笔记）+ 工具能力，对话节点/面板经
 * `agentId` 实时解析引用。模型/推理等级不进 Agent，由节点/面板头部独立选择。
 */
export interface AgentConfig {
  /** 稳定 id（uuid；节点/会话按它引用；预置 Agent 用固定 id）。 */
  id: string;
  /** 显示名（Agent 下拉 / 配置列表展示）。 */
  name: string;
  /**
   * 引用已注册提示词笔记（相对仓库根 `.md` 路径，右键笔记「注册为提示词」的候选）。
   * 发送时实时读正文注入，外部编辑即时生效；未设置 = 不带系统提示词。
   */
  systemPromptFile?: string;
  /** 启用的工具 id 列表（constants/tools.ts 的 AGENT_TOOLS_META；全部工具按勾选生效，空数组 = 纯对话、无任何工具）。 */
  tools: string[];
  /**
   * 预置标记（缺省 = 用户自建）：预置 Agent（默认随仓库出现）不可删除；
   * 复制得到的副本不继承该标记（普通用户 Agent，可删除）。
   */
  builtin?: boolean;
}
