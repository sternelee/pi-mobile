import type { ChatItem } from "~/lib/types";

export function fmtTok(n: number): string {
  if (n < 1000) return `${n} tok`;
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)}k tok`;
  return `${(n / 1_000_000).toFixed(2)}M tok`;
}

export function fmtRel(ms: number): string {
  const mins = Math.floor((Date.now() - ms) / 60000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

export const copyText = (text: string) => {
  navigator.clipboard?.writeText(text).catch(() => {});
};

export const toolState = (it: ChatItem) =>
  it.pending ? "pending" : it.isError ? "error" : "ok";

// 工具耗时：移动端工具普遍 <1s，用 ms 更直观；≥1s 换成秒（参考图纯 ms
// 在长耗时下可读性差，故按阈值混合格式）
export const fmtDur = (ms: number) =>
  ms < 1000 ? `${Math.round(ms)} ms` : `${(ms / 1000).toFixed(1)} s`;

export const prettyArgs = (raw?: string) => {
  if (!raw) return "";
  try {
    return JSON.stringify(JSON.parse(raw), null, 2);
  } catch {
    return raw;
  }
};
