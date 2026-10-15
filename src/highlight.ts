/**
 * Syntax highlighting via a single headless Neovim. Neovim's tree-sitter
 * highlighter is attached directly to scratch buffers (bypassing filetype
 * detection so no LSP server is started), then Neovim's own `nvim.tohtml`
 * module is used to turn the highlighted buffer into HTML. The token classes
 * are inlined afterwards so the result drops straight into the page.
 */

import { spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { escapeHtml, findExecutable } from "./util.ts";

const NVIM = findExecutable("nvim", ["/usr/bin/nvim"]);
const CACHE_DIR = join(homedir(), ".cache", "limpid");
const NVIM_TIMEOUT_MS = 20_000;
const CODE_MARK_OPEN = "\uE000";
const CODE_MARK_CLOSE = "\uE001";

/** Fence aliases that are not Neovim filetype/tree-sitter names. */
const LANG_ALIASES: Record<string, string> = {
	rs: "rust",
	py: "python",
	python3: "python",
	js: "javascript",
	jsx: "javascript",
	ts: "typescript",
	tsx: "tsx",
	sh: "bash",
	shell: "bash",
	zsh: "bash",
	yml: "yaml",
	rb: "ruby",
	"c++": "cpp",
	cc: "cpp",
	hpp: "cpp",
	cs: "cs",
	md: "markdown",
	docker: "dockerfile",
	golang: "go",
};

export function normalizeLang(lang: string): string {
	const key = lang.toLowerCase();
	return key in LANG_ALIASES ? LANG_ALIASES[key] : lang;
}

type PendingCode = { id: number; lang: string; text: string };

/**
 * Collects code blocks across every turn so they can all be highlighted by a
 * single Neovim process. `register` returns a placeholder that survives the
 * markdown render; `flush` resolves them all at once; `apply` substitutes the
 * highlighted HTML (or the escaped fallback) back into the page.
 */
export class NvimCodeBatch {
	private readonly blocks: PendingCode[] = [];
	private nextId = 0;
	private result: Map<number, string> | undefined;
	private readonly nvim: string | undefined;

	/** Pass `nvim: null` to disable highlighting (escaped fallback only). */
	constructor(options: { nvim?: string | null } = {}) {
		this.nvim = options.nvim === null ? undefined : (options.nvim ?? NVIM);
	}

	register(lang: string, text: string): string {
		if (!lang) return escapeHtml(text);
		const id = this.nextId++;
		this.blocks.push({ id, lang, text });
		return `${CODE_MARK_OPEN}${id}${CODE_MARK_CLOSE}`;
	}

	async flush(): Promise<Map<number, string>> {
		if (this.result) return this.result;

		// Every block starts as escaped plain text; successful highlights replace
		// the fallback so a missing/failed Neovim never loses the source.
		const result = new Map<number, string>();
		for (const block of this.blocks) result.set(block.id, escapeHtml(block.text));

		if (!this.nvim || this.blocks.length === 0) {
			this.result = result;
			return result;
		}

		let dir: string | undefined;
		try {
			mkdirSync(CACHE_DIR, { recursive: true });
			dir = mkdtempSync(join(CACHE_DIR, "hl-"));
			const inputFile = join(dir, "in.json");
			const outputFile = join(dir, "out.json");
			const scriptFile = join(dir, "highlight.lua");
			writeFileSync(inputFile, JSON.stringify(this.blocks), "utf8");
			writeFileSync(scriptFile, buildNvimScript(inputFile, outputFile), "utf8");

			await runNvim(this.nvim, scriptFile);
			if (!existsSync(outputFile)) throw new Error("nvim produced no output");

			const parsed = JSON.parse(readFileSync(outputFile, "utf8")) as Array<{
				id: number;
				html?: string;
			}>;
			for (const item of parsed) {
				if (!item.html) continue;
				const inlined = inlineNvimHtml(item.html);
				if (inlined) result.set(item.id, inlined);
			}
		} catch {
			/* keep the escaped fallbacks */
		} finally {
			if (dir) {
				try {
					rmSync(dir, { recursive: true, force: true });
				} catch {
					/* ignore cleanup failures */
				}
			}
		}

		this.result = result;
		return result;
	}
}

function runNvim(nvim: string, scriptFile: string): Promise<void> {
	return new Promise((resolve) => {
		let settled = false;
		const finish = () => {
			if (settled) return;
			settled = true;
			resolve();
		};
		try {
			const child = spawn(nvim, ["--headless", "-c", `luafile ${scriptFile}`, "-c", "qa!"], {
				stdio: "ignore",
			});
			const timer = setTimeout(() => {
				try {
					child.kill("SIGKILL");
				} catch {
					/* already gone */
				}
				finish();
			}, NVIM_TIMEOUT_MS);
			child.on("error", () => {
				clearTimeout(timer);
				finish();
			});
			child.on("exit", () => {
				clearTimeout(timer);
				finish();
			});
		} catch {
			finish();
		}
	});
}

function buildNvimScript(inputFile: string, outputFile: string): string {
	const luaPath = (value: string) => JSON.stringify(value);
	return `local input_path = ${luaPath(inputFile)}
local output_path = ${luaPath(outputFile)}

local tohtml_file =
  vim.api.nvim_get_runtime_file("pack/dist/opt/nvim.tohtml/lua/tohtml.lua", false)[1]
if not tohtml_file then
  vim.fn.writefile({ vim.json.encode({}) }, output_path)
  return
end
local tohtml = assert(loadfile(tohtml_file))()

local blocks = vim.json.decode(table.concat(vim.fn.readfile(input_path), "\\n"))
local results = {}

for _, b in ipairs(blocks) do
  local ok, res = pcall(function()
    local buf = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_buf_set_lines(buf, 0, -1, false, vim.split(b.text, "\\n", { plain = true }))
    vim.api.nvim_win_set_buf(0, buf)
    local wo = vim.wo
    wo.number = false
    wo.relativenumber = false
    wo.signcolumn = "no"
    wo.foldcolumn = "0"
    wo.wrap = false
    wo.cursorline = false
    wo.list = false
    wo.spell = false
    local lang = vim.treesitter.language.get_lang(b.lang) or b.lang
    pcall(vim.treesitter.start, buf, lang)
    return table.concat(
      tohtml.tohtml(0, { title = false, number_lines = false, font = { "monospace" } }),
      "\\n"
    )
  end)
  results[#results + 1] = { id = b.id, html = ok and res or nil }
end

vim.fn.writefile({ vim.json.encode(results) }, output_path)
`;
}

/** Turn Neovim's class-based `<span>`s into self-contained inline styles. */
function inlineNvimHtml(html: string): string {
	const rules = new Map<string, string>();
	const styleMatch = /<style>([\s\S]*?)<\/style>/.exec(html);
	if (styleMatch) {
		for (const match of styleMatch[1].matchAll(/\.([A-Za-z0-9_@-]+)\s*\{([^}]*)\}/g)) {
			rules.set(match[1], match[2].trim());
		}
	}

	const preMatch = /<pre>([\s\S]*?)<\/pre>/.exec(html);
	let inner = preMatch ? preMatch[1] : html;
	inner = inner.replace(/<span class="([^"]+)">/g, (_whole, classes: string) => {
		const style = classes
			.split(/\s+/)
			.map((name) => rules.get(name))
			.filter(Boolean)
			.join(";");
		return style ? `<span style="${style}">` : "<span>";
	});
	return inner.replace(/^\n/, "").replace(/\n\s*$/, "");
}

/** Substitute the placeholders left by `NvimCodeBatch.register`. */
export function applyNvimHighlights(body: string, result: Map<number, string>): string {
	const pattern = new RegExp(`${CODE_MARK_OPEN}(\\d+)${CODE_MARK_CLOSE}`, "g");
	return body.replace(pattern, (_whole, id: string) => result.get(Number(id)) ?? "");
}
