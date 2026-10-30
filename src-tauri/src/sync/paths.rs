use super::scan_types::ScanError;
use std::path::{Component, Path};

pub fn validate_relative(path: &Path) -> Result<(), ScanError> {
    let raw = path
        .as_os_str()
        .to_str()
        .ok_or_else(|| ScanError::InvalidPath("non-UTF8 path".into()))?;
    if raw.is_empty()
        || path.is_absolute()
        || raw.contains('\0')
        || raw.contains('\\')
        || raw.starts_with('/')
        || raw.starts_with('\\')
        || raw.contains(":")
        || raw.contains("//")
    {
        return Err(ScanError::InvalidPath(raw.into()));
    }
    for c in path.components() {
        let Component::Normal(name) = c else {
            return Err(ScanError::InvalidPath(raw.into()));
        };
        let s = name
            .to_str()
            .ok_or_else(|| ScanError::InvalidPath("non-UTF8 path".into()))?;
        if s == "." || s == ".." || s.ends_with(['.', ' ']) {
            return Err(ScanError::InvalidPath(s.into()));
        }
        let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
        if matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        {
            return Err(ScanError::InvalidPath(s.into()));
        }
    }
    Ok(())
}

pub fn validate_root_protection(root: &Path, protected: &[&Path]) -> Result<(), ScanError> {
    let root = root.canonicalize().map_err(|source| ScanError::Io {
        path: root.into(),
        source,
    })?;
    for p in protected {
        let p = p.canonicalize().map_err(|source| ScanError::Io {
            path: (*p).into(),
            source,
        })?;
        if root.starts_with(&p) || p.starts_with(&root) {
            return Err(ScanError::InvalidPath(
                "scan root overlaps protected path".into(),
            ));
        }
    }
    Ok(())
}
