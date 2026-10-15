import { mkdirSync, readdirSync, statSync, unlinkSync, writeFileSync } from "node:fs";
import { homedir, platform } from "node:os";
import { join, resolve } from "node:path";
import { indexTurns, renderPageFor } from "./render.ts";
import type { InputTurn } from "./render.ts";
import { parseTurnSpec, resolveTurnSpec } from "./spec.ts";
import { openInViewer, resolveViewer } from "./viewer.ts";

const DEFAULT_DIR = join(homedir(), ".cache", "limpid");
const KEEP_FILES = 50;

export class Fail extends Error {
	code: number;
	constructor(message: string, code = 1) {
		super(message);
		this.code = code;
	}
}

/** Keep only the newest KEEP_FILES `*.html` files in `dir`. */
function prune(dir: string): void {
	try {
		const files = readdirSync(dir)
			.filter((name) => name.endsWith(".html"))
			.map((name) => ({ name, mtime: statSync(join(dir, name)).mtimeMs }))
			.sort((a, b) => b.mtime - a.mtime || (a.name < b.name ? 1 : -1));
		for (const stale of files.slice(KEEP_FILES)) unlinkSync(join(dir, stale.name));
	} catch {
		/* pruning is best effort */
	}
}

export type PipelineInput = {
	model?: string;
	turns: InputTurn[];
	spec: string;
	open: boolean;
	out?: string;
};

/** Render the selected turns to a file and optionally open it. Shared by stdin and codex modes. */
export async function renderAndOpen(input: PipelineInput): Promise<{ file: string; context: string }> {
	const parsed = parseTurnSpec(input.spec);
	if (!parsed.ok) throw new Fail(parsed.error);

	const turns = indexTurns(input.turns);
	const resolved = resolveTurnSpec(parsed.spec, turns.length);
	if (!resolved.ok) throw new Fail(resolved.error);

	let viewer: string[] | undefined;
	if (input.open) {
		viewer = resolveViewer({ env: process.env, home: homedir(), platform: platform() });
		if (!viewer) {
			throw new Fail("No viewer found. Set LIMPID_VIEWER, or install htmlview, xdg-open, or open.");
		}
	}

	const now = new Date();
	const { html, context } = await renderPageFor({ turns, resolved, model: input.model, now });

	const dir = input.out ? resolve(input.out) : DEFAULT_DIR;
	const file = join(dir, `turns-${now.toISOString().replace(/[:.]/g, "-")}.html`);
	try {
		mkdirSync(dir, { recursive: true });
		writeFileSync(file, html, "utf8");
	} catch (error) {
		throw new Fail(`Could not write ${file}: ${(error as Error).message}`);
	}
	if (!input.out) prune(dir);

	if (viewer) openInViewer(viewer, file);
	return { file, context };
}
