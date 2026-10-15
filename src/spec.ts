/** Turn selection: 0 = newest. Empty, a number, or a Rust-style range. */

export const MAX_RANGE = 10;

export type RawTurnSpec =
	| { kind: "single"; index: number }
	| { kind: "range"; start?: number; end?: number; inclusive: boolean };

export type ParsedTurnSpec = { ok: true; spec: RawTurnSpec } | { ok: false; error: string };

export function parseTurnSpec(args: string): ParsedTurnSpec {
	const text = args.trim();
	if (!text) return { ok: true, spec: { kind: "single", index: 0 } };

	if (/^\d+$/.test(text)) {
		return { ok: true, spec: { kind: "single", index: Number(text) } };
	}

	const match = /^(\d*)\.\.(=?)(\d*)$/.exec(text);
	if (!match) {
		return {
			ok: false,
			error: `Invalid argument "${text}". Use 0, 3, 0..2, 1..=3, 2.., ..4, or ..`,
		};
	}

	const [, left, equals, right] = match;
	const inclusive = equals === "=";

	// A bare `..` selects everything, subject to the cap.
	if (!left && !right) return { ok: true, spec: { kind: "range", inclusive } };

	return {
		ok: true,
		spec: {
			kind: "range",
			start: left ? Number(left) : undefined,
			end: right ? Number(right) : undefined,
			inclusive,
		},
	};
}

export type ResolvedTurnSpec =
	| { ok: true; kind: "single"; indices: number[] }
	| { ok: true; kind: "range"; indices: number[] }
	| { ok: false; error: string };

export function resolveTurnSpec(spec: RawTurnSpec, total: number): ResolvedTurnSpec {
	if (total === 0) return { ok: false, error: "No response to read yet." };

	if (spec.kind === "single") {
		if (spec.index >= total) {
			return { ok: false, error: `Turn ${spec.index} does not exist (only ${total} available).` };
		}
		return { ok: true, kind: "single", indices: [spec.index] };
	}

	const start = spec.start ?? 0;
	// Count the range as requested, before clamping, so `0..11` is rejected even
	// when fewer turns actually exist.
	const requestedEnd = spec.end === undefined ? total : spec.end + (spec.inclusive ? 1 : 0);

	if (start >= total) {
		return { ok: false, error: `Turn ${start} does not exist (only ${total} available).` };
	}
	if (start >= requestedEnd) return { ok: false, error: "Empty range." };

	const count = requestedEnd - start;
	if (count > MAX_RANGE) {
		return { ok: false, error: `Range covers ${count} turns; the maximum is ${MAX_RANGE}.` };
	}

	const endExclusive = Math.min(requestedEnd, total);
	const indices: number[] = [];
	for (let i = start; i < endExclusive; i++) indices.push(i);
	return { ok: true, kind: "range", indices };
}
