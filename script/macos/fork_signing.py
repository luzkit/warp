#!/usr/bin/env python3
"""Sign fork installers using an existing Developer ID identity and Keychain profile."""

import argparse
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import shutil
import subprocess
import sys


APP_ID = "dev.warp.WarpOss"
MACHO_MAGIC = {
    b"\xfe\xed\xfa\xce", b"\xce\xfa\xed\xfe", b"\xfe\xed\xfa\xcf", b"\xcf\xfa\xed\xfe",
    b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca", b"\xca\xfe\xba\xbf", b"\xbf\xba\xfe\xca",
}


@dataclass(frozen=True)
class Signing:
    identity: str
    team: str
    profile: str

    @classmethod
    def from_environment(cls):
        values = [os.environ.get(name, "").strip() for name in (
            "MAC_SIGNING_IDENTITY", "APPLE_TEAM_ID", "APPLE_KEYCHAIN_PROFILE"
        )]
        identity, team, profile = values
        if not all(values):
            raise ValueError("Developer ID mode requires MAC_SIGNING_IDENTITY, APPLE_TEAM_ID, and APPLE_KEYCHAIN_PROFILE")
        if not re.fullmatch(r"[A-Z0-9]{10}", team):
            raise ValueError("APPLE_TEAM_ID must be a ten-character Apple team identifier")
        if not identity.startswith("Developer ID Application: ") or not identity.endswith(f" ({team})"):
            raise ValueError("MAC_SIGNING_IDENTITY must be a full Developer ID Application identity for APPLE_TEAM_ID")
        return cls(identity, team, profile)


def run(arguments, check=True):
    return subprocess.run([str(arg) for arg in arguments], check=check, text=True, capture_output=True)


def preflight(signing):
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        raise ValueError("Developer ID packaging requires an Apple Silicon Mac")
    for tool in ("security", "codesign", "xcrun", "ditto", "spctl", "hdiutil", "lipo"):
        if shutil.which(tool) is None:
            raise ValueError(f"Required signing tool is unavailable: {tool}")
    for tool in ("notarytool", "stapler"):
        run(["xcrun", "--find", tool])
    identities = run(["security", "find-identity", "-v", "-p", "codesigning"]).stdout
    if not any(f'"{signing.identity}"' in line for line in identities.splitlines()):
        raise ValueError("The configured Developer ID identity/private key is not available in this account's Keychain")
    # Authentication must succeed before a costly build; submission history stays private.
    run(["xcrun", "notarytool", "history", "--keychain-profile", signing.profile, "--output-format", "json"])
    print("Developer ID identity and notarization profile preflight passed")


def requirement(signing, identifier=None):
    value = (f'=anchor apple generic and certificate leaf[subject.OU] = "{signing.team}" '
             'and certificate leaf[field.1.2.840.113635.100.6.1.13] exists')
    return value + (f' and identifier "{identifier}"' if identifier else "")


def verify_code(path, signing, runtime=False, identifier=None):
    run(["codesign", "--verify", "--strict", "--all-architectures", "-R", requirement(signing, identifier), path])
    result = run(["codesign", "--display", "--verbose=4", path])
    details = result.stdout + result.stderr
    lines = details.splitlines()
    if f"TeamIdentifier={signing.team}" not in lines:
        raise ValueError(f"Signing team mismatch: {path}")
    if not any(line.startswith("Authority=Developer ID Application:") for line in lines):
        raise ValueError(f"Developer ID authority missing: {path}")
    if not any(line.startswith("Timestamp=") for line in lines):
        raise ValueError(f"Secure timestamp missing: {path}")
    if runtime and not re.search(r"^CodeDirectory .*\([^)]*\bruntime\b[^)]*\)", details, re.MULTILINE):
        raise ValueError(f"Hardened runtime missing: {path}")


def is_macho(path):
    if path.is_symlink() or not path.is_file():
        return False
    with path.open("rb") as stream:
        header = stream.read(16)
    if header[:4] not in MACHO_MAGIC:
        return False
    if header[:4] in {b"\xca\xfe\xba\xbe", b"\xca\xfe\xba\xbf", b"\xbe\xba\xfe\xca", b"\xbf\xba\xfe\xca"}:
        endian = "big" if header[0] == 0xCA else "little"
        return int.from_bytes(header[8:12], endian) in (7, 12, 0x01000007, 0x0100000C)
    return True


def nested_signing_targets(app, executable):
    native_files = [path for path in app.rglob("*") if is_macho(path) and path != executable]
    containers = {
        parent for path in native_files for parent in path.parents
        if parent != app and app in parent.parents
        and parent.suffix in {".app", ".framework", ".xpc", ".appex", ".bundle", ".docktileplugin"}
        and not parent.is_symlink()
    }
    return sorted(set(native_files) | containers, key=lambda path: (-len(path.parts), str(path)))


def sign_app(app, entitlements, signing):
    with (app / "Contents/Info.plist").open("rb") as stream:
        info = plistlib.load(stream)
    if info.get("CFBundleIdentifier") != APP_ID:
        raise ValueError("Unexpected fork app bundle identifier")
    executable = app / "Contents/MacOS" / info["CFBundleExecutable"]
    if executable.resolve().parent != (app / "Contents/MacOS").resolve() or not is_macho(executable):
        raise ValueError("Unexpected fork app executable")
    with entitlements.open("rb") as stream:
        rights = plistlib.load(stream)
    if rights.get("com.apple.security.get-task-allow") or "com.apple.security.application-groups" in rights:
        raise ValueError("Distribution entitlements must exclude debugger access and upstream app groups")
    targets = nested_signing_targets(app, executable)
    for target in targets:
        run(["codesign", "--force", "--sign", signing.identity, "--timestamp", "--options", "runtime", target])
    run(["codesign", "--force", "--sign", signing.identity, "--timestamp", "--options", "runtime",
         "--entitlements", entitlements, app])
    for target in targets:
        verify_code(target, signing, runtime=True)
    verify_code(app, signing, runtime=True, identifier=APP_ID)
    run(["codesign", "--verify", "--deep", "--strict", "--all-architectures", app])
    if run(["lipo", "-archs", executable]).stdout.strip() != "arm64":
        raise ValueError("Expected an arm64 app executable")


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def installer_readme(notarized):
    description = ("This build is Developer ID signed and notarized by Apple." if notarized else
                   "This build is ad hoc signed and is not notarized by Apple. macOS may require\n"
                   "approval in System Settings > Privacy & Security before its first launch.")
    return f"""WarpOss — luzkit/warp fork for Apple Silicon

Drag WarpOss.app into Applications, then open it from Applications.
{description}

Persistent sessions: use the terminal pane menu to name or detach a session.
Use Attach to Session in Warp search to reconnect. End Session stops the shell.
Sessions survive closing the app after being retained, but not a reboot or loss
of their owning process. Scrollback is bounded.

Source and installation details: https://github.com/luzkit/warp
"""


def fork_release_version(version):
    if not version.startswith("fork-v"):
        return None
    match = re.fullmatch(r"fork-v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", version)
    if not match or any(int(value) > maximum for value, maximum in zip(match.groups(), (9999, 99, 99))):
        raise ValueError("Fork release versions must be fork-vMAJOR.MINOR.PATCH within Apple bundle version limits")
    return ".".join(match.groups())


def set_fork_version(app, version):
    numeric_version = fork_release_version(version)
    if numeric_version:
        path = app / "Contents/Info.plist"
        with path.open("rb") as stream:
            info = plistlib.load(stream)
        info.update(CFBundleShortVersionString=numeric_version, CFBundleVersion=numeric_version)
        path.write_bytes(plistlib.dumps(info))


def notarize(artifact, evidence, signing):
    state = {"artifact": artifact.name, "sha256": sha256(artifact), "status": "upload-started"}
    try:
        with evidence.open("x") as stream:
            stream.write(json.dumps(state, indent=2) + "\n")
    except FileExistsError as error:
        raise ValueError(f"Notarization evidence already exists; inspect its submission before retrying: {evidence}") from error
    submitted = run(["xcrun", "notarytool", "submit", artifact, "--keychain-profile", signing.profile,
                     "--no-wait", "--output-format", "json"])
    submission = json.loads(submitted.stdout)
    submission_id = submission.get("id")
    if not isinstance(submission_id, str) or not re.fullmatch(r"[0-9a-fA-F-]{36}", submission_id):
        raise ValueError(f"Apple returned no valid submission ID; preserve and inspect {evidence}")
    state.update(id=submission_id, status="In Progress")
    evidence.write_text(json.dumps(state, indent=2) + "\n")
    print(f"Notarization submission: {submission_id}", flush=True)
    completed = run(["xcrun", "notarytool", "wait", submission_id, "--keychain-profile", signing.profile,
                     "--timeout", "15m", "--output-format", "json"], check=False)
    state["status"] = json.loads(completed.stdout).get("status")
    evidence.write_text(json.dumps(state, indent=2) + "\n")
    if state["status"] != "Accepted" or completed.returncode:
        raise ValueError(f"Notarization was not accepted ({state['status']}); inspect submission {submission_id}")


def notarize_app(app, archive, evidence, signing):
    run(["ditto", "-c", "-k", "--keepParent", app, archive])
    notarize(archive, evidence, signing)
    run(["xcrun", "stapler", "staple", app])
    run(["xcrun", "stapler", "validate", app])
    run(["codesign", "--verify", "--deep", "--strict", app])
    run(["spctl", "--assess", "--type", "execute", "--verbose=2", app])


def finish_dmg(image, evidence, signing):
    run(["codesign", "--force", "--sign", signing.identity, "--timestamp", image])
    verify_code(image, signing)
    notarize(image, evidence, signing)
    run(["xcrun", "stapler", "staple", image])
    run(["xcrun", "stapler", "validate", image])
    verify_code(image, signing)
    run(["spctl", "--assess", "--type", "open", "--context", "context:primary-signature", "--verbose=2", image])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("preflight")
    readme = commands.add_parser("write-readme")
    readme.add_argument("destination", type=Path)
    readme.add_argument("--notarized", action="store_true")
    version = commands.add_parser("set-version")
    version.add_argument("app", type=Path)
    version.add_argument("version")
    validate_version = commands.add_parser("validate-version")
    validate_version.add_argument("version")
    app_sign = commands.add_parser("sign-app")
    app_sign.add_argument("app", type=Path)
    app_sign.add_argument("entitlements", type=Path)
    app_notarize = commands.add_parser("notarize-app")
    app_notarize.add_argument("app", type=Path)
    app_notarize.add_argument("archive", type=Path)
    app_notarize.add_argument("evidence", type=Path)
    dmg = commands.add_parser("finish-dmg")
    dmg.add_argument("image", type=Path)
    dmg.add_argument("evidence", type=Path)
    args = parser.parse_args()
    if args.command == "write-readme":
        args.destination.write_text(installer_readme(args.notarized))
        return
    if args.command == "set-version":
        set_fork_version(args.app, args.version)
        return
    if args.command == "validate-version":
        fork_release_version(args.version)
        return
    signing = Signing.from_environment()
    if args.command == "preflight":
        preflight(signing)
    elif args.command == "sign-app":
        sign_app(args.app, args.entitlements, signing)
    elif args.command == "notarize-app":
        notarize_app(args.app, args.archive, args.evidence, signing)
    elif args.command == "finish-dmg":
        finish_dmg(args.image, args.evidence, signing)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"Developer ID packaging failed: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr, file=sys.stderr)
        sys.exit(1)
