import { after, before, test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const CLI = join(dirname(fileURLToPath(import.meta.url)), "..", "src", "cli.ts");
const SCRATCH = join(homedir(), ".cache", "limpid-test");
let out: string;

before(() => {
	mkdirSync(SCRATCH, { recursive: true });
	out = mkdtempSync(join(SCRATCH, "cli-"));
});
after(() => rmSync(SCRATCH, { recursive: true, force: true }));

function run(args: string[], stdin: string) {
	const result = spawnSync("node", ["--no-warnings=ExperimentalWarning", CLI, ...args], {
		input: stdin,
		encoding: "utf8",
	});
	return { code: result.status, stdout: result.stdout, stderr: result.stderr };
}

const turns = (...responses: string[]) =>
	JSON.stringify({ model: "test-model", turns: responses.map((response, i) => ({ prompt: `p${i}`, response })) });

test("should print a single JSON line and exit 0 when rendering the newest turn", () => {
	const result = run(["--no-open", "--out", out], turns("# One", "# Two"));
	assert.equal(result.code, 0, result.stderr);
	assert.equal(result.stdout.trim().split("\n").length, 1);
	const parsed = JSON.parse(result.stdout);
	assert.deepEqual(Object.keys(parsed).sort(), ["context", "file"]);
	assert.equal(parsed.context, "turn 0");
	assert.ok(parsed.file.startsWith(out) && /turns-.*\.html$/.test(parsed.file));
	const html = readFileSync(parsed.file, "utf8");
	assert.match(html, /<title>Two<\/title>/);
	assert.doesNotMatch(html, /<title>One/);
});

test("should select the requested turn when --spec is a number", () => {
	const result = run(["--no-open", "--out", out, "--spec", "1"], turns("# One", "# Two"));
	assert.equal(result.code, 0, result.stderr);
	assert.equal(JSON.parse(result.stdout).context, "turn 1");
});

test("should accept --spec=value when written with an equals sign", () => {
	const result = run(["--no-open", `--out=${out}`, "--spec=0..2"], turns("a", "b", "c"));
	assert.equal(result.code, 0, result.stderr);
	assert.equal(JSON.parse(result.stdout).context, "turns 0–1");
});

test("should show prompts oldest first when rendering a range", () => {
	const result = run(["--no-open", "--out", out, "--spec", "0..3"], turns("# A", "# B", "# C"));
	const html = readFileSync(JSON.parse(result.stdout).file, "utf8");
	assert.ok(html.indexOf("Turn 2") < html.indexOf("Turn 1"));
	assert.ok(html.indexOf("Turn 1") < html.indexOf("Turn 0"));
	assert.match(html, /most recent/);
	assert.match(html, /class="prompt-text">p0</);
});

test("should exit 1 with a message when the turn does not exist", () => {
	const result = run(["--no-open", "--out", out, "--spec", "5"], turns("a", "b", "c"));
	assert.equal(result.code, 1);
	assert.equal(result.stdout, "");
	assert.equal(result.stderr.trim(), "Turn 5 does not exist (only 3 available).");
});

test("should exit 1 when the spec is invalid", () => {
	const result = run(["--no-open", "--out", out, "--spec", "x"], turns("a"));
	assert.equal(result.code, 1);
	assert.equal(result.stderr.trim(), 'Invalid argument "x". Use 0, 3, 0..2, 1..=3, 2.., ..4, or ..');
});

test("should exit 1 when the range exceeds the maximum", () => {
	const result = run(["--no-open", "--out", out, "--spec", "0..11"], turns("a", "b", "c"));
	assert.equal(result.code, 1);
	assert.equal(result.stderr.trim(), "Range covers 11 turns; the maximum is 10.");
});

test("should exit 1 when stdin is malformed JSON", () => {
	const result = run(["--no-open", "--out", out], "{not json");
	assert.equal(result.code, 1);
	assert.match(result.stderr, /Malformed JSON/);
	assert.equal(result.stdout, "");
});

test("should exit 1 when stdin lacks a turns array", () => {
	const result = run(["--no-open", "--out", out], JSON.stringify({ model: "m" }));
	assert.equal(result.code, 1);
});

test("should exit 1 with no response message when turns is empty", () => {
	const result = run(["--no-open", "--out", out], JSON.stringify({ turns: [] }));
	assert.equal(result.code, 1);
	assert.equal(result.stderr.trim(), "No response to read yet.");
});

test("should ignore turns when the response is blank or whitespace", () => {
	const result = run(["--no-open", "--out", out, "--spec", "1"], turns("# Keep", "  \n\t ", "# Newest", ""));
	assert.equal(result.code, 0, result.stderr);
	const html = readFileSync(JSON.parse(result.stdout).file, "utf8");
	assert.match(html, /<title>Keep<\/title>/);
	const none = run(["--no-open", "--out", out], turns("  ", "\n"));
	assert.equal(none.code, 1);
	assert.equal(none.stderr.trim(), "No response to read yet.");
});

test("should omit the model from the page when the input has none", () => {
	const result = run(["--no-open", "--out", out], JSON.stringify({ turns: [{ prompt: "p", response: "x" }] }));
	const html = readFileSync(JSON.parse(result.stdout).file, "utf8");
	assert.match(html, /<p class="meta">turn 0 · /);
});

test("should exit 2 with usage when a flag is unknown", () => {
	const result = run(["--bogus"], turns("a"));
	assert.equal(result.code, 2);
	assert.match(result.stderr, /Usage: limpid/);
	assert.equal(result.stdout, "");
});

test("should exit 2 when a flag is missing its value", () => {
	assert.equal(run(["--spec"], turns("a")).code, 2);
	assert.equal(run(["--out"], turns("a")).code, 2);
});

test("should create the output directory when it does not exist", () => {
	const nested = join(out, "a", "b");
	const result = run(["--no-open", "--out", nested], turns("a"));
	assert.equal(result.code, 0, result.stderr);
	assert.ok(existsSync(JSON.parse(result.stdout).file));
});

test("should not prune the directory when --out is given", () => {
	const dir = join(out, "noprune");
	mkdirSync(dir);
	for (let i = 0; i < 55; i++) writeFileSync(join(dir, `old-${i}.html`), "x");
	run(["--no-open", "--out", dir], turns("a"));
	assert.equal(readdirSync(dir).length, 56);
});

function runWithHome(home: string, args: string[], stdin: string, path?: string) {
	const result = spawnSync(process.execPath, ["--no-warnings=ExperimentalWarning", CLI, ...args], {
		input: stdin,
		encoding: "utf8",
		env: { ...process.env, HOME: home, ...(path === undefined ? {} : { PATH: path }) },
	});
	return { code: result.status, stdout: result.stdout, stderr: result.stderr };
}

function fakeViewer(dir: string, name: string, marker: string): string {
	mkdirSync(dir, { recursive: true });
	const script = join(dir, name);
	writeFileSync(script, `#!/bin/sh\necho "$@" >> '${marker}'\n`);
	chmodSync(script, 0o755);
	return script;
}

function waitFor(path: string): string | undefined {
	for (let i = 0; i < 50; i++) {
		if (existsSync(path)) return readFileSync(path, "utf8");
		spawnSync("sleep", ["0.1"]);
	}
	return undefined;
}

function runWithEnv(env: Record<string, string>, args: string[], stdin: string) {
	const result = spawnSync(process.execPath, ["--no-warnings=ExperimentalWarning", CLI, ...args], {
		input: stdin,
		encoding: "utf8",
		env: { ...process.env, LIMPID_VIEWER: "", ...env },
	});
	return { code: result.status, stdout: result.stdout, stderr: result.stderr };
}

test("should exit 1 when no viewer can be found and opening is requested", () => {
	const home = mkdtempSync(join(SCRATCH, "home-"));
	const result = runWithHome(home, [], turns("a"), "/nonexistent");
	assert.equal(result.code, 1);
	assert.match(result.stderr, /No viewer found/);
	assert.equal(result.stdout, "");
});

test("should launch LIMPID_VIEWER with its arguments and the file when it is set", () => {
	const dir = mkdtempSync(join(SCRATCH, "viewer-"));
	const marker = join(dir, "marker");
	const script = fakeViewer(dir, "myviewer", marker);
	const result = runWithEnv({ LIMPID_VIEWER: `${script} --flag`, PATH: "/nonexistent" }, ["--out", out], turns("a"));
	assert.equal(result.code, 0, result.stderr);
	const file = JSON.parse(result.stdout).file as string;
	assert.equal(waitFor(marker)?.trim(), `--flag ${file}`);
});

test("should prefer htmlview over xdg-open when LIMPID_VIEWER is unset", () => {
	const dir = mkdtempSync(join(SCRATCH, "viewer-"));
	const htmlMarker = join(dir, "html");
	const xdgMarker = join(dir, "xdg");
	fakeViewer(join(dir, "bin"), "htmlview", htmlMarker);
	fakeViewer(join(dir, "bin"), "xdg-open", xdgMarker);
	const result = runWithEnv({ PATH: join(dir, "bin") }, ["--out", out], turns("a"));
	assert.equal(result.code, 0, result.stderr);
	assert.ok(waitFor(htmlMarker));
	assert.ok(!existsSync(xdgMarker));
});

test("should fall back to xdg-open when htmlview is absent", { skip: process.platform === "darwin" }, () => {
	const dir = mkdtempSync(join(SCRATCH, "viewer-"));
	const marker = join(dir, "xdg");
	fakeViewer(join(dir, "bin"), "xdg-open", marker);
	const home = mkdtempSync(join(SCRATCH, "home-"));
	const result = runWithEnv({ PATH: join(dir, "bin"), HOME: home }, ["--out", out], turns("a"));
	assert.equal(result.code, 0, result.stderr);
	assert.ok(waitFor(marker));
});

test("should use ~/.local/bin/htmlview when it is not on PATH", () => {
	const home = mkdtempSync(join(SCRATCH, "home-"));
	const marker = join(home, "marker");
	fakeViewer(join(home, ".local", "bin"), "htmlview", marker);
	const result = runWithEnv({ PATH: "/nonexistent", HOME: home }, ["--out", out], turns("a"));
	assert.equal(result.code, 0, result.stderr);
	assert.ok(waitFor(marker));
});

test("should write under ~/.cache/limpid and keep the newest 50 files when using the default directory", () => {
	const home = mkdtempSync(join(SCRATCH, "home-"));
	const dir = join(home, ".cache", "limpid");
	mkdirSync(dir, { recursive: true });
	for (let i = 0; i < 60; i++) {
		const file = join(dir, `old-${i}.html`);
		writeFileSync(file, "x");
		const time = new Date(Date.now() - (100 - i) * 60_000);
		utimesSync(file, time, time);
	}
	writeFileSync(join(dir, "keep.txt"), "not html");
	const result = runWithHome(home, ["--no-open"], turns("a"));
	assert.equal(result.code, 0, result.stderr);
	const file = JSON.parse(result.stdout).file as string;
	assert.ok(file.startsWith(dir));
	const html = readdirSync(dir).filter((name) => name.endsWith(".html"));
	assert.equal(html.length, 50);
	assert.ok(html.includes(file.slice(dir.length + 1)));
	assert.ok(html.includes("old-59.html"));
	assert.ok(!html.includes("old-0.html"));
	assert.ok(existsSync(join(dir, "keep.txt")));
});
