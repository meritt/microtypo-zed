use std::fmt::Write;
use std::io::{self, Read};

use sha2::{Digest, Sha256};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const CHECKSUMS: &str = include_str!("checksums.txt");

const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
const MAX_BINARY: u64 = 64 * 1024 * 1024;

pub fn asset_name(os: &str, arch: &str) -> String {
    let suffix = if os == "windows" { ".exe" } else { "" };
    format!("microtypo-lsp-{os}-{arch}{suffix}")
}

pub fn download_url(version: &str, asset: &str) -> String {
    format!("{REPOSITORY}/releases/download/v{version}/{asset}")
}

pub fn expected<'a>(checksums: &'a str, asset: &str) -> Option<&'a str> {
    checksums.lines().find_map(|line| {
        let (digest, name) = line.split_once("  ")?;
        let valid = digest.len() == 64 && digest.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        (valid && name == asset).then_some(digest)
    })
}

pub fn digest(reader: impl Read) -> io::Result<String> {
    let mut reader = reader.take(MAX_BINARY + 1);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    let mut total = 0;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > MAX_BINARY {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "binary too large"));
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize().iter().fold(String::with_capacity(64), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn version_in(manifest: &str) -> Option<&str> {
        manifest.lines().find_map(|line| line.strip_prefix("version = \""))?.strip_suffix('"')
    }

    #[test]
    fn names_assets_per_platform() {
        assert_eq!(asset_name("darwin", "arm64"), "microtypo-lsp-darwin-arm64");
        assert_eq!(asset_name("linux", "x64"), "microtypo-lsp-linux-x64");
        assert_eq!(asset_name("windows", "x64"), "microtypo-lsp-windows-x64.exe");
    }

    #[test]
    fn downloads_the_exact_version() {
        assert_eq!(
            download_url("1.2.3", "microtypo-lsp-linux-x64"),
            "https://github.com/meritt/microtypo-zed/releases/download/v1.2.3/microtypo-lsp-linux-x64"
        );
    }

    #[test]
    fn hashes_a_known_vector() {
        assert_eq!(digest(&b"abc"[..]).unwrap(), ABC);
    }

    #[test]
    fn detects_a_replaced_binary() {
        let checksums = format!("{ABC}  microtypo-lsp-linux-x64\n");
        let wanted = expected(&checksums, "microtypo-lsp-linux-x64").unwrap();
        assert_eq!(digest(&b"abc"[..]).unwrap(), wanted);
        assert_ne!(digest(&b"abd"[..]).unwrap(), wanted);
    }

    #[test]
    fn refuses_oversized_binaries() {
        let endless = io::repeat(0);
        assert!(digest(endless).is_err());
    }

    #[test]
    fn finds_only_well_formed_entries() {
        let short = &ABC[..63];
        let upper = ABC.to_uppercase();
        let checksums = format!("{short}  short\n{upper}  upper\n{ABC} one-space\n{ABC}  right\n");
        assert_eq!(expected(&checksums, "right"), Some(ABC));
        for asset in ["short", "upper", "one-space", "missing"] {
            assert_eq!(expected(&checksums, asset), None, "{asset}");
        }
        assert_eq!(expected("", "right"), None);
    }

    #[test]
    fn pinned_checksums_cover_every_platform() {
        if CHECKSUMS.is_empty() {
            return;
        }
        let platforms = [
            ("darwin", "arm64"),
            ("darwin", "x64"),
            ("linux", "arm64"),
            ("linux", "x64"),
            ("windows", "arm64"),
            ("windows", "x64"),
        ];
        assert_eq!(CHECKSUMS.lines().count(), platforms.len());
        for (os, arch) in platforms {
            assert!(expected(CHECKSUMS, &asset_name(os, arch)).is_some(), "{os}-{arch}");
        }
    }

    #[test]
    fn every_manifest_carries_one_version() {
        assert_eq!(version_in(include_str!("../extension.toml")), Some(VERSION));
        assert_eq!(version_in(include_str!("../lsp/Cargo.toml")), Some(VERSION));
    }
}
