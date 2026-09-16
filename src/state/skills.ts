import { invoke } from "@tauri-apps/api/core";
import { createSignal } from "solid-js";
import type { ChatItem, SkillMeta } from "~/lib/types";

// ── Skills（D12）：URL 安装 / 启停 / 删除，改完经 skills_reconnect 热注入 ──
export function useSkills(push: (item: ChatItem) => void) {
  const [skills, setSkills] = createSignal<SkillMeta[]>([]);
  const [skillUrl, setSkillUrl] = createSignal("");
  const [installingSkill, setInstallingSkill] = createSignal(false);

  // 仅刷新列表（不兜错）：openDrawer 组合调用，错误统一由 App 的 catch 处理
  const refreshList = async () => {
    setSkills(JSON.parse(await invoke<string>("skills_list")));
  };

  async function refreshSkills() {
    try {
      await refreshList();
    } catch (e) {
      push({ role: "status", text: `skills_list failed: ${e}` });
    }
  }

  async function installSkill(e: Event) {
    e.preventDefault();
    const url = skillUrl().trim();
    if (!url || installingSkill()) return;
    setInstallingSkill(true);
    try {
      const entry = JSON.parse(await invoke<string>("skills_install", { url }));
      await invoke("skills_reconnect");
      await refreshSkills();
      setSkillUrl("");
      push({
        role: "status",
        text: `skill installed: ${entry.id} (${entry.version})`,
      });
    } catch (err) {
      push({ role: "status", text: `skills_install failed: ${err}` });
    } finally {
      setInstallingSkill(false);
    }
  }

  async function toggleSkill(id: string, enabled: boolean) {
    try {
      await invoke("skills_toggle", { id, enabled });
      await invoke("skills_reconnect");
      await refreshSkills();
    } catch (e) {
      push({ role: "status", text: `skills_toggle failed: ${e}` });
    }
  }

  async function removeSkill(id: string) {
    try {
      await invoke("skills_remove", { id });
      await invoke("skills_reconnect");
      await refreshSkills();
      push({ role: "status", text: `skill removed: ${id}` });
    } catch (e) {
      push({ role: "status", text: `skills_remove failed: ${e}` });
    }
  }

  return {
    skills,
    skillUrl,
    setSkillUrl,
    installingSkill,
    refreshList,
    refreshSkills,
    installSkill,
    toggleSkill,
    removeSkill,
  };
}

export type SkillsState = ReturnType<typeof useSkills>;
