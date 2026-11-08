//! Shared policy for the Workspace Reading Pack: path rules, visibility, denial, name rendering.
//!
//! Every tool applies these rules through this module, so a rule has one implementation. The
//! exclusion and denial lists are documented guards (TS-M1-REVIEW-002, D2): they do not
//! guarantee that no secret inside a root is readable.

use crate::limits;

pub const ERR_MISSING_PATH: &str = "missing path";
pub const ERR_INVALID_PATH: &str = "invalid path";
pub const ERR_NOT_FOUND: &str = "not found";
pub const ERR_NOT_READABLE: &str = "not readable";
pub const ERR_NOT_A_FILE: &str = "not a file";
pub const ERR_NOT_A_DIRECTORY: &str = "not a directory";
pub const ERR_DENIED_PATH: &str = "denied path";
pub const ERR_INVALID_ARGUMENT: &str = "invalid argument";
pub const ERR_UNEXPECTED_ARGUMENT: &str = "unexpected argument";
pub const ERR_ARGUMENTS_TOO_LARGE: &str = "arguments too large";
pub const ERR_BUSY: &str = "busy";
pub const ERR_INTERNAL: &str = "internal error";

/// Directories that traversal never descends into. Listed with the `excluded` marker.
pub const EXCLUDED_DIRS: &[&str] = &[".git", "target", "node_modules", ".venv", "__pycache__"];

/// Denied final-component names: exact matches.
const DENIED_EXACT: &[&str] = &[
    ".env",
    "id_rsa",
    "id_ed25519",
    "id_ecdsa",
    ".netrc",
    ".npmrc",
    ".pypirc",
    "credentials",
];
/// Denied final-component names: `<prefix>` followed by anything (`.env.*`, `id_rsa.*`, ...).
const DENIED_PREFIX: &[&str] = &[
    ".env.",
    "id_rsa.",
    "id_ed25519.",
    "id_ecdsa.",
    "credentials.",
];
/// Denied final-component names: `*<suffix>`.
const DENIED_SUFFIX: &[&str] = &[
    ".pem",
    ".key",
    ".p12",
    ".pfx",
    ".kdbx",
    ".keychain",
    ".keychain-db",
];

/// A component is hidden when its name starts with `.`.
pub fn is_hidden(name: &[u8]) -> bool {
    name.first() == Some(&b'.')
}

pub fn is_excluded_dir(name: &[u8]) -> bool {
    EXCLUDED_DIRS.iter().any(|d| d.as_bytes() == name)
}

pub fn is_denied(name: &[u8]) -> bool {
    DENIED_EXACT.iter().any(|d| d.as_bytes() == name)
        // `<prefix>*`: the wildcard may match the empty string, so the bare prefix is denied too.
        || DENIED_PREFIX.iter().any(|p| name.starts_with(p.as_bytes()))
        || DENIED_SUFFIX.iter().any(|s| name.ends_with(s.as_bytes()))
}

/// Lexical validation of a path argument. Returns its components; the empty string denotes the
/// root and yields no components. No I/O is performed.
pub fn validate_path(path: &str) -> Result<Vec<&str>, &'static str> {
    if path.is_empty() {
        return Ok(Vec::new());
    }
    if path.len() > limits::MAX_PATH_BYTES || path.as_bytes().contains(&0) || path.starts_with('/')
    {
        return Err(ERR_INVALID_PATH);
    }
    let mut comps = Vec::new();
    for comp in path.split('/') {
        if comp.is_empty()
            || comp == "."
            || comp == ".."
            || comp.len() > limits::MAX_COMPONENT_BYTES
        {
            return Err(ERR_INVALID_PATH);
        }
        comps.push(comp);
        if comps.len() > limits::MAX_PATH_COMPONENTS {
            return Err(ERR_INVALID_PATH);
        }
    }
    Ok(comps)
}

/// Generated paths obey the argument limits: a traversal entry beyond them is `too_deep` and is
/// neither listed, descended, nor scanned, because no tool could address it.
pub fn within_path_limits(path_bytes: usize, components: usize) -> bool {
    path_bytes <= limits::MAX_PATH_BYTES && components <= limits::MAX_PATH_COMPONENTS
}

/// Renders a name or path for a text block: bytes below 0x20 and 0x7F become `\xNN`, a backslash
/// becomes `\\`, and invalid UTF-8 is replaced by U+FFFD.
pub fn render_name(bytes: &[u8]) -> String {
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len() + 8);
    for &b in bytes {
        match b {
            b'\\' => out.extend_from_slice(b"\\\\"),
            0..=0x1f | 0x7f => {
                out.extend_from_slice(format!("\\x{b:02x}").as_bytes());
            }
            _ => out.push(b),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Upper bound on the rendered length of a path within the limits: every byte may become five
/// JSON-escaped bytes (`\xNN` rendered, then the backslash escaped).
pub const RENDERED_PATH_ESCAPED_MAX: usize = 5 * limits::MAX_PATH_BYTES;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_validation_rules() {
        assert_eq!(validate_path(""), Ok(vec![]));
        assert_eq!(validate_path("a/b/c.rs"), Ok(vec!["a", "b", "c.rs"]));
        for bad in ["/abs", "a//b", "./a", "a/./b", "../a", "a/..", "a/", "a\0b"] {
            assert_eq!(validate_path(bad), Err(ERR_INVALID_PATH), "{bad:?}");
        }
        let long_component = "x".repeat(256);
        assert_eq!(validate_path(&long_component), Err(ERR_INVALID_PATH));
        let ok_component = "x".repeat(255);
        assert!(validate_path(&ok_component).is_ok());
        let many = vec!["a"; 65].join("/");
        assert_eq!(validate_path(&many), Err(ERR_INVALID_PATH));
        let sixty_four = vec!["a"; 64].join("/");
        assert_eq!(validate_path(&sixty_four).unwrap().len(), 64);
        let long_path = [
            "y".repeat(255),
            "y".repeat(255),
            "y".repeat(255),
            "y".repeat(254),
            "x".to_owned(),
        ]
        .join("/");
        assert_eq!(long_path.len(), 1_024);
        assert!(validate_path(&long_path).is_ok());
        let longer = format!("{long_path}x");
        assert_eq!(validate_path(&longer), Err(ERR_INVALID_PATH));
    }

    #[test]
    fn denial_and_exclusion_lists() {
        for d in [
            ".env",
            ".env.local",
            "server.pem",
            "k.key",
            "a.p12",
            "a.pfx",
            "id_rsa",
            "id_rsa.pub",
            "id_ed25519",
            "id_ed25519.pub",
            "id_ecdsa",
            "id_ecdsa.pub",
            "vault.kdbx",
            ".netrc",
            ".npmrc",
            ".pypirc",
            "credentials",
            "credentials.json",
            "login.keychain",
            "login.keychain-db",
            ".env.",
            "id_rsa.",
            "id_ed25519.",
            "id_ecdsa.",
            "credentials.",
        ] {
            assert!(is_denied(d.as_bytes()), "{d}");
        }
        for ok in [
            "env",
            "environment.md",
            "key.rs",
            "credential",
            "pem.txt",
            "id_rsa_notes",
        ] {
            assert!(!is_denied(ok.as_bytes()), "{ok}");
        }
        for e in EXCLUDED_DIRS {
            assert!(is_excluded_dir(e.as_bytes()));
        }
        assert!(!is_excluded_dir(b"targets"));
        assert!(is_hidden(b".hidden"));
        assert!(!is_hidden(b"visible"));
    }

    #[test]
    fn name_rendering_escapes_controls_and_backslashes() {
        assert_eq!(render_name(b"plain.txt"), "plain.txt");
        assert_eq!(render_name(b"a\x01b\x7fc"), "a\\x01b\\x7fc");
        assert_eq!(render_name(b"back\\slash"), "back\\\\slash");
        assert_eq!(render_name(&[0xff, b'x']), "\u{fffd}x");
        assert_eq!(render_name("été".as_bytes()), "été");
    }
}
