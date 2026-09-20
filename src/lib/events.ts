// pi-agent-event 事件的 TypeScript 判别联合 —— 单一真源在 emit 侧：
// - Rust（src-tauri/src/）：approval_required / ask_user / preview_open，
//   经 lib.rs 的 `pi-agent-event` 通道转发；
// - bundle（pi-bundle/agent-qjs.js）：业务事件，经 guest 的 outbox → pi-agent-event
//   透传；
// - pi-agent-core（Agent.subscribe 透传）：agent_start/agent_end/turn_*/
//   message_*/tool_execution_*。
// 本文件的字段名/必填性以这些 emit 点为准，不按 App.tsx 的消费端倒推。

import type { ProviderInfo, ProviderModel, TodoTask } from "./types";

// ── 消息 content blocks（结构对齐 @earendil-works/pi-ai 的
// TextContent / ThinkingContent / ToolCall / ImageContent）──

export type TextContentBlock = {
  type: "text";
  text: string;
  textSignature?: string;
};

export type ThinkingContentBlock = {
  type: "thinking";
  thinking: string;
  thinkingSignature?: string;
  /** 被安全过滤 redact 时 thinking 为空，载荷在 thinkingSignature 里回传。 */
  redacted?: boolean;
};

export type ToolCallContentBlock = {
  type: "toolCall";
  id: string;
  name: string;
  arguments: Record<string, unknown>;
  thoughtSignature?: string;
  namespace?: string;
};

export type ImageContentBlock = {
  type: "image";
  data: string;
  mimeType: string;
};

/** assistant 消息 content 的全部可能块。 */
export type AssistantContentBlock =
  | TextContentBlock
  | ThinkingContentBlock
  | ToolCallContentBlock;
/** user / toolResult 消息 content 的可能块（无 thinking/toolCall）。 */
export type ContentBlock = TextContentBlock | ImageContentBlock;

// ── usage（pi-ai Usage 的 JSON 子集：UI 只读这几个字段）──

export type AgentUsage = {
  input: number;
  output: number;
  cacheRead?: number;
  cacheWrite?: number;
  totalTokens: number;
};

// ── agent_history 命令的消息数组元素（bundle __pi_history 回放，
// 与 pi-agent-core 的 AgentMessage / pi-ai Message 结构一致）──

export type AgentHistoryMessage =
  | {
      role: "user";
      content: string | ContentBlock[];
      timestamp?: number;
      usage?: AgentUsage;
    }
  | {
      role: "assistant";
      content: AssistantContentBlock[];
      api?: string;
      provider?: string;
      model?: string;
      usage?: AgentUsage;
      stopReason?: string;
      errorMessage?: string;
    }
  | {
      role: "toolResult";
      toolCallId: string;
      toolName: string;
      content: ContentBlock[];
      details?: unknown;
      usage?: AgentUsage;
      isError: boolean;
    };

/** agent_history 命令返回值（JSON 字符串 parse 后）。 */
export type AgentHistoryResponse = {
  sessionId: string | null;
  messages: AgentHistoryMessage[];
};

/** tool_execution_end 的 result 载荷：UI 只读 content 文本块。 */
export type ToolResultPayload = {
  content?: ContentBlock[];
};

// ── PiAgentEvent 判别联合 ──

type MessageEvent = {
  message: AgentHistoryMessage;
};

export type PiAgentEvent =
  // ── pi-agent-core 透传（bundle agent-qjs.js 的 subscribe → outbox → emit）──
  | { type: "agent_start" }
  | { type: "agent_end"; messages: AgentHistoryMessage[] }
  | { type: "turn_start" }
  | {
      type: "turn_end";
      message: AgentHistoryMessage;
      toolResults: AgentHistoryMessage[];
    }
  | ({ type: "message_start" } & MessageEvent)
  | ({ type: "message_update"; assistantMessageEvent?: unknown } & MessageEvent)
  | ({ type: "message_end" } & MessageEvent)
  | {
      type: "tool_execution_start";
      toolCallId: string;
      toolName: string;
      args: Record<string, unknown>;
    }
  | {
      type: "tool_execution_update";
      toolCallId: string;
      toolName: string;
      args: Record<string, unknown>;
      partialResult: unknown;
    }
  | {
      type: "tool_execution_end";
      toolCallId: string;
      toolName: string;
      /** AgentToolResult<any> 的 JSON 化，content 为文本块数组。 */
      result: unknown;
      isError: boolean;
    }
  // ── bundle 业务事件 ──
  | { type: "agent_ready"; tools: string[] }
  | { type: "agent_error"; error: string }
  | { type: "session_restored"; sessionId: string | null; messages: number }
  | { type: "session_created"; sessionId: string }
  | { type: "session_new" }
  | { type: "session_error"; error: string }
  | { type: "agents_md_loaded"; bytes: number }
  | { type: "skills_applied"; count: number }
  | { type: "mcp_connecting"; server: string }
  | { type: "mcp_ready"; server: string; tools: string[] }
  | { type: "mcp_error"; server: string; error: string }
  | { type: "mcp_tools_registered"; count: number }
  | { type: "oauth_open_url"; provider: string; url: string }
  | {
      type: "oauth_device_code";
      provider: string;
      /** 值为 undefined 时 JSON 序列化会丢字段，故 optional。 */
      userCode?: string;
      verificationUri?: string;
    }
  | {
      type: "oauth_progress";
      /** notify 回调里的 progress 事件不带 provider（login 流程内的才有）。 */
      provider?: string;
      message: string;
    }
  | { type: "oauth_done"; provider: string; error: string | null }
  | { type: "providers_listed"; providers: ProviderInfo[] }
  | { type: "providers_error"; error: string }
  | { type: "models_listed"; provider: string; models: ProviderModel[] }
  | { type: "models_error"; provider: string; error: string }
  | { type: "model_applied"; provider: string; modelId: string; name: string }
  | { type: "compaction_start"; tokens: number; messages: number }
  | { type: "compaction_done"; summarized: number; kept: number }
  | { type: "plan_drafting"; objective: string }
  | { type: "plan_drafted"; objective: string; content: string }
  | { type: "plan_error"; objective: string; error: string }
  | { type: "btw_thinking"; question: string }
  | { type: "btw_answer"; question: string; answer: string }
  | { type: "btw_error"; question: string; error: string }
  | { type: "subagent_start"; name: string; task: string }
  | { type: "subagent_end"; name: string }
  | { type: "todo_updated"; tasks: TodoTask[]; nextId: number }
  | { type: "goal_applied"; objective: string | null }
  | { type: "goal_auto_continue"; count: number; cap: number }
  | { type: "goal_auto_done" }
  | { type: "goal_error"; error: string }
  // ── Rust 事件（approval.rs / ask_user.rs / loopback.rs）──
  | {
      type: "approval_required";
      requestId: string;
      tool: string;
      path: string;
      diff: string;
      capabilities: string[];
      code: string;
      script: boolean;
    }
  | {
      type: "ask_user";
      requestId: string;
      question: string;
      context: string | null;
      options: { title: string; description?: string }[];
      allowMultiple: boolean;
      allowFreeform: boolean;
      allowComment: boolean;
    }
  | { type: "preview_open"; path: string; port: number }
  // ⚠️ 当前 emit 侧（Rust + bundle）均无发送点，仅 App.tsx 消费；保留以兼容
  // 旧版本宿主，boot 失败目前走 agent_init 命令的 Err 返回。
  | { type: "boot_error"; error: string };

/** 解析 pi-agent-event 的 JSON payload；非法 JSON / 非对象 / 无 type 返回 null。 */
export function parsePiEvent(raw: string): PiAgentEvent | null {
  try {
    const parsed: unknown = JSON.parse(raw);
    if (
      typeof parsed === "object" &&
      parsed !== null &&
      "type" in parsed &&
      typeof parsed.type === "string"
    ) {
      return parsed as PiAgentEvent;
    }
    return null;
  } catch {
    return null;
  }
}
