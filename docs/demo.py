"""Record docs/demo.svg: run kondor on a demo tree, press keys and save the screens as an
animated SVG.

Usage: python docs/demo.py [path/to/kondor]  (needs `pyte`, run from the repository root)
"""

import fcntl
import os
import pty
import select
import struct
import sys
import tempfile
import termios
import time
from html import escape
from pathlib import Path

import pyte

COLUMNS = 80
LINES = 10
CELL_WIDTH = 8.4
CELL_HEIGHT = 18
PADDING = 16
HEADER = 46
HEADER_COLOR = "#303030"
TITLE = "~/code"
FOREGROUND = "#d0d0d0"
BACKGROUND = "#1a1b1e"
NAMED_COLORS = {
    "black": "#000000",
    "red": "#cd3131",
    "green": "#0dbc79",
    "brown": "#e5e510",
    "blue": "#2472c8",
    "magenta": "#bc3fbc",
    "cyan": "#11a8cd",
    "white": "#e5e5e5",
}

# Sizes are apparent sizes of sparse files, so the demo tree takes no real disk space.
TREE = {
    "api/Cargo.toml": 0,
    "api/target/debug/api": 1_950_000_000,
    "api/frontend/package.json": 0,
    "api/frontend/node_modules/react/index.js": 412_000_000,
    "api/frontend/.next/cache/data": 38_000_000,
    "legacy/go.mod": 0,
    "legacy/vendor/modules.txt": 28_000_000,
    "scripts/pyproject.toml": 0,
    "scripts/.venv/lib/python": 96_000_000,
    "scripts/__pycache__/main.pyc": 1_200_000,
}

# Seconds to wait before each key, then the key.
KEYS = [
    (2.0, "\x1b[D"),
    (0.8, "\r"),
    (1.6, "y"),
    (1.6, "n"),
    (1.6, "y"),
]
LAST_FRAME = 4.0


def make_tree(root: Path) -> None:
    for name, size in TREE.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        with open(path, "wb") as file:
            file.truncate(size)


def read_until_quiet(fd: int, stream: pyte.ByteStream, quiet: float = 0.4) -> None:
    while select.select([fd], [], [], quiet)[0]:
        try:
            data = os.read(fd, 65536)
        except OSError:
            return
        if not data:
            return
        stream.feed(data)


def color(name: str, default: str) -> str:
    if name == "default":
        return default
    return NAMED_COLORS.get(name, f"#{name}")


def render_frame(screen: pyte.Screen) -> str:
    parts = []
    for y in range(LINES):
        row = screen.buffer[y]
        top = HEADER + PADDING + y * CELL_HEIGHT
        x = 0
        while x < COLUMNS:
            cell = row[x]
            start = x
            text = ""
            while x < COLUMNS and (row[x].fg, row[x].bg, row[x].bold, row[x].reverse) == (
                cell.fg,
                cell.bg,
                cell.bold,
                cell.reverse,
            ):
                text += row[x].data
                x += 1
            fg = color(cell.fg, FOREGROUND)
            bg = color(cell.bg, BACKGROUND)
            if cell.reverse:
                fg, bg = bg, fg
            left = PADDING + start * CELL_WIDTH
            width = (x - start) * CELL_WIDTH
            if bg != BACKGROUND:
                parts.append(
                    f'<rect x="{left:.1f}" y="{top}" width="{width:.1f}" '
                    f'height="{CELL_HEIGHT}" fill="{bg}"/>'
                )
            if text.strip():
                weight = ' font-weight="bold"' if cell.bold else ""
                parts.append(
                    f'<text x="{left:.1f}" y="{top + 13}" fill="{fg}"{weight} '
                    f'textLength="{width:.1f}">{escape(text)}</text>'
                )
    return "".join(parts)


def record(kondor: str) -> list[tuple[float, str]]:
    screen = pyte.Screen(COLUMNS, LINES)
    stream = pyte.ByteStream(screen)
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp) / "code"
        make_tree(root)
        pid, fd = pty.fork()
        if pid == 0:
            os.chdir(root)
            prompt = "printf '\\033[38;5;244m~/code\\033[0m $ kondor\\n'"
            os.execvp("sh", ["sh", "-c", f'{prompt}; exec "$0"', kondor])
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", LINES, COLUMNS, 0, 0))
        frames = []
        read_until_quiet(fd, stream, 1.0)
        for delay, key in KEYS:
            frames.append((delay, render_frame(screen)))
            time.sleep(0.1)
            os.write(fd, key.encode())
            read_until_quiet(fd, stream)
        frames.append((LAST_FRAME, render_frame(screen)))
        os.waitpid(pid, 0)
    return frames


def header(width: float) -> str:
    """GNOME Terminal style header bar: centered title, round window buttons on the right."""
    middle = HEADER / 2
    icons = [
        "M-4 0H4",
        "M-4 -4H4V4H-4Z",
        "M-4 -4L4 4M4 -4L-4 4",
    ]
    buttons = "".join(
        f'<g transform="translate({width - 26 - i * 36:.1f} {middle})">'
        f'<circle r="12" fill="#ffffff" fill-opacity="0.1"/>'
        f'<path d="{icon}" stroke="#e0e0e0" stroke-width="1.5" fill="none"/></g>'
        for i, icon in enumerate(reversed(icons))
    )
    return (
        f'<text class="title" x="{width / 2:.1f}" y="{middle + 5}" text-anchor="middle" '
        f'fill="#e0e0e0">{escape(TITLE)}</text>{buttons}'
    )


def write_svg(frames: list[tuple[float, str]], path: Path) -> None:
    total = sum(duration for duration, _ in frames)
    width = PADDING * 2 + COLUMNS * CELL_WIDTH
    height = HEADER + PADDING * 2 + LINES * CELL_HEIGHT
    styles = []
    groups = []
    start = 0.0
    for i, (duration, body) in enumerate(frames):
        begin = start / total * 100
        end = (start + duration) / total * 100
        # step-end keeps each keyframe value until the next one, so frames switch instantly.
        keyframes = f"{begin:.3f}%{{opacity:1}}{end:.3f}%{{opacity:0}}"
        if begin > 0:
            keyframes = "0%{opacity:0}" + keyframes
        animation = f"animation:f{i} {total}s step-end infinite"
        styles.append(f"@keyframes f{i}{{{keyframes}}}.f{i}{{{animation}}}")
        groups.append(f'<g class="f f{i}">{body}</g>')
        start += duration
    svg = (
        f'<svg xmlns="http://www.w3.org/2000/svg" xml:space="preserve" width="{width:.0f}" '
        f'height="{height}" '
        f'viewBox="0 0 {width:.0f} {height}">'
        "<style>.f{opacity:0}"
        "text{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,'DejaVu Sans Mono',"
        "monospace;font-size:14px;white-space:pre}"
        ".title{font-family:Cantarell,'Noto Sans',Ubuntu,sans-serif;font-weight:bold}"
        f"{''.join(styles)}</style>"
        # The window is a rounded header-colored rectangle with the terminal area drawn over its
        # lower part; the square strip hides the terminal area's rounded top corners.
        f'<rect width="100%" height="100%" rx="12" fill="{HEADER_COLOR}"/>'
        f'<rect y="{HEADER}" width="100%" height="{height - HEADER}" rx="12" fill="{BACKGROUND}"/>'
        f'<rect y="{HEADER}" width="100%" height="12" fill="{BACKGROUND}"/>'
        f"{header(width)}"
        f'<rect y="{HEADER}" width="100%" height="1" fill="#000000" fill-opacity="0.4"/>'
        f'<rect x="0.5" y="0.5" width="{width - 1:.1f}" height="{height - 1}" rx="12" '
        'fill="none" stroke="#000000" stroke-opacity="0.5"/>'
        f"{''.join(groups)}</svg>\n"
    )
    path.write_text(svg)


def main() -> None:
    kondor = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "target/debug/kondor")
    write_svg(record(kondor), Path("docs/demo.svg"))


if __name__ == "__main__":
    main()
