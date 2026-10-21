// Build the editor into one HTML file that the app embeds. Run `npm run build` after a change.
import { build } from 'esbuild';
import { writeFileSync } from 'node:fs';

const result = await build({
  entryPoints: ['src/main.js'],
  bundle: true,
  minify: true,
  format: 'iife',
  write: false,
  outdir: 'out',
  loader: { '.svg': 'dataurl', '.woff2': 'dataurl', '.woff': 'dataurl', '.ttf': 'dataurl' },
  legalComments: 'none',
});
const file = (ext) => result.outputFiles.find((f) => f.path.endsWith(ext))?.text ?? '';
// A closing tag in the code must not end the element.
const js = file('.js').replaceAll('</script', '<\\/script');
const css = file('.css').replaceAll('</style', '<\\/style');
const html = `<!doctype html>
<html>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>${css}</style>
</head>
<body>
<div id="editor"></div>
<script>${js}</script>
</body>
</html>
`;
writeFileSync('../../crates/app/web/doc-editor.html', html);
console.log(`doc-editor.html: ${(html.length / 1024).toFixed(0)} KB`);
