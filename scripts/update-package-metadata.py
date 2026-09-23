#!/usr/bin/env python3
"""Regenerate the Homebrew formula and the Scoop manifest for one stable release.

Use the SHA256SUMS downloaded from the published release, never a local rebuild.
The generated files must match the formats reviewed in the repository;
a stable release is required because Homebrew stable and Scoop follow stable versions.
With --prerelease, only the termgram@pre formula for the latest prerelease is written.
"""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = "https://github.com/iebb/termgram"
DESCRIPTION = "Focused, keyboard-first Telegram client for the terminal"

TARGETS = {
    "mac_arm": ("macos", "tar.gz"),
    "mac_intel": ("macos-x86_64", "tar.gz"),
    "linux_arm": ("linux-aarch64", "tar.gz"),
    "linux_intel": ("linux", "tar.gz"),
    "windows_intel": ("windows", "zip"),
    "windows_arm": ("windows-aarch64", "zip"),
}

STABLE_FORMULA_CAVEATS = (
    "Run `tg` to launch Termgram.",
    "Update this installation with `brew upgrade termgram` instead of `tg update`",
    "so Homebrew can track the installed version.",
)

PRERELEASE_FORMULA_CAVEATS = (
    "termgram@pre tracks the latest published prerelease.",
    "Run `tg` to launch Termgram.",
    "Update this installation with `brew upgrade termgram@pre` instead of `tg update`.",
)


def asset_name(version: str, target: str, extension: str) -> str:
    return f"termgram-{version}-{target}.{extension}"


def read_checksums(version: str, path: Path) -> dict[str, str]:
    checksums: dict[str, str] = {}
    for line in path.read_text().splitlines():
        fields = line.split()
        if len(fields) != 2 or not re.fullmatch(r"[0-9a-fA-F]{64}", fields[0]):
            raise SystemExit(f"invalid checksum line: {line!r}")
        filename = fields[1].removeprefix("*")
        if filename in checksums:
            raise SystemExit(f"duplicate checksum for {filename}")
        checksums[filename] = fields[0].lower()

    expected = {asset_name(version, target, extension) for target, extension in TARGETS.values()}
    missing = sorted(expected - checksums.keys())
    if missing:
        raise SystemExit(f"SHA256SUMS does not cover every release archive: missing={missing}")
    return checksums


def release_url(version: str, target: str, extension: str) -> str:
    asset = asset_name(version, target, extension)
    return f"{REPOSITORY}/releases/download/v{version}/{asset}"


def formula(
    class_name: str,
    version: str,
    checksums: dict[str, str],
    caveats: tuple[str, ...],
    conflicts: str | None = None,
) -> str:
    def source(key: str) -> tuple[str, str]:
        target, extension = TARGETS[key]
        asset = asset_name(version, target, extension)
        return release_url(version, target, extension), checksums[asset]

    mac_arm_url, mac_arm_sha = source("mac_arm")
    mac_intel_url, mac_intel_sha = source("mac_intel")
    linux_arm_url, linux_arm_sha = source("linux_arm")
    linux_intel_url, linux_intel_sha = source("linux_intel")
    conflict = (
        f'\n  conflicts_with "{conflicts}", because: "both install the tg binary"\n'
        if conflicts
        else ""
    )
    caveats_text = "\n".join(f"      {line}" for line in caveats)
    return f'''class {class_name} < Formula
  desc "{DESCRIPTION}"
  homepage "{REPOSITORY}"
  version "{version}"
  license "MIT"
{conflict}
  on_macos do
    on_arm do
      url "{mac_arm_url}"
      sha256 "{mac_arm_sha}"
    end
    on_intel do
      url "{mac_intel_url}"
      sha256 "{mac_intel_sha}"
    end
  end

  on_linux do
    on_arm do
      url "{linux_arm_url}"
      sha256 "{linux_arm_sha}"
    end
    on_intel do
      url "{linux_intel_url}"
      sha256 "{linux_intel_sha}"
    end
  end

  def install
    bin.install "tg"
  end

  def caveats
    <<~EOS
{caveats_text}
    EOS
  end

  test do
    assert_match(/^(?:tg|version)\\s+#{{Regexp.escape(version.to_s)}}$/, shell_output("#{{bin}}/tg --version"))
    assert_match "Open the TUI", shell_output("#{{bin}}/tg --help")
    assert_match "unknown argument", shell_output("#{{bin}}/tg invalid-command 2>&1", 1)
  end
end
'''


def checksum_extractor(version: str, target: str, extension: str) -> dict[str, object]:
    asset = asset_name(version, target, extension)
    return {
        "mode": "extract",
        "url": f"{REPOSITORY}/releases/download/v{version}/SHA256SUMS",
        "find": rf"([a-fA-F0-9]{{64}})\s+\*?{asset}".replace(".", r"\."),
    }


def scoop_manifest(version: str, checksums: dict[str, str]) -> dict[str, object]:
    def entry(key: str) -> dict[str, object]:
        target, extension = TARGETS[key]
        asset = asset_name(version, target, extension)
        return {
            "url": release_url(version, target, extension),
            "hash": checksums[asset],
        }

    def autoupdate_entry(key: str) -> dict[str, object]:
        target, extension = TARGETS[key]
        asset = asset_name(version, target, extension)
        return {
            "url": f"{REPOSITORY}/releases/download/v$version/{asset.replace(version, '$version')}",
            "hash": checksum_extractor("$version", target, extension),
        }

    return {
        "version": version,
        "description": DESCRIPTION,
        "homepage": REPOSITORY,
        "license": "MIT",
        "notes": "Official release binaries include the project's Telegram application credentials; sign in with your Telegram account after starting tg.",
        "architecture": {
            "64bit": entry("windows_intel"),
            "arm64": entry("windows_arm"),
        },
        "bin": "tg.exe",
        "checkver": {"github": REPOSITORY},
        "autoupdate": {
            "architecture": {
                "64bit": autoupdate_entry("windows_intel"),
                "arm64": autoupdate_entry("windows_arm"),
            }
        },
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version")
    parser.add_argument("checksums", type=Path)
    parser.add_argument("--output-dir", type=Path, default=ROOT)
    parser.add_argument(
        "--prerelease",
        action="store_true",
        help="write only the termgram@pre formula tracking the latest prerelease",
    )
    args = parser.parse_args()
    version = args.version
    if not re.fullmatch(r"\d+\.\d+\.\d+", version):
        raise SystemExit("version must contain three numeric components")
    checksums = read_checksums(version, args.checksums)

    (args.output_dir / "Formula").mkdir(parents=True, exist_ok=True)
    if args.prerelease:
        (args.output_dir / "Formula/termgram@pre.rb").write_text(
            formula(
                "TermgramATPre",
                version,
                checksums,
                PRERELEASE_FORMULA_CAVEATS,
                conflicts="termgram",
            )
        )
        return
    (args.output_dir / "bucket").mkdir(exist_ok=True)
    (args.output_dir / "Formula/termgram.rb").write_text(
        formula("Termgram", version, checksums, STABLE_FORMULA_CAVEATS)
    )
    (args.output_dir / "bucket/termgram.json").write_text(
        json.dumps(scoop_manifest(version, checksums), indent=4) + "\n"
    )


if __name__ == "__main__":
    main()
