#!/usr/bin/env python3
"""Drive the release binary through a real PTY and assert search TUI behavior."""

import codecs
import os
import pathlib
import pty
import re
import select
import signal
import tempfile
import time

REPOSITORY = pathlib.Path(__file__).resolve().parents[2]
BINARY = pathlib.Path(
    os.environ.get("MINFM_BINARY", REPOSITORY / "target" / "release" / "minfm")
)
BASE = pathlib.Path(tempfile.mkdtemp(prefix="minfm-tui-acceptance-"))
ESC = b"\x1b"
ENTER = b"\r"
UP = b"\x1b[A"
DOWN = b"\x1b[B"
RIGHT = b"\x1b[C"
LEFT = b"\x1b[D"
TAB = b"\t"
SPACE = b" "

CSI_RE = re.compile(r"\x1b\[([0-9;?]*)([ -/]?)([@-~])")
OSC_RE = re.compile(r"\x1b\].*?(?:\x07|\x1b\\)", re.S)


class Screen:
    def __init__(self, width=120, height=36):
        self.width = width
        self.height = height
        self.lines = [[" "] * width for _ in range(height)]
        self.backgrounds = [[None] * width for _ in range(height)]
        self.row = 0
        self.col = 0
        self.saved = (0, 0)
        self.background = None

    def feed(self, text):
        text = OSC_RE.sub("", text)
        i = 0
        while i < len(text):
            if text[i] == "\x1b":
                m = CSI_RE.match(text, i)
                if m:
                    self._csi(m.group(1), m.group(3))
                    i = m.end()
                    continue
                if text.startswith("\x1b7", i):
                    self.saved = (self.row, self.col); i += 2; continue
                if text.startswith("\x1b8", i):
                    self.row, self.col = self.saved; i += 2; continue
                i += 1
                continue
            ch = text[i]
            if ch == "\r":
                self.col = 0
            elif ch == "\n":
                self.row = min(self.height - 1, self.row + 1)
            elif ch == "\b":
                self.col = max(0, self.col - 1)
            elif ch >= " ":
                if 0 <= self.row < self.height and 0 <= self.col < self.width:
                    self.lines[self.row][self.col] = ch
                    self.backgrounds[self.row][self.col] = self.background
                self.col = min(self.width - 1, self.col + 1)
            i += 1

    def _n(self, params, default=1):
        try:
            return int(params.split(";")[0] or default)
        except ValueError:
            return default

    def _csi(self, params, final):
        clean = params.lstrip("?")
        nums = [int(x or 0) for x in clean.split(";")] if clean else []
        if final in "Hf":
            r = (nums[0] if nums else 1) or 1
            c = (nums[1] if len(nums) > 1 else 1) or 1
            self.row = min(self.height - 1, r - 1)
            self.col = min(self.width - 1, c - 1)
        elif final == "A": self.row = max(0, self.row - self._n(clean))
        elif final == "B": self.row = min(self.height - 1, self.row + self._n(clean))
        elif final == "C": self.col = min(self.width - 1, self.col + self._n(clean))
        elif final == "D": self.col = max(0, self.col - self._n(clean))
        elif final == "G": self.col = min(self.width - 1, self._n(clean) - 1)
        elif final == "d": self.row = min(self.height - 1, self._n(clean) - 1)
        elif final == "J":
            if not nums or nums[0] in (2, 3):
                self.lines = [[" "] * self.width for _ in range(self.height)]
                self.backgrounds = [[self.background] * self.width for _ in range(self.height)]
                self.row = self.col = 0
        elif final == "K":
            mode = nums[0] if nums else 0
            if mode == 0:
                for c in range(self.col, self.width):
                    self.lines[self.row][c] = " "
                    self.backgrounds[self.row][c] = self.background
            elif mode == 1:
                for c in range(0, self.col + 1):
                    self.lines[self.row][c] = " "
                    self.backgrounds[self.row][c] = self.background
            elif mode == 2:
                self.lines[self.row] = [" "] * self.width
                self.backgrounds[self.row] = [self.background] * self.width
        elif final == "m":
            values = nums or [0]
            index = 0
            while index < len(values):
                value = values[index]
                if value == 0 or value == 49:
                    self.background = None
                elif 40 <= value <= 47:
                    self.background = ("ansi", value - 40)
                elif 100 <= value <= 107:
                    self.background = ("ansi", value - 100 + 8)
                elif value == 48 and index + 2 < len(values) and values[index + 1] == 5:
                    self.background = ("indexed", values[index + 2])
                    index += 2
                elif value == 48 and index + 4 < len(values) and values[index + 1] == 2:
                    self.background = tuple(values[index + 2:index + 5])
                    index += 4
                index += 1
        elif final == "s": self.saved = (self.row, self.col)
        elif final == "u": self.row, self.col = self.saved
        # styles, modes, cursor visibility, and alternate-screen switches do not alter cells here

    def text(self):
        return "\n".join("".join(line).rstrip() for line in self.lines)

    def cell(self, x, y):
        return self.lines[y][x], self.backgrounds[y][x]


class Session:
    def __init__(self, root, width=200, height=36, env=None, name="session"):
        self.root = pathlib.Path(root)
        self.name = name
        self.width = width
        self.height = height
        self.screen = Screen(width, height)
        self.raw = bytearray()
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        pid, fd = pty.fork()
        if pid == 0:
            os.environ.update({"TERM":"xterm-256color", "COLUMNS":str(width), "LINES":str(height)})
            if env: os.environ.update(env)
            os.chdir(self.root)
            os.execve(str(BINARY), [str(BINARY), str(self.root)], os.environ)
        self.pid, self.fd = pid, fd
        os.set_blocking(fd, False)
        import fcntl, struct, termios
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0))
        self.pump(0.7)

    def pump(self, duration=0.2):
        end = time.monotonic() + duration
        while time.monotonic() < end:
            ready, _, _ = select.select([self.fd], [], [], min(0.03, max(0, end-time.monotonic())))
            if not ready: continue
            try: data = os.read(self.fd, 65536)
            except (BlockingIOError, OSError): break
            if not data: break
            self.raw.extend(data)
            # Answer cursor position reports requested by crossterm.
            if b"\x1b[6n" in data or b"\x1b[?6n" in data:
                try: os.write(self.fd, b"\x1b[1;1R")
                except OSError: pass
            self.screen.feed(self.decoder.decode(data))

    def send(self, data, wait=0.18):
        if isinstance(data, str): data = data.encode()
        os.write(self.fd, data)
        self.pump(wait)

    def wait_for(self, needle, timeout=5.0):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            self.pump(0.08)
            if needle in self.screen.text(): return self.screen.text()
        self.dump()
        raise AssertionError(f"{self.name}: did not render {needle!r}")

    def assert_has(self, needle):
        text = self.screen.text()
        if needle not in text:
            self.dump(); raise AssertionError(f"{self.name}: missing {needle!r}")

    def dump(self):
        path = BASE / f"{self.name}.screen.txt"
        path.write_text(self.screen.text())
        (BASE / f"{self.name}.raw.bin").write_bytes(self.raw)

    def close(self):
        try: self.send(b"q", 0.1)
        except OSError: pass
        try:
            deadline=time.monotonic()+1
            while time.monotonic()<deadline:
                got, _ = os.waitpid(self.pid, os.WNOHANG)
                if got: break
                time.sleep(.03)
            else:
                os.kill(self.pid, signal.SIGTERM); os.waitpid(self.pid, 0)
        except (ProcessLookupError, ChildProcessError): pass
        try: os.close(self.fd)
        except OSError: pass


def fixture(name):
    root = BASE / name
    root.mkdir()
    (root/"report-final.txt").write_text("revenue needle quarterly\n")
    (root/"rpeort-draft.md").write_text("draft needle\n")
    (root/"photo.bin").write_bytes(b"\x00\x01\x02")
    (root/"nested").mkdir()
    (root/"nested"/"deep-note.txt").write_text("deep unique-content\n")
    (root/".hidden-note.txt").write_text("hidden unique-hidden\n")
    (root/"ignored-note.txt").write_text("ignored unique-ignored\n")
    (root/".gitignore").write_text("ignored-note.txt\n")
    (root/"dest").mkdir()
    return root


RESULTS = "Search results"

def centered_rect(screen_width, screen_height, width, height):
    return ((screen_width - width) // 2, (screen_height - height) // 2, width, height)

def surrounding_cells(screen, rect):
    x, y, width, height = rect
    coordinates = []
    if y > 0:
        coordinates.extend((column, y - 1) for column in range(x, x + width))
    if y + height < screen.height:
        coordinates.extend((column, y + height) for column in range(x, x + width))
    if x > 0:
        coordinates.extend((x - 1, row) for row in range(y, y + height))
    if x + width < screen.width:
        coordinates.extend((x + width, row) for row in range(y, y + height))
    return {coordinate: screen.cell(*coordinate) for coordinate in coordinates}

def assert_dialog_preserves_surroundings(session, before, dialog):
    after = surrounding_cells(session.screen, dialog)
    black = {("ansi", 0), ("indexed", 0), (0, 0, 0)}
    painted_black = {
        cell: (before[cell], after[cell])
        for cell in before
        if before[cell][1] != after[cell][1] and after[cell][1] in black
    }
    if painted_black:
        session.dump()
        raise AssertionError(
            f"{session.name}: search dialog painted a black halo outside its rectangle: {painted_black}"
        )

def quick_and_fuzzy():
    root=fixture("quick")
    s=Session(root,name="quick")
    try:
        s.send("/"); s.wait_for("Search current directory")
        s.assert_has("Enter a value:")
        s.assert_has("│> │")
        s.assert_has("F advanced")
        s.send("report"); s.send(ENTER, .4); s.wait_for(RESULTS)
        s.assert_has("report-final.txt")
        s.send(ESC); s.send("/"); s.send("rpeort"); s.send(ENTER,.4); s.wait_for(RESULTS)
        s.assert_has("rpeort-draft.md")
    finally: s.close()

def advanced_recursive_content_and_hidden():
    root=fixture("advanced")
    s=Session(root,name="advanced")
    try:
        s.send("/"); s.send("F"); s.wait_for("Advanced search")
        s.assert_has("Scope"); s.assert_has("Match"); s.assert_has("Filters"); s.assert_has("Traversal")
        s.send(RIGHT)  # recursive here
        s.send(DOWN)   # Match section
        s.send(TAB)    # Content field
        s.send("unique-content")
        s.send(ENTER,.8); s.wait_for(RESULTS); s.assert_has("deep-note.txt")
        # New advanced search from results, include hidden/ignored, content hidden marker.
        s.send("/"); s.send("F"); s.send(RIGHT)  # recursive
        s.send(DOWN); s.send(TAB); s.send("unique-hidden")
        s.send(DOWN)  # Filters
        for _ in range(9): s.send(TAB, .03)
        s.send(RIGHT)  # include ignored/hidden yes
        s.send(ENTER,.8); s.wait_for(RESULTS); s.assert_has(".hidden-note.txt")
    finally: s.close()

def selector_help_size_units_and_dialog_appearance():
    root=fixture("selector-help")
    (root/"large.bin").write_bytes(b"x" * (24 * 1024))
    s=Session(root,name="selector-help")
    try:
        quick_rect = centered_rect(s.width, s.height, 72, 9)
        quick_surroundings = surrounding_cells(s.screen, quick_rect)
        s.send("/"); s.wait_for("Search")
        assert_dialog_preserves_surroundings(s, quick_surroundings, quick_rect)

        advanced_rect = centered_rect(s.width, s.height, 110, 32)
        advanced_surroundings = surrounding_cells(s.screen, advanced_rect)
        s.send("F"); s.wait_for("Advanced search")
        assert_dialog_preserves_surroundings(s, advanced_surroundings, advanced_rect)

        lines = s.screen.text().splitlines()
        scope_rows = {
            label: next(index for index, line in enumerate(lines) if label in line)
            for label in ("Current directory", "Recursive here", "Entire filesystem")
        }
        if len(set(scope_rows.values())) != 3:
            raise AssertionError(f"scope choices share screen rows: {scope_rows}")

        s.send(RIGHT, .4)
        s.wait_for("[x] Recursive here")
        s.assert_has("current directory and all subfolders")

        scope_help = next(line for line in s.screen.text().splitlines() if "all subfolders" in line)
        s.send(DOWN)
        s.assert_has("Smart matching ranks exact matches first")
        if scope_help in s.screen.text():
            raise AssertionError("Match navigation retained the Scope help")

        s.send(DOWN)
        s.assert_has("Files includes regular files")
        s.send(SPACE)
        for _ in range(5): s.send(TAB, .03)
        size_help = "".join(
            re.sub(r"[^A-Za-z0-9., ]", "", line).strip()
            for line in s.screen.text().splitlines()
            if "minimum size is" in line or "Directories are excluded" in line
        )
        for text, pattern in (
            ("inclusive", r"inclusive"),
            ("KB", r"KB"),
            ("GB", r"GB"),
            ("GiB", r"GiB"),
        ):
            if not re.search(pattern, size_help):
                s.dump()
                raise AssertionError(f"minimum-size help omitted {text!r}")
        s.send("20 KB")
        s.send(ENTER,.8); s.wait_for(RESULTS)
        s.assert_has("large.bin")
        if "photo.bin" in s.screen.text():
            raise AssertionError("20 KB minimum admitted the 3-byte fixture")
    finally: s.close()

def type_size_filter():
    root=fixture("filters")
    s=Session(root,name="filters")
    try:
        s.send("/"); s.send("F")
        s.send(DOWN); s.send(DOWN)  # Filters
        s.send(SPACE)  # select Files
        for _ in range(5): s.send(TAB,.03)
        s.send("10")  # min size
        s.send(ENTER,.6); s.wait_for(RESULTS)
        s.assert_has("File")
        if "nested" in s.screen.text(): raise AssertionError("directory passed file+size filter")
    finally: s.close()

def glob_regex_validation_and_result_context():
    root=fixture("modes")
    s=Session(root,name="modes")
    try:
        # Glob mode is selected in the real advanced form and filters by extension.
        s.send("/"); s.send("F"); s.send("*.txt")
        s.send(DOWN); s.send(RIGHT); s.send(ENTER,.6); s.wait_for(RESULTS)
        s.assert_has("report-final.txt")
        if "rpeort-draft.md" in s.screen.text():
            raise AssertionError("glob name mode admitted a non-matching extension")

        # Invalid regex is rejected inline without replacing the prior result set.
        s.send("/"); s.send("F"); s.send("[")
        s.send(DOWN); s.send(RIGHT); s.send(RIGHT); s.send(ENTER,.3)
        s.wait_for("invalid")
        s.send(ESC,.2); s.wait_for(RESULTS)
        s.assert_has("report-final.txt")

        # Result context supports info but rejects operations that need a directory.
        s.send("I"); s.wait_for("Application information"); s.assert_has("report-final.txt")
        s.send(ESC); s.send("p"); s.wait_for("Unavailable in search results")
    finally: s.close()

def cancellation():
    root=fixture("cancel")
    # add enough traversal work to make filesystem/root search observable
    for i in range(1500): (root/f"noise-{i:04}.txt").write_text("noise")
    s=Session(root,name="cancel")
    try:
        s.send("F"); s.wait_for("Advanced search")
        s.send("noise")
        s.send(ENTER,.08)
        # Either progress is visible or fast completion; exercise Esc either way.
        if "Searching" in s.screen.text():
            s.send(ESC,.5)
            s.wait_for(str(root),3)
        else:
            s.wait_for(RESULTS,4); s.send(ESC)
        s.assert_has(str(root))
    finally: s.close()

def copy_paste():
    root=fixture("copy")
    s=Session(root,name="copy")
    try:
        s.send("/"); s.send("report-final"); s.send(ENTER,.5); s.wait_for(RESULTS)
        s.send("c"); s.send(ESC)
        # go to dest via go-to prompt
        s.send("g"); s.send(str(root/"dest")); s.send(ENTER,.4)
        s.send("p",.8)
        deadline=time.monotonic()+4
        while time.monotonic()<deadline and not (root/"dest"/"report-final.txt").exists():
            s.pump(.1)
        assert (root/"dest"/"report-final.txt").read_text()=="revenue needle quarterly\n"
    finally: s.close()

def rename_archive_trash_and_stale():
    # rename
    root=fixture("mutations")
    xdg = root / ".xdg-data"
    xdg.mkdir()
    s=Session(root,name="mutations",env={"XDG_DATA_HOME":str(xdg)})
    try:
        s.send("/"); s.send("rpeort-draft"); s.send(ENTER,.5); s.wait_for(RESULTS)
        s.send("r"); s.wait_for("Rename")
        # Ctrl+u is not supported; use Home, Shift? delete existing with backspaces from end.
        for _ in range(len("rpeort-draft.md")): s.send(b"\x7f",.01)
        s.send("renamed.md"); s.send(ENTER,.5)
        assert (root/"renamed.md").exists() and not (root/"rpeort-draft.md").exists()
        # archive renamed result
        s.send("z"); s.wait_for("Create archive")
        s.send(ENTER); s.wait_for("Archive filename")
        s.send(ENTER,1.0)
        archives=list(root.glob("renamed.md*.tar*"))+list(root.glob("renamed*.tar*"))
        assert archives, f"archive not created in {root}"
        # search target again and trash with confirmation
        if RESULTS not in s.screen.text():
            s.send("/"); s.send("renamed.md"); s.send(ENTER,.5); s.wait_for(RESULTS)
        s.send("d"); s.wait_for("Confirm move to trash"); s.send(ENTER,.2)
        deadline=time.monotonic()+5
        while time.monotonic()<deadline and (root/"renamed.md").exists():
            s.pump(.1)
        if (root/"renamed.md").exists():
            s.dump()
            raise AssertionError("confirmed trash operation did not remove source")
    finally: s.close()

    # stale deletion in separate clean session
    stale=fixture("stale")
    s=Session(stale,name="stale")
    try:
        s.send("/"); s.send("report-final"); s.send(ENTER,.5); s.wait_for(RESULTS)
        (stale/"report-final.txt").unlink()
        s.send(ENTER,.4)
        text=s.screen.text()
        assert "Search results" in text and "report-final.txt" not in text
    finally: s.close()

def missing_rg_and_narrow():
    root=fixture("missing-rg")
    toolbin=BASE/"no-rg-bin"; toolbin.mkdir()
    # release binary needs no build tools; empty PATH guarantees rg unavailable
    s=Session(root,width=52,height=14,env={"PATH":str(toolbin)},name="missing-rg")
    try:
        s.send("/"); s.send("F"); s.send(RIGHT); s.send(DOWN); s.send(TAB)
        s.send("needle"); s.send(ENTER,.3)
        s.wait_for("content search requires ripgrep")
        # long query must retain caret marker in narrow UI
        s.send("abcdefghijklmnopqrstuvwxyz0123456789")
        s.assert_has("│")
    finally: s.close()


tests=[quick_and_fuzzy, advanced_recursive_content_and_hidden,
       selector_help_size_units_and_dialog_appearance, type_size_filter,
       glob_regex_validation_and_result_context,
       cancellation, copy_paste, rename_archive_trash_and_stale, missing_rg_and_narrow]
passed=[]
try:
    for test in tests:
        test(); passed.append(test.__name__); print(f"PASS {test.__name__}", flush=True)
    print(f"ALL_PASS {len(passed)}")
except Exception:
    print(f"FAILED after={passed} root={BASE}", flush=True)
    raise
