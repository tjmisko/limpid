#!/usr/bin/env python3
"""Merge the reader hook and install its skill without replacing other hooks."""
import argparse
import json
import os
from pathlib import Path
import shlex
import tempfile


def install(codex_home: Path, binary: Path) -> None:
    binary = binary.resolve(strict=True)
    if not os.access(binary, os.X_OK):
        raise ValueError(f"not executable: {binary}")
    codex_home.mkdir(parents=True, exist_ok=True)
    hooks_path = codex_home / "hooks.json"
    config = json.loads(hooks_path.read_text()) if hooks_path.exists() else {}
    groups = config.setdefault("hooks", {}).setdefault("UserPromptSubmit", [])
    command = f"{shlex.quote(str(binary))} codex-hook"
    handler = {"type": "command", "command": command, "timeout": 30}
    # Match our complete command, never delete another integration's handler.
    if not any(handler == item for group in groups for item in group.get("hooks", [])):
        groups.append({"hooks": [handler]})
    skill_path = codex_home / "skills/read/SKILL.md"
    skill = Path(__file__).with_name("skills").joinpath("read/SKILL.md").read_text()
    if skill_path.exists() and skill_path.read_text() != skill:
        raise ValueError(f"existing skill differs; refusing to overwrite {skill_path}")
    # Preserve existing permissions and write atomically beside the destination.
    with tempfile.NamedTemporaryFile(mode="w", dir=codex_home, delete=False) as stream:
        temporary = Path(stream.name)
        try:
            stream.write(json.dumps(config, indent=2) + "\n")
            stream.flush()
            os.chmod(temporary, hooks_path.stat().st_mode & 0o777 if hooks_path.exists() else 0o600)
            temporary.replace(hooks_path)
        finally:
            temporary.unlink(missing_ok=True)
    skill_path.parent.mkdir(parents=True, exist_ok=True)
    if not skill_path.exists():
        with skill_path.open("x") as stream:
            stream.write(skill)
    # Remove only the exact compatibility prompt written by the first prototype.
    prompt_path = codex_home / "prompts/read.md"
    old_prompt = '---\ndescription: Open a response or a range of turns in Limpid\nargument-hint: "[0 | 0..2 | 1..=3 | 2.. | ..4]"\n---\n/read $ARGUMENTS\n'
    if prompt_path.is_file() and prompt_path.read_text() == old_prompt:
        prompt_path.unlink()
    print(f"Installed reader hook in {hooks_path}")
    print("Restart Codex and review/trust the new handler with /hooks.")
    print("Use $read (or the app's read skill menu), or type limpid-read as plain text.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--codex-home", type=Path, default=Path(os.environ.get("CODEX_HOME", str(Path.home() / ".codex"))))
    parser.add_argument("--bin", type=Path, default=Path.home() / ".local/bin/limpid-core")
    args = parser.parse_args()
    install(args.codex_home, args.bin)
