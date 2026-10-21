use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn inventory(
    root: &Path,
    path: &Path,
    b3: &mut blake3::Hasher,
    sha: &mut Sha256,
) -> Result<(), String> {
    println!("cargo:rerun-if-changed={}", path.display());
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if meta.file_type().is_symlink() {
        return Err("symlink build input".into());
    }
    if meta.is_dir() {
        let mut entries = fs::read_dir(path)
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            inventory(root, &entry.path(), b3, sha)?;
        }
    } else if meta.is_file() {
        let relative = path
            .strip_prefix(root)
            .map_err(|e| e.to_string())?
            .to_str()
            .ok_or("non-UTF8 build input")?;
        let bytes = fs::read(path).map_err(|e| e.to_string())?;
        for part in [relative.as_bytes(), &[0], bytes.as_slice(), &[0]] {
            b3.update(part);
            sha.update(part);
        }
    } else {
        return Err("nonregular build input".into());
    }
    Ok(())
}
fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}
fn hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn checked_receipt(path: &Path, inventory: &str) -> Option<String> {
    println!("cargo:rerun-if-changed={}", path.display());
    let raw = fs::read(path).ok()?;
    let mut receipt: Value = serde_json::from_slice(&raw).ok()?;
    if serde_json::to_vec(&receipt).ok()? != raw {
        return None;
    }
    let keys = [
        "recipe",
        "inventory_sha256",
        "toolchain",
        "compiler_vv",
        "cargo_version",
        "host",
        "target",
        "profile",
        "packages",
        "build_config_digest",
    ];
    if receipt.as_object()?.len() != keys.len()
        || keys.iter().any(|key| receipt.get(*key).is_none())
    {
        return None;
    }
    if text(&receipt, "recipe")? != "mote.legacy-dev.v1"
        || text(&receipt, "inventory_sha256")? != inventory
        || text(&receipt, "toolchain")? != "1.85"
        || text(&receipt, "profile")? != "dev"
        || text(&receipt, "host")? != env::var("HOST").ok()?
        || text(&receipt, "target")? != env::var("TARGET").ok()?
        || env::var("PROFILE").ok()? != "debug"
        || env::var("OPT_LEVEL").ok()? != "0"
        || env::vars().any(|(key, _)| key.starts_with("CARGO_FEATURE_"))
        || [
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
        ]
        .iter()
        .any(|name| env::var(name).is_ok_and(|v| !v.is_empty()))
    {
        return None;
    }
    let rustc = Command::new(env::var_os("RUSTC")?)
        .arg("-vV")
        .output()
        .ok()?;
    let cargo = Command::new(env::var_os("CARGO")?)
        .arg("--version")
        .output()
        .ok()?;
    if !rustc.status.success()
        || !cargo.status.success()
        || text(&receipt, "compiler_vv")? != std::str::from_utf8(&rustc.stdout).ok()?
        || text(&receipt, "cargo_version")? != std::str::from_utf8(&cargo.stdout).ok()?.trim()
    {
        return None;
    }
    for package in receipt.get("packages")?.as_array()? {
        if package.as_object()?.len() != 5
            || text(package, "source")? != "registry+https://github.com/rust-lang/crates.io-index"
            || !hex(text(package, "checksum")?)
            || text(package, "name")?.is_empty()
            || text(package, "version")?.is_empty()
        {
            return None;
        }
        let features = package.get("features")?.as_array()?;
        if features.iter().any(|feature| !feature.is_string())
            || !features.windows(2).all(|w| w[0].as_str() < w[1].as_str())
        {
            return None;
        }
    }
    let expected = text(&receipt, "build_config_digest")?.to_owned();
    if !hex(&expected) {
        return None;
    }
    receipt.as_object_mut()?.remove("build_config_digest");
    let actual = format!("{:x}", Sha256::digest(serde_json::to_vec(&receipt).ok()?));
    (expected == actual).then_some(expected)
}
fn main() {
    for name in [
        "MOTE_LEGACY_BUILD_RECEIPT",
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory"));
    let mut b3 = blake3::Hasher::new();
    b3.update(b"mote.legacy-build-input.v1\0");
    let mut sha = Sha256::new();
    let result = [
        "src",
        "skills/mote-tracker",
        "skills/mote-message-board",
        "web/dist",
        "build.rs",
        "Cargo.toml",
        "Cargo.lock",
        "scripts/build_legacy_profile.py",
    ]
    .iter()
    .try_for_each(|path| inventory(&root, &root.join(path), &mut b3, &mut sha));
    let inventory_sha256 = format!("{:x}", sha.finalize());
    let config = if result.is_ok() {
        env::var_os("MOTE_LEGACY_BUILD_RECEIPT")
            .and_then(|path| checked_receipt(Path::new(&path), &inventory_sha256))
    } else {
        None
    };
    println!(
        "cargo:rustc-env=MOTE_LEGACY_INPUT_DIGEST={}",
        if config.is_some() {
            b3.finalize().to_hex().to_string()
        } else {
            String::new()
        }
    );
    println!(
        "cargo:rustc-env=MOTE_LEGACY_BUILD_CONFIG_DIGEST={}",
        config.unwrap_or_default()
    );
}
