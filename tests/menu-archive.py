"""After build-menu.sh, check the exact archive embedded in the signed CLI."""

import gzip
import io
from pathlib import Path
import stat
import subprocess
import sys
import tarfile
import tempfile


repo = Path(__file__).resolve().parents[1]
host_paths = (
    b"/Users/", b"/var/folders/", b"/Cellar/", b"/lib/rustlib/src/rust/",
    b"/.cargo/registry/", b"/.rustup/toolchains/", bytes(repo) + b"/",
)


def issues(payload):
    problems = set()
    if payload[3] != 0:
        problems.add("gzip header metadata")
    data = gzip.decompress(payload)
    if any(path in data for path in host_paths):
        problems.add("build path")
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        names = set()
        offset = 0
        for entry in archive:
            if (entry.uid, entry.gid, entry.uname, entry.gname) != (0, 0, "", ""):
                problems.add("non-neutral owner")
            if (entry.pax_headers or entry.offset != offset or entry.offset_data != entry.offset + 512
                    or data[entry.offset + 257:entry.offset + 265] != b"ustar\x0000"):
                problems.add("extended archive metadata")
            parts = entry.name.split("/")
            if any(part.startswith("._") or part == ".DS_Store" for part in parts):
                problems.add("Mac metadata file")
            if (parts[0] != "Milo.app" or any(part in ("", ".", "..") for part in parts)
                    or entry.name in names or entry.type not in (tarfile.REGTYPE, tarfile.DIRTYPE)):
                problems.add("unexpected member")
            if parts[-1] in ("ProfilePaths.plist", ".milo-menu-profile.plist"):
                problems.add("installer profile")
            names.add(entry.name)
            offset = entry.offset_data + (entry.size + 511) // 512 * 512
        if not names or any(data[offset:]):
            problems.add("unexpected archive data")
    return problems


def fixture(directory):
    app = directory / "Milo.app"
    app.mkdir()
    helper = app / "helper"
    helper.write_bytes(b"fixture")
    helper.chmod(0o755)
    subprocess.run(["/usr/bin/xattr", "-w", "test.milo.fixture", "/Users/fixture/build", helper], check=True)
    (app / "._helper").write_bytes(b"synthetic sidecar")
    (app / ".DS_Store").touch()
    archive = directory / "menu.tar.gz"

    subprocess.run([
        "/usr/bin/env", "COPYFILE_DISABLE=1", "/usr/bin/tar", "-czf", archive,
        "--uid", "1234", "--gid", "1235", "--uname", "fixture-user", "--gname", "fixture-group",
        "--xattrs", "-C", directory, app.name,
    ], check=True)
    expected = {"non-neutral owner", "extended archive metadata", "Mac metadata file", "build path"}
    assert expected <= issues(archive.read_bytes()), "contamination fixture was not detected"
    subprocess.run(["sh", repo / "macos/archive-menu.sh", directory, archive], check=True)
    assert not issues(archive.read_bytes()), "fixture metadata survived packaging"
    with tarfile.open(archive) as clean:
        assert set(clean.getnames()) == {"Milo.app", "Milo.app/helper"}
        member = clean.getmember("Milo.app/helper")
        assert member.mode == 0o755 and clean.extractfile(member).read() == b"fixture"


def tree(app):
    return {
        path.relative_to(app): (stat.S_IMODE(path.stat().st_mode), path.read_bytes() if path.is_file() else None)
        for path in (app, *app.rglob("*"))
    }


def verify(build, directory):
    cli = build / "milo"
    binary = cli.read_bytes()
    payload = (build / "menu.tar.gz").read_bytes()
    assert binary.count(payload) == 1, "checked archive is not the embedded archive"
    assert not any(path in binary for path in host_paths), "build path in CLI"
    problems = issues(payload)
    assert not problems, ", ".join(sorted(problems))
    subprocess.run(["/usr/bin/tar", "-xzpf", "-", "-C", directory], input=payload, check=True)
    app = directory / "Milo.app"
    assert tree(app) == tree(build / "Milo.app"), "signed files, modes or paths changed in transit"
    verify_signature = ["/usr/bin/codesign", "--verify", "--strict"]
    subprocess.run([*verify_signature, cli], check=True)
    subprocess.run([*verify_signature, app / "Contents/Resources/Tools/milo"], check=True)
    subprocess.run([*verify_signature, "--deep", "-R", '=identifier "net.usemilo.usage.local"', app], check=True)


if __name__ == "__main__":
    if not __debug__:
        sys.exit("Menu archive checks require Python assertions.")
    with tempfile.TemporaryDirectory() as root:
        fixture(Path(root))
    with tempfile.TemporaryDirectory() as root:
        verify(Path(sys.argv[1]) if len(sys.argv) > 1 else repo / "target/local-menu", Path(root))
    print("Menu archive: synthetic fixture, embedded bytes, contents, modes and signatures passed.")
