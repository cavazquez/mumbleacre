"""Release regressions: dirty output directories, hashes and repeatability."""
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location(
    "release_package", Path(__file__).resolve().parents[1] / "scripts/package.py"
)
package_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package_module)


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.out = self.root / "dist/windows-x64"
        self.prefix = "MumbleACRE-0.1.13-windows-x64/"
        self.write("Cargo.toml", b'[workspace.package]\nversion = "0.1.13"\n')
        for name in (
            "README.md", "LICENSE", "NOTICE.md", "mod.cpp",
            "scripts/start-mumble.ps1", "scripts/diagnose-mumbleacre.ps1",
            "docs/runtime.md", "docs/qa.md", "docs/acre-mission-integration.md",
            "docs/distribution.md", "missions/mumbleacre_smoke.VR/mission.sqm",
            "missions/mumbleacre_smoke.VR/init.sqf",
            "dist/windows-x64/mumbleacre_plugin.dll",
            "dist/windows-x64/@mumbleacre/addons/mumbleacre_acre.pbo",
        ):
            self.write(name, f"fixture: {name}\n".encode())

    def write(self, name, data):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)

    def test_removed_mission_files_and_old_outputs_do_not_enter_the_release(self):
        old = "missions/mumbleacre_smoke.VR/obsolete.sqf"
        self.write(old, b"old mission script")
        package_module.package(self.root)
        (self.root / old).unlink()
        self.write("dist/windows-x64/unrelated.zip", b"old archive")
        self.write("dist/windows-x64/missions/mumbleacre_smoke.VR/extra.sqf", b"stale")
        archive = package_module.package(self.root)
        with zipfile.ZipFile(archive) as release:
            names = release.namelist()
            self.assertNotIn(self.prefix + old, names)
            self.assertFalse(any(name.endswith(("extra.sqf", "unrelated.zip")) for name in names))
            self.assertIn(self.prefix + "missions/mumbleacre_smoke.VR/init.sqf", names)
            lines = release.read(self.prefix + "SHA256SUMS").decode().splitlines()
            hashed = set()
            for line in lines:
                expected, path = line.split("  ", 1)
                hashed.add(self.prefix + path)
                self.assertEqual(hashlib.sha256(release.read(self.prefix + path)).hexdigest(), expected)
            self.assertEqual(hashed, set(names) - {self.prefix + "SHA256SUMS"})

    def test_repackaging_identical_inputs_is_byte_identical(self):
        archive = package_module.package(self.root)
        first = archive.read_bytes()
        first_bundle = (self.out / "MumbleACRE.mumble_plugin").read_bytes()
        package_module.package(self.root)
        self.assertEqual(first, archive.read_bytes())
        self.assertEqual(first_bundle, (self.out / "MumbleACRE.mumble_plugin").read_bytes())
        with zipfile.ZipFile(archive) as release:
            self.assertEqual(release.namelist(), sorted(release.namelist()))
            self.assertTrue(all(info.date_time == (1980, 1, 1, 0, 0, 0) for info in release.infolist()))

    def test_missing_input_preserves_the_previous_package(self):
        archive = package_module.package(self.root)
        previous = archive.read_bytes()
        (self.root / "README.md").unlink()
        with self.assertRaisesRegex(SystemExit, "Missing required build artifact"):
            package_module.package(self.root)
        self.assertEqual(previous, archive.read_bytes())


if __name__ == "__main__":
    unittest.main()
