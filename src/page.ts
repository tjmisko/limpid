import type { TocItem } from "./markdown.ts";
import { escapeHtml } from "./util.ts";

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** Short local date, e.g. "Oct 15, 11:22". */
export function formatShortDate(date: Date): string {
	const pad = (n: number) => String(n).padStart(2, "0");
	return `${MONTHS[date.getMonth()]} ${date.getDate()}, ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

export function buildPage(options: {
	title: string;
	model?: string;
	context: string;
	date: Date;
	body: string;
	toc: TocItem[];
}): string {
	const { title, model, context, date, body, toc } = options;

	const meta = [model, context, formatShortDate(date)]
		.filter((part) => part && part.trim())
		.map((part) => escapeHtml(part))
		.join(" · ");

	const items = toc
		.map(
			(item, index) =>
				`<li class="depth-${item.depth}"><a href="#${item.id}"><span class="num">${String(
					index + 1,
				).padStart(2, "0")}</span>${escapeHtml(item.text)}</a></li>`,
		)
		.join("");

	const rail = toc.length
		? `  <aside class="rail"><nav aria-label="Contents"><h2>Contents</h2><ul class="toc">${items}</ul></nav></aside>\n`
		: "";
	const compact = toc.length
		? `<details class="toc-compact"><summary>Contents · ${toc.length}</summary><ul class="toc">${items}</ul></details>\n`
		: "";

	return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>${escapeHtml(title)}</title>
<style>
:root{
  --bg:#0c1015; --rail:#0a0e12; --surface:#12181f; --raised:#18212a;
  --rule:#27313d; --rule-soft:#1b242d;
  --ink:#e8eeed; --muted:#95a4b1; --faint:#627484;
  --teal:#7cdec7; --blue:#90bdf5; --amber:#f0bc72; --purple:#b6a3ee;
  --code:#0e1419;
  --mono:"SFMono-Regular",Consolas,"Liberation Mono",Menlo,monospace;
  --sans:Inter,ui-sans-serif,system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",Arial,sans-serif;
  --rail-w:220px;
  color-scheme:dark;
}
*{box-sizing:border-box}
html{scroll-behavior:smooth}
body{margin:0;background:var(--bg);color:var(--ink);font-family:var(--sans);font-size:16px;line-height:1.65;-webkit-font-smoothing:antialiased}
a{color:var(--teal);text-decoration:none}
a:hover{text-decoration:underline;text-underline-offset:3px}
:focus-visible{outline:2px solid var(--teal);outline-offset:3px}
.shell{display:grid;grid-template-columns:minmax(0,1fr);min-height:100vh}
.shell.has-rail{grid-template-columns:var(--rail-w) minmax(0,1fr)}
.rail{display:none;position:sticky;top:0;align-self:start;height:100vh;overflow-y:auto;padding:20px 14px 40px;background:var(--rail);border-right:1px solid var(--rule)}
.has-rail .rail{display:block}
.rail h2{font-size:10px;letter-spacing:.16em;text-transform:uppercase;color:var(--faint);margin:0 0 8px 8px;font-weight:600;padding:0;border:0}
.toc{list-style:none;margin:0;padding:0}
.toc li{margin:0}
.toc a{position:relative;display:block;padding:5px 8px 5px 14px;color:var(--muted);font-size:13px;line-height:1.4;border-left:2px solid transparent}
.toc a:hover{color:var(--ink);background:var(--surface);text-decoration:none}
.toc a.active{color:var(--teal);border-left-color:var(--teal);background:var(--surface)}
.toc a .num{font-family:var(--mono);font-size:10px;color:var(--faint);margin-right:7px}
.toc .depth-3 a{padding-left:28px;font-size:12px}
.content{min-width:0}
.page{max-width:860px;margin:0 auto;padding:22px clamp(20px,5vw,64px) 120px}
.meta{font-family:var(--mono);font-size:11px;line-height:1.4;color:var(--faint);margin:0 0 18px}
.toc-compact{display:block;margin:-6px 0 18px;font-size:13px}
.toc-compact summary{cursor:pointer;color:var(--faint);font-family:var(--mono);font-size:11px;list-style:none;padding:2px 0}
.toc-compact summary::-webkit-details-marker{display:none}
.toc-compact summary::before{content:"\\25B8";display:inline-block;margin-right:6px}
.toc-compact[open] summary::before{content:"\\25BE"}
.toc-compact summary:hover{color:var(--muted)}
.toc-compact .toc{margin-top:6px;padding:4px 0;border:1px solid var(--rule-soft);border-radius:8px;background:var(--surface)}
.toc-compact .toc a{padding:3px 12px;font-size:12.5px}
.toc-compact .toc .depth-3 a{padding-left:26px}
.page > :first-child{margin-top:0}
h1,h2,h3,h4,h5,h6{line-height:1.25;letter-spacing:-.02em;scroll-margin-top:24px;position:relative}
h1{font-size:clamp(24px,3vw,32px);font-weight:800;margin:0 0 14px}
h2{font-size:clamp(21px,2.6vw,27px);font-weight:720;margin:36px 0 14px;padding-top:20px;border-top:1px solid var(--rule-soft)}
h3{font-size:18px;font-weight:680;margin:30px 0 10px}
h4{font-size:15.5px;font-weight:680;margin:24px 0 10px;color:var(--muted);text-transform:uppercase;letter-spacing:.06em}
p{margin:0 0 16px}
strong{font-weight:680;color:#fff}
em{color:#d6dee4}
ul,ol{margin:0 0 18px;padding-left:26px}
li{margin:6px 0}
li > ul,li > ol{margin:6px 0}
blockquote{margin:20px 0;padding:4px 18px;border-left:3px solid var(--teal);background:rgba(124,222,199,.05);color:var(--muted);border-radius:0 8px 8px 0}
blockquote p:last-child{margin-bottom:0}
code{font-family:var(--mono);font-size:.87em}
p code,li code,td code,h1 code,h2 code,h3 code,h4 code{background:var(--raised);border:1px solid var(--rule);border-radius:5px;padding:1px 5px;color:var(--blue)}
pre{margin:0;padding:16px 18px;background:var(--code);overflow-x:auto;line-height:1.6;font-size:13px}
pre code{font-size:inherit;background:none;border:0;padding:0;color:var(--ink)}
figure.code{margin:22px 0;border:1px solid var(--rule);border-radius:10px;overflow:hidden;background:var(--code)}
figure.code figcaption{font-family:var(--mono);font-size:10.5px;letter-spacing:.08em;text-transform:uppercase;color:var(--faint);background:var(--surface);padding:8px 14px;border-bottom:1px solid var(--rule)}
/* Neovim tree-sitter token colours arrive as inline styles on each span. */
figure.code pre code{color:#cdd6f4}
hr{border:0;border-top:1px solid var(--rule);margin:36px 0}
.table-wrap{margin:22px 0;overflow-x:auto;border:1px solid var(--rule);border-radius:10px}
table{border-collapse:collapse;width:100%;font-size:14px}
th,td{padding:9px 13px;border-bottom:1px solid var(--rule-soft);text-align:left;vertical-align:top}
th{background:var(--surface);color:var(--muted);font-weight:600;font-size:12px;letter-spacing:.04em;text-transform:uppercase;white-space:nowrap}
tr:last-child td{border-bottom:0}
tbody tr:nth-child(even){background:rgba(255,255,255,.018)}
img{max-width:100%;height:auto;border-radius:8px}
.anchor{position:absolute;left:-1.15em;top:0;color:var(--faint);text-decoration:none;opacity:0;font-weight:400;transition:opacity .12s}
h1:hover .anchor,h2:hover .anchor,h3:hover .anchor,h4:hover .anchor{opacity:1}
.anchor:hover{color:var(--teal);text-decoration:none}
.turn{margin:36px 0 0;padding-top:20px;border-top:1px solid var(--rule)}
.turn:first-of-type{margin-top:0;padding-top:0;border-top:0}
.turn-head{display:flex;align-items:center;gap:10px;margin-bottom:10px}
.turn-index{font-family:var(--mono);font-size:11px;letter-spacing:.08em;color:var(--teal);font-weight:600}
.turn-tag{font-family:var(--mono);font-size:10px;letter-spacing:.08em;text-transform:uppercase;color:var(--amber);border:1px solid var(--rule);border-radius:999px;padding:1px 8px}
.prompt{margin:0 0 20px;padding:9px 14px;border:1px solid var(--rule);border-left:3px solid var(--amber);border-radius:0 8px 8px 0;background:var(--surface);font-size:14.5px;line-height:1.55}
.prompt .role{display:block;font-family:var(--mono);font-size:10px;letter-spacing:.13em;text-transform:uppercase;color:var(--amber);margin-bottom:4px}
.prompt-text{white-space:pre-wrap;overflow-wrap:anywhere;color:var(--ink)}
.prompt-text.placeholder{color:var(--faint);font-style:italic}
.response > :first-child{margin-top:0}
@media (min-width:1100px){
  .toc-compact{display:none}
}
@media (max-width:1099px){
  .shell.has-rail{grid-template-columns:minmax(0,1fr)}
  .has-rail .rail{display:none}
}
@media (max-width:880px){
  .page{padding:18px 20px 90px}
  h2{padding-top:16px;margin-top:30px}
}
@media print{
  .rail,.toc-compact{display:none !important}
  .shell{display:block}
  body{background:#fff;color:#111}
  .page{max-width:none}
  a{color:inherit}
  h2{break-after:avoid}
  figure.code,.table-wrap{break-inside:avoid}
}
</style>
</head>
<body>
<div class="shell${toc.length ? " has-rail" : ""}">
${rail}  <main class="content">
    <article class="page">
      <p class="meta">${meta}</p>
${compact}${body}
    </article>
  </main>
</div>
<script>
(function () {
  var links = Array.prototype.slice.call(document.querySelectorAll(".toc a"));
  var map = {};
  links.forEach(function (a) {
    var id = a.getAttribute("href").slice(1);
    (map[id] = map[id] || []).push(a);
  });
  var heads = Array.prototype.slice.call(
    document.querySelectorAll("article h1[id],article h2[id],article h3[id],article section.turn[id]")
  );
  if (!heads.length || !("IntersectionObserver" in window)) return;
  var observer = new IntersectionObserver(function (entries) {
    entries.forEach(function (entry) {
      if (!entry.isIntersecting) return;
      links.forEach(function (l) { l.classList.remove("active"); });
      (map[entry.target.id] || []).forEach(function (a) { a.classList.add("active"); });
    });
  }, { rootMargin: "-15% 0px -75% 0px", threshold: 0 });
  heads.forEach(function (h) { observer.observe(h); });
})();
</script>
</body>
</html>
`;
}
