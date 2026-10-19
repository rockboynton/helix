# Building the Neohelix book

Run from the repository root:

```sh
cargo xtask docgen
mdbook build book
python3 contrib/check-book.py book/book
HX=target/release/hx python3 -m unittest discover -s contrib -p test_check_book.py -v
HX=target/release/hx python3 -m unittest discover -s contrib -p test_neohelix_test_fixture.py -v
HX=target/release/hx python3 -m unittest discover -s contrib -p test_health.py -v
```

Use `mdbook serve book` for local browsing. CI builds the `neohelix` branch under
its own Pages directory, without claiming upstream's custom domain or stable
release channel. Publishing still requires GitHub Pages to be enabled on the fork.

The guides in `book/src/neohelix/` include their canonical Markdown from `docs/`.
Edit those source guides, not the include wrappers. Keep their links to other
included guides relative; use full repository URLs for source files outside the
book. New guides need both a wrapper and an entry in `book/src/SUMMARY.md`.
The overview in `book/src/neohelix/index.md` is book-specific; `NEOHELIX.md` remains
the repository's fork rationale and provenance inventory.

Regenerate command tables after command/binding changes; do not maintain them
by hand. The link checker validates local output files and fragment IDs without
network access. A successful build/link check does not validate external links,
live editor behavior, terminal support or native macOS execution.
