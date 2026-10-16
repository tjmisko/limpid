/** /read and Ctrl-R: pass the current branch to the Rust reader. */
import { spawn } from "node:child_process";
import { homedir } from "node:os";
import { join } from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";

type Result = { code: number | null; stdout: string; stderr: string; missing?: boolean };
const BIN = join(homedir(), ".local", "bin");
const QUESTION_INSTRUCTION = "Put questions that need the user's answer under a `## Questions` heading as a numbered list; give suggested answers as nested `- ` items.";

function run(command: string, args: string[], input = ""): Promise<Result> {
	return new Promise(resolve => {
		let stdout = "";
		let stderr = "";
		const child = spawn(command, args, { stdio: ["pipe", "pipe", "pipe"] });
		const timer = setTimeout(() => child.kill("SIGKILL"), 30_000);
		child.stdout.on("data", chunk => (stdout += chunk));
		child.stderr.on("data", chunk => (stderr += chunk));
		child.on("error", () => {
			clearTimeout(timer);
			resolve({ code: null, stdout, stderr, missing: true });
		});
		child.on("close", code => {
			clearTimeout(timer);
			resolve({ code, stdout, stderr });
		});
		child.stdin.on("error", () => {});
		child.stdin.end(input);
	});
}

async function command(name: string, args: string[], input = ""): Promise<Result> {
	let result = await run(name, args, input);
	if (result.missing) result = await run(join(BIN, name), args, input);
	return result;
}

async function openTurns(spec: string, ctx: ExtensionContext): Promise<void> {
	const model = ctx.model?.name ?? ctx.model?.id;
	const input = JSON.stringify({
		model,
		session_id: ctx.sessionManager.getSessionId(),
		branch: ctx.sessionManager.getBranch(),
	});
	const page = await command("limpid-core", ["pi", spec], input);
	if (page.code !== 0) {
		process.stderr.write(`limpid: ${page.stderr.trim().split("\n")[0] || "could not render the turn"}\n`);
		return;
	}
	const opened = await command("limpid", [page.stdout.trim()]);
	if (opened.code !== 0) process.stderr.write(`limpid: ${opened.stderr.trim().split("\n")[0] || "could not open the page"}\n`);
}

export default function readExtension(pi: ExtensionAPI) {
	pi.on("before_agent_start", event => {
		if (!event.systemPromptOptions.promptGuidelines.includes(QUESTION_INSTRUCTION)) {
			event.systemPromptOptions.promptGuidelines.push(QUESTION_INSTRUCTION);
		}
	});
	pi.registerCommand("read", {
		description: "Open a response, or a Rust-style range of turns, as a styled HTML page",
		handler: async (args, ctx) => {
			try { await ctx.waitForIdle(); } catch { /* not streaming */ }
			await openTurns(args.trim() || "0", ctx);
		},
	});
	pi.registerShortcut("ctrl+r", {
		description: "Open the latest response as a styled HTML page",
		handler: ctx => openTurns("0", ctx),
	});
}
