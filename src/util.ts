import { accessSync, constants } from "node:fs";
import { join } from "node:path";

export function escapeHtml(value: unknown): string {
	return String(value ?? "")
		.replace(/&/g, "&amp;")
		.replace(/</g, "&lt;")
		.replace(/>/g, "&gt;")
		.replace(/"/g, "&quot;")
		.replace(/'/g, "&#39;");
}

export function slugify(text: string): string {
	const base = text
		.replace(/<[^>]+>/g, "")
		.replace(/&[a-zA-Z0-9#]+;/g, " ")
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, "-")
		.replace(/^-+|-+$/g, "")
		.slice(0, 60);
	return base || "section";
}

export function uniqueSlug(base: string, used: Map<string, number>): string {
	let count = used.get(base) ?? 0;
	let id = base;
	// Suffix until free; registered suffixed ids count as used too.
	while (count > 0) {
		count++;
		id = `${base}-${count}`;
		if (!used.has(id)) break;
	}
	used.set(base, count === 0 ? 1 : count);
	if (id !== base) used.set(id, 1);
	return id;
}

export function firstLine(text: string): string | undefined {
	return text
		.split(/\r?\n/)
		.map((line) => line.replace(/^#{1,6}\s*/, "").replace(/[*_`>#]/g, "").trim())
		.find((line) => line.length > 0);
}

export function decodeEntities(text: string): string {
	return text
		.replace(/&lt;/g, "<")
		.replace(/&gt;/g, ">")
		.replace(/&quot;/g, '"')
		.replace(/&#39;/g, "'")
		.replace(/&amp;/g, "&")
		.trim();
}

export function findExecutable(name: string, fallbacks: string[]): string | undefined {
	// Prefer an absolute path we can verify (covers sessions whose PATH lacks
	// ~/.local/bin), then search PATH.
	for (const candidate of fallbacks) {
		try {
			accessSync(candidate, constants.X_OK);
			return candidate;
		} catch {
			/* try the next candidate */
		}
	}
	for (const dir of (process.env.PATH ?? "").split(":")) {
		if (!dir) continue;
		const candidate = join(dir, name);
		try {
			accessSync(candidate, constants.X_OK);
			return candidate;
		} catch {
			/* keep looking */
		}
	}
	return undefined;
}
