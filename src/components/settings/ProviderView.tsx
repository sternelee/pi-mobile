import { FiCheck, FiKey, FiLock, FiRotateCw } from "solid-icons/fi";
import { For, Show } from "solid-js";
import { Button } from "~/components/ui/button";
import { OAUTH_PROVIDERS } from "~/lib/providers";
import type { CurrentModel, ProviderModel } from "~/lib/types";

interface Props {
  busy: () => boolean;
  selProvider: () => string;
  providerLabel: (id: string) => string;
  providerKey: () => string;
  setProviderKey: (v: string) => void;
  saveProviderKey: (e: Event) => void;
  keySaved: () => boolean;
  setKeySaved: (v: boolean) => void;
  loadingModels: () => boolean;
  loadModels: (id: string) => void;
  providerModels: () => ProviderModel[];
  currentModel: () => CurrentModel | null;
  selectModel: (p: string, m: ProviderModel) => void;
  onOAuthLogin: () => void;
}

export function ProviderView(props: Props) {
  return (
    <>
      <div class="settings-section-title">
        {props.providerLabel(props.selProvider())}
      </div>
      <div class="settings-subtitle">
        Tap a model to make it the active model — applies immediately and
        persists across restarts.
      </div>
      <Show when={OAUTH_PROVIDERS.has(props.selProvider())}>
        <div class="item-card">
          <div class="item-title">
            <FiLock size="0.95em" style={{ "vertical-align": "-0.12em" }} />{" "}
            Subscription sign-in
          </div>
          <div class="item-sub">
            Opens the provider's login page in your browser and returns via the
            pimobile:// deep link or a local callback — no API key needed.
          </div>
          <Button
            variant="outline"
            size="sm"
            class="mt-1"
            disabled={props.busy()}
            onClick={props.onOAuthLogin}
          >
            Sign in with {props.providerLabel(props.selProvider())}
          </Button>
        </div>
      </Show>
      <div class="-mx-1 flex-1 overflow-y-auto px-1">
        <Show
          when={!props.keySaved()}
          fallback={
            <div class="item-card">
              <div class="item-title">
                <FiKey size="0.95em" style={{ "vertical-align": "-0.12em" }} />{" "}
                API key configured
              </div>
              <div class="item-sub">
                tap <span class="underline">replace</span> below to change it
              </div>
              <Button
                variant="ghost"
                size="sm"
                class="mt-1 h-7 text-xs text-muted-foreground"
                onClick={() => props.setKeySaved(false)}
              >
                Replace key
              </Button>
            </div>
          }
        >
          <form
            class="mb-2 flex flex-col gap-1.5"
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
        <div class="flex items-center justify-between">
          <span class="item-sub">{props.providerModels().length} models</span>
          <Button
            variant="ghost"
            size="sm"
            class="h-7 text-xs"
            onClick={() => props.loadModels(props.selProvider())}
          >
            <FiRotateCw size="0.9em" /> Refresh
          </Button>
        </div>
        <Show when={props.loadingModels()}>
          <div class="empty-note">loading models…</div>
        </Show>
        <For each={props.providerModels()}>
          {(m) => (
            <button
              type="button"
              class={`item-card ${
                props.currentModel()?.provider === props.selProvider() &&
                props.currentModel()?.id === m.id
                  ? "active"
                  : ""
              }`}
              onClick={() => props.selectModel(props.selProvider(), m)}
            >
              <div class="item-title">
                <Show
                  when={
                    props.currentModel()?.provider === props.selProvider() &&
                    props.currentModel()?.id === m.id
                  }
                >
                  <span class="model-check">
                    <FiCheck size="0.9em" />
                  </span>
                </Show>
                {m.name}
              </div>
            </button>
          )}
        </For>
      </div>
    </>
  );
}
