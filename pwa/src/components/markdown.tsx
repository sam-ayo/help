// The Markdown of the agent, rendered with Streamdown. Streamdown and its Markdown parser are
// about two thirds of the JavaScript of the app, so the app loads them only for the first answer
// of the agent. Until then, the answer shows as plain text.

import { lazy, Suspense } from "react";

const Streamdown = lazy(() => import("streamdown").then((module) => ({ default: module.Streamdown })));

export function Markdown({ children, streaming }: { children: string; streaming: boolean }) {
  return (
    <Suspense fallback={<div className="text-sm leading-relaxed whitespace-pre-wrap">{children}</div>}>
      <Streamdown className="text-sm leading-relaxed" isAnimating={streaming}>
        {children}
      </Streamdown>
    </Suspense>
  );
}
