import { FiChevronRight } from "solid-icons/fi";
import { For, Show } from "solid-js";
import { providerIcon } from "~/lib/providers";
import type { CurrentModel, ProviderInfo } from "~/lib/types";

interface Props {
  providers: () => ProviderInfo[];
  currentModel: () => CurrentModel | null;
  configured: () => Set<string>;
  onOpenProvider: (id: string) => void;
}

export function ProvidersView(props: Props) {
  return (
    <div class="-mx-1 flex-1 overflow-y-auto px-1">
      <div class="settings-subtitle">
        Provider catalogs come from the pi-ai models registry. Tap a provider to
        set its API key and pick a model.
      </div>
      <div class="flex flex-col gap-2">
        <For each={props.providers()}>
          {(p) => (
            <button
              type="button"
              class="settings-row"
              onClick={() => {
                props.onOpenProvider(p.id);
              }}
            >
              <span class="settings-icon-chip">{providerIcon(p.id)}</span>
              <div class="settings-row-body">
                <div class="settings-row-title">
                  {p.name}
                  <Show when={props.currentModel()?.provider === p.id}>
                    <span class="provider-badge">active</span>
                  </Show>
                </div>
                <div class="settings-row-sub">
                  {p.models.length} models ·{" "}
                  {props.configured().has(p.id) ? "API key set" : "no key"}
                </div>
              </div>
              <span class="settings-chevron">
                <FiChevronRight size="1em" />
              </span>
            </button>
          )}
        </For>
      </div>
      <div class="settings-footer">
        pi-mobile · sessions stay on this device
      </div>
    </div>
  );
}
