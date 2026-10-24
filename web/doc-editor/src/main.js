// The editor of the memory file and the conversation summary. The app shows this page in a
// web view, and the user edits the Markdown as formatted text, as in Notion.
//
// The app and the page send JSON messages:
// - page to app: {type: "ready"}, {type: "loaded"}, {type: "change", markdown}, {type: "focus"},
//   {type: "paste"}
// - app to page: helpDoc.load(markdown), helpDoc.theme(vars), helpDoc.paste(text)

import { CrepeBuilder } from '@milkdown/crepe/builder';
import { blockEdit } from '@milkdown/crepe/feature/block-edit';
import { cursor } from '@milkdown/crepe/feature/cursor';
import { linkTooltip } from '@milkdown/crepe/feature/link-tooltip';
import { listItem } from '@milkdown/crepe/feature/list-item';
import { placeholder } from '@milkdown/crepe/feature/placeholder';
import { table } from '@milkdown/crepe/feature/table';
import { toolbar } from '@milkdown/crepe/feature/toolbar';
import { editorViewCtx, remarkStringifyOptionsCtx } from '@milkdown/kit/core';
import { replaceAll } from '@milkdown/kit/utils';

import '@milkdown/crepe/theme/common/prosemirror.css';
import '@milkdown/crepe/theme/common/reset.css';
import '@milkdown/crepe/theme/common/block-edit.css';
import '@milkdown/crepe/theme/common/cursor.css';
import '@milkdown/crepe/theme/common/link-tooltip.css';
import '@milkdown/crepe/theme/common/list-item.css';
import '@milkdown/crepe/theme/common/placeholder.css';
import '@milkdown/crepe/theme/common/toolbar.css';
import '@milkdown/crepe/theme/common/table.css';
import './style.css';

const send = (message) => window.ipc.postMessage(JSON.stringify(message));

let crepe = null;
// The Markdown that the app has. An update with the same text is not a change.
let known = null;

async function load(markdown) {
  if (crepe) {
    crepe.editor.action(replaceAll(markdown));
    known = crepe.getMarkdown();
    return;
  }
  crepe = new CrepeBuilder({ root: document.getElementById('editor'), defaultValue: markdown });
  crepe
    .addFeature(cursor)
    .addFeature(listItem)
    .addFeature(linkTooltip)
    .addFeature(blockEdit)
    .addFeature(toolbar)
    .addFeature(table)
    .addFeature(placeholder, { text: "Type '/' for commands", mode: 'block' });
  // Keep the Markdown as people write it: "-" for a list item.
  crepe.editor.config((ctx) => {
    ctx.update(remarkStringifyOptionsCtx, (options) => ({ ...options, bullet: '-' }));
  });
  crepe.on((listener) => {
    listener.markdownUpdated((_, markdown) => {
      if (markdown === known) return;
      known = markdown;
      send({ type: 'change', markdown });
    });
  });
  // The app shows the web view when the editor has the text. Before that, the page is empty.
  try {
    await crepe.create();
    known = crepe.getMarkdown();
  } finally {
    send({ type: 'loaded' });
  }
}

// The colors and the fonts of the app, as CSS variables.
function theme(vars) {
  for (const [name, value] of Object.entries(vars)) {
    document.documentElement.style.setProperty(`--help-${name}`, value);
  }
}

// The app has no Edit menu, so the web view does not get the copy and paste commands.
// The page copies and cuts itself, and the app gives the text to paste.
function paste(text) {
  crepe?.editor.action((ctx) => ctx.get(editorViewCtx).pasteText(text));
}

document.addEventListener(
  'keydown',
  (event) => {
    const plain = !event.ctrlKey && !event.altKey;
    if (event.metaKey && plain && !event.shiftKey) {
      const key = event.key.toLowerCase();
      if (key === 'c' || key === 'x') {
        event.preventDefault();
        document.execCommand(key === 'c' ? 'copy' : 'cut');
      } else if (key === 'v') {
        event.preventDefault();
        send({ type: 'paste' });
      }
      return;
    }
    // Home and End move to the start and the end of the line.
    if (!event.metaKey && plain && (event.key === 'Home' || event.key === 'End')) {
      event.preventDefault();
      const direction = event.key === 'Home' ? 'backward' : 'forward';
      window.getSelection()?.modify(event.shiftKey ? 'extend' : 'move', direction, 'lineboundary');
    }
  },
  true,
);

// A function key must not put a control character in the text.
document.addEventListener(
  'beforeinput',
  (event) => {
    if (event.data && /[\u0000-\u0008\u000b\u000c\u000e-\u001f\uf700-\uf8ff]/.test(event.data)) {
      event.preventDefault();
    }
  },
  true,
);

// The app takes its keyboard focus away, so that its keys go to the editor.
window.addEventListener('focus', () => send({ type: 'focus' }));
document.addEventListener('focusin', () => send({ type: 'focus' }));

window.helpDoc = { load, theme, paste };
send({ type: 'ready' });
