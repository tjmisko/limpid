# Browser editor bundle

`editor.min.js` contains CodeMirror 6 and `@replit/codemirror-vim`. It is loaded
only when a reader opens an answer or comment editor. Cargo copies the committed
bundle into the shared page asset cache; a normal build needs no Node or npm.

Versions used for this bundle:

| Package | Version | License |
|---|---:|---|
| `codemirror` | 6.0.2 | MIT |
| `@replit/codemirror-vim` | 6.4.0 | MIT |
| `esbuild` (build tool only) | 0.28.2 | MIT |

To rebuild outside the normal Cargo build:

```sh
npm install --prefix /tmp/limpid-vendor --no-save --package-lock=false \
  esbuild@0.28.2 codemirror@6.0.2 @replit/codemirror-vim@6.4.0
NODE_PATH=/tmp/limpid-vendor/node_modules \
  /tmp/limpid-vendor/node_modules/.bin/esbuild vendor/editor-entry.js \
  --bundle --minify --format=iife --target=es2022 --outfile=vendor/editor.min.js
```

The licenses for packages included in the bundle are copied into
`THIRD_PARTY_NOTICES.md`. No package manager is invoked by Cargo or by limpid
at runtime.

## Mermaid

`mermaid.min.js` contains Mermaid 12.2.0 (MIT) and its browser dependencies.
It loads only after the first paint on pages with uncached diagrams. License
notices for the packages included in the bundle are in `MERMAID_NOTICES.md`.

To rebuild it outside the normal Cargo build:

```sh
npm install --prefix /tmp/limpid-mermaid --no-save --package-lock=false \
  mermaid@12.2.0 esbuild@0.28.2
NODE_PATH=/tmp/limpid-mermaid/node_modules \
  /tmp/limpid-mermaid/node_modules/.bin/esbuild vendor/mermaid-entry.js \
  --bundle --minify --format=iife --target=es2022 --outfile=vendor/mermaid.min.js
```
