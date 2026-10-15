import { test } from "node:test";
import assert from "node:assert/strict";
import { formatShortDate } from "../src/page.ts";
import { indexTurns, renderPageFor } from "../src/render.ts";
import { resolveTurnSpec, parseTurnSpec } from "../src/spec.ts";

const NOW = new Date(2026, 9, 15, 11, 22);

async function render(responses: string[], spec = "", model: string | null = "test-model") {
	const turns = indexTurns(responses.map((response, i) => ({ prompt: `prompt ${i}`, response })));
	const parsed = parseTurnSpec(spec);
	assert.ok(parsed.ok);
	const resolved = resolveTurnSpec(parsed.spec, turns.length);
	assert.ok(resolved.ok);
	// nvim: null keeps these tests independent of Neovim.
	return renderPageFor({ turns, resolved, model: model ?? undefined, now: NOW, nvim: null });
}

test("should format the date compactly when building the meta line", () => {
	assert.equal(formatShortDate(NOW), "Oct 15, 11:22");
	assert.equal(formatShortDate(new Date(2026, 0, 3, 4, 5)), "Jan 3, 04:05");
});

test("should show model, context and date in one meta line when a model is given", async () => {
	const { html } = await render(["# Hi"]);
	assert.match(html, /<p class="meta">test-model · turn 0 · Oct 15, 11:22<\/p>/);
});

test("should omit the model from the meta line when none is given", async () => {
	const { html } = await render(["# Hi"], "", null);
	assert.match(html, /<p class="meta">turn 0 · Oct 15, 11:22<\/p>/);
});

test("should escape the model when it contains markup", async () => {
	const { html } = await render(["x"], "", "<b>m</b>");
	assert.match(html, /&lt;b&gt;m&lt;\/b&gt;/);
	assert.doesNotMatch(html, /<b>m<\/b>/);
});

test("should contain no brand strings or key-hint footer when rendering", async () => {
	const { html } = await render(["# Hello\n\nbody", "## Two"], "0..2");
	assert.doesNotMatch(html, /\bPi\b/);
	assert.doesNotMatch(html, /Claude/i);
	assert.doesNotMatch(html, /Press\s*<code>q/);
	assert.doesNotMatch(html, /to reload/);
	assert.doesNotMatch(html, /READ VIEW|read view|eyebrow|rail-foot|class="brand"/);
});

test("should match the compact summary count to the headings when depth is at most 3", async () => {
	const { html } = await render(["# A\n\n## B\n\n### C\n\n#### D\n\n##### E\n\ntext"]);
	assert.match(html, /<summary>Contents · 3<\/summary>/);
	assert.equal((html.match(/<li class="depth-/g) ?? []).length, 6); // rail + compact
});

test("should keep the compact TOC collapsed when rendering", async () => {
	const { html } = await render(["# A\n\n## B"]);
	assert.match(html, /<details class="toc-compact"><summary>/);
	assert.doesNotMatch(html, /<details[^>]* open/);
});

test("should give unique ids when headings are duplicated", async () => {
	const { html } = await render(["## Same\n\n## Same\n\n## Same\n\n## Same-2"]);
	const ids = [...html.matchAll(/<h2 id="([^"]+)"/g)].map((m) => m[1]);
	assert.equal(ids.length, 4);
	assert.equal(new Set(ids).size, 4);
	assert.equal(ids[0], "same");
});

test("should give unique ids across turns when ranges repeat headings", async () => {
	const { html } = await render(["## Plan", "## Plan"], "0..2");
	const ids = [...html.matchAll(/<h2 id="([^"]+)"/g)].map((m) => m[1]);
	assert.deepEqual(ids, ["plan", "plan-2"]);
});

test("should escape script tags when a response contains raw HTML", async () => {
	const { html } = await render(["before <script>alert(1)</script> after\n\n<script>alert(2)</script>\n\n<img src=x onerror=alert(3)>"]);
	assert.doesNotMatch(html, /<script>alert/);
	assert.doesNotMatch(html, /<img src=x/);
	assert.match(html, /&lt;script&gt;alert\(1\)&lt;\/script&gt;/);
});

test("should escape markup when a prompt contains HTML", async () => {
	const turns = indexTurns([{ prompt: "<script>x()</script>", response: "r" }, { prompt: "q", response: "s" }]);
	const resolved = resolveTurnSpec({ kind: "range", inclusive: false }, turns.length);
	assert.ok(resolved.ok);
	const { html } = await renderPageFor({ turns, resolved, now: NOW, nvim: null });
	assert.doesNotMatch(html, /<script>x\(\)/);
	assert.match(html, /&lt;script&gt;x\(\)/);
});

test("should render without a rail or compact TOC when the response has no headings", async () => {
	const { html } = await render(["Just a paragraph.\n\n- a\n- b"]);
	assert.doesNotMatch(html, /<aside class="rail"/);
	assert.doesNotMatch(html, /<details/);
	assert.doesNotMatch(html, /class="shell has-rail"/);
	assert.match(html, /<div class="shell">/);
});

test("should render a rail with a Contents heading when headings exist", async () => {
	const { html } = await render(["# A\n\ntext"]);
	assert.match(html, /<aside class="rail"><nav aria-label="Contents"><h2>Contents<\/h2>/);
	assert.match(html, /class="shell has-rail"/);
});

test("should preserve CJK, emoji and RTL text when rendering", async () => {
	const text = "# 日本語見出し 🚀\n\nمرحبا שלום 中文測試 👍\n";
	const { html } = await render([text]);
	for (const fragment of ["日本語見出し", "🚀", "مرحبا", "שלום", "中文測試", "👍"]) {
		assert.ok(html.includes(fragment), fragment);
	}
	assert.match(html, /<h1 id="section">/); // non-latin heading falls back to a generic slug
});

test("should set the title to the first line of the newest response when it is long", async () => {
	const { html } = await render(["# Older", `# ${"x".repeat(200)}`], "1");
	assert.match(html, /<title>Older<\/title>/);
	const long = await render([`# ${"x".repeat(200)}`]);
	assert.match(long.html, new RegExp(`<title>x{80}</title>`));
});

test("should fall back to a generic title when the response has no text line", async () => {
	const { html } = await render(["***"]);
	assert.match(html, /<title>Response<\/title>/);
});

test("should render a compact turn header with the most recent tag when in range mode", async () => {
	const { html, context } = await render(["a", "b", "c"], "0..3");
	assert.equal(context, "turns 0–2");
	assert.match(html, /<div class="turn-head"><span class="turn-index">Turn 0<\/span><span class="turn-tag">most recent<\/span><\/div>/);
	assert.equal((html.match(/most recent/g) ?? []).length, 1);
});

test("should wrap tables for scrolling when a response has a table", async () => {
	const { html } = await render(["| a | b |\n|---|---|\n| 1 | 2 |"]);
	assert.match(html, /<div class="table-wrap"><table>/);
});

test("should render a 2MB response when the input is very large", async () => {
	const block = "## Section\n\nSome *prose* with `code` and a [link](https://example.com).\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n";
	const big = block.repeat(Math.ceil((2 * 1024 * 1024) / block.length));
	const started = performance.now();
	const { html } = await render([big]);
	const elapsed = performance.now() - started;
	assert.ok(html.length > 2 * 1024 * 1024);
	assert.ok(elapsed < 15_000, `took ${elapsed}ms`);
});
