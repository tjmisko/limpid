import { closeSync, openSync, readFileSync, readSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";

export type CodexTurns = { model?: string; turns: Array<{ prompt: string; response: string }> };

const MAX_FALLBACK_CANDIDATES = 200;
const FIRST_LINE_MAX_BYTES = 1 << 20;
const INJECTED_PREFIXES = ["# AGENTS.md instructions", "<environment_context>", "<user_shell_command>"];

function listDirs(path: string): string[] {
	try {
		return readdirSync(path, { withFileTypes: true })
			.filter((entry) => entry.isDirectory())
			.map((entry) => entry.name)
			.sort()
			.reverse();
	} catch {
		return [];
	}
}

function listRollouts(dir: string): string[] {
	try {
		return readdirSync(dir).filter((name) => name.startsWith("rollout-") && name.endsWith(".jsonl"));
	} catch {
		return [];
	}
}

function* walkRollouts(root: string): Generator<string> {
	for (const year of listDirs(root)) {
		for (const month of listDirs(join(root, year))) {
			for (const day of listDirs(join(root, year, month))) {
				const dir = join(root, year, month, day);
				for (const name of listRollouts(dir).sort().reverse()) yield join(dir, name);
			}
		}
	}
}

function readFirstLine(path: string): string | undefined {
	let fd: number | undefined;
	try {
		fd = openSync(path, "r");
		const chunks: Buffer[] = [];
		const chunk = Buffer.alloc(8192);
		let total = 0;
		while (total < FIRST_LINE_MAX_BYTES) {
			const read = readSync(fd, chunk, 0, chunk.length, null);
			if (read === 0) break;
			const newline = chunk.subarray(0, read).indexOf(10);
			if (newline >= 0) {
				chunks.push(Buffer.from(chunk.subarray(0, newline)));
				break;
			}
			chunks.push(Buffer.from(chunk.subarray(0, read)));
			total += read;
		}
		return Buffer.concat(chunks).toString("utf8");
	} catch {
		return undefined;
	} finally {
		if (fd !== undefined) closeSync(fd);
	}
}

function findByThreadId(root: string, id: string): string | undefined {
	const suffix = `-${id}.jsonl`;
	for (const path of walkRollouts(root)) {
		if (path.endsWith(suffix)) return path;
	}
	return undefined;
}

function findByCwd(root: string, cwd: string): string | undefined {
	const candidates: Array<{ path: string; mtime: number }> = [];
	for (const path of walkRollouts(root)) {
		try {
			candidates.push({ path, mtime: statSync(path).mtimeMs });
		} catch {
			continue;
		}
	}
	candidates.sort((a, b) => b.mtime - a.mtime);
	for (const { path } of candidates.slice(0, MAX_FALLBACK_CANDIDATES)) {
		const line = readFirstLine(path);
		if (!line) continue;
		try {
			const record = JSON.parse(line);
			if (record?.type === "session_meta" && record.payload?.cwd === cwd) return path;
		} catch {
			continue;
		}
	}
	return undefined;
}

export function findCodexRollout(options: {
	env: Record<string, string | undefined>;
	cwd: string;
	home: string;
}): string | undefined {
	const { env, cwd, home } = options;
	const root = join(env.CODEX_HOME || join(home, ".codex"), "sessions");
	const id = env.CODEX_THREAD_ID || env.CODEX_SESSION_ID;
	if (id) {
		const found = findByThreadId(root, id);
		if (found) return found;
	}
	return findByCwd(root, cwd);
}

function isInjected(text: string): boolean {
	const trimmed = text.trimStart();
	return INJECTED_PREFIXES.some((prefix) => trimmed.startsWith(prefix));
}

function contentTexts(content: unknown, type: string): string[] {
	if (!Array.isArray(content)) return [];
	const texts: string[] = [];
	for (const item of content) {
		if (item?.type === type && typeof item.text === "string") texts.push(item.text);
	}
	return texts;
}

type OpenTurn = {
	prompt?: string;
	lastAgentMessage?: string;
	finalAnswer?: string;
	lastAssistant?: string;
};

export function parseCodexRollout(jsonl: string): CodexTurns {
	const turns: CodexTurns["turns"] = [];
	let model: string | undefined;
	let turn: OpenTurn | undefined;

	const close = () => {
		if (!turn) return;
		const prompt = (turn.prompt ?? "").trim();
		const response = (turn.lastAgentMessage || turn.finalAnswer || turn.lastAssistant || "").trim();
		if (prompt && response) turns.push({ prompt, response });
		turn = undefined;
	};

	for (const line of jsonl.split("\n")) {
		if (!line.trim()) continue;
		let record: any;
		try {
			record = JSON.parse(line);
		} catch {
			continue;
		}
		const payload = record?.payload;
		if (!payload || typeof payload !== "object") continue;

		if (record.type === "turn_context") {
			if (typeof payload.model === "string" && payload.model) model = payload.model;
			continue;
		}
		if (record.type === "event_msg") {
			if (payload.type === "task_started") {
				close();
				turn = {};
			} else if (payload.type === "task_complete") {
				if (turn && typeof payload.last_agent_message === "string") turn.lastAgentMessage = payload.last_agent_message;
				close();
			}
			continue;
		}
		if (record.type !== "response_item" || payload.type !== "message") continue;

		if (payload.role === "user") {
			const text = contentTexts(payload.content, "input_text")
				.filter((t) => !isInjected(t))
				.join("\n\n");
			if (!text.trim()) continue;
			// A prompt outside any turn, or a second prompt after a response, starts a new turn.
			if (turn && turn.prompt !== undefined && (turn.finalAnswer || turn.lastAssistant)) close();
			turn ??= {};
			turn.prompt ??= text;
		} else if (payload.role === "assistant" && turn) {
			const text = contentTexts(payload.content, "output_text").join("\n\n");
			if (!text.trim()) continue;
			turn.lastAssistant = text;
			if (payload.phase === "final_answer") turn.finalAnswer = text;
		}
	}
	close();
	return model ? { model, turns } : { turns };
}
