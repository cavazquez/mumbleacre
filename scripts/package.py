"""Package current Windows artifacts in deterministic distributable archives."""
from pathlib import Path
import hashlib
import shutil
import tempfile
import tomllib
import zipfile


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


def require_file(path: Path) -> None:
    if not path.is_file():
        raise SystemExit(f"Missing required build artifact: {path}")


def package(root: Path, out: Path | None = None) -> Path:
    """Build from a fresh staging directory, never from leftover release files."""
    if out is None:
        out = root / "dist/windows-x64"
    version = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))[
        "workspace"
    ]["package"]["version"]
    release_name = f"MumbleACRE-{version}-windows-x64"
    inputs = [
        (out / "mumbleacre_plugin.dll", Path("mumbleacre_plugin.dll")),
        (out / "@mumbleacre/addons/mumbleacre_acre.pbo", Path("@mumbleacre/addons/mumbleacre_acre.pbo")),
        (root / "mod.cpp", Path("@mumbleacre/mod.cpp")),
    ]
    inputs.extend((root / name, Path(name)) for name in ("README.md", "LICENSE", "NOTICE.md"))
    inputs.extend(
        (root / "scripts" / name, Path(name))
        for name in ("start-mumble.ps1", "diagnose-mumbleacre.ps1")
    )
    inputs.extend(
        (root / "docs" / name, Path("docs") / name)
        for name in ("runtime.md", "qa.md", "acre-mission-integration.md", "distribution.md")
    )
    mission = root / "missions/mumbleacre_smoke.VR"
    require_file(mission / "mission.sqm")
    inputs.extend((path, path.relative_to(root)) for path in mission.rglob("*") if path.is_file())
    for source, _ in inputs:
        require_file(source)

    out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".mumbleacre-", dir=out.parent) as directory:
        staging = Path(directory)
        release_files = []
        for source, relative in inputs:
            destination = staging / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, destination)
            release_files.append(relative)
        manifest = f'''<?xml version="1.0" encoding="UTF-8"?>
<bundle version="1.0.0">
  <assets><plugin os="windows" arch="x64">mumbleacre_plugin.dll</plugin></assets>
  <name>MumbleACRE</name><version>{version}</version>
</bundle>
'''
        write_deterministic_zip(
            staging / "MumbleACRE.mumble_plugin",
            [("manifest.xml", manifest.encode("utf-8")),
             ("mumbleacre_plugin.dll", (staging / "mumbleacre_plugin.dll").read_bytes())],
        )
        release_files.append(Path("MumbleACRE.mumble_plugin"))
        release_files.sort(key=lambda path: path.as_posix())
        hashes = "".join(
            f"{hashlib.sha256((staging / path).read_bytes()).hexdigest()}  {path.as_posix()}\n"
            for path in release_files
        )
        (staging / "SHA256SUMS").write_text(hashes, encoding="utf-8", newline="\n")
        release_files.append(Path("SHA256SUMS"))
        archive = staging / f"{release_name}.zip"
        write_deterministic_zip(
            archive,
            [(f"{release_name}/{path.as_posix()}", (staging / path).read_bytes())
             for path in release_files],
        )
        for relative in release_files:
            destination = out / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(staging / relative, destination)
        release_archive = archive.replace(out / archive.name)
    return release_archive


if __name__ == "__main__":
    root = Path(__file__).resolve().parent.parent
    archive = package(root)
    print(f"Paquete preparado: {archive.parent / 'MumbleACRE.mumble_plugin'}")
    print(f"Release preparada: {archive}")
