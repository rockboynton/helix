#!/usr/bin/env python3
"""Check local page/fragment links in an already built mdBook (no network)."""

import argparse
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urlsplit


class Page(HTMLParser):
    def __init__(self, source):
        super().__init__()
        self.ids = set()
        self.links = []
        self.feed(source)

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if "id" in attrs:
            self.ids.add(attrs["id"])
        if tag == "a" and "name" in attrs:
            self.ids.add(attrs["name"])
        if tag == "a" and "href" in attrs:
            self.links.append(attrs["href"])


def check(root):
    root = root.resolve()
    pages = {path: Page(path.read_text(encoding="utf-8")) for path in root.rglob("*.html")}
    errors = []
    count = 0
    for path, page in pages.items():
        for link in page.links:
            parts = urlsplit(link)
            if parts.scheme or parts.netloc:
                continue
            count += 1
            target = ((root if parts.path.startswith("/") else path.parent)
                      / unquote(parts.path).lstrip("/")).resolve() if parts.path else path
            if target.is_dir():
                target /= "index.html"
            if not target.is_file():
                errors.append(f"{path.relative_to(root)}: missing target {link}")
            elif parts.fragment and target in pages and unquote(parts.fragment) not in pages[target].ids:
                errors.append(f"{path.relative_to(root)}: missing anchor {link}")
    if errors:
        raise SystemExit("\n".join(sorted(set(errors))))
    print(f"Checked {count} local links across {len(pages)} HTML pages.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("build_dir", type=Path)
    args = parser.parse_args()
    if not (args.build_dir / "index.html").is_file():
        parser.error("build_dir must contain a built mdBook index.html")
    check(args.build_dir)
