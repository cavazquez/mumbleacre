"""Package only current Windows artifacts using Mumble's native bundle format."""
from pathlib import Path
import hashlib
import shutil
import tomllib
import zipfile

root = Path(__file__).resolve().parent.parent
out = root / "dist/windows-x64"
version = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
manifest = f'''<?xml version="1.0" encoding="UTF-8"?>
<bundle version="1.0.0">
  <assets><plugin os="windows" arch="x64">mumbleacre_plugin.dll</plugin></assets>
  <name>MumbleACRE</name><version>{version}</version>
</bundle>
'''
with zipfile.ZipFile(out / "MumbleACRE.mumble_plugin", "w", zipfile.ZIP_DEFLATED) as bundle:
    bundle.writestr("manifest.xml", manifest)
    bundle.write(out / "mumbleacre_plugin.dll", "mumbleacre_plugin.dll")
for name in ("README.md", "LICENSE", "NOTICE.md"):
    shutil.copy2(root / name, out / name)
shutil.copy2(root / "scripts/start-mumble.ps1", out / "start-mumble.ps1")
(out / "docs").mkdir(exist_ok=True)
for name in ("runtime.md", "qa.md", "acre-mission-integration.md"):
    shutil.copy2(root / "docs" / name, out / "docs" / name)
shutil.copytree(root / "missions/mumbleacre_smoke.VR", out / "missions/mumbleacre_smoke.VR", dirs_exist_ok=True)
files = sorted(p for p in out.rglob("*") if p.is_file() and p.name != "SHA256SUMS")
(out / "SHA256SUMS").write_text("".join(f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.relative_to(out).as_posix()}\n" for p in files))
print(f"Paquete preparado: {out / 'MumbleACRE.mumble_plugin'}")
