//! Verification d'un hote de destination contre l'allowlist egress d'un
//! Workshop (`WorkshopSpec.egress_allowlist`).
//!
//! Formats d'entree acceptes :
//! - `example.com` : correspondance exacte (insensible a la casse).
//! - `*.example.com` : correspondance sur tout sous-domaine (mais pas
//!   `example.com` lui-meme).
//! - `*` : autorise tout (a n'utiliser qu'en dev).

pub fn is_allowed(host: &str, allowlist: &[String]) -> bool {
    let host = normalize(host);
    allowlist.iter().any(|entry| matches_entry(&host, entry))
}

fn normalize(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

fn matches_entry(host: &str, entry: &str) -> bool {
    let entry = normalize(entry);
    if entry.is_empty() {
        return false;
    }
    if entry == "*" {
        return true;
    }
    if let Some(suffix) = entry.strip_prefix("*.") {
        return host.ends_with(&format!(".{suffix}"));
    }
    host == entry
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowlist(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn exact_match() {
        let list = allowlist(&["github.com"]);
        assert!(is_allowed("github.com", &list));
        assert!(is_allowed("GitHub.com", &list));
        assert!(!is_allowed("api.github.com", &list));
        assert!(!is_allowed("evilgithub.com", &list));
    }

    #[test]
    fn wildcard_subdomain() {
        let list = allowlist(&["*.githubusercontent.com"]);
        assert!(is_allowed("raw.githubusercontent.com", &list));
        assert!(is_allowed("a.b.githubusercontent.com", &list));
        assert!(!is_allowed("githubusercontent.com", &list));
        assert!(!is_allowed("notgithubusercontent.com", &list));
    }

    #[test]
    fn wildcard_star_allows_everything() {
        let list = allowlist(&["*"]);
        assert!(is_allowed("anything.example.org", &list));
    }

    #[test]
    fn empty_allowlist_denies_everything() {
        assert!(!is_allowed("github.com", &[]));
    }

    #[test]
    fn trailing_dot_and_whitespace_are_ignored() {
        let list = allowlist(&[" github.com "]);
        assert!(is_allowed("github.com.", &list));
    }
}
