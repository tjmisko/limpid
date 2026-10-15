/**
 * /read (and Ctrl-R) — render assistant responses as a styled HTML page and
 * open it. This extension only gathers the turns; rendering, the turn spec
 * (0, 3, 0..2, 1..=3, 2.., ..4, ..) and launching the viewer belong to the
 * `limpid` CLI.
 */

import { spawn } from "node:child_process";
import { homedir } from "node:os";
import { join } from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";

type ContentPart = { type?: string; text?: string };

type SessionEntryLike = {
	type?: string;
	message?: { role?: string; content?: unknown };
};

type Turn = { prompt: string; response: string };

type LimpidResult = { exitCode: number | null; stdout: string; stderr: string; spawnError?: Error };

// `limpid` on PATH first, then where install.sh links it by default.
const LIMPID_CANDIDATES = ["limpid", join(homedir(), ".local", "bin", "limpid")];
const LIMPID_TIMEOUT_MS = 30_000;

/** Text content of a message, whether stored as a string or a content array. */
function messageText(message: SessionEntryLike["message"]): string {
	if (!message) return "";
	if (typeof message.content === "string") return message.content.trim();
	const parts = Array.isArray(message.content) ? (message.content as ContentPart[]) : [];
	return parts
		.filter((part) => part?.type === "text" && typeof part.text === "string")
		.map((part) => part.text as string)
		.join("")
		.trim();
}

/** Group the branch into prompt → final-response turns, oldest first. */
function collectTurns(ctx: ExtensionContext): Turn[] {
	const branch = ctx.sessionManager.getBranch() as SessionEntryLike[];
	const turns: Turn[] = [];
	let current: Turn | undefined;

	for (const entry of branch) {
		if (entry?.type !== "message" || !entry.message) continue;
		const role = entry.message.role;

		if (role === "user") {
			if (current?.response) turns.push(current);
			current = { prompt: messageText(entry.message), response: "" };
			continue;
		}

		if (role !== "assistant" || !current) continue;
		// Keep the final answer of the turn, like /copy.
		const text = messageText(entry.message);
		if (text) current.response = text;
	}
	if (current?.response) turns.push(current);
	return turns;
}

function spawnLimpid(command: string, spec: string, input: string): Promise<LimpidResult> {
	return new Promise((resolve) => {
		let stdout = "";
		let stderr = "";
		const child = spawn(command, ["--spec", spec], { stdio: ["pipe", "pipe", "pipe"] });
		const timer = setTimeout(() => child.kill("SIGKILL"), LIMPID_TIMEOUT_MS);
		child.stdout.on("data", (chunk) => (stdout += chunk));
		child.stderr.on("data", (chunk) => (stderr += chunk));
		child.on("error", (spawnError) => {
			clearTimeout(timer);
			resolve({ exitCode: null, stdout, stderr, spawnError });
		});
		child.on("close", (exitCode) => {
			clearTimeout(timer);
			resolve({ exitCode, stdout, stderr });
		});
		child.stdin.on("error", () => {
			/* limpid may exit before reading stdin (e.g. a bad spec) */
		});
		child.stdin.end(input);
	});
}

async function runLimpid(spec: string, input: string): Promise<LimpidResult> {
	let result: LimpidResult | undefined;
	for (const command of LIMPID_CANDIDATES) {
		result = await spawnLimpid(command, spec, input);
		if (!result.spawnError) return result;
	}
	return result as LimpidResult;
}

/** Render `spec` for the current branch and report the outcome in the UI. */
async function openTurns(spec: string, ctx: ExtensionContext): Promise<void> {
	const model = ctx.model?.name ?? ctx.model?.id;
	const input = JSON.stringify({ model, turns: collectTurns(ctx) });
	const result = await runLimpid(spec, input);

	if (result.spawnError) {
		ctx.ui.notify("limpid was not found. Install it from https://github.com/tjmisko/limpid", "error");
		return;
	}
	if (result.exitCode !== 0) {
		const message = result.stderr.trim().split("\n")[0] || `limpid exited with ${result.exitCode}`;
		ctx.ui.notify(message, "warning");
		return;
	}

	try {
		const { context } = JSON.parse(result.stdout) as { file: string; context: string };
		ctx.ui.notify(`Opened ${context}.`, "info");
	} catch {
		ctx.ui.notify("limpid returned output that could not be parsed.", "error");
	}
}

export default function readExtension(pi: ExtensionAPI) {
	pi.registerCommand("read", {
		description: "Open a response, or a Rust-style range of turns, as a styled HTML page",
		handler: async (args, ctx) => {
			// If invoked mid-stream, wait for the turn to finish so we capture all of it.
			try {
				await ctx.waitForIdle();
			} catch {
				/* not streaming, or waiting is unavailable */
			}
			await openTurns(args.trim() || "0", ctx);
		},
	});

	// Ctrl-R is bound only inside the session picker, so it is free in the editor.
	pi.registerShortcut("ctrl+r", {
		description: "Open the latest response as a styled HTML page",
		handler: (ctx) => openTurns("0", ctx),
	});
}
