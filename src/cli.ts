import { readFileSync } from "node:fs";
import type { InputTurn } from "./render.ts";
import { Fail, renderAndOpen } from "./pipeline.ts";

const USAGE =
	"Usage: limpid [--spec <spec>] [--no-open] [--out <dir>]  (JSON turns on stdin)\n       limpid codex [spec] [--no-open] [--out <dir>]";

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

async function main(): Promise<void> {
	const argv = process.argv.slice(2);
	if (argv[0] === "codex") {
		const { runCodexCommand } = await import("./codex-command.ts");
		await runCodexCommand(argv.slice(1));
		return;
	}

	const options = parseArgs(argv);
	if (options === "help") {
		process.stdout.write(`${USAGE}\n`);
		return;
	}

	const input = readInput();
	const { file, context } = await renderAndOpen({ ...input, spec: options.spec, open: options.open, out: options.out });
	process.stdout.write(`${JSON.stringify({ file, context })}\n`);
}

main().catch((error) => {
	const code = error instanceof Fail ? error.code : 1;
	process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
	process.exitCode = code;
});
