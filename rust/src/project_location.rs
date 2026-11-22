use std::{
    fs::{self, DirBuilder, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub(super) fn local_directory() -> Option<PathBuf> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })?;
    let home = PathBuf::from(home);
    home.is_absolute()
        .then(|| home.join(".torquetum").join("connections").join("projects"))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Location {
    port: u16,
    instance_id: Uuid,
    process_id: u32,
}

/// A local, credential-free address record owned by one live Studio registration.
pub(super) struct ProjectLocation {
    pub(super) key: String,
    file: PathBuf,
}

impl ProjectLocation {
    pub(super) fn publish(
        directory: &Path,
        origin: &str,
        project_id: &str,
        port: u16,
        instance_id: Uuid,
    ) -> io::Result<Self> {
        let mut digest = Sha256::new();
        digest.update(origin);
        digest.update([0]);
        digest.update(project_id);
        let key = format!("{:x}", digest.finalize());
        let project = directory.join(&key);
        let mut directories = DirBuilder::new();
        directories.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut directories, 0o700);
        directories.create(&project)?;

        let file = project.join(format!("{instance_id}.json"));
        let staged = file.with_extension("tmp");
        let location = Location {
            port,
            instance_id,
            process_id: std::process::id(),
        };
        let mut output = OpenOptions::new();
        output.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut output, 0o600);
        let mut writer = output.open(&staged)?;
        let result = serde_json::to_writer(&mut writer, &location)
            .map_err(io::Error::other)
            .and_then(|()| writer.flush());
        drop(writer);
        if let Err(error) = result.and_then(|()| fs::rename(&staged, &file)) {
            let _ = fs::remove_file(&staged);
            return Err(error);
        }
        Ok(Self { key, file })
    }
}

impl Drop for ProjectLocation {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.file);
    }
}
