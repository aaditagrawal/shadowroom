#!/usr/bin/env python3
"""Plan native checks and promote only verified artifacts of the exact commit."""

import argparse
import importlib.util
import json
import os
import subprocess
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "copr_publish", Path(__file__).with_name("copr-publish.py")
)
publisher = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(publisher)


def needs_native(paths):
    # An allowlist of documentation-only edits; unknown/new files build normally.
    return any(
        name
        and not (
            name.startswith("docs/")
            or name
            in {
                "README.md",
                "CONTRIBUTING.md",
                "LICENSE",
                "THIRD_PARTY_NOTICES.md",
                ".gitignore",
            }
        )
        for name in paths
    )


def reusable_run(sha, api=publisher.github):
    runs = api(
        f"actions/workflows/ci.yml/runs?head_sha={sha}&branch=main&event=push&status=success&per_page=100"
    )["workflow_runs"]
    for run in runs:
        try:
            jobs = api(f"actions/runs/{run['id']}/jobs?per_page=100")["jobs"]
            publisher.validate_build_run(run, jobs, sha)
            artifacts = api(f"actions/runs/{run['id']}/artifacts?per_page=100")[
                "artifacts"
            ]
            publisher.validate_artifacts(artifacts)
        except ValueError:
            continue  # Expired/incomplete builds fall back to fresh native checks.
        return run["id"]
    return ""


def plan(sha):
    reuse = ""
    native = True
    if (
        os.environ.get("GITHUB_EVENT_NAME") == "pull_request"
        and os.environ.get("IS_DRAFT") == "true"
    ):
        native = False
    elif (
        os.environ.get("GITHUB_REF_TYPE") == "tag"
        and os.environ.get("GITHUB_EVENT_NAME") == "push"
    ):
        reuse = reusable_run(sha)
    elif os.environ.get("GITHUB_REF_TYPE") != "tag" and (
        base := os.environ.get("BASE_SHA")
    ):
        changed = subprocess.run(
            ["git", "diff", "--name-only", "-z", base, "HEAD"],
            capture_output=True,
            check=False,
        )
        if changed.returncode == 0:
            native = needs_native(changed.stdout.decode().strip("\0").split("\0"))
    return {"reuse_run_id": str(reuse), "native": str(native).lower()}


def receipt(sha, tag, build_id, run_id, api=publisher.github):
    run = api(f"actions/runs/{build_id}")
    jobs = api(f"actions/runs/{build_id}/jobs?per_page=100")["jobs"]
    if build_id != run_id:
        publisher.validate_build_run(run, jobs, sha)
    else:
        # The publishing job is still running; all native predecessors must pass.
        publisher.require(
            run.get("head_sha") == sha
            and run.get("head_branch") == tag
            and run.get("event") == "push"
            and run.get("path", "").split("@", 1)[0] == ".github/workflows/ci.yml"
            and run.get("head_repository", {}).get("full_name") == publisher.REPOSITORY,
            "Fresh native build differs from the release tag",
        )
        succeeded = {j["name"] for j in jobs if j.get("conclusion") == "success"}
        publisher.require(
            publisher.NATIVE_JOBS <= succeeded, "Native build checks are incomplete"
        )
    artifacts = publisher.validate_artifacts(
        api(f"actions/runs/{build_id}/artifacts?per_page=100")["artifacts"]
    )
    return {
        "schema": 1,
        "tag": tag,
        "source_commit": sha,
        "build_run_id": build_id,
        "publish_run_id": run_id,
        "artifacts": [
            {
                key: a[key]
                for key in ("id", "name", "digest", "size_in_bytes", "expired")
            }
            for a in artifacts
        ],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--receipt", type=Path)
    args = parser.parse_args()
    sha = os.environ["GITHUB_SHA"]
    if args.receipt:
        run_id = int(os.environ["GITHUB_RUN_ID"])
        build_id = int(os.environ.get("BUILD_RUN_ID") or run_id)
        data = receipt(sha, os.environ["GITHUB_REF_NAME"], build_id, run_id)
        args.receipt.parent.mkdir(parents=True, exist_ok=True)
        args.receipt.write_text(json.dumps(data, indent=2) + "\n")
    else:
        data = plan(sha)
        with Path(os.environ["GITHUB_OUTPUT"]).open("a") as output:
            output.writelines(f"{k}={v}\n" for k, v in data.items())
    print(json.dumps(data))


if __name__ == "__main__":
    main()
