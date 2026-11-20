"""Regressions for preparing an application's patched Cargo dependencies."""

import hashlib
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import prepare_native


class PrepareNativeTests(unittest.TestCase):
    def test_preparation_inside_a_git_hook_applies_the_patch(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            git_config = root / "gitconfig"
            git_config.write_text("[core]\n\tautocrlf = true\n", encoding="utf-8")
            caller = root / "app"
            subprocess.run(["git", "init", "--quiet", caller], check=True)
            original = root / "note"
            original.write_bytes(b"original\n")
            package = "sample-1"
            archive = root / "cargo" / "registry" / "cache" / "sample" / f"{package}.crate"
            archive.parent.mkdir(parents=True)
            with tarfile.open(archive, "w:gz") as tar:
                tar.add(original, arcname=f"{package}/note")
            checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
            (root / f"{package}-fix.patch").write_bytes(
                b"--- a/note\n+++ b/note\n@@ -1 +1 @@\n-original\n+patched\n"
            )
            destination = caller / "vendor" / package
            with (
                patch.dict(os.environ, {
                    "CARGO_HOME": str(root / "cargo"),
                    "CARGO_NET_OFFLINE": "true",
                    "GIT_DIR": str(caller / ".git"),
                    "GIT_WORK_TREE": str(caller),
                    "GIT_CONFIG_GLOBAL": str(git_config),
                }),
                patch.dict(prepare_native.PROFILES, {"sample": [(package, checksum, ("fix",))]}),
                patch.object(prepare_native, "__file__", str(root / "prepare_native.py")),
            ):
                prepare_native.prepare("sample", destination)
            self.assertEqual((destination / "note").read_bytes(), b"patched\n")
