#!/usr/bin/env python3
"""Publish one verified GitHub release SRPM; reuse, never duplicate its COPR EVR.

Run with uv tool run --from copr-cli==2.6 --with copr==2.7
--with rpmfile==2.2.1 --with zstandard==0.25.0 python scripts/copr-publish.py ...
Only --submit uses COPR_CONFIG. Verification never executes release archive code.
"""

import argparse
import configparser
import hashlib
import json
import os
import re
import subprocess
import tarfile
import tempfile
import time
from pathlib import Path
from urllib.parse import urlencode
from urllib.request import Request, urlopen

REPOSITORY = "aaditagrawal/shadowroom"
PROJECT = "aaditagrawal/shadowroom"
COPR_URL = "https://copr.fedorainfracloud.org"
CHROOT = "fedora-44-x86_64"
REQUIRED_JOBS = {
    "Core / ubuntu-24.04",
    "Core / macos-15",
    "Linux x86_64 / glibc 2.28",
    "macOS arm64 native package",
    "Fedora 44 x86_64 RPM",
    "Public source bundle",
    "Publish verified GitHub release",
}
TERMINAL_FAILURES = {"failed", "canceled", "cancelled", "skipped"}
MAX_ASSET_BYTES = 512 * 1024 * 1024


def require(condition, message):
    if not condition:
        raise ValueError(message)


def github(path):
    endpoint = f"repos/{REPOSITORY}" + (f"/{path}" if path else "")
    result = subprocess.run(
        ["gh", "api", "--method", "GET", endpoint],
        capture_output=True,
        timeout=90,
        check=False,
    )
    require(result.returncode == 0, f"GitHub API request failed: {path}")
    return json.loads(result.stdout)


def public_json(path, **query):
    suffix = "?" + urlencode(query) if query else ""
    request = Request(
        f"{COPR_URL}/api_3/{path}{suffix}", headers={"User-Agent": "Shadowroom-release"}
    )
    with urlopen(request, timeout=60) as response:
        return json.load(response)


def download(url, destination, expected_sha=None):
    require(url.startswith("https://"), "Only HTTPS release downloads are allowed")
    digest = hashlib.sha256()
    size = 0
    request = Request(url, headers={"User-Agent": "Shadowroom-release"})
    with urlopen(request, timeout=90) as response, destination.open("xb") as output:
        while block := response.read(1024 * 1024):
            size += len(block)
            require(
                size <= MAX_ASSET_BYTES, "Release asset exceeds the download size limit"
            )
            output.write(block)
            digest.update(block)
    actual = digest.hexdigest()
    require(
        expected_sha is None or actual == expected_sha,
        "Downloaded SRPM checksum does not match",
    )
    return actual


def checksum_manifest(data):
    entries = {}
    for line in data.splitlines():
        match = re.fullmatch(r"([0-9a-f]{64}) [ *]([A-Za-z0-9_.+-]+)", line)
        require(match is not None, "Malformed release checksum manifest")
        digest, name = match.groups()
        require(name not in entries, "Duplicate filename in release checksum manifest")
        entries[name] = digest
    require(entries, "Empty release checksum manifest")
    return entries


def release_commit(tag, api=github):
    require(
        re.fullmatch(r"v\d+\.\d+\.\d+", tag), "Use a published numeric v<VERSION> tag"
    )
    obj = api(f"git/ref/tags/{tag}")["object"]
    for _ in range(5):
        if obj["type"] == "commit":
            require(re.fullmatch(r"[0-9a-f]{40}", obj["sha"]), "Invalid release commit")
            return obj["sha"]
        require(obj["type"] == "tag", "Release ref does not resolve to a commit")
        obj = api(f"git/tags/{obj['sha']}")["object"]
    raise ValueError("Excessively nested release tag")


def validate_source_run(run, jobs, tag, sha, current=False):
    require(run.get("event") == "push", "Release source was not a push run")
    require(
        run.get("head_sha") == sha and run.get("head_branch") == tag,
        "Native checks did not run on the exact release tag and commit",
    )
    require(
        run.get("path", "").split("@", 1)[0] == ".github/workflows/ci.yml",
        "Release source run is not Native checks",
    )
    require(
        run.get("head_repository", {}).get("full_name") == REPOSITORY,
        "Release source run belongs to another repository",
    )
    if not current:
        require(
            run.get("status") == "completed", "Native source checks are still running"
        )
    succeeded = {job["name"] for job in jobs if job.get("conclusion") == "success"}
    require(
        REQUIRED_JOBS <= succeeded,
        "Required native checks or GitHub publication did not succeed",
    )


def verified_release(tag, current_run=None, api=github):
    sha = release_commit(tag, api)
    release = api(f"releases/tags/{tag}")
    require(
        not release.get("draft")
        and release.get("published_at")
        and release.get("tag_name") == tag,
        "Only a published GitHub release may be submitted",
    )
    # The publisher implementation comes from the default branch, and only
    # reviewed mainline commits can supply privileged release bytes.
    default_branch = api("")["default_branch"]
    comparison = api(f"compare/{sha}...{default_branch}")
    require(
        comparison.get("merge_base_commit", {}).get("sha") == sha,
        "Release source is not contained in the repository's default branch",
    )
    if current_run:
        require(
            str(current_run) == os.environ.get("GITHUB_RUN_ID"),
            "Current source run must be this workflow run",
        )
        candidates = [api(f"actions/runs/{current_run}")]
    else:
        candidates = api(
            f"actions/workflows/ci.yml/runs?head_sha={sha}&event=push&status=completed&per_page=100"
        )["workflow_runs"]
    for run in candidates:
        if run.get("head_branch") != tag:
            continue
        jobs = api(f"actions/runs/{run['id']}/jobs?per_page=100")["jobs"]
        validate_source_run(run, jobs, tag, sha, current=bool(current_run))
        return release, sha, run["id"]
    raise ValueError(
        "No successful Native checks run was found for the exact release tag"
    )


def tar_member(stream, name):
    with tarfile.open(fileobj=stream, mode="r|gz") as archive:
        for index, member in enumerate(archive):
            require(index < 200_000, "Source archive contains too many entries")
            if member.name == name:
                require(
                    member.isfile() and member.size <= 8 * 1024 * 1024,
                    "Unsafe provenance/lockfile archive entry",
                )
                return archive.extractfile(member).read()
    raise ValueError(f"Required source archive entry is absent: {name}")


def inspect_srpm(path, version, release, sha, lock_sha):
    import rpmfile  # Pinned isolated publisher dependency, not an app dependency.

    with rpmfile.open(path) as archive:
        for key, expected in {
            "name": "shadowroom",
            "version": version,
            "release": release,
        }.items():
            require(
                archive.headers.get(key) == expected.encode(),
                f"SRPM {key} differs from the released package",
            )
        require(
            archive.headers.get("sourcepackage") == 1,
            "Release asset is not a source RPM",
        )
        names = [member.name for member in archive.getmembers()]
        source = f"shadowroom-{version}.tar.gz"
        vendor = f"shadowroom-{version}-vendor.tar.gz"
        require(
            sorted(names) == sorted([source, vendor, "shadowroom.spec"]),
            "Unexpected source RPM contents",
        )
        checksums = {}
        for name in names:
            member = archive.getmember(name)
            require(not member.issymlink, "Source RPM contains a symlink")
            with archive.extractfile(member) as stream:
                digest = hashlib.sha256()
                while block := stream.read(1024 * 1024):
                    digest.update(block)
                checksums[name] = digest.hexdigest()
        with archive.extractfile(vendor) as stream:
            provenance = json.loads(
                tar_member(stream, "rpm-notices/SOURCE-PROVENANCE.json")
            )
        with archive.extractfile(source) as stream:
            embedded_lock = hashlib.sha256(
                tar_member(stream, f"shadowroom-{version}/Cargo.lock")
            ).hexdigest()
    expected = {
        "source_commit": sha,
        "package_version": version,
        "rpm_release": release,
        "cargo_lock_sha256": lock_sha,
    }
    require(
        all(provenance.get(key) == value for key, value in expected.items()),
        "SRPM provenance differs from the released source",
    )
    require(embedded_lock == lock_sha, "SRPM contains a different Cargo.lock")
    return checksums


def existing_build(builds, version_release):
    matches = []
    for build in builds:
        source = build.get("source_package") or {}
        if (
            source.get("name") == "shadowroom"
            and source.get("version") == version_release
        ):
            matches.append(build)
        elif build.get("state") not in {
            "succeeded",
            *TERMINAL_FAILURES,
        } and not (source.get("name") and source.get("version")):
            raise ValueError(
                "An unidentified COPR upload is active; inspect it before submitting"
            )
    require(
        len(matches) <= 1,
        "Multiple COPR builds already exist for this EVR; operator review required",
    )
    if matches:
        require(
            matches[0].get("state") not in TERMINAL_FAILURES,
            "This EVR already has a failed/cancelled COPR build; repair/review it instead of creating a duplicate",
        )
        return matches[0]
    return None


def all_builds():
    owner, project = PROJECT.split("/")
    result = []
    for offset in range(0, 10_000, 100):
        page = public_json(
            "build/list", ownername=owner, projectname=project, limit=100, offset=offset
        )["items"]
        result.extend(page)
        if len(page) < 100:
            return result
    raise ValueError(
        "COPR history exceeds the inspection limit; refusing an unchecked submission"
    )


def write_receipt(path, receipt):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(receipt, indent=2) + "\n")
    temporary.replace(path)


def credentials(text):
    parser = configparser.ConfigParser(interpolation=None)
    try:
        parser.read_string(text)
        section = parser["copr-cli"]
        require(
            section.get("copr_url", "").rstrip("/") == COPR_URL,
            "COPR configuration must use the Fedora HTTPS endpoint",
        )
        require(
            section.get("username") == PROJECT.split("/")[0],
            "COPR configuration has another owner",
        )
        require(
            section.get("login") and section.get("token"),
            "COPR configuration is missing API credentials",
        )
    except (configparser.Error, KeyError):
        raise ValueError("Invalid COPR API configuration") from None


def submit_once(srpm, secret):
    from copr.v3 import Client

    credentials(secret)
    descriptor, filename = tempfile.mkstemp(prefix="shadowroom-copr-")
    try:
        with os.fdopen(descriptor, "w") as output:
            os.fchmod(output.fileno(), 0o600)
            output.write(secret)
        client = Client.create_from_config_file(filename)
        # A connection error after POST can still mean acceptance. Never retry
        # a mutating request; a later run first inspects public build history.
        client.config["connection_attempts"] = 1
        owner, project = PROJECT.split("/")
        result = client.build_proxy.create_from_file(
            owner,
            project,
            str(srpm),
            buildopts={"chroots": [CHROOT], "enable_net": False},
        )
        if isinstance(result, list):
            require(len(result) == 1, "COPR did not return exactly one build")
            result = result[0]
        return int(result["id"])
    finally:
        Path(filename).unlink(missing_ok=True)


def wait_build(build_id, receipt, output, work, expected, archives, minutes):
    deadline = time.monotonic() + minutes * 60
    validated_existing = not receipt["reused"]
    while True:
        build = public_json(f"build/{build_id}")
        require(
            build.get("ownername") == PROJECT.split("/")[0]
            and build.get("projectname") == PROJECT.split("/")[1],
            "Recorded COPR build belongs to another project",
        )
        require(
            CHROOT in build.get("chroots", []),
            "COPR build does not cover Fedora 44 x86_64",
        )
        state = build["state"]
        receipt["state"] = state
        write_receipt(output, receipt)
        print(f"COPR build {build_id}: {state}", flush=True)
        source = build.get("source_package") or {}
        if not validated_existing and state == "succeeded" and source.get("url"):
            url = source["url"]
            require(
                url.startswith(
                    f"https://download.copr.fedorainfracloud.org/results/{PROJECT}/"
                ),
                "Unexpected COPR source download URL",
            )
            copied = work / "copr-existing.src.rpm"
            download(url, copied)
            require(
                inspect_srpm(copied, **expected) == archives,
                "Existing COPR source differs from the verified release SRPM",
            )
            validated_existing = True
        require(
            state not in TERMINAL_FAILURES,
            f"COPR build {build_id} did not succeed; no duplicate was submitted",
        )
        if state == "succeeded":
            require(validated_existing, "Cannot verify the existing COPR source RPM")
            return
        require(
            time.monotonic() < deadline,
            f"COPR build {build_id} is still active; rerun to resume watching this EVR",
        )
        time.sleep(30)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--source-run-id", type=int)
    parser.add_argument(
        "--output", type=Path, default=Path(".local/copr-publication.json")
    )
    parser.add_argument("--submit", action="store_true")
    parser.add_argument("--wait-minutes", type=int, default=300)
    args = parser.parse_args()
    require(
        os.environ.get("COPR_PROJECT", PROJECT) == PROJECT, "Unexpected COPR project"
    )
    require(
        1 <= args.wait_minutes <= 330, "COPR wait must be between 1 and 330 minutes"
    )
    release, sha, run = verified_release(args.tag, args.source_run_id)
    version = args.tag[1:]
    candidates = [
        asset
        for asset in release["assets"]
        if re.fullmatch(
            rf"shadowroom-{re.escape(version)}-[1-9]\d*\.src\.rpm", asset["name"]
        )
    ]
    require(
        len(candidates) == 1, "Release must contain exactly one Shadowroom source RPM"
    )
    asset = candidates[0]
    rpm_release = asset["name"][len(f"shadowroom-{version}-") : -len(".src.rpm")]
    digest = asset.get("digest", "")
    require(
        re.fullmatch(r"sha256:[0-9a-f]{64}", digest),
        "GitHub source RPM asset lacks its SHA-256 digest",
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(
        prefix="copr-release-", dir=args.output.parent
    ) as directory:
        work = Path(directory)
        manifest_asset = [
            item for item in release["assets"] if item["name"] == "SHA256SUMS"
        ]
        require(
            len(manifest_asset) == 1,
            "Release checksum manifest is missing or ambiguous",
        )
        manifest = work / "SHA256SUMS"
        download(manifest_asset[0]["browser_download_url"], manifest)
        checksums = checksum_manifest(manifest.read_text())
        require(
            checksums.get(asset["name"]) == digest[7:],
            "GitHub digest differs from the release checksum manifest",
        )
        srpm = work / asset["name"]
        print(f"Downloading {asset['name']} and checking release SHA-256", flush=True)
        download(asset["browser_download_url"], srpm, digest[7:])
        lock = github(f"contents/Cargo.lock?ref={sha}")
        import base64

        lock_sha = hashlib.sha256(base64.b64decode(lock["content"])).hexdigest()
        expected = {
            "version": version,
            "release": rpm_release,
            "sha": sha,
            "lock_sha": lock_sha,
        }
        archives = inspect_srpm(srpm, **expected)
        receipt = {
            "tag": args.tag,
            "source_commit": sha,
            "source_run_id": run,
            "asset_id": asset["id"],
            "srpm_sha256": digest[7:],
            "source_archives": archives,
            "project": PROJECT,
            "chroot": CHROOT,
            "evr": f"{version}-{rpm_release}",
            "state": "verified",
        }
        write_receipt(args.output, receipt)
        print(
            f"Verified {args.tag} ({sha}) source RPM and embedded Cargo/provenance",
            flush=True,
        )
        if not args.submit:
            return
        owner, project = PROJECT.split("/")
        configuration = public_json("project", ownername=owner, projectname=project)
        require(
            configuration.get("enable_net") is False
            and CHROOT in configuration.get("chroot_repos", {}),
            "COPR project must enable Fedora 44 x86_64 with build networking disabled",
        )
        previous = existing_build(all_builds(), receipt["evr"])
        receipt["reused"] = previous is not None
        if previous:
            build_id = previous["id"]
        else:
            secret = os.environ.pop("COPR_CONFIG", "")
            require(secret, "COPR_CONFIG repository secret is required for submission")
            credentials(secret)
            receipt["state"] = "submitting"
            write_receipt(args.output, receipt)
            try:
                build_id = submit_once(srpm, secret)
            except Exception:  # noqa: BLE001 - Do not expose authenticated HTTP/configuration errors.
                receipt["state"] = "submission_uncertain"
                write_receipt(args.output, receipt)
                raise ValueError(
                    "COPR submission response was not confirmed. Inspect project history before retrying; no POST was retried."
                ) from None
            finally:
                del secret
        receipt["build_id"] = build_id
        receipt["build_url"] = f"{COPR_URL}/coprs/build/{build_id}/"
        write_receipt(args.output, receipt)
        wait_build(
            build_id, receipt, args.output, work, expected, archives, args.wait_minutes
        )
        summary = os.environ.get("GITHUB_STEP_SUMMARY")
        if summary:
            with open(summary, "a") as stream:
                stream.write(
                    f"Published `{receipt['evr']}` to [{PROJECT}]({receipt['build_url']}) ({CHROOT}).\n"
                )


if __name__ == "__main__":
    try:
        main()
    except Exception as error:  # noqa: BLE001 - Print controlled errors without credential-bearing tracebacks.
        # Only our controlled errors are printed; third-party exception text may
        # include configuration/HTTP internals and must not expose credentials.
        message = (
            str(error)
            if isinstance(error, ValueError)
            else "Publisher failed; inspect the sanitized receipt and service status"
        )
        raise SystemExit(message) from None
