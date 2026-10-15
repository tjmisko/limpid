import { marked } from "marked";
import type { NvimCodeBatch } from "./highlight.ts";
import { normalizeLang } from "./highlight.ts";
import { decodeEntities, escapeHtml, slugify, uniqueSlug } from "./util.ts";

export type TocItem = { id: string; text: string; depth: number };
export type RenderResult = { body: string; toc: TocItem[] };

/** Add ids to headings, build the TOC, and wrap tables for scrolling. */
export function decorate(html: string, used: Map<string, number> = new Map()): RenderResult {
	const toc: TocItem[] = [];

	let body = html.replace(/<h([1-6])>([\s\S]*?)<\/h\1>/g, (_match, depthText: string, inner: string) => {
		const text = decodeEntities(inner.replace(/<[^>]+>/g, ""));
		const id = uniqueSlug(slugify(text), used);
		const depth = Number(depthText);
		if (depth <= 3) toc.push({ id, text: text || "Section", depth });
		return `<h${depth} id="${id}"><a class="anchor" href="#${id}" aria-hidden="true">#</a>${inner}</h${depth}>`;
	});

	body = body
		.replace(/<table>/g, '<div class="table-wrap"><table>')
		.replace(/<\/table>/g, "</table></div>");

	return { body, toc };
}

export function renderMarkdown(
	markdown: string,
	used?: Map<string, number>,
	batch?: NvimCodeBatch,
): RenderResult {
	const renderer = new marked.Renderer();

	// Fenced/indented code: escape ourselves and caption with the language. With
	// a batch, the block is queued for Neovim highlighting behind a placeholder.
	renderer.code = ({ text, lang }: { text: string; lang?: string }) => {
		const language = (lang ?? "").trim().split(/\s+/)[0] ?? "";
		const safeLang = language.replace(/[^a-zA-Z0-9_+#.-]/g, "");
		const className = safeLang ? ` class="language-${safeLang}"` : "";
		const caption = safeLang ? `<figcaption>${escapeHtml(safeLang)}</figcaption>` : "";
		const content = batch
			? batch.register(safeLang ? normalizeLang(safeLang) : "", text)
			: escapeHtml(text);
		return `<figure class="code">${caption}<pre><code${className}>${content}</code></pre></figure>\n`;
	};

	// Raw HTML in the response is shown as text rather than executed.
	renderer.html = ({ text }: { text: string }) => escapeHtml(text);

	const html = marked.parse(markdown, { renderer, gfm: true, breaks: false, async: false });
	return decorate(html, used);
}
