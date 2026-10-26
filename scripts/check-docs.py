#!/usr/bin/env python3
"""Check repository Markdown links and reject links to private build evidence."""

import re
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit


def main():
    root = Path(__file__).resolve().parents[1]
    files = [
        root / "README.md",
        root / "CONTRIBUTING.md",
        root / "THIRD_PARTY_NOTICES.md",
    ]
    files.extend(sorted((root / "docs").rglob("*.md")))
    errors = []
    for source in files:
        for match in re.finditer(
            r'!?\[[^\]]*\]\(([^\s)]+)(?:\s+[^)]*)?\)|(?:src|href)="([^"]+)"', source.read_text()
        ):
            url = (match.group(1) or match.group(2)).strip("<>")
            parsed = urlsplit(url)
            if parsed.scheme or parsed.netloc or not parsed.path:
                continue
            path = unquote(parsed.path)
            target = (source.parent / path).resolve()
            line = source.read_text()[: match.start()].count("\n") + 1
            location = f"{source.relative_to(root)}:{line}"
            if not target.is_relative_to(root) or any(
                part in {".local", "target", "dist"}
                for part in target.relative_to(root).parts
            ):
                errors.append(f"{location}: private or external local target: {url}")
            elif not target.exists():
                errors.append(f"{location}: missing target: {url}")
    for error in errors:
        print(error, file=sys.stderr)
    print(f"Checked {len(files)} Markdown files; {len(errors)} broken/private links")
    return bool(errors)


if __name__ == "__main__":
    sys.exit(main())
