import hashlib
import importlib.util
import io
import pathlib
import subprocess
import tarfile
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).with_name("build_legacy_profile.py")
spec = importlib.util.spec_from_file_location("driver", SCRIPT)
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)


def source_fixture(root):
    for name in driver.INPUTS:
        path = root / name
        if name in ("src", "skills/mote-tracker", "skills/mote-message-board", "web/dist"):
            path.mkdir(parents=True)
            (path / "input.txt").write_text("original")
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("original")
    (root / "Cargo.toml").write_text("[package]\nname='fixture'\nversion='0.1.0'\n")


class DriverTests(unittest.TestCase):
    def test_inventory_covers_dirty_helper_untracked_asset_and_build_script(self):
        with tempfile.TemporaryDirectory() as td:
            root = pathlib.Path(td)
            source_fixture(root)
            before = driver.inventory(root)
            for name in ("src/input.txt", "web/dist/input.txt", "skills/mote-tracker/input.txt", "build.rs", "Cargo.lock"):
                path = root / name
                original = path.read_bytes()
                path.write_bytes(original + b"changed")
                self.assertNotEqual(before, driver.inventory(root), name)
                path.write_bytes(original)
            (root / "src/untracked.rs").write_text("new helper")
            self.assertNotEqual(before, driver.inventory(root))

    def test_symlink_file_and_directory_inputs_refuse(self):
        with tempfile.TemporaryDirectory() as td:
            root = pathlib.Path(td)
            source_fixture(root)
            target = root / "src/input.txt"
            for source in (target, target.parent):
                alias = root / "web/dist/alias"
                alias.symlink_to(source)
                with self.assertRaisesRegex(ValueError, "symlink"):
                    driver.inventory(root)
                alias.unlink()

    def test_unsupported_configuration_refuses(self):
        with tempfile.TemporaryDirectory() as td:
            root = pathlib.Path(td)
            source_fixture(root)
            for variable in ("CARGO_HOME", "RUSTFLAGS", "RUSTC_WRAPPER", "SDKROOT"):
                with self.assertRaises(ValueError):
                    driver.refuse_configuration(root, {variable: "override"})
            for section in ("[dependencies]\nx={path='x'}", "[dependencies]\nx={git='https://example.invalid/x'}", "[patch.crates-io]\nx='1'", "[features]\nx=[]"):
                (root / "Cargo.toml").write_text("[package]\nname='x'\nversion='0.1.0'\n" + section + "\n")
                with self.assertRaises(ValueError):
                    driver.refuse_configuration(root, {})
            (root / "Cargo.toml").write_text("[package]\nname='x'\nversion='0.1.0'\n")
            (root / ".cargo").mkdir()
            (root / ".cargo/config.toml").write_text("")
            with self.assertRaisesRegex(ValueError, "hierarchical"):
                driver.refuse_configuration(root, {})

    def registry_fixture(self, root):
        source = root / "registry/src/index/example-1.0.0"
        source.mkdir(parents=True)
        content = {"Cargo.toml": b"[package]\nname='example'\nversion='1.0.0'\n", "src/lib.rs": b"pub fn value()->u8 {1}\n"}
        archive = root / "registry/cache/index/example-1.0.0.crate"
        archive.parent.mkdir(parents=True)
        with tarfile.open(archive, "w:gz") as tar:
            for name, data in content.items():
                path = source / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
                info = tarfile.TarInfo("example-1.0.0/" + name)
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
        (source / ".cargo-ok").write_text("")
        package = {"name": "example", "version": "1.0.0", "source": driver.REGISTRY, "manifest_path": str(source / "Cargo.toml")}
        lock = {("example", "1.0.0", driver.REGISTRY): hashlib.sha256(archive.read_bytes()).hexdigest()}
        return package, lock, source, archive

    def test_locked_archive_checks_actual_extracted_bytes(self):
        with tempfile.TemporaryDirectory() as td:
            root = pathlib.Path(td)
            package, lock, source, archive = self.registry_fixture(root)
            receipt = driver.verify_registry_package(package, lock, root)
            self.assertEqual(receipt["checksum"], lock[("example", "1.0.0", driver.REGISTRY)])
            library = source / "src/lib.rs"
            original = library.read_bytes()
            library.write_bytes(b"modified extracted dependency")
            with self.assertRaisesRegex(ValueError, "extracted registry"):
                driver.verify_registry_package(package, lock, root)
            library.write_bytes(original)
            archive.write_bytes(archive.read_bytes() + b"damaged")
            with self.assertRaisesRegex(ValueError, "checksum"):
                driver.verify_registry_package(package, lock, root)

    def test_dependency_identity_extra_input_and_source_class_refuse(self):
        with tempfile.TemporaryDirectory() as td:
            root = pathlib.Path(td)
            package, lock, source, _ = self.registry_fixture(root)
            with self.assertRaises(ValueError):
                driver.verify_registry_package(package, {}, root)
            extra = source / "extra.rs"
            extra.write_text("new compiler input")
            with self.assertRaisesRegex(ValueError, "file set"):
                driver.verify_registry_package(package, lock, root)
            extra.unlink()
            package["source"] = "git+https://example.invalid/dependency"
            with self.assertRaisesRegex(ValueError, "unsupported"):
                driver.verify_registry_package(package, lock, root)

    def test_retained_artifacts_are_never_overwritten(self):
        with tempfile.TemporaryDirectory() as td:
            output = pathlib.Path(td) / "binary"
            driver.retained_file(output, b"first", 0o600)
            with self.assertRaises(FileExistsError):
                driver.retained_file(output, b"second", 0o600)
            self.assertEqual(output.read_bytes(), b"first")

    def test_external_library_target_and_compiler_reported_module_refuse(self):
        with tempfile.TemporaryDirectory() as td:
            root = pathlib.Path(td).resolve()
            source_fixture(root)
            escaped = root / "escaped.rs"
            escaped.write_text("pub fn value()->u8 { 7 }\n")
            metadata = {"packages": [{"manifest_path": str(root / "Cargo.toml"), "targets": [{"name": "mote", "kind": ["lib"], "src_path": str(escaped)}]}]}
            with self.assertRaisesRegex(ValueError, "target source escapes"):
                driver.validate_targets(metadata, root)
            library = root / "src/lib.rs"
            library.write_text('#[path="../escaped.rs"] mod escaped; pub fn value()->u8 { escaped::value() }\n')
            metadata["packages"][0]["targets"][0]["src_path"] = str(library)
            driver.validate_targets(metadata, root)
            dep_info = root / "compiler.d"
            subprocess.run(["rustc", "--crate-type", "lib", "--crate-name", "closure", str(library),
                            f"--emit=dep-info={dep_info},metadata={root / 'library.rmeta'}"],
                           cwd=root, check=True, capture_output=True)
            with self.assertRaisesRegex(ValueError, "compiler dependency escapes"):
                driver.validate_dep_info(dep_info, root)
            escaped.unlink()
            library.write_text("pub fn value()->u8 { 7 }\n")
            subprocess.run(["rustc", "--crate-type", "lib", "--crate-name", "closure", str(library),
                            f"--emit=dep-info={dep_info},metadata={root / 'library.rmeta'}"],
                           cwd=root, check=True, capture_output=True)
            self.assertIn("src/lib.rs", driver.validate_dep_info(dep_info, root))


if __name__ == "__main__":
    unittest.main()
