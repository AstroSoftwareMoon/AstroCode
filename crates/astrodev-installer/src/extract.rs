//! Extracción de .zip y .tar.gz.

use std::path::{Path, PathBuf};

use crate::{InstallError, Result};

pub fn extract(archive: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let name = archive.to_string_lossy().to_lowercase();
    if name.ends_with(".zip") {
        let f = std::fs::File::open(archive)?;
        let mut z = zip::ZipArchive::new(f).map_err(|e| InstallError::Other(format!("zip inválido: {e}")))?;
        z.extract(dest).map_err(|e| InstallError::Other(format!("error al descomprimir: {e}")))?;
    } else if name.ends_with(".tgz") || name.ends_with(".tar.gz") {
        let f = std::fs::File::open(archive)?;
        let mut t = tar::Archive::new(flate2::read::GzDecoder::new(f));
        t.set_preserve_permissions(true);
        t.unpack(dest)?;
    } else {
        return Err(InstallError::Other(format!("formato no soportado: {}", archive.display())));
    }
    Ok(())
}

/// Busca un archivo por nombre recursivamente.
pub fn find_file(root: &Path, file_name: &str) -> Option<PathBuf> {
    let rd = std::fs::read_dir(root).ok()?;
    let mut dirs = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            dirs.push(p);
        } else if p.file_name().is_some_and(|n| n == file_name) {
            return Some(p);
        }
    }
    dirs.into_iter().find_map(|d| find_file(&d, file_name))
}

#[cfg(unix)]
pub fn make_executable(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(p)?.permissions();
    perm.set_mode(perm.mode() | 0o755);
    std::fs::set_permissions(p, perm)?;
    Ok(())
}

#[cfg(not(unix))]
pub fn make_executable(_p: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn zip_and_tgz_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        // zip
        let zp = d.path().join("a.zip");
        {
            let mut w = zip::ZipWriter::new(std::fs::File::create(&zp).unwrap());
            w.start_file::<_, ()>("build/bin/llama-server", Default::default()).unwrap();
            w.write_all(b"bin").unwrap();
            w.finish().unwrap();
        }
        extract(&zp, &d.path().join("z")).unwrap();
        assert!(find_file(&d.path().join("z"), "llama-server").is_some());
        // tgz
        let tp = d.path().join("b.tgz");
        {
            let enc = flate2::write::GzEncoder::new(std::fs::File::create(&tp).unwrap(), flate2::Compression::fast());
            let mut b = tar::Builder::new(enc);
            let mut h = tar::Header::new_gnu();
            h.set_size(3);
            h.set_mode(0o755);
            h.set_cksum();
            b.append_data(&mut h, "bin/ollama", &b"abc"[..]).unwrap();
            b.into_inner().unwrap().finish().unwrap();
        }
        extract(&tp, &d.path().join("t")).unwrap();
        assert_eq!(std::fs::read(d.path().join("t/bin/ollama")).unwrap(), b"abc");
        assert!(extract(&d.path().join("x.rar"), d.path()).is_err());
    }
}
