import { spawn } from "node:child_process";
import { mkdirSync, readdirSync, readFileSync, statSync, unlinkSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import { indexTurns, renderPageFor } from "./render.ts";
import type { InputTurn } from "./render.ts";
import { parseTurnSpec, resolveTurnSpec } from "./spec.ts";
import { findExecutable } from "./util.ts";

const USAGE = "Usage: limpid [--spec <spec>] [--no-open] [--out <dir>]  (JSON turns on stdin)";
const DEFAULT_DIR = join(homedir(), ".cache", "limpid");
const KEEP_FILES = 50;

class Fail extends Error {
	code: number;
	constructor(message: string, code = 1) {
		super(message);
		this.code = code;
	}
}

type Options = { spec: string; open: boolean; out?: string };

function parseArgs(argv: string[]): Options | "help" {
	const options: Options = { spec: "", open: true };
	for (let i = 0; i < argv.length; i++) {
		const arg = argv[i];
		if (arg === "-h" || arg === "--help") return "help";
		if (arg === "--no-open") {
			options.open = false;
		} else if (arg === "--spec" || arg === "--out") {
			const value = argv[++i];
			if (value === undefined) throw new Fail(`${arg} needs a value.\n${USAGE}`, 2);
			if (arg === "--spec") options.spec = value;
			else options.out = value;
		} else if (arg.startsWith("--spec=")) {
			options.spec = arg.slice("--spec=".length);
		} else if (arg.startsWith("--out=")) {
			options.out = arg.slice("--out=".length);
		} else {
			throw new Fail(`Unknown argument "${arg}".\n${USAGE}`, 2);
		}
	}
	return options;
}

function readInput(): { model?: string; turns: InputTurn[] } {
	let raw: string;
	try {
		raw = readFileSync(0, "utf8");
	} catch (error) {
		throw new Fail(`Could not read stdin: ${(error as Error).message}`);
	}
	let data: unknown;
	try {
		data = JSON.parse(raw);
	} catch (error) {
		throw new Fail(`Malformed JSON on stdin: ${(error as Error).message}`);
	}
	if (!data || typeof data !== "object" || !Array.isArray((data as { turns?: unknown }).turns)) {
		throw new Fail('Input must be {"model": "...", "turns": [{"prompt": "...", "response": "..."}]}.');
	}
	const { model, turns } = data as { model?: unknown; turns: InputTurn[] };
	return {
		model: typeof model === "string" && model.trim() ? model.trim() : undefined,
		turns,
	};
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

function open(htmlview: string, file: string): void {
	const setsid = findExecutable("setsid", ["/usr/bin/setsid"]);
	const [command, args] = setsid ? [setsid, ["-f", htmlview, file]] : [htmlview, [file]];
	const child = spawn(command, args, { detached: true, stdio: "ignore" });
	child.on("error", () => {});
	child.unref();
}

async function main(): Promise<void> {
	const options = parseArgs(process.argv.slice(2));
	if (options === "help") {
		process.stdout.write(`${USAGE}\n`);
		return;
	}

	const parsed = parseTurnSpec(options.spec);
	if (!parsed.ok) throw new Fail(parsed.error);

	const input = readInput();
	const turns = indexTurns(input.turns);
	const resolved = resolveTurnSpec(parsed.spec, turns.length);
	if (!resolved.ok) throw new Fail(resolved.error);

	let htmlview: string | undefined;
	if (options.open) {
		htmlview = findExecutable("htmlview", [join(homedir(), ".local", "bin", "htmlview")]);
		if (!htmlview) throw new Fail("htmlview was not found on PATH or in ~/.local/bin.");
	}

	const now = new Date();
	const { html, context } = await renderPageFor({ turns, resolved, model: input.model, now });

	const dir = options.out ? resolve(options.out) : DEFAULT_DIR;
	const file = join(dir, `turns-${now.toISOString().replace(/[:.]/g, "-")}.html`);
	try {
		mkdirSync(dir, { recursive: true });
		writeFileSync(file, html, "utf8");
	} catch (error) {
		throw new Fail(`Could not write ${file}: ${(error as Error).message}`);
	}
	if (!options.out) prune(dir);

	if (htmlview) open(htmlview, file);
	process.stdout.write(`${JSON.stringify({ file, context })}\n`);
}

main().catch((error) => {
	const code = error instanceof Fail ? error.code : 1;
	process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
	process.exitCode = code;
});
