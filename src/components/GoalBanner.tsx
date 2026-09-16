import { FiPlay, FiTarget, FiX } from "solid-icons/fi";
import { Show } from "solid-js";

interface Props {
  goal: () => string | null;
  goalAuto: () => { count: number; cap: number } | null;
  busy: () => boolean;
  onContinue: () => void;
  onClear: () => void;
}

export function GoalBanner(props: Props) {
  return (
    <Show when={props.goal()}>
      {(g) => (
        <div class="goal-banner">
          <span class="goal-text">
            <FiTarget size="0.95em" style={{ "vertical-align": "-0.1em" }} />{" "}
            {g()}
            <Show when={props.goalAuto()}>
              <span class="goal-auto">
                {" "}
                · auto {props.goalAuto()!.count}/{props.goalAuto()!.cap}
              </span>
            </Show>
          </span>
          <div class="goal-actions">
            <Show when={!props.busy()}>
              <button type="button" class="goal-btn" onClick={props.onContinue}>
                <FiPlay size="0.85em" /> Continue
              </button>
            </Show>
            <button
              type="button"
              class="goal-btn"
              onClick={props.onClear}
              aria-label="clear goal"
            >
              <FiX size="0.9em" />
            </button>
          </div>
        </div>
      )}
    </Show>
  );
}
