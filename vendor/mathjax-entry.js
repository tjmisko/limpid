// Entry for vendor/mathjax.min.js: MathJax TeX input, SVG output with no font
// cache (every SVG is self-contained), on the DOM-free liteAdaptor so it runs
// under QuickJS. It defines one global:
//
//   limpidTex2Svg(latex, display) -> JSON string
//     { svg, widthEx, heightEx, baselineEx }   on success
//     { error }                                on a TeX error
//
// Sizes are in ex. baselineEx is the distance from the top of the SVG down to
// the text baseline (height plus the negative vertical-align MathJax emits).
import { mathjax } from 'mathjax-full/js/mathjax.js';
import { TeX } from 'mathjax-full/js/input/tex.js';
import { SVG } from 'mathjax-full/js/output/svg.js';
import { liteAdaptor } from 'mathjax-full/js/adaptors/liteAdaptor.js';
import { RegisterHTMLHandler } from 'mathjax-full/js/handlers/html.js';
import { AllPackages } from 'mathjax-full/js/input/tex/AllPackages.js';

// `autoload` and `require` load further files at run time through MathJax's
// loader, which does not exist in this bundle; every other package is inside.
const packages = AllPackages.filter((name) => name !== 'autoload' && name !== 'require');

const adaptor = liteAdaptor();
RegisterHTMLHandler(adaptor);
const tex = new TeX({
  packages,
  formatError: (_jax, error) => {
    throw error;
  },
});
const svg = new SVG({ fontCache: 'none' });
const document = mathjax.document('', { InputJax: tex, OutputJax: svg });

function exValue(text) {
  const match = /(-?[0-9.]+)ex/.exec(text || '');
  return match ? parseFloat(match[1]) : 0;
}

globalThis.limpidTex2Svg = (latex, display) => {
  try {
    const container = document.convert(latex, { display: !!display });
    const root = adaptor.firstChild(container);
    if (!root || adaptor.kind(root) !== 'svg') {
      return JSON.stringify({ error: 'MathJax produced no SVG' });
    }
    const widthEx = exValue(adaptor.getAttribute(root, 'width'));
    const heightEx = exValue(adaptor.getAttribute(root, 'height'));
    const verticalAlign = exValue(adaptor.getAttribute(root, 'style'));
    return JSON.stringify({
      svg: adaptor.outerHTML(root),
      widthEx,
      heightEx,
      baselineEx: heightEx + verticalAlign,
    });
  } catch (error) {
    return JSON.stringify({ error: String((error && error.message) || error) });
  }
};
