#!/usr/bin/env python3
"""Build portable legacy replay compatibility with exact build provenance.

This deliberately refuses unsupported Cargo configuration. It verifies extracted
registry inputs against locked .crate archives, retains the binary and receipt,
and neither migrates a store nor performs any remote authority operation.
"""
import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib

ROOT = pathlib.Path(__file__).resolve().parents[1]
INPUTS = (
    "src", "skills/mote-tracker", "skills/mote-message-board", "web/dist",
    "build.rs", "Cargo.toml", "Cargo.lock", "scripts/build_legacy_profile.py",
)
RECIPE = "mote.legacy-dev.v1"
TOOLCHAIN = "1.85"
REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


def input_files(root=ROOT):
    def walk(path):
        if path.is_symlink():
            raise ValueError(f"symlink build input: {path.relative_to(root)}")
        if path.is_file():
            yield path
        elif path.is_dir():
            for child in sorted(path.iterdir()):
                yield from walk(child)
        else:
            raise ValueError(f"missing or unsupported build input: {path.relative_to(root)}")
    for name in INPUTS:
        yield from walk(root / name)


def inventory(root=ROOT):
    hasher = hashlib.sha256()
    for path in input_files(root):
        hasher.update(path.relative_to(root).as_posix().encode() + b"\0")
        hasher.update(path.read_bytes())
        hasher.update(b"\0")
    return hasher.hexdigest()


def refuse_configuration(root=ROOT, env=None):
    env = os.environ if env is None else env
    data = tomllib.loads((root / "Cargo.toml").read_text())
    for target in [data.get("lib", {}), *data.get("bin", [])]:
        path = target.get("path")
        if path and (pathlib.PurePosixPath(path).is_absolute() or ".." in pathlib.PurePosixPath(path).parts or pathlib.PurePosixPath(path).parts[0] != "src"):
            raise ValueError("target source escapes the inventoried source tree")
    if any(key in data for key in ("workspace", "patch", "replace", "features", "target")):
        raise ValueError("unsupported Cargo manifest section")
    if data.get("profile", {}).get("dev") or data.get("package", {}).get("build", "build.rs") != "build.rs":
        raise ValueError("unsupported build/profile override")
    for section in ("dependencies", "dev-dependencies", "build-dependencies"):
        for dependency in data.get(section, {}).values():
            if isinstance(dependency, dict) and any(key in dependency for key in ("path", "git", "registry", "registry-index", "workspace")):
                raise ValueError("only locked crates.io dependencies are supported")
    for parent in (root, *root.parents):
        if any((parent / ".cargo" / name).exists() for name in ("config", "config.toml")):
            raise ValueError("hierarchical Cargo config is unsupported")
    overrides = {"CC", "CXX", "CFLAGS", "CXXFLAGS", "CPPFLAGS", "AR", "LD", "SDKROOT", "MACOSX_DEPLOYMENT_TARGET"}
    for key in env:
        if key.startswith(("CARGO_", "RUST")) and key != "RUSTUP_HOME" or key in overrides:
            raise ValueError(f"unsupported inherited build override: {key}")


def verify_registry_package(package, lock, cargo_home):
    """Verify actual extracted bytes, not just a claimed checksum file."""
    if package.get("source") != REGISTRY:
        raise ValueError("path, git and alternate registry packages are unsupported")
    key = (package["name"], package["version"], package["source"])
    checksum = lock.get(key)
    if not checksum:
        raise ValueError("resolved registry package is absent from the locked closure")
    manifest = pathlib.Path(package["manifest_path"])
    source = manifest.parent
    try:
        source.relative_to(cargo_home / "registry" / "src")
    except ValueError as error:
        raise ValueError("registry source escaped the private Cargo home") from error
    archives = list((cargo_home / "registry" / "cache").glob(f"*/{package['name']}-{package['version']}.crate"))
    if len(archives) != 1 or archives[0].is_symlink():
        raise ValueError("missing or ambiguous locked registry archive")
    if hashlib.sha256(archives[0].read_bytes()).hexdigest() != checksum:
        raise ValueError("registry archive checksum differs from Cargo.lock")
    expected = set()
    prefix = f"{package['name']}-{package['version']}"
    with tarfile.open(archives[0], "r:gz") as archive:
        for member in archive:
            path = pathlib.PurePosixPath(member.name)
            if not path.parts or path.parts[0] != prefix or path.is_absolute() or ".." in path.parts:
                raise ValueError("unsafe registry archive path")
            if member.isdir():
                continue
            if not member.isfile():
                raise ValueError("nonregular registry input is unsupported")
            relative = pathlib.Path(*path.parts[1:])
            if relative.as_posix() in expected:
                raise ValueError("duplicate registry archive input")
            expected.add(relative.as_posix())
            actual = source / relative
            if actual.is_symlink() or not actual.is_file() or actual.read_bytes() != archive.extractfile(member).read():
                raise ValueError("extracted registry bytes differ from locked archive")
    actual_files = set()
    for path in source.rglob("*"):
        if path.is_symlink():
            raise ValueError("symlink registry input is unsupported")
        if path.is_file():
            actual_files.add(path.relative_to(source).as_posix())
    # Cargo's unpack-completion marker is not a compiler input.
    actual_files.discard(".cargo-ok")
    if actual_files != expected:
        raise ValueError("registry input file set differs from locked archive")
    return {"name": package["name"], "version": package["version"], "source": REGISTRY, "checksum": checksum}


def registry_receipt(metadata, root, cargo_home):
    locked = tomllib.loads((root / "Cargo.lock").read_text())
    lock = {(p["name"], p["version"], p.get("source")): p.get("checksum") for p in locked["package"]}
    features = {node["id"]: sorted(node["features"]) for node in metadata["resolve"]["nodes"]}
    records = []
    roots = []
    for package in metadata["packages"]:
        if package.get("source") is None:
            if pathlib.Path(package["manifest_path"]).resolve() != (root / "Cargo.toml").resolve():
                raise ValueError("path dependency is unsupported")
            roots.append(package["id"])
            continue
        record = verify_registry_package(package, lock, cargo_home)
        record["features"] = features.get(package["id"], [])
        records.append(record)
    if len(roots) != 1 or features.get(roots[0], []):
        raise ValueError("unsupported workspace or root feature set")
    return sorted(records, key=lambda p: (p["name"], p["version"]))


def validate_targets(metadata, root):
    root = root.resolve()
    inputs = {path.resolve() for path in input_files(root)}
    package = next((package for package in metadata["packages"] if pathlib.Path(package["manifest_path"]).resolve() == (root / "Cargo.toml").resolve()), None)
    if package is None:
        raise ValueError("root package is missing")
    for target in package["targets"]:
        kinds = set(target["kind"])
        if kinds.intersection({"test", "bench", "example"}):
            continue  # The fixed binary build never compiles these targets.
        if "bin" in kinds and target["name"] != "mote":
            raise ValueError("additional binary targets are outside this recipe")
        if pathlib.Path(target["src_path"]).resolve() not in inputs:
            raise ValueError("resolved target source escapes the inventoried closure")


def validate_dep_info(path, root, build_receipt=None):
    root = root.resolve()
    # Rust/Cargo dep-info is Makefile syntax. The deliberately narrow recipe
    # refuses escaped path forms instead of approximating that grammar.
    text = path.read_text().replace("\\\n", "")
    first_rule = text.splitlines()[0] if text.splitlines() else ""
    _, separator, dependencies = first_rule.partition(": ")
    if not separator or not dependencies.strip():
        raise ValueError("missing compiler dependency input evidence")
    inputs = {path.resolve() for path in input_files(root)}
    directories = set()
    for item in inputs:
        directories.update(parent for parent in item.parents if parent == root or root in parent.parents)
    observed = set()
    receipt_path = build_receipt.resolve() if build_receipt else None
    for token in dependencies.split():
        if "\\" in token or ":" in token:
            raise ValueError("escaped compiler dependency paths are unsupported")
        source = pathlib.Path(token)
        if not source.is_absolute():
            source = root / source
        source = source.resolve(strict=True)
        if source == receipt_path:
            observed.add("@validated-build-receipt")
            continue
        if source not in inputs and source not in directories:
            raise ValueError(f"compiler dependency escapes inventoried closure: {source}")
        observed.add(source.relative_to(root).as_posix())
    return sorted(observed)


def retained_file(path, data, mode):
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
    with os.fdopen(descriptor, "wb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: build_legacy_profile.py RETAINED_OUTPUT")
    output = pathlib.Path(sys.argv[1]).absolute()
    receipt_output = output.with_name(output.name + ".build.json")
    if output.exists() or output.is_symlink() or receipt_output.exists() or receipt_output.is_symlink():
        raise ValueError("refusing to overwrite a retained artifact")
    refuse_configuration()
    if any(character.isspace() or character in ":\\" for character in str(ROOT)):
        raise ValueError("escaped source checkout paths are outside this recipe")
    rustup = shutil.which("rustup")
    if not rustup:
        raise ValueError("the pinned rustup toolchain is required")
    # Compiler/package subprocesses receive no inherited Cargo configuration,
    # registry credentials, wrappers or compiler flags.
    env = {"HOME": str(pathlib.Path.home()), "PATH": "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin"}
    env["RUSTUP_HOME"] = os.environ.get("RUSTUP_HOME", str(pathlib.Path.home() / ".rustup"))
    rustc_path = pathlib.Path(subprocess.run([rustup, "which", "--toolchain", TOOLCHAIN, "rustc"],
                                          env=env, check=True, capture_output=True, text=True).stdout.strip())
    env["PATH"] = str(rustc_path.parent) + ":" + env["PATH"]
    command = [rustup, "run", TOOLCHAIN]
    def run(args, capture=False):
        return subprocess.run(command + args, cwd=ROOT, env=env, check=True,
                              stdout=subprocess.PIPE if capture else None, text=capture)
    compiler = run(["rustc", "-vV"], True).stdout
    cargo_version = run(["cargo", "--version"], True).stdout.strip()
    host = next(line.removeprefix("host: ") for line in compiler.splitlines() if line.startswith("host: "))
    with tempfile.TemporaryDirectory(prefix="mote-legacy-build-") as temporary:
        temporary = pathlib.Path(temporary)
        cargo_home = temporary / "cargo-home"
        target = temporary / "target"
        env.update(CARGO_HOME=str(cargo_home), CARGO_TARGET_DIR=str(target))
        metadata = json.loads(run(["cargo", "metadata", "--locked", "--format-version", "1", "--no-default-features"], True).stdout)
        validate_targets(metadata, ROOT)
        packages = registry_receipt(metadata, ROOT, cargo_home)
        before = inventory()
        configuration = {"recipe": RECIPE, "inventory_sha256": before, "toolchain": TOOLCHAIN,
                         "compiler_vv": compiler, "cargo_version": cargo_version, "host": host,
                         "target": host, "profile": "dev", "packages": packages}
        configuration["build_config_digest"] = hashlib.sha256(canonical(configuration)).hexdigest()
        receipt = temporary / "receipt.json"
        receipt.write_bytes(canonical(configuration))
        env["MOTE_LEGACY_BUILD_RECEIPT"] = str(receipt)
        run(["cargo", "build", "--locked", "--bin", "mote", "--target", host, "--profile", "dev", "--no-default-features"])
        local_inputs = validate_dep_info(target / host / "debug" / "mote.d", ROOT, receipt)
        if receipt.read_bytes() != canonical(configuration):
            raise ValueError("build configuration receipt changed while compiling")
        if inventory() != before:
            raise ValueError("checkout inputs changed while building; no artifact retained")
        registry_receipt(metadata, ROOT, cargo_home)
        artifact = target / host / "debug" / "mote"
        if artifact.is_symlink() or not artifact.is_file():
            raise ValueError("missing or unsafe build artifact")
        binary = artifact.read_bytes()
        output.parent.mkdir(parents=True, exist_ok=True)
        retained_file(output, binary, 0o755)
        # The retained binary proves its embedded receipt is available. No store
        # or session is needed for this diagnostic.
        profile = json.loads(subprocess.run([str(output), "--json", "authority", "legacy-profile"], env=env,
                                           cwd=temporary, check=True, capture_output=True, text=True).stdout)
        retained_file(receipt_output, canonical({"configuration": configuration, "verified_local_inputs": local_inputs, "binary_sha256": hashlib.sha256(binary).hexdigest(), "profile": profile}), 0o600)
        directory = os.open(output.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
        print(json.dumps({"binary": str(output), "receipt": str(receipt_output), "profile": profile}, sort_keys=True))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        raise SystemExit(str(error))
