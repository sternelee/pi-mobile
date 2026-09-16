import { invoke } from "@tauri-apps/api/core";
import { BsOpenai } from "solid-icons/bs";
import { FiCpu } from "solid-icons/fi";
import { SiAnthropic, SiGooglegemini, SiOpenrouter, SiX } from "solid-icons/si";
import { createSignal } from "solid-js";
import type { ProviderInfo } from "~/lib/types";

// OAuth 订阅型 provider（bundle 侧 __pi_oauth_login 支持登录）
export const OAUTH_PROVIDERS = new Set([
  "anthropic",
  "openai-codex",
  "kimi-coding",
  "xai",
  "openrouter",
]);
// 图标随文字色/字号（solid-icons 默认 1em + currentColor）
export const providerIcon = (id: string) =>
  id === "openai" || id === "openai-codex" ? (
    <BsOpenai size="1.1em" />
  ) : id === "openrouter" ? (
    <SiOpenrouter size="1.1em" />
  ) : id === "anthropic" ? (
    <SiAnthropic size="1.1em" />
  ) : id === "xai" ? (
    <SiX size="1.1em" />
  ) : id === "google-gemini" ? (
    <SiGooglegemini size="1.1em" />
  ) : (
    <FiCpu size="1.1em" />
  );

export const UI_PROVIDERS: ProviderInfo[] = [
  { id: "openai", name: "OpenAI", models: [] },
  { id: "openrouter", name: "OpenRouter", models: [] },
  { id: "deepseek", name: "DeepSeek", models: [] },
  { id: "google-gemini", name: "Google Gemini", models: [] },
  { id: "anthropic", name: "Anthropic (Claude Pro/Max)", models: [] },
  { id: "openai-codex", name: "OpenAI Codex (ChatGPT)", models: [] },
  { id: "kimi-coding", name: "Kimi For Coding", models: [] },
  { id: "xai", name: "xAI (SuperGrok/X Premium)", models: [] },
];

export const SUGGESTIONS = [
  "List my workspace files",
  "Create hello.py that prints a greeting",
  "What can you do?",
];

export const COMMANDS = [
  { cmd: "/plan", desc: "draft an implementation plan" },
  { cmd: "/btw", desc: "quick side question (context-aware)" },
  { cmd: "/goal", desc: "set a persistent objective (/goal off clears)" },
  { cmd: "/todos", desc: "show/hide the agent's task list panel" },
];

// 技能自定义指令（/commit-it 等）：skills_applied 事件后从 bundle 回读。
// 面板合并展示；handleCommand 对匹配的未知命令透传 agent_prompt（bundle 展开）。
export const [skillCmds, setSkillCmds] = createSignal<
  { cmd: string; desc: string }[]
>([]);
export const allCommands = () => [...COMMANDS, ...skillCmds()];
export const refreshSkillCmds = () => {
  invoke<string>("pi_call_global", { fnName: "__pi_commands", arg: "" })
    .then((r) =>
      setSkillCmds(
        JSON.parse(r).map((c: any) => ({
          cmd: c.cmd,
          desc: c.description ?? c.name,
        })),
      ),
    )
    .catch(() => setSkillCmds([]));
};
