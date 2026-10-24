#!/usr/bin/env python3
"""Sign a GitHub release's assets with the OpenPGP release key.

Run locally after the release workflow has uploaded the assets. Each asset is
downloaded and checked against the SHA-256 digest GitHub recorded at upload.
Unsigned assets get a detached binary signature, uploaded as `<asset>.sig`.
Assets that already have one are re-verified against their current bytes and
the release key, so a replaced asset with a stale signature fails instead of
passing. Existing assets and signatures are never overwritten.

Draft releases are supported; `--publish` publishes the draft once every asset
is signed. A repository with immutable releases must sign the draft this way,
because a published immutable release rejects new assets.

The private key stays on the maintainer's machine. Signing in CI would put it
in a repository secret, where anyone able to change a workflow could use it.
"""

import argparse
import hashlib
import json
import subprocess
import tempfile
from pathlib import Path

RELEASE_KEY = "6F0F0D30C09D3A036246A8102CB6524BB54B2CDA"


def run(*command, capture=False):
    result = subprocess.run(command, check=True, text=True, capture_output=capture)
    return result.stdout if capture else None


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as stream:
        for block in iter(lambda: stream.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def signer(path, signature):
    """Return the primary-key fingerprint of a valid signature, else None."""
    result = subprocess.run(
        ["gpg", "--status-fd", "1", "--verify", str(signature), str(path)],
        capture_output=True, text=True,
    )
    if result.returncode != 0:
        return None
    for line in result.stdout.splitlines():
        fields = line.split()
        # VALIDSIG <subkey-fpr> ... <primary-key-fpr> is the last field.
        if fields[:2] == ["[GNUPG:]", "VALIDSIG"]:
            return fields[-1]
    return None


def require_signed_by(path, signature, key):
    if signer(path, signature) != key:
        raise SystemExit(f"{path.name}: {signature.name} is not a valid signature by {key}")


def sign(path, key):
    signature = path.with_name(path.name + ".sig")
    run("gpg", "--local-user", key, "--detach-sign", "--output", str(signature), str(path))
    require_signed_by(path, signature, key)
    return signature


def download(tag, repo, name, directory):
    run("gh", "release", "download", tag, "--repo", repo, "--pattern", name, "--dir", str(directory))
    return directory / name


def find_release(repo, tag):
    # The tags endpoint omits drafts; the release list includes them.
    pages = json.loads(run("gh", "api", "--paginate", "--slurp", f"repos/{repo}/releases", capture=True))
    matches = [release for page in pages for release in page if release["tag_name"] == tag]
    if len(matches) != 1:
        raise SystemExit(f"expected one release for {tag}, found {len(matches)}")
    return matches[0]


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--tag", required=True)
    parser.add_argument("--repo", default="18-lab-dev/vosulay")
    parser.add_argument("--key", default=RELEASE_KEY)
    parser.add_argument("--publish", action="store_true", help="publish the draft release after signing")
    args = parser.parse_args()

    release = find_release(args.repo, args.tag)
    names = {asset["name"] for asset in release["assets"]}
    with tempfile.TemporaryDirectory(prefix="release-sign-") as directory:
        root = Path(directory)
        signatures = []
        verified = []

        for asset in release["assets"]:
            if asset["name"].endswith(".sig"):
                continue
            digest = asset.get("digest") or ""
            if not digest.startswith("sha256:"):
                raise SystemExit(f"{asset['name']}: GitHub recorded no SHA-256 digest")
            path = download(args.tag, args.repo, asset["name"], root)
            if sha256(path) != digest.removeprefix("sha256:"):
                raise SystemExit(f"{asset['name']}: download does not match GitHub's digest")
            if path.name + ".sig" in names:
                existing = download(args.tag, args.repo, path.name + ".sig", root)
                require_signed_by(path, existing, args.key)
                verified.append(path.name)
            else:
                signatures.append(sign(path, args.key))
        if signatures:
            run("gh", "release", "upload", args.tag, "--repo", args.repo, *map(str, signatures))
        for signature in signatures:
            print(f"uploaded {signature.name}")
        for name in verified:
            print(f"verified existing signature for {name}")
    if args.publish and release["draft"]:
        run("gh", "api", "--method", "PATCH", f"repos/{args.repo}/releases/{release['id']}",
            "-F", "draft=false", capture=True)
        print(f"published {args.tag}")


if __name__ == "__main__":
    main()
