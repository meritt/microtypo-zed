mod release;

use std::fs::{self, File};

use zed_extension_api::{
    self as zed, LanguageServerId, LanguageServerInstallationStatus as Status, Result, serde_json,
    settings::LspSettings,
};

const SERVER: &str = "microtypo";
const INSTALL_PREFIX: &str = "microtypo-lsp-";
const STDIO: &str = "--stdio";

struct Microtypo;

fn os_name(os: zed::Os) -> &'static str {
    match os {
        zed::Os::Mac => "darwin",
        zed::Os::Linux => "linux",
        zed::Os::Windows => "windows",
    }
}

fn arch_name(arch: zed::Architecture) -> Result<&'static str> {
    match arch {
        zed::Architecture::Aarch64 => Ok("arm64"),
        zed::Architecture::X8664 => Ok("x64"),
        zed::Architecture::X86 => Err("MicroTypo: unsupported architecture".into()),
    }
}

fn verified(path: &str, expected: &str) -> bool {
    File::open(path).and_then(release::digest).is_ok_and(|digest| digest == expected)
}

fn remove_stale(current: &str) {
    let Ok(entries) = fs::read_dir(".") else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(INSTALL_PREFIX) && name != current {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

fn fetch(id: &LanguageServerId) -> Result<String> {
    let (os, arch) = zed::current_platform();
    let asset = release::asset_name(os_name(os), arch_name(arch)?);
    let expected = release::expected(release::CHECKSUMS, &asset)
        .ok_or("MicroTypo: this build has no checksum for the platform")?;
    let dir = format!("{INSTALL_PREFIX}{}", release::VERSION);
    let path = format!("{dir}/{asset}");

    if !verified(&path, expected) {
        zed::set_language_server_installation_status(id, &Status::Downloading);
        fs::create_dir_all(&dir).map_err(|_| "MicroTypo: cannot create the install directory")?;
        let partial = format!("{path}.partial");
        let _ = fs::remove_file(&partial);
        zed::download_file(
            &release::download_url(release::VERSION, &asset),
            &partial,
            zed::DownloadedFileType::Uncompressed,
        )
        .map_err(|_| "MicroTypo: download failed")?;
        if !verified(&partial, expected) {
            let _ = fs::remove_file(&partial);
            return Err("MicroTypo: the downloaded binary failed verification".into());
        }
        fs::rename(&partial, &path).map_err(|_| "MicroTypo: cannot install the binary")?;
    }

    zed::make_file_executable(&path).map_err(|_| "MicroTypo: cannot mark the binary executable")?;
    remove_stale(&dir);
    Ok(path)
}

fn install(id: &LanguageServerId) -> Result<String> {
    let result = fetch(id);
    let status = match &result {
        Ok(_) => Status::None,
        Err(error) => Status::Failed(error.clone()),
    };
    zed::set_language_server_installation_status(id, &status);
    result
}

fn binary_override(worktree: &zed::Worktree) -> Option<zed::Command> {
    let binary = LspSettings::for_worktree(SERVER, worktree).ok()?.binary?;
    Some(zed::Command {
        command: binary.path?,
        args: binary.arguments.unwrap_or_else(|| vec![STDIO.into()]),
        env: Vec::new(),
    })
}

impl zed::Extension for Microtypo {
    fn new() -> Self {
        Self
    }

    fn language_server_command(&mut self, id: &LanguageServerId, worktree: &zed::Worktree) -> Result<zed::Command> {
        if let Some(command) = binary_override(worktree) {
            return Ok(command);
        }
        Ok(zed::Command { command: install(id)?, args: vec![STDIO.into()], env: Vec::new() })
    }

    fn language_server_initialization_options(
        &mut self,
        _id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<serde_json::Value>> {
        Ok(LspSettings::for_worktree(SERVER, worktree).ok().and_then(|settings| settings.initialization_options))
    }
}

zed::register_extension!(Microtypo);
