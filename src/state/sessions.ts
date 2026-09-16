import { invoke } from "@tauri-apps/api/core";
import { createMemo, createSignal } from "solid-js";
import type { ChatItem, SessionMeta } from "~/lib/types";

interface SessionsDeps {
  closeDrawer: () => void;
  loadHistory: () => Promise<void>;
  /** 换会话/删会话后重置聊天流与 token 计数（resetTokens 仅新会话为 true） */
  resetChat: (status: string, resetTokens: boolean) => void;
}

export function useSessions(
  push: (item: ChatItem) => void,
  deps: SessionsDeps,
) {
  const [sessions, setSessions] = createSignal<SessionMeta[]>([]);
  const [currentSession, setCurrentSession] = createSignal<string | null>(null);
  const [sessionSearch, setSessionSearch] = createSignal("");

  // 仅刷新列表（不兜错）：openDrawer 组合调用，错误统一由 App 的 catch 处理
  const refreshList = async () => {
    setSessions(JSON.parse(await invoke<string>("session_list")));
  };

  // 会话分组（Today / Yesterday / Earlier）+ 搜索过滤（id / 最后消息文本）
  const sessionGroups = createMemo(() => {
    const q = sessionSearch().trim().toLowerCase();
    const list = sessions().filter(
      (s) =>
        !q ||
        s.id.toLowerCase().includes(q) ||
        (s.lastMessage ?? "").toLowerCase().includes(q),
    );
    const dayStart = new Date();
    dayStart.setHours(0, 0, 0, 0);
    const yesterday = dayStart.getTime() - 86_400_000;
    const groups: { label: string; items: SessionMeta[] }[] = [
      { label: "Today", items: [] },
      { label: "Yesterday", items: [] },
      { label: "Earlier", items: [] },
    ];
    for (const s of list) {
      if (s.modifiedAt >= dayStart.getTime()) groups[0].items.push(s);
      else if (s.modifiedAt >= yesterday) groups[1].items.push(s);
      else groups[2].items.push(s);
    }
    return groups.filter((g) => g.items.length > 0);
  });

  async function switchSession(id: string) {
    deps.closeDrawer();
    try {
      await invoke("session_open", { id });
      await deps.loadHistory();
    } catch (e) {
      push({ role: "status", text: `session switch failed: ${e}` });
    }
  }

  async function newSession() {
    deps.closeDrawer();
    try {
      await invoke("session_new");
      setCurrentSession(null);
      deps.resetChat("new session started", true);
    } catch (e) {
      push({ role: "status", text: `session_new failed: ${e}` });
    }
  }

  async function deleteSession(id: string) {
    try {
      // 当前会话先切空白：避免运行中 repo 的追加写把已删文件重建为无 header 孤儿
      if (id === currentSession()) {
        await invoke("session_new");
        setCurrentSession(null);
        deps.resetChat("session deleted — new session started", false);
      } else {
        push({ role: "status", text: "session deleted" });
      }
      await invoke("session_delete", { id });
      setSessions((prev) => prev.filter((s) => s.id !== id));
    } catch (e) {
      push({ role: "status", text: `session delete failed: ${e}` });
    }
  }

  return {
    sessions,
    setSessions,
    currentSession,
    setCurrentSession,
    sessionSearch,
    setSessionSearch,
    refreshList,
    sessionGroups,
    switchSession,
    newSession,
    deleteSession,
  };
}

export type SessionsState = ReturnType<typeof useSessions>;
