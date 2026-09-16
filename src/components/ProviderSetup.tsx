import { For, Show } from "solid-js";
import { Button } from "~/components/ui/button";
import type { ProviderInfo, ProviderModel } from "~/lib/types";

interface Props {
  /** 首启卡片的显隐条件：!ready() || !currentModel() */
  show: () => boolean;
  providers: () => ProviderInfo[];
  selProvider: () => string;
  chooseProvider: (id: string) => void;
  keySaved: () => boolean;
  setKeySaved: (v: boolean) => void;
  providerKey: () => string;
  setProviderKey: (v: string) => void;
  saveProviderKey: (e: Event) => void;
  loadingModels: () => boolean;
  providerModels: () => ProviderModel[];
  providerLabel: (id: string) => string;
  selectModel: (p: string, m: ProviderModel) => void;
}

// Provider 选择节：首启卡片与抽屉共用（chooseProvider 自动拉已配置
// provider 的模型列表；OpenRouter 动态目录首次刷新拉全量）。
export function ProviderSetup(props: Props) {
  return (
    <Show when={props.show()}>
      <div class="keyform provider-setup">
        <div class="item-sub">
          Choose an AI provider — the model list loads automatically after your
          key is saved.
        </div>
        <div class="flex flex-col gap-1.5">
          <div class="flex flex-wrap gap-1">
            <For each={props.providers()}>
              {(p) => (
                <Button
                  variant={props.selProvider() === p.id ? "default" : "outline"}
                  size="sm"
                  class="h-7 text-xs"
                  onClick={() => props.chooseProvider(p.id)}
                >
                  {p.name}
                </Button>
              )}
            </For>
          </div>
          <Show when={props.selProvider()}>
            <Show
              when={!props.keySaved()}
              fallback={
                <div class="item-sub">
                  API key configured ·{" "}
                  <button
                    type="button"
                    class="underline"
                    onClick={() => props.setKeySaved(false)}
                  >
                    replace
                  </button>
                </div>
              }
            >
              <form
                class="flex flex-col gap-1.5"
                onSubmit={props.saveProviderKey}
              >
                <input
                  class="ask-input"
                  type="text"
                  placeholder={`${props.providerLabel(props.selProvider())} API key…`}
                  value={props.providerKey()}
                  onInput={(e) => props.setProviderKey(e.currentTarget.value)}
                />
                <Button variant="outline" size="sm" type="submit">
                  Save key & load models
                </Button>
              </form>
            </Show>
            <Show when={props.loadingModels()}>
              <div class="item-sub">loading models…</div>
            </Show>
            <For each={props.providerModels()}>
              {(m) => (
                <button
                  type="button"
                  class="item-card"
                  onClick={() => props.selectModel(props.selProvider(), m)}
                >
                  <div class="item-title">{m.name}</div>
                </button>
              )}
            </For>
          </Show>
        </div>
      </div>
    </Show>
  );
}
