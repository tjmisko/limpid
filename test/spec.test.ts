import { test } from "node:test";
import assert from "node:assert/strict";
import { parseTurnSpec, resolveTurnSpec } from "../src/spec.ts";

function parse(text: string) {
	const result = parseTurnSpec(text);
	assert.ok(result.ok, `expected "${text}" to parse`);
	return result.spec;
}

test("should select the newest turn when the spec is empty", () => {
	assert.deepEqual(parse(""), { kind: "single", index: 0 });
});

test("should parse a single index when the spec is a number", () => {
	assert.deepEqual(parse("0"), { kind: "single", index: 0 });
	assert.deepEqual(parse("3"), { kind: "single", index: 3 });
});

test("should ignore surrounding whitespace when the spec is padded", () => {
	assert.deepEqual(parse(" 2 "), { kind: "single", index: 2 });
});

test("should parse an exclusive range when the spec uses ..", () => {
	assert.deepEqual(parse("0..2"), { kind: "range", start: 0, end: 2, inclusive: false });
});

test("should parse an inclusive range when the spec uses ..=", () => {
	assert.deepEqual(parse("1..=3"), { kind: "range", start: 1, end: 3, inclusive: true });
});

test("should parse open-ended ranges when a bound is omitted", () => {
	assert.deepEqual(parse("2.."), { kind: "range", start: 2, end: undefined, inclusive: false });
	assert.deepEqual(parse("..4"), { kind: "range", start: undefined, end: 4, inclusive: false });
	assert.deepEqual(parse(".."), { kind: "range", inclusive: false });
});

test("should reject the spec when it is malformed", () => {
	for (const bad of ["abc", "-1", "1..2..3", "1...3", "=3", "1.5", "0..x"]) {
		const result = parseTurnSpec(bad);
		assert.equal(result.ok, false, bad);
		if (!result.ok) {
			assert.equal(result.error, `Invalid argument "${bad}". Use 0, 3, 0..2, 1..=3, 2.., ..4, or ..`);
		}
	}
});

function resolve(text: string, total: number) {
	return resolveTurnSpec(parse(text), total);
}

test("should return the index when a single turn exists", () => {
	assert.deepEqual(resolve("2", 3), { ok: true, kind: "single", indices: [2] });
});

test("should fail when a single index is out of range", () => {
	assert.deepEqual(resolve("5", 3), { ok: false, error: "Turn 5 does not exist (only 3 available)." });
});

test("should fail when the range start is out of range", () => {
	assert.deepEqual(resolve("4..6", 3), { ok: false, error: "Turn 4 does not exist (only 3 available)." });
});

test("should fail when the range is empty", () => {
	for (const empty of ["1..1", "2..1"]) {
		assert.deepEqual(resolve(empty, 5), { ok: false, error: "Empty range." }, empty);
	}
});

test("should fail when an empty range starts in bounds", () => {
	assert.deepEqual(resolve("3..3", 5), { ok: false, error: "Empty range." });
	assert.deepEqual(resolve("3..1", 5), { ok: false, error: "Empty range." });
});

test("should reject an 11 turn range when fewer turns exist", () => {
	assert.deepEqual(resolve("0..11", 3), {
		ok: false,
		error: "Range covers 11 turns; the maximum is 10.",
	});
});

test("should accept a range of exactly 10 turns when using ..=9", () => {
	const result = resolve("..=9", 12);
	assert.ok(result.ok);
	assert.deepEqual(result.indices, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
});

test("should reject the full range when more than 10 turns exist", () => {
	assert.deepEqual(resolve("..", 15), {
		ok: false,
		error: "Range covers 15 turns; the maximum is 10.",
	});
});

test("should clamp the range end when it exceeds the turn count", () => {
	assert.deepEqual(resolve("2..", 5), { ok: true, kind: "range", indices: [2, 3, 4] });
	assert.deepEqual(resolve("0..8", 3), { ok: true, kind: "range", indices: [0, 1, 2] });
});

test("should report nothing to read when there are no turns", () => {
	for (const text of ["0", "..", "0..2"]) {
		assert.deepEqual(resolve(text, 0), { ok: false, error: "No response to read yet." }, text);
	}
});
