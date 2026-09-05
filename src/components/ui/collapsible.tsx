import type { JSX, ValidComponent } from "solid-js";
import { splitProps } from "solid-js";
import * as CollapsiblePrimitive from "@kobalte/core/collapsible";
import type { PolymorphicProps } from "@kobalte/core/polymorphic";
import { cn } from "~/lib/utils";

type CollapsibleProps<T extends ValidComponent = "div"> =
  CollapsiblePrimitive.CollapsibleRootProps<T> & { class?: string };

const Collapsible = <T extends ValidComponent = "div">(
  props: PolymorphicProps<T, CollapsibleProps<T>>,
) => {
  const [local, others] = splitProps(props as CollapsibleProps, ["class"]);
  return (
    <CollapsiblePrimitive.Root
      class={cn(local.class)}
      {...others}
    />
  );
};

type CollapsibleTriggerProps<T extends ValidComponent = "button"> =
  CollapsiblePrimitive.CollapsibleTriggerProps<T> & { class?: string; children?: JSX.Element };

const CollapsibleTrigger = <T extends ValidComponent = "button">(
  props: PolymorphicProps<T, CollapsibleTriggerProps<T>>,
) => {
  const [local, others] = splitProps(props as CollapsibleTriggerProps, [
    "class",
  ]);
  return (
    <CollapsiblePrimitive.Trigger
      class={cn("flex w-full items-center", local.class)}
      {...others}
    />
  );
};

type CollapsibleContentProps<T extends ValidComponent = "div"> =
  CollapsiblePrimitive.CollapsibleContentProps<T> & { class?: string };

const CollapsibleContent = <T extends ValidComponent = "div">(
  props: PolymorphicProps<T, CollapsibleContentProps<T>>,
) => {
  const [local, others] = splitProps(props as CollapsibleContentProps, [
    "class",
  ]);
  return (
    <CollapsiblePrimitive.Content
      class={cn(
        "animate-accordion-down overflow-hidden",
        local.class,
      )}
      {...others}
    />
  );
};

export {
  Collapsible,
  CollapsibleTrigger,
  CollapsibleContent,
};
