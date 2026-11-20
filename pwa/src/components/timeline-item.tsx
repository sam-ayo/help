// One item of the timeline, made with the AI Elements patterns of shadcn/ui: Message for the text,
// Reasoning for the thoughts, Tool for the tool calls, Plan, and Confirmation for the permissions.
// See https://ui.shadcn.com/docs/ai-elements.

import {
  BrainIcon,
  CheckIcon,
  CircleCheckIcon,
  CircleIcon,
  CircleXIcon,
  FileTextIcon,
  GlobeIcon,
  ListTodoIcon,
  LoaderCircleIcon,
  PencilIcon,
  SearchIcon,
  ShieldQuestionIcon,
  TerminalIcon,
  TrashIcon,
  WrenchIcon,
  type LucideIcon,
} from "lucide-react";
import { Streamdown } from "streamdown";
import { Bubble, BubbleContent } from "@/components/ui/bubble";
import { Button } from "@/components/ui/button";
import { CodeBlock, CodeBlockContent } from "@/components/ui/code-block";
import { Item as ItemCard, ItemActions, ItemContent, ItemDescription, ItemMedia, ItemTitle } from "@/components/ui/item";
import { Message, MessageContent } from "@/components/ui/message";
import {
  Trace,
  TraceCollapsible,
  TraceCollapsibleContent,
  TraceCollapsibleTrigger,
  TraceDescription,
  TraceIcon,
  TraceLine,
  TraceTitle,
} from "@/components/ui/trace";
import type { Status } from "@/protocol.ts";
import { remote, useRemote, type PermissionItem } from "@/remote.ts";
import type { Item } from "@/timeline.ts";

export function TimelineItem({ item, last }: { item: Item; last: boolean }) {
  // The agent still writes the last item while it is busy.
  const streaming = last && remote.busy;
  switch (item.kind) {
    case "user":
      return (
        <Message align="end">
          <MessageContent>
            <Bubble align="end" variant="secondary">
              <BubbleContent className="whitespace-pre-wrap">{item.text}</BubbleContent>
            </Bubble>
          </MessageContent>
        </Message>
      );
    case "agent":
      return (
        <Message>
          <MessageContent>
            <Streamdown className="text-sm leading-relaxed" isAnimating={streaming}>
              {item.text}
            </Streamdown>
          </MessageContent>
        </Message>
      );
    case "thought":
      return <Reasoning text={item.text} streaming={streaming} />;
    case "tool":
      return <Tool item={item} />;
    case "plan":
      return <Plan entries={item.entries} />;
    case "permission":
      return <Confirmation item={item} />;
    case "end":
      return (
        <Trace>
          <TraceLine>
            <TraceIcon>
              <CheckIcon />
            </TraceIcon>
            <TraceTitle>{item.reason === "end_turn" ? "Done" : item.reason}</TraceTitle>
          </TraceLine>
        </Trace>
      );
    case "error":
      return (
        <Bubble variant="destructive">
          <BubbleContent className="whitespace-pre-wrap">{item.message}</BubbleContent>
        </Bubble>
      );
  }
}

function Reasoning({ text, streaming }: { text: string; streaming: boolean }) {
  return (
    <Trace>
      <TraceCollapsible>
        <TraceCollapsibleTrigger>
          <TraceLine status={streaming ? "running" : undefined}>
            <TraceIcon>
              <BrainIcon />
            </TraceIcon>
            <TraceTitle>{streaming ? "Thinking" : "Thought"}</TraceTitle>
          </TraceLine>
        </TraceCollapsibleTrigger>
        <TraceCollapsibleContent className="text-sm whitespace-pre-wrap">{text}</TraceCollapsibleContent>
      </TraceCollapsible>
    </Trace>
  );
}

// The icons for the `tool_kind` values of the Agent Client Protocol.
const TOOL_ICONS: Record<string, LucideIcon> = {
  read: FileTextIcon,
  edit: PencilIcon,
  delete: TrashIcon,
  search: SearchIcon,
  execute: TerminalIcon,
  think: BrainIcon,
  fetch: GlobeIcon,
};

function traceStatus(status: Status): "running" | "error" | "pending" | undefined {
  if (status === "done") return undefined;
  return status === "failed" ? "error" : status;
}

function Tool({ item }: { item: Extract<Item, { kind: "tool" }> }) {
  const Icon = TOOL_ICONS[item.toolKind] ?? WrenchIcon;
  const line = (
    <TraceLine status={traceStatus(item.status)}>
      <TraceIcon>
        <Icon />
      </TraceIcon>
      <TraceTitle>{item.title}</TraceTitle>
    </TraceLine>
  );
  return (
    <Trace className={item.parent ? "pl-6" : undefined}>
      {item.detail ? (
        <TraceCollapsible>
          <TraceCollapsibleTrigger>{line}</TraceCollapsibleTrigger>
          <TraceCollapsibleContent>
            <CodeBlock size="sm" variant={item.status === "failed" ? "destructive" : "default"}>
              <CodeBlockContent wrap className="max-h-64">
                {item.detail}
              </CodeBlockContent>
            </CodeBlock>
          </TraceCollapsibleContent>
        </TraceCollapsible>
      ) : (
        <div>{line}</div>
      )}
    </Trace>
  );
}

const PLAN_ICONS: Record<Status, LucideIcon> = {
  pending: CircleIcon,
  running: LoaderCircleIcon,
  done: CircleCheckIcon,
  failed: CircleXIcon,
};

function Plan({ entries }: { entries: { text: string; status: Status }[] }) {
  const done = entries.filter((entry) => entry.status === "done").length;
  const running = entries.some((entry) => entry.status === "running");
  return (
    <Trace>
      <TraceCollapsible defaultOpen>
        <TraceCollapsibleTrigger>
          <TraceLine status={running ? "running" : undefined}>
            <TraceIcon>
              <ListTodoIcon />
            </TraceIcon>
            <TraceTitle>Plan</TraceTitle>
            <TraceDescription>
              {done}/{entries.length}
            </TraceDescription>
          </TraceLine>
        </TraceCollapsibleTrigger>
        <TraceCollapsibleContent>
          <ol className="flex flex-col gap-1.5 text-sm">
            {entries.map((entry, index) => {
              const Icon = PLAN_ICONS[entry.status];
              return (
                <li key={index} className="flex items-start gap-2 data-[status=done]:line-through" data-status={entry.status}>
                  <Icon className={`mt-0.5 size-4 shrink-0 ${entry.status === "running" ? "animate-spin" : ""}`} />
                  <span>{entry.text}</span>
                </li>
              );
            })}
          </ol>
        </TraceCollapsibleContent>
      </TraceCollapsible>
    </Trace>
  );
}

function Confirmation({ item }: { item: PermissionItem }) {
  const remote = useRemote();
  const answer = item.answer ?? (item.open ? undefined : "Answered on the laptop");
  return (
    <ItemCard variant="outline" size="sm">
      <ItemMedia variant="icon">
        <ShieldQuestionIcon />
      </ItemMedia>
      <ItemContent>
        <ItemTitle>{item.title}</ItemTitle>
        {answer && <ItemDescription>{answer}</ItemDescription>}
      </ItemContent>
      {!answer && (
        <ItemActions className="flex-wrap">
          {item.options.map((option) => (
            <Button
              key={option.id}
              size="sm"
              variant={option.allow ? "default" : "outline"}
              disabled={!remote.ready}
              onClick={() => remote.answer(item, option)}
            >
              {option.name}
            </Button>
          ))}
        </ItemActions>
      )}
    </ItemCard>
  );
}
