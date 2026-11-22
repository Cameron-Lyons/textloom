"""Prepare optional host dependency patches from pinned Cargo archives."""

import argparse
import hashlib
import io
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
from urllib.request import urlopen


# Directory names match Cargo.lock. Each patch is relative to its crate root.
EGUI_INPUT = (
    "egui-0.37.0",
    "bec9144f49b7a28b5f395d3147e325b4fe3e77ba686eac11d1bdf0a7da3f375f",
    ("touch-cancel",),
)
WINIT = (
    "winit-0.30.13",
    "a6755fa58a9f8350bd1e472d4c3fcc25f824ec358933bba33306d0b63df5978d",
    ("x11-area", "x11-focus", "x11-reset", "x11-touch-pointer", "windows-layout", "windows-touch-cancel"),
)
PROFILES = {
    "eframe-web": [
        (
            "eframe-0.37.0",
            "9c5642501a65ca657d05f00c643a5c14ff2346cb3818032bfe23d58e1f8e5f84",
            ("touch",),
        ),
        EGUI_INPUT,
    ],
    "egui-winit": [
        (
            "egui-winit-0.37.0",
            "e4e449f4c4a2a60c7ccb4ba7fa4078417bfe1f9ddc03b5e05ce75748e33a70de",
            ("touch",),
        ),
        EGUI_INPUT,
        WINIT,
    ],
    "winit": [WINIT],
    "accesskit": [
        (
            "accesskit_atspi_common-0.21.1",
            "24f5baf62e34a4a6c30e3d697e4782f821a7337049c44c3b40576a27dfbb580d",
            ("hypertext",),
        ),
        (
            "accesskit_unix-0.24.1",
            "30d5192871228c54f1b0fb4a85cef000e11a3804f25d2165f14afa0daa67f3f6",
            ("hypertext",),
        ),
    ],
    "accesskit-windows": [
        (
            "accesskit_consumer-0.39.2",
            "f3da8f27636015191b2264ee1df681503170a632cafd832fdc1edfdd4e3a2fd3",
            ("format",),
        ),
        (
            "accesskit_windows-0.35.2",
            "498668b90da9995302a0055aaecad4756932333da2ddef6baee1c00844c430fc",
            ("heading-level",),
        ),
    ],
}


def crate_archive(package, checksum):
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    for path in (cargo_home / "registry" / "cache").glob(f"*/{package}.crate"):
        archive = path.read_bytes()
        if hashlib.sha256(archive).hexdigest() == checksum:
            return archive
    if os.environ.get("CARGO_NET_OFFLINE", "").lower() == "true":
        raise FileNotFoundError(f"no cached {package}.crate with the expected checksum in offline mode")
    name, _ = package.rsplit("-", 1)
    with urlopen(f"https://static.crates.io/crates/{name}/{package}.crate", timeout=30) as response:
        archive = response.read()
    if hashlib.sha256(archive).hexdigest() != checksum:
        raise ValueError(f"checksum mismatch for {package}.crate")
    return archive


def prepare(profile, destination):
    if os.path.lexists(destination):
        raise FileExistsError(f"destination already exists: {destination}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=destination.parent) as working:
        working = Path(working)
        # A Git hook can export its worktree and index to this process. If Git
        # treats the staging directory as a subdirectory of that worktree,
        # apply can skip every patch path and still exit successfully.
        local_vars = subprocess.check_output(
            ["git", "rev-parse", "--local-env-vars"], cwd=working, text=True
        ).splitlines()
        git_env = os.environ.copy()
        for name in local_vars:
            git_env.pop(name, None)
        subprocess.run(["git", "init", "--quiet"], cwd=working, env=git_env, check=True)
        source = working / "source"
        source.mkdir()
        packages = PROFILES[profile]
        for package, checksum, patches in packages:
            archive = crate_archive(package, checksum)
            with tarfile.open(fileobj=io.BytesIO(archive), mode="r:gz") as crate:
                # The checksum also pins the archive on Python versions that
                # predate extraction filters.
                if hasattr(tarfile, "data_filter"):
                    crate.extractall(source, filter="data")
                else:
                    crate.extractall(source)
            patch_files = [
                Path(__file__).resolve().with_name(f"{package}-{name}.patch") for name in patches
            ]
            # The Cargo archive and patch both use LF, including on Windows.
            subprocess.run(
                ["git", "-c", "core.autocrlf=false", "-c", "core.eol=lf", "apply",
                 "--whitespace=error-all", f"--directory=source/{package}", *patch_files],
                cwd=working,
                env=git_env,
                check=True,
            )
        # Publish the profile only after every patch applied successfully.
        result = source / packages[0][0] if len(packages) == 1 else source
        result.rename(destination)


def main(profile=None):
    parser = argparse.ArgumentParser(description=__doc__)
    if profile is None:
        parser.add_argument("profile", choices=PROFILES)
    else:
        parser.set_defaults(profile=profile)
    parser.add_argument("destination", type=Path, help="new directory for the patched source")
    args = parser.parse_args()
    try:
        prepare(args.profile, args.destination.absolute())
    except (OSError, ValueError, subprocess.CalledProcessError, tarfile.TarError) as error:
        parser.exit(1, f"{parser.prog}: {error}\n")
    print(args.destination.absolute())


if __name__ == "__main__":
    main()
