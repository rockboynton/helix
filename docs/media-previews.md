# PNG and PDF picker previews

In a direct Ghostty or Kitty session, open a file picker (`Space f` or `Space e`)
and select a `.png` or `.pdf`. The existing preview pane displays the image instead
of binary text. File extensions are case-insensitive. No Steel plugin is needed.

PDF previews require `pdftoppm` on `PATH` (Poppler's command-line utilities;
`poppler-utils` on Nix, `poppler` on Homebrew). PNG previews need no external tool.
Neohelix does not link Poppler. Each requested PDF page is rasterized in a child
process, without shell interpolation or temporary output files.

`Alt-j` / `Alt-k` (or Shift-Down / Shift-Up) select the next / previous PDF page.
Other preview scrolling keys also move one page at a time for PDFs. Selecting a
different picker entry resets to page one. A page beyond the document's end shows
an error; go back a page to recover. Encrypted/invalid PDFs and missing tools show
an explanation rather than raw process output.

## Behavior and limits

- Conversion starts after a 150 ms selection debounce and never blocks the UI.
  Changing files/pages or closing/hiding the preview cancels pending work and
  kills its renderer child. At most two media workers run at once.
- PDF conversion has a ten-second timeout, 16 MiB output and 16 KiB stderr limits,
  and rasterizes only the requested page, at up to 1600 pixels on its longest edge.
- Inputs must be regular files up to 64 MiB. PNG data is limited to 16 MiB and
  32 megapixels. PDF page requests are limited to the first 10000 pages.
- Each picker keeps at most eight cached images, within a 32 MiB encoded-data
  budget, plus its active preview. Cache identity includes path, size, modification
  time and page. Revisiting an entry checks that identity; an active preview is a
  snapshot, not continuously reread on each frame.
- The first transport supports Kitty Unicode placeholders in direct Ghostty and
  Kitty sessions. Tmux and Zellij currently show a fallback message; their
  passthrough/graphics support is not assumed. Other protocols are not implemented.
- Images fit the available rectangle while retaining their aspect ratio. Rendering
  uses at most 128 columns by 128 rows. Placeholder cells participate in the normal
  compositor, so overlapping UI erases them normally. Terminal image data is freed
  when the preview disappears; only Neohelix's own image IDs are deleted.

Opening a PNG/PDF as an ordinary document is unchanged for now. This is a
**preview-first** implementation, not a PDF text editor. The asynchronous provider
and native image transport are reusable by a future dedicated read-only pane.
