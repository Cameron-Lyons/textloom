"""Build the native QA host and verify its prepared input and reader libraries."""

import argparse
import json
from pathlib import Path
import subprocess
import tempfile

from prepare_native import PROFILES, prepare


def prepare_workspace(example, workspace, profiles):
    manifest = (example / "Cargo.toml").read_text(encoding="utf-8")
    # Compile the checkout's original sources even when target is on another volume.
    root_path = json.dumps(example.parents[1].as_posix(), ensure_ascii=False)
    manifest = manifest.replace('path = "../.."', f'path = {root_path}')
    main_path = json.dumps((example / "src/main.rs").as_posix(), ensure_ascii=False)
    manifest += f'\n[[bin]]\nname = "textloom-native-example"\npath = {main_path}\n'
    lock = (example / "Cargo.lock").read_text(encoding="utf-8")
    overrides = []
    paths = []
    for profile in profiles:
        source = workspace / profile
        prepare(profile, source)
        for package, checksum, _ in PROFILES[profile]:
            name, version = package.rsplit("-", 1)
            paths.append(source / package)
            overrides.append(f'{name} = {{ path = "{profile}/{package}" }}')
            # Path packages have no registry source or checksum. Keep every
            # version and dependency edge in the checked-in lock for --locked.
            identity = f'name = "{name}"\nversion = "{version}"\n'
            registry = (
                f'{identity}source = "registry+https://github.com/rust-lang/crates.io-index"\n'
                f'checksum = "{checksum}"\n'
            )
            if lock.count(registry) != 1:
                raise SystemExit(f"Native Cargo.lock does not pin the expected archive for {package}")
            lock = lock.replace(registry, identity)
    (workspace / "Cargo.toml").write_text(
        manifest + "\n[patch.crates-io]\n" + "\n".join(overrides) + "\n", encoding="utf-8"
    )
    (workspace / "Cargo.lock").write_text(lock, encoding="utf-8")
    return paths


def main():
    reader_profiles = {"orca": "accesskit", "nvda": "accesskit-windows"}
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reader", choices=reader_profiles,
                        help="also build the Linux Orca or Windows NVDA source overrides")
    args = parser.parse_args()
    profiles = ["egui-winit"]
    if args.reader:
        profiles.append(reader_profiles[args.reader])

    root = Path(__file__).resolve().parents[2]
    target = root / "target"
    target.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="native-input-", dir=target) as temp:
        workspace = Path(temp)
        paths = prepare_workspace(root / "examples/native-editor", workspace, profiles)
        output = subprocess.check_output([
            "cargo", "build", "--locked", "--manifest-path", str(workspace / "Cargo.toml"),
            "--target-dir", str(target), "--message-format=json-render-diagnostics",
        ], cwd=root, encoding="utf-8")
        expected = {(path / "Cargo.toml").resolve() for path in paths}
        compiled = set()
        executable = None
        for line in output.splitlines():
            event = json.loads(line)
            if event["reason"] != "compiler-artifact":
                continue
            if "lib" in event["target"]["kind"]:
                compiled.add(Path(event["manifest_path"]).resolve())
            elif event["target"]["name"] == "textloom-native-example" and event["executable"]:
                executable = Path(event["executable"])
        missing = expected - compiled
        if missing:
            names = ", ".join(path.parent.name for path in sorted(missing))
            raise SystemExit(f"Build did not use the prepared libraries: {names}")
        if executable is None or not executable.is_file():
            raise SystemExit("Cargo did not produce the native QA executable")
        for path in paths:
            print(f"Compiled {path.name} from the prepared source")
        print(f"Native QA executable: {executable}")


if __name__ == "__main__":
    main()
