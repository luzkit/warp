"""Sign-only installer regressions; all Apple commands and credentials are mocked."""

import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import fork_signing as signing
import notarize_fork as installer


class NotarizeForkTests(unittest.TestCase):
    def setUp(self):
        environment = patch.dict(os.environ, {}, clear=True)
        environment.start()
        self.addCleanup(environment.stop)
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.input = self.root / "unsigned"
        self.input.mkdir()
        self.output = self.root / "signed"
        self.version = "fork-v0.1.0"
        self.commit = "a" * 40
        self.name = f"WarpOss-macos-arm64-{self.version}"
        self.image = self.input / f"{self.name}.dmg"
        self.image.write_bytes(b"unsigned image")
        self.digest = signing.sha256(self.image)
        (self.input / f"{self.name}.dmg.sha256").write_text(f"{self.digest}  {self.image.name}\n")
        self.metadata = {"repository": "luzkit/warp", "version": self.version, "commit": self.commit,
                         "source_dirty": False, "target": "aarch64-apple-darwin", "profile": "release",
                         "signing": "ad-hoc", "notarized": False,
                         "features": ["gui", "release_bundle", "extern_plist"]}
        self.write_metadata()
        self.config = signing.Signing("Developer ID Application: Test (ABCDEFGHIJ)", "ABCDEFGHIJ", "test-profile")
        self.calls = []
        self.app = self.root / "WarpOss.app"
        binary = self.app / "Contents/MacOS/warp-oss"
        binary.parent.mkdir(parents=True)
        binary.write_bytes(b"\xcf\xfa\xed\xfe" + bytes(12))
        (self.app / "Contents/Info.plist").write_bytes(plistlib.dumps({
            "CFBundleIdentifier": signing.APP_ID, "CFBundleExecutable": "warp-oss",
            "CFBundleName": "WarpOss", "WarpVersion": self.version,
            "CFBundleShortVersionString": "0.1.0", "CFBundleVersion": "0.1.0",
        }))
        resources = self.app / "Contents/Resources"
        resources.mkdir()
        (resources / "THIRD_PARTY_LICENSES.txt").write_text("test licenses")
        (resources / "settings_schema.json").write_text("{}")
        command_patch = patch.object(signing, "run", side_effect=self.fake_run)
        command_patch.start()
        self.addCleanup(command_patch.stop)
        preflight_patch = patch.object(signing, "preflight")
        self.preflight = preflight_patch.start()
        self.addCleanup(preflight_patch.stop)

    def write_metadata(self):
        (self.input / f"{self.name}.json").write_text(json.dumps(self.metadata))

    def fake_run(self, arguments, check=True):
        args = [str(arg) for arg in arguments]
        self.calls.append(args)
        output = ""
        if args[:2] == ["hdiutil", "attach"]:
            shutil.copytree(self.app, Path(args[-1]) / "WarpOss.app")
        elif args[:2] == ["hdiutil", "create"]:
            Path(args[-1]).write_bytes(b"signed image")
        elif args[0] == "ditto":
            if args[1] == "-c":
                Path(args[-1]).write_bytes(b"app zip")
            else:
                shutil.copytree(args[-2], args[-1])
        elif args[:2] == ["lipo", "-archs"]:
            output = "arm64\n"
        elif args[:2] == ["codesign", "--display"]:
            output = (f"TeamIdentifier={self.config.team}\nAuthority={self.config.identity}\n"
                      "Timestamp=Nov 7, 2026\nCodeDirectory flags=0x10000(runtime)\n")
        elif args[:3] == ["xcrun", "notarytool", "submit"]:
            output = json.dumps({"id": "11111111-2222-3333-4444-555555555555"})
        elif args[:3] == ["xcrun", "notarytool", "wait"]:
            output = json.dumps({"status": "Accepted"})
        elif args[:3] == ["xcrun", "stapler", "staple"] and args[-1].endswith(".dmg"):
            with Path(args[-1]).open("ab") as stream:
                stream.write(b"stapled ticket")
        return subprocess.CompletedProcess(args, 0, output, "")

    def test_input_rejects_wrong_source_build_or_signature_metadata(self):
        valid = self.metadata.copy()
        for change in ({"repository": "someone/warp"}, {"commit": "b" * 40}, {"version": "wrong"},
                       {"source_dirty": True}, {"source_dirty": 0}, {"profile": "debug"},
                       {"target": "x86_64-apple-darwin"}, {"notarized": True},
                       {"signing": "developer-id"}, {"features": ["gui"]}):
            with self.subTest(change=change):
                self.metadata = valid | change
                self.write_metadata()
                with self.assertRaises(ValueError):
                    installer.notarize_installer(self.input, self.output, self.commit, self.version, self.config)
        self.assertFalse(self.output.exists())
        self.preflight.assert_not_called()
        self.assertEqual(self.calls, [])

    def test_input_requires_exact_files_and_matching_checksum(self):
        extra = self.input / "extra.txt"
        extra.write_text("extra")
        with self.assertRaisesRegex(ValueError, "exactly"):
            installer.validate_input(self.input, self.commit, self.version)
        extra.unlink()
        self.image.write_bytes(b"changed image")
        with self.assertRaisesRegex(ValueError, "checksum"):
            installer.validate_input(self.input, self.commit, self.version)
        self.image.unlink()
        self.image.symlink_to(self.root / "outside")
        with self.assertRaisesRegex(ValueError, "symlinks"):
            installer.validate_input(self.input, self.commit, self.version)

    def test_existing_output_retains_known_submission_without_resubmitting(self):
        self.output.mkdir()
        receipt = self.output / f"{self.name}.app-notarization.json"
        receipt.write_text('{"id":"existing-submission","status":"In Progress"}')
        with self.assertRaisesRegex(ValueError, "fresh directory"):
            installer.notarize_installer(self.input, self.output, self.commit, self.version, self.config)
        self.assertEqual(json.loads(receipt.read_text())["id"], "existing-submission")
        self.preflight.assert_not_called()
        self.assertEqual(self.calls, [])

    def test_app_version_mismatch_unmounts_before_any_signing(self):
        plist = self.app / "Contents/Info.plist"
        info = plistlib.loads(plist.read_bytes())
        info["WarpVersion"] = "wrong"
        plist.write_bytes(plistlib.dumps(info))
        with self.assertRaisesRegex(ValueError, "identity or version"):
            installer.notarize_installer(self.input, self.output, self.commit, self.version, self.config)
        self.assertEqual(self.calls[-1][:2], ["hdiutil", "detach"])
        self.assertFalse(any("submit" in call or "--force" in call for call in self.calls))

    def test_app_cannot_reference_files_outside_bundle(self):
        (self.app / "Contents/external").symlink_to(self.root / "outside")
        with self.assertRaisesRegex(ValueError, "symlink outside"):
            installer.validate_app(self.app, self.version)
        self.assertEqual(self.calls, [])

    def test_sign_only_keeps_original_and_hashes_stapled_output(self):
        env = {"GITHUB_ACTIONS": "true", "GITHUB_REPOSITORY": "luzkit/nab-release-app", "GITHUB_RUN_ID": "123",
               "GITHUB_RUN_ATTEMPT": "2", "GITHUB_WORKFLOW_REF": installer.SIGNING_WORKFLOW_REF,
               "GITHUB_WORKFLOW_SHA": "b" * 40}
        with patch.dict(os.environ, env, clear=True):
            installer.notarize_installer(self.input, self.output, self.commit, self.version, self.config)
        self.assertEqual(signing.sha256(self.image), self.digest)
        signed_image = self.output / self.image.name
        self.assertTrue(signed_image.read_bytes().endswith(b"stapled ticket"))
        self.assertEqual((self.output / f"{self.name}.dmg.sha256").read_text(),
                         f"{signing.sha256(signed_image)}  {signed_image.name}\n")
        metadata = json.loads((self.output / f"{self.name}.json").read_text())
        self.assertEqual(metadata["commit"], self.commit)
        self.assertEqual(metadata["unsigned_installer_sha256"], self.digest)
        self.assertEqual(metadata["signing_team"], self.config.team)
        self.assertIs(metadata["notarized"], True)
        self.assertEqual(metadata["signing_workflow"]["run_attempt"], "2")
        self.assertEqual(metadata["repository"], "luzkit/warp")
        self.assertEqual(metadata["signing_workflow"]["repository"], "luzkit/nab-release-app")
        detach = next(i for i, call in enumerate(self.calls) if call[:2] == ["hdiutil", "detach"])
        first_sign = next(i for i, call in enumerate(self.calls) if call[:2] == ["codesign", "--force"])
        self.assertLess(detach, first_sign)
        self.assertIn("-readonly", next(call for call in self.calls if call[:2] == ["hdiutil", "attach"]))
        self.assertFalse(any(call[0] in {"cargo", "rustc"} or call[0].endswith("warp-oss") for call in self.calls))
        self.assertEqual(len(list(self.output.iterdir())), 5)

    def test_signing_workflow_must_be_approved_controller_on_main(self):
        for repository, ref in (("luzkit/warp", installer.SIGNING_WORKFLOW_REF),
                                (installer.SIGNING_CONTROLLER, installer.SIGNING_WORKFLOW_REF.replace("main", "other"))):
            with self.subTest(repository=repository, ref=ref), patch.dict(os.environ, {
                "GITHUB_ACTIONS": "true", "GITHUB_REPOSITORY": repository, "GITHUB_WORKFLOW_REF": ref,
                "GITHUB_RUN_ID": "123", "GITHUB_RUN_ATTEMPT": "1",
            }, clear=True):
                with self.assertRaisesRegex(ValueError, "workflow provenance"):
                    installer.notarize_installer(self.input, self.output, self.commit, self.version, self.config)
        self.preflight.assert_not_called()
        self.assertEqual(self.calls, [])


if __name__ == "__main__":
    unittest.main()
