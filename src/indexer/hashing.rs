use std::{fs, path::Path};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

pub fn hash_file(path: &Path) -> Result<(String, Vec<u8>)> {
    let bytes = fs::read(path).with_context(|| format!("file read failed: {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok((format!("{:x}", hasher.finalize()), bytes))
}
