# Presenting the standard-library talk

`stdlib-lessons.ipynb` is the talk; `lab.py` is the helper its demos use.

## Before the talk

1. `just notebook` opens it in Jupyter (installed with RISE on first use into `target/notebook-venv`).
2. Run → Run All Cells, so every demo has its output.
3. Start the slideshow with the RISE button in the toolbar, or `Alt+R`.

## On stage

- `Space` steps through everything in order; the arrow keys move between slides.
- Each finding's demo sits on a slide *below* its headline slide (down arrow).
- `Shift+Enter` on a demo re-runs it live.
- The speaker notes hold the longer explanation for each finding.

## Fallback

`just notebook-slides` builds a static copy with every output, `target/notebook/stdlib-lessons.slides.html`, for when a live kernel is not an option. It loads reveal.js from a CDN, so it needs a network connection.

## Checking it

`just notebook-test` runs every cell headless and fails on a cell that raises. Commit the notebook without outputs.
