"""Keep retired runtime components out of the workspace and check docs links."""
from pathlib import Path
import re
import tomllib

root = Path(__file__).resolve().parent.parent
manifest = tomllib.loads((root / "Cargo.toml").read_text())
assert set(manifest["workspace"]["members"]) == {
    "crates/mumbleacre-acre", "crates/mumbleacre-logging", "crates/mumbleacre-plugin"
}
for base in (root / "crates", root / "acre-addon", root / "missions"):
    for path in base.rglob("*"):
        if not path.is_file() or path.suffix not in (".rs", ".toml", ".sqf", ".cpp", ".hpp"):
            continue
        text = path.read_text()
        assert not re.search(r"UdpSocket|RadioStateMessage|TFAR_fnc_|RMTFAR_fnc_|rmtfar_x64|callExtension", text), path
for path in [root / "README.md", * (root / "docs").glob("*.md")]:
    for target in re.findall(r"\]\(([^)]+)\)", path.read_text()):
        if "://" not in target and not target.startswith("#"):
            assert (path.parent / target.split("#")[0]).exists(), (path, target)
print("Workspace, runtime boundaries and documentation links: PASS")
