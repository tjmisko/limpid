import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("reader_install", Path(__file__).with_name("install.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class InstallTests(unittest.TestCase):
    def test_preserves_other_handlers_and_is_idempotent(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "binary with spaces"
            binary.write_text("#!/bin/sh\n")
            binary.chmod(0o755)
            other = {"hooks": [{"type": "command", "command": "existing-hook"}]}
            hooks = root / "hooks.json"
            hooks.write_text(json.dumps({"hooks": {"Stop": [other], "UserPromptSubmit": [other]}}))
            module.install(root, binary)
            first = hooks.read_bytes()
            module.install(root, binary)
            self.assertEqual(first, hooks.read_bytes())
            result = json.loads(first)
            self.assertEqual(result["hooks"]["Stop"], [other])
            self.assertEqual(result["hooks"]["UserPromptSubmit"][0], other)
            self.assertEqual(len(result["hooks"]["UserPromptSubmit"]), 2)
            self.assertIn("name: read", (root / "skills/read/SKILL.md").read_text())

    def test_refuses_conflicting_skill_without_changing_hooks(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "skills/read").mkdir(parents=True)
            (root / "skills/read/SKILL.md").write_text("my existing skill")
            (root / "hooks.json").write_text("{}")
            with self.assertRaises(ValueError):
                module.install(root, Path("/bin/true"))
            self.assertEqual((root / "hooks.json").read_text(), "{}")
            self.assertEqual((root / "skills/read/SKILL.md").read_text(), "my existing skill")


if __name__ == "__main__":
    unittest.main()
