import { invoke } from "@tauri-apps/api/core";
import { createSignal } from "solid-js";
import type { ChatItem, McpServer } from "~/lib/types";

export function useMcp(push: (item: ChatItem) => void) {
  const [mcpServers, setMcpServers] = createSignal<McpServer[]>([]);
  const [mcpName, setMcpName] = createSignal("");
  const [mcpUrl, setMcpUrl] = createSignal("");
  const [mcpTimeout, setMcpTimeout] = createSignal("");
  const [mcpHeaders, setMcpHeaders] = createSignal("");
  const [mcpPasteOpen, setMcpPasteOpen] = createSignal(false);
  const [mcpPaste, setMcpPaste] = createSignal("");
  // MCP 服务器连接状态（mcp_ready / mcp_error 事件驱动）
  const [mcpReady, setMcpReady] = createSignal<Set<string>>(new Set());

  // 仅刷新列表（不兜错）：openDrawer 组合调用，错误统一由 App 的 catch 处理
  const refreshList = async () => {
    setMcpServers(JSON.parse(await invoke<string>("mcp_list")));
  };

  // 粘贴 JSON 批量导入：支持单服务器对象、数组、以及 {"mcpServers": {...}} 形态
  async function importMcpJson() {
    let parsed: any;
    try {
      parsed = JSON.parse(mcpPaste());
    } catch (e) {
      push({ role: "status", text: "paste JSON parse failed" });
      return;
    }
    let entries: any[] = [];
    if (Array.isArray(parsed)) entries = parsed;
    else if (parsed?.mcpServers && typeof parsed.mcpServers === "object") {
      entries = Object.entries(parsed.mcpServers).map(
        ([name, v]: [string, any]) => ({
          name,
          ...(typeof v === "string" ? { url: v } : v),
        }),
      );
    } else if (parsed?.name && parsed?.url) entries = [parsed];
    if (!entries.length) {
      push({ role: "status", text: "no servers found in pasted JSON" });
      return;
    }
    let added = 0;
    for (const e of entries) {
      if (!e?.name || !e?.url) continue;
      try {
        await invoke("mcp_add", {
          name: String(e.name),
          url: String(e.url),
          timeoutMs: e.timeoutMs ?? undefined,
          headers: e.headers ?? undefined,
        });
        added++;
      } catch (err) {
        push({ role: "status", text: `${e.name}: ${err}` });
      }
    }
    await refreshList();
    setMcpPaste("");
    setMcpPasteOpen(false);
    if (added) {
      push({
        role: "status",
        text: `imported ${added} server(s) — reconnecting…`,
      });
      await invoke("mcp_reconnect");
    }
  }

  async function addMcpServer(e: Event) {
    e.preventDefault();
    if (!mcpName().trim() || !mcpUrl().trim()) return;
    // 请求头：每行 "Key: Value"，解析为 JSON 对象
    const headers: Record<string, string> = {};
    for (const line of mcpHeaders().split("\n")) {
      const idx = line.indexOf(":");
      if (idx === -1) continue;
      const key = line.slice(0, idx).trim();
      const val = line.slice(idx + 1).trim();
      if (key && val) headers[key] = val;
    }
    const timeoutMs = Number(mcpTimeout()) || 30_000;
    try {
      await invoke("mcp_add", {
        name: mcpName().trim(),
        url: mcpUrl().trim(),
        timeoutMs,
        headers,
      });
      await refreshList();
      setMcpName("");
      setMcpUrl("");
      setMcpTimeout("");
      setMcpHeaders("");
      push({ role: "status", text: `mcp server saved — reconnecting…` });
      await invoke("mcp_reconnect");
    } catch (e) {
      push({ role: "status", text: `mcp_add failed: ${e}` });
    }
  }

  async function reconnectMcp() {
    push({ role: "status", text: "reconnecting mcp servers…" });
    try {
      await invoke("mcp_reconnect");
    } catch (e) {
      push({ role: "status", text: `mcp_reconnect failed: ${e}` });
    }
  }

  async function removeMcpServer(name: string) {
    try {
      await invoke("mcp_remove", { name });
      setMcpReady((prev) => {
        const next = new Set(prev);
        next.delete(name);
        return next;
      });
      await refreshList();
    } catch (e) {
      push({ role: "status", text: `mcp_remove failed: ${e}` });
    }
  }

  return {
    mcpServers,
    mcpName,
    setMcpName,
    mcpUrl,
    setMcpUrl,
    mcpTimeout,
    setMcpTimeout,
    mcpHeaders,
    setMcpHeaders,
    mcpPasteOpen,
    setMcpPasteOpen,
    mcpPaste,
    setMcpPaste,
    mcpReady,
    setMcpReady,
    refreshList,
    importMcpJson,
    addMcpServer,
    reconnectMcp,
    removeMcpServer,
  };
}

export type McpState = ReturnType<typeof useMcp>;
