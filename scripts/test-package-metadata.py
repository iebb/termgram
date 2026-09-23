#!/usr/bin/env python3
"""Exercise metadata publishing against local Git remotes and release fixtures."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("publish-package-metadata.sh").resolve()
PLATFORMS = ("linux.tar.gz", "linux-aarch64.tar.gz", "macos.tar.gz",
             "macos-x86_64.tar.gz", "windows.zip", "windows-aarch64.zip")


class PackageMetadataTests(unittest.TestCase):
    def run_command(self, *args, cwd=None):
        return subprocess.run(args, cwd=cwd or self.repo, env=self.env,
                              text=True, capture_output=True, check=True).stdout.strip()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / "source"
        self.remote = self.root / "origin.git"
        self.env = os.environ | {"GITHUB_REPOSITORY": "fixture/termgram", "DEFAULT_BRANCH": "main"}
        self.run_command("git", "init", "--bare", "--initial-branch=main", str(self.remote), cwd=self.root)
        self.run_command("git", "clone", str(self.remote), str(self.repo), cwd=self.root)
        self.run_command("git", "config", "user.name", "Metadata test")
        self.run_command("git", "config", "user.email", "test@example.invalid")
        self.run_command("git", "config", f"url.{self.remote}.insteadOf", "git@github.com:fixture/termgram.git")
        (self.repo / "README.md").write_text("source commit\n")
        self.run_command("git", "add", ".")
        self.run_command("git", "commit", "-m", "release: Publish fixture")
        self.run_command("git", "push", "origin", "main")
        self.source_sha = self.run_command("git", "rev-parse", "HEAD")

        # Published archives differ from the same version rebuilt by another CI run.
        self.hashes = {f"termgram-0.1.21-{platform}": hashlib.sha256(platform.encode()).hexdigest()
                       for platform in PLATFORMS}
        checksums = "".join(f"{digest}  {name}\n" for name, digest in self.hashes.items())
        (self.root / "SHA256SUMS").write_text(checksums)
        # The prerelease track carries its own published checksums.
        self.pre_hashes = {
            f"termgram-0.1.30-{platform}": hashlib.sha256(f"pre-{platform}".encode()).hexdigest()
            for platform in PLATFORMS}
        (self.root / "SHA256SUMS-0.1.30").write_text(
            "".join(f"{digest}  {name}\n" for name, digest in self.pre_hashes.items()))
        rebuilt = self.repo / "prepared/0.1.21"
        rebuilt.mkdir(parents=True)
        (rebuilt / "SHA256SUMS").write_text("".join(f"{'0' * 64}  {name}\n" for name in self.hashes))
        self.releases = self.root / "releases.json"
        self.releases.write_text(json.dumps([
            {"tag_name": "v0.1.9", "draft": False, "prerelease": False},
            {"tag_name": "v0.1.21", "draft": False, "prerelease": False},
            {"tag_name": "v0.1.30", "draft": False, "prerelease": True},
            {"tag_name": "v0.1.40", "draft": True, "prerelease": False},
        ]))
        commands = self.root / "bin"
        commands.mkdir()
        gh = commands / "gh"
        gh.write_text('''#!/usr/bin/env python3
import os, pathlib, shutil, subprocess, sys
args = sys.argv[1:]
root = pathlib.Path(os.environ["RELEASE_FIXTURE"])
if args[0] == "api":
    subprocess.run(["jq", "-r", args[args.index("--jq") + 1], str(root / "releases.json")], check=True)
elif args[:3] == ["release", "download", "v0.1.21"]:
    assert args[args.index("--pattern") + 1] == "SHA256SUMS"
    shutil.copyfile(root / "SHA256SUMS", pathlib.Path(args[args.index("--dir") + 1]) / "SHA256SUMS")
elif args[:3] == ["release", "download", "v0.1.30"]:
    assert args[args.index("--pattern") + 1] == "SHA256SUMS"
    shutil.copyfile(root / "SHA256SUMS-0.1.30", pathlib.Path(args[args.index("--dir") + 1]) / "SHA256SUMS")
else:
    raise SystemExit(f"unexpected gh call: {args}")
''')
        gh.chmod(0o755)
        self.env |= {"PATH": str(commands) + os.pathsep + self.env["PATH"], "RELEASE_FIXTURE": str(self.root)}

    def remote_head(self):
        return self.run_command("git", "--git-dir", str(self.remote), "rev-parse", "main")

    def remote_file(self, name):
        return self.run_command("git", "--git-dir", str(self.remote), "show", f"main:{name}")

    def test_published_hashes_repair_prerelease_plan_and_rerun_is_noop(self):
        metadata = self.repo / "metadata"
        metadata.mkdir()
        (metadata / "release-plan.tsv").write_text(f"{self.source_sha}\t30\t0.1.30\n")
        self.run_command("bash", str(SCRIPT))
        manifest = json.loads(self.remote_file("bucket/termgram.json"))
        self.assertEqual(manifest["version"], "0.1.21")
        self.assertEqual(manifest["architecture"]["64bit"]["hash"], self.hashes["termgram-0.1.21-windows.zip"])
        formula = self.remote_file("Formula/termgram.rb")
        self.assertIn(self.hashes["termgram-0.1.21-linux.tar.gz"], formula)
        pre = self.remote_file("Formula/termgram@pre.rb")
        self.assertIn("class TermgramATPre < Formula", pre)
        self.assertIn('conflicts_with "termgram", because: "both install the tg binary"', pre)
        self.assertIn('version "0.1.30"', pre)
        self.assertIn(self.pre_hashes["termgram-0.1.30-macos.tar.gz"], pre)
        self.assertEqual(
            self.run_command("git", "--git-dir", str(self.remote), "log", "-1", "--format=%s", "main"),
            "chore(release): Update package metadata for v0.1.21 and v0.1.30")
        first_head = self.remote_head()
        # A rerun still starts at the tested source SHA, before the metadata commit.
        self.assertEqual(self.run_command("git", "rev-parse", "HEAD"), self.source_sha)
        (metadata / "release-plan.tsv").write_text("")
        self.run_command("bash", str(SCRIPT))
        self.assertEqual(self.remote_head(), first_head)

    def test_new_main_commits_survive_initial_fetch_and_push_race(self):
        (self.repo / "new-main.txt").write_text("new main work\n")
        self.run_command("git", "add", ".")
        self.run_command("git", "commit", "-m", "fix: Advance main before metadata")
        self.run_command("git", "push", "origin", "main")
        (self.repo / "push-race.txt").write_text("concurrent work\n")
        self.run_command("git", "add", ".")
        self.run_command("git", "commit", "-m", "fix: Advance main during metadata push")
        racing_sha = self.run_command("git", "rev-parse", "HEAD")
        self.run_command("git", "push", "origin", "HEAD:refs/heads/race-fixture")
        self.run_command("git", "checkout", "--detach", self.source_sha)
        hook = self.repo / ".git/hooks/pre-push"
        hook.write_text('''#!/usr/bin/env python3
import os, pathlib, subprocess
root = pathlib.Path(os.environ["RELEASE_FIXTURE"])
marker = root / "push-raced"
if not marker.exists():
    marker.touch()
    subprocess.run(["git", "--git-dir", str(root / "origin.git"), "update-ref", "refs/heads/main", os.environ["RACING_SHA"]], check=True)
''')
        hook.chmod(0o755)
        self.env["RACING_SHA"] = racing_sha
        self.run_command("bash", str(SCRIPT))
        self.assertTrue((self.root / "push-raced").exists())
        self.assertEqual(self.remote_file("new-main.txt"), "new main work")
        self.assertEqual(self.remote_file("push-race.txt"), "concurrent work")
        self.assertEqual(self.run_command("git", "--git-dir", str(self.remote), "rev-parse", "main^"), racing_sha)
        self.assertEqual(json.loads(self.remote_file("bucket/termgram.json"))["version"], "0.1.21")

    def test_no_stable_release_does_not_commit(self):
        self.releases.write_text("[]")
        self.run_command("bash", str(SCRIPT))
        self.assertEqual(self.remote_head(), self.source_sha)

    def test_prerelease_only_commits_only_the_prerelease_formula(self):
        self.releases.write_text(json.dumps([
            {"tag_name": "v0.1.30", "draft": False, "prerelease": True},
        ]))
        self.run_command("bash", str(SCRIPT))
        pre = self.remote_file("Formula/termgram@pre.rb")
        self.assertIn('version "0.1.30"', pre)
        self.assertIn(self.pre_hashes["termgram-0.1.30-linux.tar.gz"], pre)
        self.assertEqual(
            self.run_command("git", "--git-dir", str(self.remote), "ls-tree", "main", "--", "Formula/termgram.rb"),
            "")
        self.assertEqual(
            self.run_command("git", "--git-dir", str(self.remote), "ls-tree", "main", "--", "bucket/termgram.json"),
            "")
        self.assertEqual(
            self.run_command("git", "--git-dir", str(self.remote), "log", "-1", "--format=%s", "main"),
            "chore(release): Update package metadata for v0.1.30")


if __name__ == "__main__":
    unittest.main()
