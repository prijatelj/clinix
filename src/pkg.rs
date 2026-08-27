//! The package specification and helper utilities.
use create::error::Result;

/// A package with an optional pinned version, parsed from `name[=version]`.
#[derive(Debug, Clone)]
pub struct Pkg {
    pub name: String,
    pub version: Option<String>,
}

impl std::str::FromStr for Pkg {
    type Err = crate::error::ClinixError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(crate::error::ClinixError::InvalidPackage(s.to_string()));
        }
        match s.split_once('=') {
            Some((name, ver)) if !name.is_empty() && !ver.is_empty() => Ok(Pkg {
                name: name.to_string(),
                version: Some(ver.to_string()),
            }),
            Some(_) => Err(crate::error::ClinixError::InvalidPackage(s.to_string())),
            None => Ok(Pkg {
                name: s.to_string(),
                version: None,
            }),
        }
    }
}
