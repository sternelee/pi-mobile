import { invoke } from "@tauri-apps/api/core";
import { createSignal } from "solid-js";
import { UI_PROVIDERS } from "~/lib/providers";
import type {
  ChatItem,
  CurrentModel,
  ProviderInfo,
  ProviderModel,
} from "~/lib/types";

// ── AI provider / model 选择（pi-ai createModels 目录驱动）──
// 静态清单先渲染（产品定死 4 家）；runtime 的 providers_listed 事件带
// 完整目录（含模型列表）后覆盖。OpenRouter 为动态目录，首次刷新才拉全量。
export function useProviders(push: (item: ChatItem) => void) {
  const [providers, setProviders] = createSignal<ProviderInfo[]>(UI_PROVIDERS);
  const [selProvider, setSelProvider] = createSignal("");
  const [providerKey, setProviderKey] = createSignal("");
  const [keySaved, setKeySaved] = createSignal(false);
  const [loadingModels, setLoadingModels] = createSignal(false);
  const [currentModel, setCurrentModel] = createSignal<CurrentModel | null>(
    null,
  );
  // 模型快选面板（composer 上方快捷条拉起）：当前 provider 的模型列表
  const [modelPickerOpen, setModelPickerOpen] = createSignal(false);
  // 已配置 key 的 provider 集合（LobeHub 式 provider 卡片状态标识）
  const [configured, setConfigured] = createSignal<Set<string>>(new Set());

  const providerModels = () =>
    providers().find((p) => p.id === selProvider())?.models ?? [];
  const providerLabel = (id: string) =>
    providers().find((p) => p.id === id)?.name ?? id;

  // 模型快选：优先当前生效模型的 provider，未选择时用抽屉里选中的 provider
  const pickerProvider = () =>
    currentModel()?.provider || selProvider() || "openai";
  const pickerModels = () =>
    providers().find((p) => p.id === pickerProvider())?.models ?? [];
  const openModelPicker = () => {
    setModelPickerOpen(true);
    if (pickerModels().length === 0) void loadModels(pickerProvider());
  };

  const refreshConfigured = async () => {
    const results = await Promise.all(
      UI_PROVIDERS.map((p) =>
        invoke<boolean>("has_creds", { provider: p.id }).catch(() => false),
      ),
    );
    setConfigured(
      new Set(UI_PROVIDERS.filter((_, i) => results[i]).map((p) => p.id)),
    );
  };

  // ── AI provider / model 选择流程 ──
  // 目录来自 bundle 的 providers_listed/models_listed 事件；key 存 Rust
  // creds（D4）；选择经 set_default_model 持久化、__pi_model_select 热切换。
  async function refreshProviders() {
    try {
      await invoke("pi_call_global", {
        fnName: "__pi_providers_list",
        arg: "",
      });
    } catch {
      // runtime 未就绪：保留静态清单，agent_ready 后会再拉
    }
  }

  async function chooseProvider(id: string) {
    setSelProvider(id);
    try {
      const has = await invoke<boolean>("has_creds", { provider: id });
      setKeySaved(has);
      if (has) setConfigured((prev) => new Set(prev).add(id));
    } catch {
      setKeySaved(false);
    }
    if (keySaved() && providerModels().length === 0) await loadModels(id);
  }

  async function saveProviderKey(e: Event) {
    e.preventDefault();
    const p = selProvider();
    if (!p || !providerKey().trim()) return;
    try {
      await invoke("set_creds", { provider: p, apiKey: providerKey().trim() });
      setProviderKey("");
      setKeySaved(true);
      setConfigured((prev) => new Set(prev).add(p));
      push({ role: "status", text: `API key saved (${p})` });
      await loadModels(p);
    } catch (err) {
      push({ role: "status", text: `save key failed: ${err}` });
    }
  }

  async function loadModels(id: string) {
    setLoadingModels(true);
    try {
      await invoke("pi_call_global", {
        fnName: "__pi_models_refresh",
        arg: id,
      });
    } catch (err) {
      setLoadingModels(false);
      push({ role: "status", text: `model list failed: ${err}` });
    }
  }

  async function selectModel(p: string, m: ProviderModel) {
    try {
      const r = await invoke<string>("pi_call_global", {
        fnName: "__pi_model_select",
        arg: JSON.stringify({ provider: p, modelId: m.id }),
      });
      if (r !== "started") throw new Error(r);
      await invoke("set_default_model", { provider: p, modelId: m.id });
      setCurrentModel({ provider: p, id: m.id, name: m.name });
      setModelPickerOpen(false);
      push({ role: "status", text: `model set: ${m.name}` });
    } catch (err) {
      push({ role: "status", text: `model select failed: ${err}` });
    }
  }

  return {
    providers,
    setProviders,
    selProvider,
    providerKey,
    setProviderKey,
    keySaved,
    setKeySaved,
    loadingModels,
    setLoadingModels,
    currentModel,
    setCurrentModel,
    configured,
    refreshConfigured,
    modelPickerOpen,
    setModelPickerOpen,
    providerModels,
    providerLabel,
    pickerProvider,
    pickerModels,
    openModelPicker,
    refreshProviders,
    chooseProvider,
    saveProviderKey,
    loadModels,
    selectModel,
  };
}

export type ProvidersState = ReturnType<typeof useProviders>;
