"""Build the hosted interaction example and verify the input sources it compiled."""

import json
from pathlib import Path
import subprocess
import tempfile

from prepare_native import PROFILES, prepare


def main():
    root = Path(__file__).resolve().parents[2]
    target = root / "target"
    target.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="native-input-", dir=target) as temp:
        source = Path(temp) / "input"
        prepare("egui-winit", source)
        paths = [source / package for package, _, _ in PROFILES["egui-winit"]]
        config = "paths=" + json.dumps([str(path) for path in paths])
        output = subprocess.check_output([
            "cargo", "--config", config, "build", "--locked",
            "--manifest-path", str(root / "examples/native-editor/Cargo.toml"),
            "--target-dir", str(target), "--message-format=json-render-diagnostics",
        ], cwd=root, encoding="utf-8")
        expected = {(path / "Cargo.toml").resolve() for path in paths}
        compiled = set()
        for line in output.splitlines():
            event = json.loads(line)
            if event["reason"] == "compiler-artifact" and "lib" in event["target"]["kind"]:
                compiled.add(Path(event["manifest_path"]).resolve())
        missing = expected - compiled
        if missing:
            names = ", ".join(path.parent.name for path in sorted(missing))
            raise SystemExit(f"Build did not use the prepared libraries: {names}")
        for path in paths:
            print(f"Compiled {path.name} from the prepared source")


if __name__ == "__main__":
    main()
