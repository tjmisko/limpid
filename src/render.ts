import { NvimCodeBatch, applyNvimHighlights } from "./highlight.ts";
import { renderMarkdown } from "./markdown.ts";
import type { TocItem } from "./markdown.ts";
import { buildPage } from "./page.ts";
import type { ResolvedTurnSpec } from "./spec.ts";
import { escapeHtml, firstLine } from "./util.ts";

export type Turn = { index: number; prompt: string; response: string };
export type InputTurn = { prompt?: unknown; response?: unknown };

/**
 * Normalise caller turns (oldest first) into indexed turns: drops turns with a
 * blank response and numbers the rest so the newest is 0.
 */
export function indexTurns(input: InputTurn[]): Turn[] {
	const kept = input
		.filter((turn) => turn && typeof turn === "object" && typeof turn.response === "string" && turn.response.trim())
		.map((turn) => ({
			prompt: typeof turn.prompt === "string" ? turn.prompt : "",
			response: turn.response as string,
		}));
	const total = kept.length;
	return kept.map((turn, i) => ({ index: total - 1 - i, ...turn }));
}

export async function renderPageFor(options: {
	turns: Turn[];
	resolved: Extract<ResolvedTurnSpec, { ok: true }>;
	model?: string;
	now: Date;
	nvim?: string | null;
}): Promise<{ html: string; context: string }> {
	const { turns, resolved, model, now } = options;
	const byIndex = new Map(turns.map((turn) => [turn.index, turn]));
	const batch = new NvimCodeBatch(options.nvim === undefined ? {} : { nvim: options.nvim });

	// Oldest first so the exchange reads in order.
	const selected = [...resolved.indices].sort((a, b) => b - a);
	const newest = Math.min(...selected);
	const oldest = Math.max(...selected);

	let body: string;
	let toc: TocItem[];
	let context: string;

	if (resolved.kind === "single") {
		const rendered = renderMarkdown(byIndex.get(newest)!.response, undefined, batch);
		body = rendered.body;
		toc = rendered.toc;
		context = `turn ${newest}`;
	} else {
		const used = new Map<string, number>();
		const sections: string[] = [];
		toc = [];
		for (const index of selected) {
			const turn = byIndex.get(index)!;
			const rendered = renderMarkdown(turn.response, used, batch);
			const turnId = `turn-${index}`;
			used.set(turnId, 1);
			toc.push({ id: turnId, text: `Turn ${index}`, depth: 1 });
			for (const item of rendered.toc) toc.push({ ...item, depth: 2 });

			const tag = index === 0 ? '<span class="turn-tag">most recent</span>' : "";
			const prompt = turn.prompt.trim()
				? `<div class="prompt-text">${escapeHtml(turn.prompt)}</div>`
				: '<div class="prompt-text placeholder">(no prompt recorded)</div>';
			sections.push(
				`<section class="turn" id="${turnId}">\n` +
					`  <div class="turn-head"><span class="turn-index">Turn ${index}</span>${tag}</div>\n` +
					`  <div class="prompt"><span class="role">You</span>${prompt}</div>\n` +
					`  <div class="response">\n${rendered.body}\n  </div>\n` +
					"</section>",
			);
		}
		body = sections.join("\n");
		context = selected.length > 1 ? `turns ${newest}–${oldest}` : `turn ${newest}`;
	}

	body = applyNvimHighlights(body, await batch.flush());
	const title = (firstLine(byIndex.get(newest)!.response) ?? "Response").slice(0, 80);
	return { html: buildPage({ title, model, context, date: now, body, toc }), context };
}
