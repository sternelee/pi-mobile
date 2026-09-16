import { FiCheck, FiChevronRight, FiCpu } from "solid-icons/fi";
import { For, Show } from "solid-js";
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
} from "~/components/ui/sheet";
import type { CurrentModel, ProviderModel } from "~/lib/types";

interface Props {
  open: () => boolean;
  onOpenChange: (open: boolean) => void;
  pickerProvider: () => string;
  providerLabel: (id: string) => string;
  pickerModels: () => ProviderModel[];
  currentModel: () => CurrentModel | null;
  selectModel: (p: string, m: ProviderModel) => void;
  onChangeProvider: () => void;
}

export function ModelPicker(props: Props) {
  return (
    <Sheet open={props.open()} onOpenChange={props.onOpenChange}>
      <SheetContent
        side="bottom"
        class="sheet-safe max-h-[70vh] gap-2 rounded-t-2xl p-4 pb-[calc(1rem+env(safe-area-inset-bottom))]"
      >
        <SheetHeader>
          <SheetTitle class="text-base">
            Model — {props.providerLabel(props.pickerProvider())}
          </SheetTitle>
        </SheetHeader>
        <button
          type="button"
          class="settings-row"
          onClick={props.onChangeProvider}
        >
          <span class="settings-icon-chip">
            <FiCpu size="1.05em" />
          </span>
          <div class="settings-row-body">
            <div class="settings-row-title">Change provider</div>
            <div class="settings-row-sub">
              OpenAI · OpenRouter · DeepSeek · Gemini
            </div>
          </div>
          <span class="settings-chevron">
            <FiChevronRight size="1em" />
          </span>
        </button>
        <div class="-mx-1 flex-1 overflow-y-auto px-1">
          <Show
            when={props.pickerModels().length > 0}
            fallback={<div class="empty-note">loading models…</div>}
          >
            <For each={props.pickerModels()}>
              {(m) => (
                <button
                  type="button"
                  class={`item-card model-row ${props.currentModel()?.id === m.id ? "active" : ""}`}
                  onClick={() => props.selectModel(props.pickerProvider(), m)}
                >
                  <div class="item-title">
                    <Show when={props.currentModel()?.id === m.id}>
                      <span class="model-check">
                        <FiCheck size="0.9em" />
                      </span>
                    </Show>
                    {m.name}
                  </div>
                </button>
              )}
            </For>
          </Show>
        </div>
      </SheetContent>
    </Sheet>
  );
}
