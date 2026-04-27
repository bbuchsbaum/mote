//! Path normalization and overlap rules for the path plane.
//!
//! Reservations are over repo-relative paths. We accept only two forms:
//! exact file (`src/auth/token.rs`) and directory prefix (`src/auth/`).
//! Globs and symlink resolution are deferred.

/// Normalize a repo-relative path. Rejects absolute paths, `..`/`.` components,
/// empty paths, and collapses redundant slashes. Preserves a trailing slash to
/// indicate directory-prefix form.
pub fn normalize(p: &str) -> Result<String, String> {
    if p.is_empty() {
        return Err("empty path".into());
    }
    if p.starts_with('/') {
        return Err(format!("absolute paths are forbidden: `{p}`"));
    }
    let trailing_slash = p.ends_with('/');
    let parts: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
    for c in &parts {
        if *c == ".." {
            return Err(format!("`..` components are forbidden: `{p}`"));
        }
        if *c == "." {
            return Err(format!("`.` components are forbidden: `{p}`"));
        }
    }
    if parts.is_empty() {
        return Err("empty path after normalization".into());
    }
    let mut out = parts.join("/");
    if trailing_slash {
        out.push('/');
    }
    Ok(out)
}

/// Two paths overlap iff one's component vector is a prefix of the other under
/// directory-component semantics. Trailing slashes are informational only and
/// do not affect the comparison.
pub fn overlap(a: &str, b: &str) -> bool {
    let av: Vec<&str> = a.split('/').filter(|s| !s.is_empty()).collect();
    let bv: Vec<&str> = b.split('/').filter(|s| !s.is_empty()).collect();
    let n = av.len().min(bv.len());
    if n == 0 {
        return false;
    }
    av[..n] == bv[..n]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_prd_examples() {
        // From PRD planes.path.overlap_rule
        assert!(overlap("src/auth/", "src/auth/token.rs"));
        assert!(overlap("src/auth/token.rs", "src/auth/token.rs"));
        assert!(overlap("src/auth/", "src/auth/"));
        assert!(!overlap("src/auth/", "src/authn/"));
        assert!(!overlap("src/auth/", "src/authentication/"));
    }

    #[test]
    fn overlap_dir_vs_dir() {
        assert!(overlap("src/", "src/auth/"));
        assert!(overlap("src/auth/", "src/")); // symmetric
    }

    #[test]
    fn overlap_disjoint_files() {
        assert!(!overlap("src/auth/token.rs", "src/auth/login.rs"));
    }

    #[test]
    fn overlap_trailing_slash_irrelevant() {
        assert!(overlap("src/auth", "src/auth/"));
        assert!(overlap("src/auth/", "src/auth"));
    }

    #[test]
    fn normalize_basics() {
        assert_eq!(normalize("src/auth/").unwrap(), "src/auth/");
        assert_eq!(normalize("src/auth/token.rs").unwrap(), "src/auth/token.rs");
    }

    #[test]
    fn normalize_collapses_double_slashes() {
        assert_eq!(normalize("src//auth/").unwrap(), "src/auth/");
    }

    #[test]
    fn normalize_rejects_absolute() {
        assert!(normalize("/etc/passwd").is_err());
    }

    #[test]
    fn normalize_rejects_dotdot() {
        assert!(normalize("src/../etc").is_err());
        assert!(normalize("..").is_err());
    }

    #[test]
    fn normalize_rejects_empty() {
        assert!(normalize("").is_err());
        assert!(normalize("/").is_err());
    }
}
