import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { findCodexRollout, parseCodexRollout } from "./codex.ts";
import { Fail, renderAndOpen } from "./pipeline.ts";

const USAGE = "Usage: limpid codex [spec] [--no-open] [--out <dir>]";

type CodexOptions = { spec: string; open: boolean; out?: string };

export function parseCodexArgs(argv: string[]): CodexOptions | "help" {
	const options: CodexOptions = { spec: "0", open: true };
	let specSeen = false;
	for (let i = 0; i < argv.length; i++) {
		const arg = argv[i];
		if (arg === "-h" || arg === "--help") return "help";
		if (arg === "--no-open") {
			options.open = false;
		} else if (arg === "--out") {
			const value = argv[++i];
			if (value === undefined) throw new Fail(`limpid: --out needs a value.\n${USAGE}`, 2);
			options.out = value;
		} else if (arg.startsWith("--out=")) {
			options.out = arg.slice("--out=".length);
		} else if (!arg.startsWith("-") && !specSeen) {
			options.spec = arg;
			specSeen = true;
		} else {
			throw new Fail(`limpid: unknown argument "${arg}".\n${USAGE}`, 2);
		}
	}
	return options;
}

/** `limpid codex [spec]`: render the current directory's Codex session. Prints one short line. */
export async function runCodexCommand(argv: string[]): Promise<void> {
	const options = parseCodexArgs(argv);
	if (options === "help") {
		process.stdout.write(`${USAGE}\n`);
		return;
	}

	const rollout = findCodexRollout({ env: process.env, cwd: process.cwd(), home: homedir() });
	if (!rollout) throw new Fail("limpid: no Codex session found for this directory");

	let transcript;
	try {
		transcript = parseCodexRollout(readFileSync(rollout, "utf8"));
	} catch (error) {
		throw new Fail(`limpid: could not read the Codex session: ${(error as Error).message}`);
	}

	try {
		const { context } = await renderAndOpen({ ...transcript, ...options });
		process.stdout.write(`limpid: ${options.open ? "opened" : "rendered"} ${context}\n`);
	} catch (error) {
		if (error instanceof Fail && !error.message.startsWith("limpid:")) {
			throw new Fail(`limpid: ${error.message}`, error.code);
		}
		throw error;
	}
}
