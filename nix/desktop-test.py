# NixOS test driver globals and constants are supplied by desktop-test.nix.
import csv
import io
import json
import shlex
import subprocess
from typing import Any, Iterator

APP_ID = "com.oldjobobo.splinterm"
ARTIFACTS = "/home/operator/acceptance"
WAYLAND = "wayland-1"
owned: tuple[int, int] | None = None
cursor = {"x": 0, "y": 0, "source": "guest compositor initialization"}


def as_user(command: str) -> str:
    return (
        "setpriv --reuid=1000 --regid=1000 --init-groups env -i "
        "HOME=/home/operator USER=operator LOGNAME=operator "
        "PATH=/run/current-system/sw/bin XDG_RUNTIME_DIR=/run/user/1000 "
        "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus "
        "XDG_CURRENT_DESKTOP=sway SWAYSOCK=/run/user/1000/sway-test.sock "
        f"WAYLAND_DISPLAY={WAYLAND} SHELL={SHELL} "
        f"{SHELL} -c {shlex.quote(command)}"
    )


def user(command: str) -> str:
    return desktop.succeed(as_user(command), timeout=30).strip()


def sway(command: str, kind: str = "command") -> Any:
    result = json.loads(user(f"swaymsg -r -t {kind} -- {shlex.quote(command)}"))
    if kind == "command":
        assert all(item["success"] for item in result), result
    return result


def walk(node: dict[str, Any]) -> Iterator[dict[str, Any]]:
    yield node
    for key in ("nodes", "floating_nodes"):
        for child in node.get(key, []):
            yield from walk(child)


def windows() -> list[dict[str, Any]]:
    return [node for node in walk(sway("", "get_tree")) if node.get("pid")]


def target(require_focus: bool = True) -> dict[str, Any]:
    current = windows()
    assert len(current) == 1, ("unexpected guest window; abort input", current)
    node = current[0]
    assert owned == (node["id"], node["pid"]), (owned, node)
    assert node["app_id"] == APP_ID, node
    if require_focus:
        assert node["focused"], ("target lost focus; abort input", node)
    workspace = next(w for w in sway("", "get_workspaces") if w["focused"])
    assert workspace["name"] == "8", workspace
    return node


def record(label: str) -> None:
    data = {
        "windows": windows(),
        "outputs": sway("", "get_outputs"),
        "workspaces": sway("", "get_workspaces"),
        "seats": sway("", "get_seats"),
        "cursor": cursor,
    }
    user(
        f"printf %s {shlex.quote(json.dumps(data, indent=2))} > {ARTIFACTS}/{label}.json"
    )


def capture(label: str) -> None:
    target()
    record(label)
    user(f"grim {ARTIFACTS}/{label}.png")


def keys(arguments: str) -> None:
    target()
    user(f"wtype {arguments}")
    target()


def shell(command: str, marker: str) -> None:
    target()
    user(f"wtype -- {shlex.quote(command + '; touch ' + ARTIFACTS + '/' + marker)}")
    keys("-k Return")
    desktop.wait_for_file(f"{ARTIFACTS}/{marker}", timeout=30)


def pointer(x: int, y: int, operation: str | None = None) -> None:
    node = target()
    rect = node["rect"]
    assert rect["x"] <= x < rect["x"] + rect["width"]
    assert rect["y"] <= y < rect["y"] + rect["height"]
    sway(f"seat seat0 cursor set {x} {y}")
    cursor.update(x=x, y=y, source="exact-target guest pointer command")
    if operation:
        sway(f"seat seat0 cursor {operation} button1")
    target()


def text_bounds(text: str, label: str) -> tuple[int, int, int, int]:
    # Locate literal fixture text on the unmodified guest framebuffer. No guessed
    # font metrics, translated captures, or workstation pointer coordinates.
    target()
    assert all(o["scale"] == 1 for o in sway("", "get_outputs") if o["active"])
    desktop.screenshot(label)
    tsv = subprocess.check_output(
        [
            "tesseract",
            str(desktop.out_dir / f"{label}.png"),
            "stdout",
            "--psm",
            "6",
            "tsv",
        ],
        text=True,
        timeout=30,
    )
    matches = [
        row
        for row in csv.DictReader(io.StringIO(tsv), delimiter="\t")
        if text in row["text"]
    ]
    assert len(matches) == 1, (text, matches, tsv)
    row = matches[0]
    return int(row["left"]), int(row["top"]), int(row["width"]), int(row["height"])


def daemon_pid() -> str:
    return user("systemctl --user show splinterd.service -p MainPID --value")


def wait_for_shells(count: int) -> None:
    desktop.wait_until_succeeds(
        f'test "$(pgrep -P {daemon_pid()} | wc -l)" -eq {count}', timeout=30
    )


def launch(action: str | None = None) -> None:
    global owned
    assert not windows(), "test workspace must be empty before launch"
    record("before-launch" + (action or "default"))
    if action is None:
        command = f"gio launch {PACKAGE}/share/applications/{APP_ID}.desktop"
    else:
        # Read the installed desktop action, rather than testing an invented command.
        import configparser

        entry = configparser.ConfigParser(interpolation=None)
        entry.read_string(
            desktop.succeed(f"cat {PACKAGE}/share/applications/{APP_ID}.desktop")
        )
        command = shlex.join(shlex.split(entry[f"Desktop Action {action}"]["Exec"]))
    user(f"{command} > {ARTIFACTS}/launcher-{action or 'default'}.log 2>&1 &")
    desktop.wait_until_succeeds(
        as_user("swaymsg -t get_tree | grep -F 'com.oldjobobo.splinterm'"), timeout=60
    )
    current = windows()
    assert len(current) == 1 and current[0]["app_id"] == APP_ID, current
    owned = (current[0]["id"], current[0]["pid"])
    record("identified-" + (action or "default"))
    target(require_focus=False)
    sway(f"[con_id={owned[0]}] focus")
    target()
    assert (
        desktop.succeed(f"readlink /proc/{owned[1]}/exe").strip()
        == f"{PACKAGE}/bin/splinterm"
    )


def detach() -> None:
    global owned
    node = target()
    sway(f"[con_id={node['id']}] kill")
    desktop.wait_until_fails(f"test -e /proc/{node['pid']}", timeout=30)
    assert not windows()
    owned = None


def cleanup(label: str) -> None:
    if owned is not None:
        detach()
    assert not windows(), "refuse to clean up an unidentified guest window"
    user("systemctl --user stop splinterd.service")
    desktop.wait_until_fails(
        "test -S /run/user/1000/splinterm/splinterd.sock", timeout=30
    )
    desktop.wait_until_fails("pgrep -u operator -x splinterm-pty-c", timeout=30)
    sway("output * scale 1")
    sway("workspace 8")
    sway("seat seat0 cursor set 0 0")
    cursor.update(x=0, y=0, source="guest-only cleanup")
    record("clean-" + label)


desktop.start()
desktop.wait_for_unit("multi-user.target", timeout=60)
desktop.wait_for_file(f"{ARTIFACTS}/compositor-ready", timeout=60)
desktop.wait_for_file("/run/user/1000/sway-test.sock", timeout=30)
WAYLAND = desktop.succeed(
    "find /run/user/1000 -maxdepth 1 -type s -name 'wayland-*' -printf '%f'"
).strip()
assert WAYLAND and "/" not in WAYLAND
assert not windows()
record("initial")
desktop.fail(as_user("systemctl --user is-active splinterd.service"))

with subtest("guarded installed desktop-entry smoke starts daemon and renders shell"):
    launch()
    shell("printf '\\nSMOKE-PASS\\n'", "smoke-input")
    desktop.wait_for_text("SMOKE-PASS", timeout=3)
    capture("smoke")
    pid = user("systemctl --user show splinterd.service -p MainPID --value")
    assert (
        desktop.succeed(f"readlink /proc/{pid}/exe").strip()
        == f"{PACKAGE}/bin/splinterd"
    )
    cleanup("smoke")

# The full matrix is deliberately gated on the successful smoke above.
if not SMOKE_ONLY:
    with subtest("installed New action renders a fresh terminal"):
        launch("New")
        shell("printf '\\nDESKTOP-MATRIX\\n'", "new-input")
        desktop.wait_for_text("DESKTOP-MATRIX", timeout=3)
        capture("new-action")
        shell("echo $$ > /home/operator/acceptance/first-shell", "first-shell-ready")
        wait_for_shells(1)
        keys("-M ctrl -M shift -k d -m shift -m ctrl")
        wait_for_shells(2)
        shell("echo $$ > /home/operator/acceptance/second-shell", "second-shell-ready")
        assert user(f"cat {ARTIFACTS}/first-shell") != user(
            f"cat {ARTIFACTS}/second-shell"
        )
        keys("-M ctrl -k Tab -m ctrl")
        shell("echo $$ > /home/operator/acceptance/cycled-shell", "cycle-ready")
        assert user(f"cat {ARTIFACTS}/first-shell") == user(
            f"cat {ARTIFACTS}/cycled-shell"
        )
        keys("-M ctrl -M shift -k Return -m shift -m ctrl")
        wait_for_shells(3)
        shell("printf '\\nSPLIT-PASS\\n'", "split-ready")
        desktop.wait_for_text("SPLIT-PASS", timeout=3)
        capture("tabs-and-split")
        cleanup("tabs-and-split")

    with subtest(
        "resize, multilingual fonts, scaling, clipboard, and local URL handler"
    ):
        launch("New")
        shell("stty size > /home/operator/acceptance/original-grid", "grid-before")
        node = target()
        sway(f"[con_id={node['id']}] floating enable")
        sway(f"[con_id={node['id']}] resize set width 900 px height 650 px")
        sway(f"[con_id={node['id']}] move position 80 px 80 px")
        shell("stty size > /home/operator/acceptance/resized-grid", "grid-after")
        assert user(f"cat {ARTIFACTS}/original-grid") != user(
            f"cat {ARTIFACTS}/resized-grid"
        )
        capture("resized")
        sway(f"[con_id={node['id']}] floating disable")
        # Fixtures use shell escapes, not compositor keyboard-layout assumptions.
        specimen = "ASCII 0123456789 | bold italic\n中文 日本語 한글\nEmoji: 😀 🚀 🌈\n"
        user(f"printf %s {shlex.quote(specimen)} > {ARTIFACTS}/specimen.txt")
        shell(
            "printf '\\033[2J\\033[H'; cat /home/operator/acceptance/specimen.txt",
            "fonts-ready",
        )
        desktop.wait_for_text("ASCII 0123456789", timeout=3)
        capture("fonts-scale-1")
        target()
        sway("output * scale 1.5")
        assert all(o["scale"] == 1.5 for o in sway("", "get_outputs") if o["active"])
        shell(
            "printf '\\033[2J\\033[H'; cat /home/operator/acceptance/specimen.txt",
            "fonts-scaled-ready",
        )
        desktop.wait_for_text("ASCII 0123456789", timeout=3)
        capture("fonts-scale-1-5")
        target()
        sway("output * scale 1")
        shell(
            "printf '\\033[2J\\033[H'; cat /home/operator/acceptance/specimen.txt",
            "fonts-restored-ready",
        )
        desktop.wait_for_text("ASCII 0123456789", timeout=3)

        user("printf %s CLIPBOARD-PASTE | wl-copy")
        target()
        user("wtype -- \"printf '%s' '\"")
        keys("-M ctrl -M shift -k v -m shift -m ctrl")
        user('wtype -- "\' > /home/operator/acceptance/pasted"')
        keys("-k Return")
        desktop.wait_for_file(f"{ARTIFACTS}/pasted", timeout=30)
        assert user(f"cat {ARTIFACTS}/pasted") == "CLIPBOARD-PASTE"
        shell("printf '\\033[2J\\033[HCLIPBOARD-COPY\\n'", "copy-ready")
        desktop.wait_for_text("CLIPBOARD-COPY", timeout=3)
        left, top, width, height = text_bounds("CLIPBOARD-COPY", "copy-bounds")
        pointer(left + 1, top + height // 2, "press")
        # Sway cursor warp/rebase does not dispatch grabbed drag motion.
        # Use real virtual-pointer motion on the same isolated guest seat.
        target()
        user(f"wlrctl pointer move {width - 2} 0")
        pointer(left + width - 1, top + height // 2, "release")
        keys("-M ctrl -M shift -k c -m shift -m ctrl")
        copied = user("timeout 5 wl-paste --no-newline")
        assert copied == "CLIPBOARD-COPY", repr(copied)
        capture("clipboard-copy")
        user("xdg-mime default splinterm-vm-url.desktop x-scheme-handler/http")
        shell("printf '\\033[2J\\033[Hhttp://127.0.0.1/splinterm-vm\\n'", "url-ready")
        desktop.wait_for_text(r"127\.0\.0\.1", timeout=3)
        left, top, width, height = text_bounds("127.0.0.1", "url-bounds")
        pointer(left + width // 2, top + height // 2)
        target()
        # Modifier exists only in this guest virtual keyboard's bounded lifetime.
        user(
            "wtype -M ctrl -s 1000 -m ctrl & keyboard=$!; sleep 0.2; "
            "swaymsg 'seat seat0 cursor press button1'; "
            "swaymsg 'seat seat0 cursor release button1'; wait $keyboard"
        )
        target()
        desktop.wait_for_file(f"{ARTIFACTS}/url-opened", timeout=30)
        assert user(f"cat {ARTIFACTS}/url-opened") == "http://127.0.0.1/splinterm-vm"
        capture("local-url")
        user("wl-copy --clear")
        cleanup("rendering-clipboard-url")

    with subtest("Reopen desktop action reattaches the live shell after window detach"):
        launch("New")
        shell("echo $$ > /home/operator/acceptance/reopen-pid", "reopen-before")
        shell_pid = user(f"cat {ARTIFACTS}/reopen-pid")
        detach()
        desktop.succeed(f"kill -0 {shell_pid}")
        launch("Reopen")
        shell("echo $$ > /home/operator/acceptance/reopened-pid", "reopen-after")
        assert user(f"cat {ARTIFACTS}/reopened-pid") == shell_pid
        capture("reopened")
        cleanup("reopened")

    with subtest("Dojos desktop action opens the native picker"):
        launch("Dojos")
        desktop.wait_for_text("(?i)filter dojos|New terminal", timeout=3)
        capture("dojos-action")
        cleanup("dojos-action")

assert not windows()
desktop.copy_from_vm(ARTIFACTS)
