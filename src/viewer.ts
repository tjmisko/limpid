import { spawn } from "node:child_process";
import { join } from "node:path";
import { findExecutable } from "./util.ts";

/** The command (and leading args) that opens an HTML file, or undefined when none is available. */
export function resolveViewer(options: {
	env: Record<string, string | undefined>;
	home: string;
	platform: NodeJS.Platform;
}): string[] | undefined {
	const custom = options.env.LIMPID_VIEWER?.trim();
	if (custom) return custom.split(/\s+/);

	const htmlview =
		findExecutable("htmlview", []) ?? findExecutable("htmlview", [join(options.home, ".local", "bin", "htmlview")]);
	if (htmlview) return [htmlview];

	const fallback = findExecutable(options.platform === "darwin" ? "open" : "xdg-open", []);
	return fallback ? [fallback] : undefined;
}

/** Launch the viewer on `file`, detached so limpid can return immediately. */
export function openInViewer(viewer: string[], file: string): void {
	const setsid = findExecutable("setsid", ["/usr/bin/setsid"]);
	const [command, ...args] = setsid ? [setsid, "-f", ...viewer, file] : [...viewer, file];
	const child = spawn(command, args, { detached: true, stdio: "ignore" });
	child.on("error", () => {});
	child.unref();
}
