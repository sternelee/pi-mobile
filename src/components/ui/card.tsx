import type { Component, ComponentProps } from "solid-js";
import { splitProps } from "solid-js";
import { cn } from "~/lib/utils";

type DivProps = ComponentProps<"div">;

const Card: Component<DivProps> = (props) => {
  const [local, others] = splitProps(props, ["class"]);
  return (
    <div
      class={cn("rounded-lg border bg-card text-card-foreground", local.class)}
      {...others}
    />
  );
};

const CardHeader: Component<DivProps> = (props) => {
  const [local, others] = splitProps(props, ["class"]);
  return (
    <div class={cn("flex flex-col space-y-1.5 p-4", local.class)} {...others} />
  );
};

const CardTitle: Component<ComponentProps<"h3">> = (props) => {
  const [local, others] = splitProps(props, ["class"]);
  return (
    <h3
      class={cn("font-semibold leading-none tracking-tight", local.class)}
      {...others}
    />
  );
};

const CardDescription: Component<ComponentProps<"p">> = (props) => {
  const [local, others] = splitProps(props, ["class"]);
  return (
    <p class={cn("text-sm text-muted-foreground", local.class)} {...others} />
  );
};

const CardContent: Component<DivProps> = (props) => {
  const [local, others] = splitProps(props, ["class"]);
  return <div class={cn("p-4 pt-0", local.class)} {...others} />;
};

const CardFooter: Component<DivProps> = (props) => {
  const [local, others] = splitProps(props, ["class"]);
  return (
    <div class={cn("flex items-center p-4 pt-0", local.class)} {...others} />
  );
};

export { Card, CardHeader, CardTitle, CardDescription, CardContent, CardFooter };
