"""Package current Windows artifacts in deterministic distributable archives."""
from pathlib import Path
import hashlib
import shutil
import tomllib
import zipfile

root = Path(__file__).resolve().parent.parent
out = root / "dist/windows-x64"
version = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
release_name = f"MumbleACRE-{version}-windows-x64"
release_archive = out / f"{release_name}.zip"


def write_deterministic_zip(archive: Path, entries: list[tuple[str, bytes]]) -> None:
    """Write stable archives when their inputs are unchanged."""
    with zipfile.ZipFile(
        archive,
        "w",
        compression=zipfile.ZIP_DEFLATED,
        compresslevel=9,
        strict_timestamps=True,
    ) as bundle:
        for name, data in sorted(entries):
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o100644 << 16
            bundle.writestr(info, data, compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)


def relative_files(directory: Path) -> list[Path]:
    return sorted(path.relative_to(out) for path in directory.rglob("*") if path.is_file())


def require_file(path: Path) -> None:
    if not path.is_file():
        raise SystemExit(f"Missing required build artifact: {path}")


out.mkdir(parents=True, exist_ok=True)
plugin_dll = out / "mumbleacre_plugin.dll"
addon_pbo = out / "@mumbleacre/addons/mumbleacre_acre.pbo"
require_file(plugin_dll)
require_file(addon_pbo)
manifest = f'''<?xml version="1.0" encoding="UTF-8"?>
<bundle version="1.0.0">
  <assets><plugin os="windows" arch="x64">mumbleacre_plugin.dll</plugin></assets>
  <name>MumbleACRE</name><version>{version}</version>
</bundle>
'''
bundle_path = out / "MumbleACRE.mumble_plugin"
write_deterministic_zip(
    bundle_path,
    [
        ("manifest.xml", manifest.encode()),
        ("mumbleacre_plugin.dll", plugin_dll.read_bytes()),
    ],
)
for name in ("README.md", "LICENSE", "NOTICE.md"):
    shutil.copy2(root / name, out / name)
for name in ("start-mumble.ps1", "diagnose-mumbleacre.ps1"):
    shutil.copy2(root / "scripts" / name, out / name)
(out / "docs").mkdir(exist_ok=True)
for name in ("runtime.md", "qa.md", "acre-mission-integration.md", "distribution.md"):
    shutil.copy2(root / "docs" / name, out / "docs" / name)
shutil.copytree(root / "missions/mumbleacre_smoke.VR", out / "missions/mumbleacre_smoke.VR", dirs_exist_ok=True)
release_files = [
    Path("@mumbleacre/addons/mumbleacre_acre.pbo"),
    Path("@mumbleacre/mod.cpp"),
    Path("LICENSE"),
    Path("MumbleACRE.mumble_plugin"),
    Path("NOTICE.md"),
    Path("README.md"),
    Path("diagnose-mumbleacre.ps1"),
    Path("mumbleacre_plugin.dll"),
    Path("start-mumble.ps1"),
    Path("docs/acre-mission-integration.md"),
    Path("docs/distribution.md"),
    Path("docs/qa.md"),
    Path("docs/runtime.md"),
]
release_files.extend(relative_files(out / "missions/mumbleacre_smoke.VR"))
release_files = sorted(release_files)
hashes = "".join(
    f"{hashlib.sha256((out / path).read_bytes()).hexdigest()}  {path.as_posix()}\n"
    for path in release_files
)
hashes_path = out / "SHA256SUMS"
hashes_path.write_text(hashes, encoding="utf-8", newline="\n")
release_files.append(Path("SHA256SUMS"))
write_deterministic_zip(
    release_archive,
    [
        (f"{release_name}/{path.as_posix()}", (out / path).read_bytes())
        for path in release_files
    ],
)
print(f"Paquete preparado: {out / 'MumbleACRE.mumble_plugin'}")
print(f"Release preparada: {release_archive}")
