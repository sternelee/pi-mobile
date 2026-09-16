import { FiPlay } from "solid-icons/fi";
import { Show } from "solid-js";
import { Button } from "~/components/ui/button";
import { Markdown } from "~/ui/Markdown";

interface Props {
  plan: () => { objective: string; content: string } | null;
  planning: () => boolean;
  onDiscard: () => void;
  onApproveRun: () => void;
}

export function PlanCard(props: Props) {
  return (
    <>
      <Show when={props.plan()}>
        {(p) => (
          <div class="approval">
            <div class="approval-title">📋 Plan — {p().objective}</div>
            <div class="md plan-body">
              <Markdown text={p().content} />
            </div>
            <div class="approval-actions">
              <Button variant="secondary" onClick={props.onDiscard}>
                Discard
              </Button>
              <Button
                class="bg-success text-success-foreground hover:bg-success/90"
                onClick={props.onApproveRun}
              >
                <FiPlay size="0.85em" /> Approve &amp; run
              </Button>
            </div>
          </div>
        )}
      </Show>

      <Show when={props.planning()}>
        <div class="planning-note">drafting plan…</div>
      </Show>
    </>
  );
}
