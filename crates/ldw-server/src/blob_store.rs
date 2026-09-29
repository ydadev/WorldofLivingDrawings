//! Private, immutable on-disk Paint Textures. IDs are full SHA-256 digests of
//! normalized PNG bytes; the path is never accepted from an HTTP client.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::paint_image::MAX_UPLOAD_BYTES;

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

fn digest_id(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut id = String::with_capacity(64);
    for octet in Sha256::digest(bytes) {
        id.push(HEX[(octet >> 4) as usize] as char);
        id.push(HEX[(octet & 0x0f) as usize] as char);
    }
    id
}

#[derive(Debug, thiserror::Error)]
pub enum BlobStoreError {
    #[error("invalid blob directory or digest")]
    InvalidPath,
    #[error("invalid or oversized normalized Paint Texture")]
    InvalidPaint,
    #[error("a stored blob differs from its digest")]
    CorruptBlob,
    #[error("blob storage operation failed: {0}")]
    Io(#[from] io::Error),
}

#[derive(Clone, Debug)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    /// The runtime owns this private persistent directory. It must already
    /// exist; missing storage is a startup failure, not a fallback to /tmp.
    pub fn new(root: PathBuf) -> Result<Self, BlobStoreError> {
        if !root.is_absolute() || !private_directory(&root)? {
            return Err(BlobStoreError::InvalidPath);
        }
        Ok(Self { root })
    }

    fn path(&self, id: &str) -> Result<PathBuf, BlobStoreError> {
        if id.len() != 64
            || !id
                .bytes()
                .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
        {
            return Err(BlobStoreError::InvalidPath);
        }
        Ok(self
            .root
            .join("sha256")
            .join(&id[..2])
            .join(format!("{id}.png")))
    }

    /// Call only with bytes returned by the PNG normalizer. `hard_link` is an
    /// atomic no-replace install on the same volume; a failed DB transaction
    /// can leave only an unreferenced file for a later GC pass.
    pub fn put_normalized(&self, png: &[u8]) -> Result<String, BlobStoreError> {
        if png.len() > MAX_UPLOAD_BYTES || !png.starts_with(PNG_SIGNATURE) {
            return Err(BlobStoreError::InvalidPaint);
        }
        let id = digest_id(png);
        let final_path = self.path(&id)?;
        let directory = final_path.parent().ok_or(BlobStoreError::InvalidPath)?;
        if !private_directory(&self.root)? {
            return Err(BlobStoreError::InvalidPath);
        }
        create_private_directory(&self.root.join("sha256"))?;
        create_private_directory(directory)?;

        // `create_new` avoids following an existing path, even if an attacker
        // can somehow create files in the service-owned directory.
        let temporary = directory.join(format!(".{}.tmp", Uuid::new_v4()));
        let mut file = private_new_file(&temporary)?;
        let result = (|| {
            file.write_all(png)?;
            file.sync_all()?;
            match fs::hard_link(&temporary, &final_path) {
                Ok(()) => sync_directory(directory)?,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    verify_existing(&final_path, &id, Some(png))?;
                }
                Err(error) => return Err(BlobStoreError::Io(error)),
            }
            Ok(())
        })();
        drop(file);
        let cleanup = fs::remove_file(&temporary);
        if let Err(error) = result {
            let _ = cleanup;
            return Err(error);
        }
        cleanup?;
        sync_directory(directory)?;
        Ok(id)
    }

    /// The caller must check current scene/grant permissions before returning
    /// these bytes to a client. A digest itself is never an access grant.
    pub fn read(&self, id: &str) -> Result<Vec<u8>, BlobStoreError> {
        let path = self.path(id)?;
        verify_existing(&path, id, None)
    }
}

fn verify_existing(
    path: &Path,
    id: &str,
    expected: Option<&[u8]>,
) -> Result<Vec<u8>, BlobStoreError> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() || meta.len() > MAX_UPLOAD_BYTES as u64 {
        return Err(BlobStoreError::CorruptBlob);
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    File::open(path)?
        .take(MAX_UPLOAD_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_UPLOAD_BYTES
        || !bytes.starts_with(PNG_SIGNATURE)
        || digest_id(&bytes) != id
        || expected.is_some_and(|value| value != bytes.as_slice())
    {
        return Err(BlobStoreError::CorruptBlob);
    }
    Ok(bytes)
}

fn private_directory(path: &Path) -> Result<bool, BlobStoreError> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_dir() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

fn create_private_directory(path: &Path) -> Result<(), BlobStoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(path)?;
    if !private_directory(path)? {
        return Err(BlobStoreError::InvalidPath);
    }
    Ok(())
}

fn private_new_file(path: &Path) -> Result<File, BlobStoreError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

fn sync_directory(path: &Path) -> Result<(), BlobStoreError> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(value: u8) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 512, 512);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer
                .write_image_data(&vec![value; 512 * 512 * 4])
                .unwrap();
        }
        crate::paint_image::normalize_png(&bytes).unwrap()
    }

    #[test]
    fn puts_immutable_normalized_png_and_detects_corruption() {
        let root = std::env::temp_dir().join(format!("ldw-blobs-{}", Uuid::new_v4()));
        create_private_directory(&root).unwrap();
        let store = BlobStore::new(root.clone()).unwrap();
        let png = image(42);
        let id = store.put_normalized(&png).unwrap();
        assert_eq!(id.len(), 64);
        assert_eq!(store.put_normalized(&png).unwrap(), id);
        assert_eq!(store.read(&id).unwrap(), png);
        assert!(matches!(
            store.read("../private"),
            Err(BlobStoreError::InvalidPath)
        ));
        let path = store.path(&id).unwrap();
        fs::write(&path, b"tampered").unwrap();
        assert!(matches!(store.read(&id), Err(BlobStoreError::CorruptBlob)));
        assert!(matches!(
            store.put_normalized(&png),
            Err(BlobStoreError::CorruptBlob)
        ));
        fs::remove_dir_all(root).unwrap();
    }
}
