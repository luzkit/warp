#!/usr/bin/env python3
"""Notarize a verified hosted-build installer without executing or rebuilding its app."""

import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import plistlib
import re
import subprocess
import sys
import tempfile

import fork_signing as signing


REPOSITORY = "luzkit/warp"
SIGNING_CONTROLLER = "luzkit/nab-release-app"
SIGNING_WORKFLOW_REF = f"{SIGNING_CONTROLLER}/.github/workflows/fork-notarized-release.yml@refs/heads/main"


def validate_input(directory, source_sha, version):
    if not re.fullmatch(r"[0-9a-f]{40}", source_sha):
        raise ValueError("Expected source SHA must be a full lowercase Git commit SHA")
    if not signing.fork_release_version(version):
        raise ValueError("Notarized release version must be fork-vMAJOR.MINOR.PATCH")
    artifact_name = f"WarpOss-macos-arm64-{version}"
    expected = {f"{artifact_name}{suffix}" for suffix in (".dmg", ".dmg.sha256", ".json")}
    if directory.is_symlink() or not directory.is_dir() or {path.name for path in directory.iterdir()} != expected:
        raise ValueError("Input must contain exactly the expected DMG, SHA256, and provenance files")
    for name in expected:
        path = directory / name
        if path.is_symlink() or not path.is_file():
            raise ValueError("Installer input files must be regular files, not symlinks")
    image = directory / f"{artifact_name}.dmg"
    checksum = (directory / f"{artifact_name}.dmg.sha256").read_text()
    match = re.fullmatch(rf"([0-9a-f]{{64}})  {re.escape(image.name)}\n?", checksum)
    if not match or signing.sha256(image) != match[1]:
        raise ValueError("Installer checksum mismatch")
    metadata = json.loads((directory / f"{artifact_name}.json").read_text())
    expected_metadata = {"repository": REPOSITORY, "version": version, "commit": source_sha,
                         "target": "aarch64-apple-darwin", "profile": "release", "signing": "ad-hoc"}
    if not isinstance(metadata, dict) or any(metadata.get(key) != value for key, value in expected_metadata.items()):
        raise ValueError("Installer provenance does not match the expected fork source, version, or build")
    if metadata.get("source_dirty") is not False or metadata.get("notarized") is not False:
        raise ValueError("Installer must be a clean, unnotarized hosted build")
    if metadata.get("signing_team") is not None or metadata.get("notarization", {}) != {}:
        raise ValueError("Installer already contains signing provenance")
    features = metadata.get("features")
    if not isinstance(features, list) or len(features) != 3 or set(features) != {"extern_plist", "gui", "release_bundle"}:
        raise ValueError("Unexpected installer build features")
    return image, metadata, match[1]


@contextmanager
def mounted_image(image, mountpoint):
    mountpoint.mkdir()
    signing.run(["hdiutil", "verify", image])
    signing.run(["hdiutil", "attach", image, "-readonly", "-nobrowse", "-noautoopen",
                 "-mountpoint", mountpoint])
    try:
        yield mountpoint
    finally:
        signing.run(["hdiutil", "detach", mountpoint])


def validate_app(app, version):
    if app.is_symlink() or not app.is_dir():
        raise ValueError("Installer has no regular WarpOss.app bundle")
    for path in app.rglob("*"):
        if path.is_symlink() and not path.resolve().is_relative_to(app.resolve()):
            raise ValueError("App contains a symlink outside its bundle")
    with (app / "Contents/Info.plist").open("rb") as stream:
        info = plistlib.load(stream)
    if any(info.get(key) != value for key, value in {
        "CFBundleIdentifier": signing.APP_ID, "CFBundleExecutable": "warp-oss",
        "CFBundleName": "WarpOss", "WarpVersion": version,
        "CFBundleShortVersionString": signing.fork_release_version(version),
        "CFBundleVersion": signing.fork_release_version(version),
    }.items()):
        raise ValueError("App bundle identity or version does not match the expected installer")
    executable = app / "Contents/MacOS/warp-oss"
    if not signing.is_macho(executable) or signing.run(["lipo", "-archs", executable]).stdout.strip() != "arm64":
        raise ValueError("Installer executable must be native arm64")
    signing.run(["codesign", "--verify", "--deep", "--strict", app])
    for name in ("THIRD_PARTY_LICENSES.txt", "settings_schema.json"):
        resource = app / "Contents/Resources" / name
        if not resource.is_file() or resource.stat().st_size == 0:
            raise ValueError(f"Required app resource is missing: {name}")


def workflow_provenance():
    result = {key: os.environ.get(variable) for key, variable in {
        "repository": "GITHUB_REPOSITORY", "run_id": "GITHUB_RUN_ID",
        "run_attempt": "GITHUB_RUN_ATTEMPT", "workflow_ref": "GITHUB_WORKFLOW_REF",
        "workflow_sha": "GITHUB_WORKFLOW_SHA",
    }.items()}
    if os.environ.get("GITHUB_ACTIONS") == "true":
        if result["repository"] != SIGNING_CONTROLLER or result["workflow_ref"] != SIGNING_WORKFLOW_REF or not all(
            result[key] and result[key].isdigit() for key in ("run_id", "run_attempt")
        ):
            raise ValueError("Missing or unexpected signing workflow provenance")
    return result


def notarize_installer(directory, output, source_sha, version, config):
    image, metadata, unsigned_sha256 = validate_input(directory, source_sha, version)
    if output.exists() or output.is_symlink() or directory.resolve() in output.resolve().parents:
        raise ValueError("Signed output must be a fresh directory outside the original installer")
    workflow = workflow_provenance()
    signing.preflight(config)
    output.mkdir()
    artifact_name = image.stem
    with tempfile.TemporaryDirectory(prefix="warp-fork-sign-") as temporary:
        staging = Path(temporary)
        image_directory = staging / "image"
        image_directory.mkdir()
        app = image_directory / "WarpOss.app"
        with mounted_image(image, staging / "mounted") as mounted:
            validate_app(mounted / "WarpOss.app", version)
            signing.run(["ditto", mounted / "WarpOss.app", app])
        validate_app(app, version)
        entitlements = staging / "entitlements.plist"
        with (Path(__file__).resolve().parent.parent / "Entitlements.plist").open("rb") as stream:
            rights = plistlib.load(stream)
        rights.pop("com.apple.security.application-groups", None)
        entitlements.write_bytes(plistlib.dumps(rights))
        signing.sign_app(app, entitlements, config)
        signing.notarize_app(app, staging / "WarpOss.zip",
                             output / f"{artifact_name}.app-notarization.json", config)
        (image_directory / "Applications").symlink_to("/Applications")
        (image_directory / "README.txt").write_text(signing.installer_readme(notarized=True))
        signed_image = output / image.name
        signing.run(["hdiutil", "create", "-volname", "WarpOss", "-srcfolder", image_directory,
                     "-fs", "APFS", "-format", "UDZO", signed_image])
        signing.finish_dmg(signed_image, output / f"{artifact_name}.dmg-notarization.json", config)
        signing.run(["hdiutil", "verify", signed_image])
        notarization = {}
        for kind in ("app", "dmg"):
            evidence = json.loads((output / f"{artifact_name}.{kind}-notarization.json").read_text())
            if evidence["status"] != "Accepted":
                raise ValueError("Refusing release metadata without accepted notarization")
            notarization[f"{kind}_submission_id"] = evidence["id"]
        metadata.update(signing="developer-id", signing_team=config.team, notarized=True,
                        notarization=notarization, signing_workflow=workflow,
                        unsigned_installer_sha256=unsigned_sha256)
        (output / f"{artifact_name}.json").write_text(json.dumps(metadata, indent=2) + "\n")
        (output / f"{artifact_name}.dmg.sha256").write_text(f"{signing.sha256(signed_image)}  {signed_image.name}\n")
    if os.environ.get("GITHUB_OUTPUT"):
        with Path(os.environ["GITHUB_OUTPUT"]).open("a") as stream:
            stream.write(f"artifact_dir={output.resolve()}\nartifact_name={artifact_name}\n")
    print(f"Signed and notarized installer: {signed_image}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input_directory", type=Path)
    parser.add_argument("output_directory", type=Path)
    parser.add_argument("--expected-source-sha", required=True)
    parser.add_argument("--expected-version", required=True)
    args = parser.parse_args()
    notarize_installer(args.input_directory, args.output_directory, args.expected_source_sha,
                       args.expected_version, signing.Signing.from_environment())


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"Fork installer notarization failed: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr, file=sys.stderr)
        sys.exit(1)
