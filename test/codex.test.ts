import { after, test } from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { findCodexRollout, parseCodexRollout } from "../src/codex.ts";

const FIXTURES = join(dirname(fileURLToPath(import.meta.url)), "fixtures", "codex");
const fixture = (name: string) => parseCodexRollout(readFileSync(join(FIXTURES, `${name}.jsonl`), "utf8"));

const roots: string[] = [];
after(() => roots.forEach((root) => rmSync(root, { recursive: true, force: true })));

function fakeHome(): string {
	const home = mkdtempSync(join(tmpdir(), "limpid-codex-"));
	roots.push(home);
	return home;
}

function addRollout(codexDir: string, day: string, id: string, cwd: string, mtime?: number): string {
	const dir = join(codexDir, "sessions", ...day.split("-"));
	mkdirSync(dir, { recursive: true });
	const path = join(dir, `rollout-${day}T10-00-00-${id}.jsonl`);
	writeFileSync(path, JSON.stringify({ timestamp: "t", type: "session_meta", payload: { id, cwd } }) + "\n" + '{"type":"event_msg"}\n');
	if (mtime !== undefined) utimesSync(path, mtime, mtime);
	return path;
}

test("should return both turns oldest first when parsing a simple session", () => {
	assert.deepEqual(fixture("simple"), {
		model: "m-1",
		turns: [
			{ prompt: "first prompt", response: "first answer" },
			{ prompt: "second prompt", response: "second answer" },
		],
	});
});

test("should keep only the real prompt when AGENTS.md and environment context share its message", () => {
	assert.deepEqual(fixture("injected").turns, [{ prompt: "real prompt", response: "ok" }]);
});

test("should ignore developer messages when extracting prompts", () => {
	assert.deepEqual(fixture("developer").turns, [{ prompt: "hello", response: "hi" }]);
});

test("should use the final_answer when commentary surrounds it and last_agent_message is missing", () => {
	assert.deepEqual(fixture("commentary").turns, [{ prompt: "do it", response: "the final text" }]);
});

test("should prefer last_agent_message when task_complete provides it", () => {
	assert.deepEqual(fixture("prefers-last-agent-message").turns, [{ prompt: "q", response: "from task_complete" }]);
});

test("should drop the turn when it is a user shell command with empty last_agent_message", () => {
	assert.deepEqual(fixture("shell-turn").turns, [{ prompt: "real prompt", response: "real answer" }]);
});

test("should include a trailing in-progress turn when it already has a final_answer", () => {
	assert.deepEqual(fixture("in-progress").turns, [
		{ prompt: "done prompt", response: "done answer" },
		{ prompt: "pending prompt", response: "partial final" },
	]);
});

test("should drop a trailing in-progress turn when it has no assistant output", () => {
	assert.deepEqual(fixture("in-progress-no-answer").turns, []);
});

test("should skip malformed and non-object lines when parsing", () => {
	assert.deepEqual(fixture("malformed").turns, [{ prompt: "survivor", response: "fine" }]);
});

test("should return no turns and no model when the file is empty", () => {
	assert.deepEqual(fixture("empty"), { turns: [] });
});

test("should return no turns when given only whitespace", () => {
	assert.deepEqual(parseCodexRollout("\n  \n\n"), { turns: [] });
});

test("should report the latest turn_context model when it changes mid-session", () => {
	assert.equal(fixture("model-change").model, "m-new");
});

test("should preserve unicode, quotes, backslashes and code fences when parsing", () => {
	const [turn] = fixture("unicode").turns;
	assert.equal(turn.prompt, 'naïve 日本語 🙂 "quoted" \\ back');
	assert.equal(turn.response, "réponse ✓ \n\n```js\nconst a = 1;\n```".trim());
});

test("should start turns from user prompts when task_started records are missing", () => {
	assert.deepEqual(fixture("no-task-started").turns, [
		{ prompt: "orphan one", response: "answer one" },
		{ prompt: "orphan two", response: "answer two" },
	]);
});

test("should ignore unknown record types and non-message response items when parsing", () => {
	assert.deepEqual(fixture("unknown-records").turns, [{ prompt: "p", response: "a" }]);
});

test("should find the rollout by CODEX_THREAD_ID when it is set", () => {
	const home = fakeHome();
	const want = addRollout(join(home, ".codex"), "2026-03-04", "aaaa-1111", "/other");
	addRollout(join(home, ".codex"), "2026-03-05", "bbbb-2222", "/work");
	assert.equal(findCodexRollout({ env: { CODEX_THREAD_ID: "aaaa-1111" }, cwd: "/work", home }), want);
});

test("should find the rollout by CODEX_SESSION_ID when CODEX_THREAD_ID is unset", () => {
	const home = fakeHome();
	const want = addRollout(join(home, ".codex"), "2026-03-04", "aaaa-1111", "/other");
	assert.equal(findCodexRollout({ env: { CODEX_SESSION_ID: "aaaa-1111" }, cwd: "/work", home }), want);
});

test("should not match a thread id that is only a suffix of another id when searching", () => {
	const home = fakeHome();
	addRollout(join(home, ".codex"), "2026-03-04", "xaaaa-1111", "/other");
	assert.equal(findCodexRollout({ env: { CODEX_THREAD_ID: "aaaa-1111" }, cwd: "/work", home }), undefined);
});

test("should read sessions from CODEX_HOME when it overrides the default", () => {
	const home = fakeHome();
	const codexHome = join(home, "custom");
	const want = addRollout(codexHome, "2026-03-04", "aaaa-1111", "/work");
	addRollout(join(home, ".codex"), "2026-03-04", "aaaa-1111", "/work");
	assert.equal(findCodexRollout({ env: { CODEX_HOME: codexHome, CODEX_THREAD_ID: "aaaa-1111" }, cwd: "/work", home }), want);
});

test("should pick the newest rollout with a matching cwd when no id is set", () => {
	const home = fakeHome();
	const codex = join(home, ".codex");
	addRollout(codex, "2026-03-01", "old-1", "/work", 1_000_000);
	const want = addRollout(codex, "2026-03-02", "new-2", "/work", 2_000_000);
	addRollout(codex, "2026-03-03", "newest-3", "/elsewhere", 3_000_000);
	assert.equal(findCodexRollout({ env: {}, cwd: "/work", home }), want);
});

test("should fall back to the cwd match when CODEX_THREAD_ID has no rollout file", () => {
	const home = fakeHome();
	const want = addRollout(join(home, ".codex"), "2026-03-02", "new-2", "/work");
	assert.equal(findCodexRollout({ env: { CODEX_THREAD_ID: "missing-id" }, cwd: "/work", home }), want);
});

test("should return undefined when no rollout matches the cwd", () => {
	const home = fakeHome();
	addRollout(join(home, ".codex"), "2026-03-02", "new-2", "/elsewhere");
	assert.equal(findCodexRollout({ env: {}, cwd: "/work", home }), undefined);
});

test("should return undefined when the sessions directory does not exist", () => {
	assert.equal(findCodexRollout({ env: { CODEX_THREAD_ID: "x" }, cwd: "/work", home: fakeHome() }), undefined);
});

test("should skip rollouts with an unparseable first line when falling back to cwd", () => {
	const home = fakeHome();
	const codex = join(home, ".codex");
	const want = addRollout(codex, "2026-03-01", "good-1", "/work", 1_000_000);
	const bad = addRollout(codex, "2026-03-02", "bad-2", "/work", 2_000_000);
	writeFileSync(bad, "not json\n");
	utimesSync(bad, 2_000_000, 2_000_000);
	assert.equal(findCodexRollout({ env: {}, cwd: "/work", home }), want);
});
