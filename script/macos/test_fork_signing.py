"""Credential-free regression tests; Apple commands are mocked, never executed."""

import json
import os
from pathlib import Path
import plistlib
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import fork_signing as signing


class ForkSigningTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.config = signing.Signing("Developer ID Application: Test (ABCDEFGHIJ)", "ABCDEFGHIJ", "test-profile")
        self.calls = []
        self.status = "Accepted"
        self.wait_returncode = 0
        self.timestamp = True
        self.command_patch = patch.object(signing, "run", side_effect=self.fake_run)
        self.command_patch.start()
        self.addCleanup(self.command_patch.stop)

    def fake_run(self, arguments, check=True):
        args = [str(arg) for arg in arguments]
        self.calls.append(args)
        output = ""
        if args[:2] == ["security", "find-identity"]:
            output = f'1) {"A" * 40} "{self.config.identity}"\n1 valid identities found\n'
        elif args[:2] == ["codesign", "--display"]:
            output = f"TeamIdentifier={self.config.team}\nAuthority={self.config.identity}\nCodeDirectory flags=0x10000(runtime)\n"
            if self.timestamp:
                output += "Timestamp=Nov 7, 2026\n"
        elif args[:2] == ["lipo", "-archs"]:
            output = "arm64\n"
        elif args[:3] == ["xcrun", "notarytool", "submit"]:
            output = json.dumps({"id": "11111111-2222-3333-4444-555555555555"})
        elif args[:3] == ["xcrun", "notarytool", "wait"]:
            output = json.dumps({"status": self.status})
        elif args[0] == "ditto":
            Path(args[-1]).write_bytes(b"test archive")
        returncode = self.wait_returncode if args[:3] == ["xcrun", "notarytool", "wait"] else 0
        if check and returncode:
            raise subprocess.CalledProcessError(returncode, args, output=output)
        return subprocess.CompletedProcess(args, returncode, output, "")

    def app(self):
        app = self.root / "WarpOss.app"
        binary = app / "Contents/MacOS/warp-oss"
        binary.parent.mkdir(parents=True)
        binary.write_bytes(b"\xcf\xfa\xed\xfe" + bytes(12))
        with (app / "Contents/Info.plist").open("wb") as stream:
            plistlib.dump({"CFBundleIdentifier": signing.APP_ID, "CFBundleExecutable": "warp-oss"}, stream)
        plugin = app / "Contents/PlugIns/WarpDockTilePlugin.docktileplugin"
        nested = plugin / "Contents/MacOS/WarpDockTilePlugin"
        nested.parent.mkdir(parents=True)
        nested.write_bytes(b"\xcf\xfa\xed\xfe" + bytes(12))
        entitlements = self.root / "entitlements.plist"
        entitlements.write_bytes(plistlib.dumps({}))
        return app, plugin, nested, entitlements

    def test_configuration_fails_closed_for_missing_or_wrong_identity(self):
        env = {"MAC_SIGNING_IDENTITY": self.config.identity, "APPLE_TEAM_ID": self.config.team,
               "APPLE_KEYCHAIN_PROFILE": self.config.profile}
        for replacement in ({"APPLE_KEYCHAIN_PROFILE": ""}, {"APPLE_TEAM_ID": "ZZZZZZZZZZ"},
                            {"MAC_SIGNING_IDENTITY": "Apple Development: Test (ABCDEFGHIJ)"}):
            with self.subTest(replacement=replacement), patch.dict(os.environ, env | replacement, clear=True):
                with self.assertRaises(ValueError):
                    signing.Signing.from_environment()
        self.assertEqual(self.calls, [])

    def test_preflight_authenticates_without_submitting_or_changing_keychain(self):
        with patch.object(signing.platform, "system", return_value="Darwin"), \
             patch.object(signing.platform, "machine", return_value="arm64"), \
             patch.object(signing.shutil, "which", return_value="/test/tool"):
            signing.preflight(self.config)
        self.assertEqual(self.calls[-1][:3], ["xcrun", "notarytool", "history"])
        self.assertEqual([call[1] for call in self.calls if call[0] == "security"], ["find-identity"])
        self.assertFalse(any("submit" in call for call in self.calls))

    def test_preflight_stops_when_notary_authentication_fails(self):
        original = self.fake_run

        def fail_auth(arguments):
            if arguments[:3] == ["xcrun", "notarytool", "history"]:
                raise subprocess.CalledProcessError(1, arguments, stderr="profile unavailable")
            return original(arguments)

        with patch.object(signing, "run", side_effect=fail_auth), \
             patch.object(signing.platform, "system", return_value="Darwin"), \
             patch.object(signing.platform, "machine", return_value="arm64"), \
             patch.object(signing.shutil, "which", return_value="/test/tool"):
            with self.assertRaises(subprocess.CalledProcessError):
                signing.preflight(self.config)
        self.assertFalse(any(call[0] == "codesign" for call in self.calls))

    def test_preflight_rejects_missing_identity_before_notary_authentication(self):
        original = self.fake_run

        def missing_identity(arguments):
            if arguments[:2] == ["security", "find-identity"]:
                return subprocess.CompletedProcess(arguments, 0, "0 valid identities found\n", "")
            return original(arguments)

        with patch.object(signing, "run", side_effect=missing_identity), \
             patch.object(signing.platform, "system", return_value="Darwin"), \
             patch.object(signing.platform, "machine", return_value="arm64"), \
             patch.object(signing.shutil, "which", return_value="/test/tool"):
            with self.assertRaisesRegex(ValueError, "identity/private key"):
                signing.preflight(self.config)
        self.assertFalse(any("history" in call for call in self.calls))

    def test_nested_code_is_signed_inside_out_without_deep_signing(self):
        app, plugin, nested, entitlements = self.app()
        signing.sign_app(app, entitlements, self.config)
        commands = [call for call in self.calls if call[:2] == ["codesign", "--force"]]
        self.assertEqual([call[-1] for call in commands], [str(nested), str(plugin), str(app)])
        for call in commands:
            self.assertIn("--timestamp", call)
            self.assertIn("runtime", call)
            self.assertNotIn("--deep", call)
            self.assertIn(self.config.identity, call)

    def test_missing_secure_timestamp_is_rejected(self):
        self.timestamp = False
        app, _, _, entitlements = self.app()
        with self.assertRaisesRegex(ValueError, "Secure timestamp"):
            signing.sign_app(app, entitlements, self.config)

    def test_fork_tag_sets_both_numeric_bundle_versions(self):
        app, _, _, _ = self.app()
        signing.set_fork_version(app, "fork-v1.2.3")
        info = plistlib.loads((app / "Contents/Info.plist").read_bytes())
        self.assertEqual(info["CFBundleShortVersionString"], "1.2.3")
        self.assertEqual(info["CFBundleVersion"], "1.2.3")
        for invalid in ("fork-v01.2.3", "fork-v1.100.3", "fork-v1.2.3-beta"):
            with self.subTest(version=invalid), self.assertRaises(ValueError):
                signing.fork_release_version(invalid)
        self.assertIsNone(signing.fork_release_version("snapshot-abcdef"))

    def test_app_and_dmg_tickets_require_accepted_notarization(self):
        app, _, _, _ = self.app()
        app_evidence = self.root / "app-notarization.json"
        signing.notarize_app(app, self.root / "app.zip", app_evidence, self.config)
        image = self.root / "installer.dmg"
        image.write_bytes(b"test image")
        dmg_evidence = self.root / "dmg-notarization.json"
        signing.finish_dmg(image, dmg_evidence, self.config)
        for path in (app_evidence, dmg_evidence):
            state = json.loads(path.read_text())
            self.assertEqual(state["status"], "Accepted")
            self.assertTrue(state["id"])
            self.assertEqual(len(state["sha256"]), 64)
        wait_indices = [i for i, call in enumerate(self.calls) if call[:3] == ["xcrun", "notarytool", "wait"]]
        staple_indices = [i for i, call in enumerate(self.calls) if call[:3] == ["xcrun", "stapler", "staple"]]
        self.assertEqual(len(staple_indices), 2)
        self.assertTrue(all(wait < staple for wait, staple in zip(wait_indices, staple_indices)))
        self.assertEqual(len([call for call in self.calls if call[0] == "spctl"]), 2)

    def test_rejection_keeps_evidence_and_never_staples_or_resubmits(self):
        self.status = "Invalid"
        self.wait_returncode = 1
        app, _, _, _ = self.app()
        archive = self.root / "app.zip"
        evidence = self.root / "app-notarization.json"
        with self.assertRaisesRegex(ValueError, "not accepted"):
            signing.notarize_app(app, archive, evidence, self.config)
        self.assertEqual(json.loads(evidence.read_text())["status"], "Invalid")
        self.assertFalse(any(call[:2] == ["xcrun", "stapler"] for call in self.calls))
        with self.assertRaisesRegex(ValueError, "evidence already exists"):
            signing.notarize(archive, evidence, self.config)
        self.assertEqual(len([call for call in self.calls if call[:3] == ["xcrun", "notarytool", "submit"]]), 1)

    def test_wait_failure_preserves_submission_id_without_stapling(self):
        app, _, _, _ = self.app()
        archive = self.root / "app.zip"
        evidence = self.root / "app-notarization.json"
        original = self.fake_run

        def wait_timeout(arguments, check=True):
            if arguments[:3] == ["xcrun", "notarytool", "wait"]:
                return subprocess.CompletedProcess(arguments, 1, "", "timeout")
            return original(arguments, check=check)

        with patch.object(signing, "run", side_effect=wait_timeout):
            with self.assertRaises(ValueError):
                signing.notarize_app(app, archive, evidence, self.config)
        state = json.loads(evidence.read_text())
        self.assertEqual(state["status"], "In Progress")
        self.assertEqual(state["id"], "11111111-2222-3333-4444-555555555555")
        self.assertFalse(any(call[:2] == ["xcrun", "stapler"] for call in self.calls))


if __name__ == "__main__":
    unittest.main()
