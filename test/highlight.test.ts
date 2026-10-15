import { test } from "node:test";
import assert from "node:assert/strict";
import { NvimCodeBatch, applyNvimHighlights, normalizeLang } from "../src/highlight.ts";

test("should return escaped plain text when no language is given", () => {
	const batch = new NvimCodeBatch({ nvim: null });
	assert.equal(batch.register("", "<a> & b"), "&lt;a&gt; &amp; b");
});

test("should fall back to escaped source when nvim is unavailable", async () => {
	const batch = new NvimCodeBatch({ nvim: null });
	const placeholder = batch.register("html", '<script>alert("x")</script>');
	const body = applyNvimHighlights(`<code>${placeholder}</code>`, await batch.flush());
	assert.equal(body, "<code>&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt;</code>");
});

test("should fall back to escaped source when the nvim binary does not exist", async () => {
	const batch = new NvimCodeBatch({ nvim: "/nonexistent/nvim" });
	const placeholder = batch.register("python", "a < b");
	const body = applyNvimHighlights(placeholder, await batch.flush());
	assert.equal(body, "a &lt; b");
});

test("should flush an empty map when the batch is empty", async () => {
	const batch = new NvimCodeBatch();
	assert.equal((await batch.flush()).size, 0);
});

test("should drop unknown placeholders when applying highlights", () => {
	assert.equal(applyNvimHighlights("x999y", new Map()), "xy");
});

test("should map fence aliases to tree-sitter names when normalizing", () => {
	assert.equal(normalizeLang("PY"), "python");
	assert.equal(normalizeLang("ts"), "typescript");
	assert.equal(normalizeLang("rust"), "rust");
});
