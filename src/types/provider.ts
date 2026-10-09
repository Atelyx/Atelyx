/** 供应商下的单个模型：id = API 请求用的模型名；nickname = 可选显示昵称（缺省 = id）。 */
interface ProviderModel {
  id: string;
  nickname?: string;
}

/** 模型思考档位（思考型模型须显式开启并在线上指名档位，默认不指定跟随模型）。 */
export type ReasoningEffort = "off" | "low" | "medium" | "high";

export interface ProviderConfig {
  id: string;
  name: string;
  baseUrl: string;
  apiKey: string;
  models: ProviderModel[];
}

export interface AiConfig {
  providers: ProviderConfig[];
}

/** 对话请求的目标解析结果（画布对话节点 / AI 对话面板共用，settingsStore.resolveChatTarget 产出）。
 * reasoningEffort 不在解析结果内——为调用方各自的会话级覆盖（节点 data / 面板 effortOverride），解析只负责 provider/model。 */
export type ChatTargetResult =
  | { ok: true; provider: ProviderConfig; model: string }
  | {
      ok: false;
      /** 失败原因：所选供应商已删 / 未配置默认模型 */
      reason: "provider-missing" | "no-model";
      error: string;
    };

/**
 * AI 供应商的**磁盘格式**（存 `.atelyx/config.json` 的 `VaultConfig.providers`）。
 * 默认不含 apiKey（key 走 keychain，条目按仓库隔离）；仅当仓库开启 `syncKeys`
 * （「API key 随仓库保存」，多设备同步）时写入并读取 apiKey 字段。
 * 运行时含 key 的 `ProviderConfig` 由 `settingsStore` 填充 apiKey 得到。
 */
export interface GlobalProvider {
  id: string;
  name: string;
  baseUrl: string;
  models?: ProviderModel[];
  /** 仅 syncKeys 开启时随仓库落盘/读取；关闭时剥离不写。 */
  apiKey?: string;
}
