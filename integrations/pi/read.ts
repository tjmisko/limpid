/** /read and Ctrl-R: pass the current branch to the Rust reader. */
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { chmodSync, mkdirSync, unlinkSync } from "node:fs";
import { createServer, type Server } from "node:net";
import { homedir } from "node:os";
import { join } from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";

type Result = { code: number | null; stdout: string; stderr: string; missing?: boolean };
const BIN = join(homedir(), ".local", "bin");
const QUESTION_INSTRUCTION = "Put questions that need the user's answer under a `## Questions` heading as a numbered list; give suggested answers as nested `- ` items.";

let feedbackServer: Server | undefined;
let feedbackSock: string | undefined;

function feedbackDirectory(): string {
	if (process.env.XDG_RUNTIME_DIR) return join(process.env.XDG_RUNTIME_DIR, "limpid");
	return join(homedir(), ".cache", "limpid", "run");
}

function bindFeedback(pi: ExtensionAPI, ctx: ExtensionContext): void {
	feedbackServer?.close();
	feedbackServer = undefined;
	if (feedbackSock) {
		try { unlinkSync(feedbackSock); } catch { /* already gone */ }
		feedbackSock = undefined;
	}
	const session = ctx.sessionManager.getSessionId();
	const digest = createHash("sha256").update(session).digest("hex").slice(0, 16);
	const directory = feedbackDirectory();
	mkdirSync(directory, { recursive: true, mode: 0o700 });
	const socketPath = join(directory, `${digest}.sock`);
	try { unlinkSync(socketPath); } catch { /* stale or absent */ }
	const server = createServer(connection => {
		let buffer = "";
		connection.setEncoding("utf8");
		connection.on("data", chunk => {
			buffer += chunk;
			const newline = buffer.indexOf("\n");
			if (newline < 0) return;
			const line = buffer.slice(0, newline);
			connection.end('{"ok":true}\n');
			try {
				const signal = JSON.parse(line);
				void pi.sendMessage(
					{customType: "limpid-feedback", content: signal.message, display: true},
					ctx.isIdle() ? {triggerTurn: true} : {deliverAs: "followUp"},
				);
			} catch { /* ignore malformed frames */ }
		});
	});
	server.on("error", () => {});
	server.listen(socketPath, () => {
		try { chmodSync(socketPath, 0o600); } catch { /* best effort */ }
	});
	server.unref();
	feedbackServer = server;
	feedbackSock = socketPath;
}

function releaseFeedback(): void {
	feedbackServer?.close();
	feedbackServer = undefined;
	if (feedbackSock) {
		try { unlinkSync(feedbackSock); } catch { /* already gone */ }
		feedbackSock = undefined;
	}
}

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

async function openTurns(pi: ExtensionAPI, spec: string, ctx: ExtensionContext): Promise<void> {
	const model = ctx.model?.name ?? ctx.model?.id;
	const input = JSON.stringify({
		model,
		session_id: ctx.sessionManager.getSessionId(),
		branch: ctx.sessionManager.getBranch(),
		feedback_sock: feedbackSock,
	});
	const page = await command("limpid-core", ["pi", spec], input);
	if (page.code !== 0) {
		process.stderr.write(`limpid: ${page.stderr.trim().split("\n")[0] || "could not render the turn"}\n`);
		return;
	}
	const path = page.stdout.trim();
	const opened = await command("limpid", [path]);
	if (opened.code !== 0) {
		process.stderr.write(`limpid: ${opened.stderr.trim().split("\n")[0] || "could not open the page"}\n`);
		return;
	}
	const note = `The user opened limpid page ${path}. Pull saved answers and comments with limpid-core feedback ${path} when they are relevant.`;
	void pi.sendMessage(
		{customType: "limpid-open", content: note, display: true},
		{triggerTurn: false},
	);
}

export default function readExtension(pi: ExtensionAPI) {
	pi.on("before_agent_start", event => {
		if (!event.systemPromptOptions.promptGuidelines.includes(QUESTION_INSTRUCTION)) {
			event.systemPromptOptions.promptGuidelines.push(QUESTION_INSTRUCTION);
		}
	});
	pi.on("session_start", (_event, ctx) => bindFeedback(pi, ctx));
	pi.on("session_shutdown", () => releaseFeedback());
	pi.registerCommand("read", {
		description: "Open a response, or a Rust-style range of turns, as a styled HTML page",
		handler: async (args, ctx) => {
			try { await ctx.waitForIdle(); } catch { /* not streaming */ }
			await openTurns(pi, args.trim() || "0", ctx);
		},
	});
	pi.registerShortcut("ctrl+r", {
		description: "Open the latest response as a styled HTML page",
		handler: ctx => openTurns(pi, "0", ctx),
	});
}
