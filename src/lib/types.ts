export type ChatItem = {
  role: "user" | "assistant" | "tool" | "status";
  text: string;
  toolCallId?: string;
  toolName?: string;
  argsText?: string;
  pending?: boolean;
  isError?: boolean;
  expanded?: boolean;
  thinking?: boolean;
  path?: string;
  canRevert?: boolean;
  reverted?: boolean;
  /** 工具执行耗时（ms）：tool_execution_start/end 在 App 侧本地计时 */
  durationMs?: number;
  /** 流式输出中（message_start/update 置 true，message_end 置 false） */
  streaming?: boolean;
};

export type SessionMeta = {
  id: string;
  createdAt: number;
  modifiedAt: number;
  cwd: string;
  entries: number;
  size: number;
  lastMessage?: string | null;
};

export type Approval = {
  requestId: string;
  tool: string;
  path: string;
  diff: string;
  /** D14 脚本执行：用户要批准的就是这份能力清单（id）。人话说明从 Rust 的
   *  script_capabilities 取，不在 TS 另抄一份——审批卡的意义是「所见即所授」。 */
  capabilities?: string[];
  /** 脚本源码：审一个看不见的脚本没意义，用户批的就是这段代码。 */
  code?: string;
  /** 显式脚本标志：不在 TS 里硬编码工具名「run_js」（那是把 Rust 的
   *  SCRIPT_TOOLS 另抄一份）。 */
  script?: boolean;
};

export type AskOption = { title: string; description?: string };

export type AskRequest = {
  requestId: string;
  question: string;
  context?: string | null;
  options: AskOption[];
  allowMultiple: boolean;
  allowFreeform: boolean;
  allowComment: boolean;
  selected: string[];
  freeform: string;
  comment: string;
};

export type TodoTask = {
  id: number;
  subject: string;
  description?: string;
  activeForm?: string;
  status: "pending" | "in_progress" | "completed" | "deleted";
  blockedBy?: number[];
  owner?: string;
};

export type SkillMeta = {
  id: string;
  name: string;
  description: string;
  source: string;
  version: string;
  checksum: string;
  enabled: boolean;
  installedAt: number;
};

// AI provider / model（pi-ai createModels 目录，bundle 侧解析完整模型对象）
export type ProviderModel = { id: string; name: string };
export type ProviderInfo = {
  id: string;
  name: string;
  models: ProviderModel[];
};
export type CurrentModel = { provider: string; id: string; name: string };

// M6 系统能力：能力清单 + 权限态（Rust 侧 native::status 为单一真源）
export type NativeCapability = {
  id: string;
  title: string;
  detail: string;
  tools: string[];
  needsPermission: boolean;
  permission: "granted" | "denied" | "prompt" | "unknown";
  supported: boolean;
};

export type McpServer = {
  name: string;
  url: string;
  timeoutMs?: number;
  headers?: Record<string, string>;
};

export type SettingsView =
  | "providers"
  | "provider"
  | "mcp"
  | "skills"
  | "agent";
