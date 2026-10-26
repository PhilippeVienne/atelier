//! Verification d'un hote de destination contre l'allowlist egress d'un
//! Workshop (`WorkshopSpec.egress_allowlist`).
//!
//! Formats d'entree acceptes :
//! - `example.com` : correspondance exacte (insensible a la casse).
//! - `*.example.com` : correspondance sur tout sous-domaine (mais pas
//!   `example.com` lui-meme).
//! - `*` : autorise tout (a n'utiliser qu'en dev).

/// Nouvelle liste apres relecture du fichier d'egress (tache 14.8) : les hotes
/// du fichier, plus ceux qu'un ajout a chaud (`request_egress`) a mis dans la
/// liste courante sans qu'ils figurent dans l'ancien fichier. Un hote retire
/// du fichier disparait ; un ajout a chaud survit a la relecture.
pub fn merge_reload(
    current: &[String],
    previous_file: &[String],
    new_file: &[String],
) -> Vec<String> {
    let mut merged: Vec<String> = new_file.to_vec();
    for entry in current {
        let from_file = previous_file.iter().any(|f| f.eq_ignore_ascii_case(entry));
        let already = merged.iter().any(|m| m.eq_ignore_ascii_case(entry));
        if !from_file && !already {
            merged.push(entry.clone());
        }
    }
    merged
}

/// Hotes d'un fichier d'egress : un par ligne, lignes vides ignorees.
pub fn parse_file(content: &str) -> Vec<String> {
    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

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

    #[test]
    fn a_reload_follows_the_file_and_keeps_hot_additions() {
        let v = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // `b.com` retire du fichier, `c.com` ajoute au fichier, `hot.com`
        // ajoute a chaud entre-temps.
        let merged = merge_reload(
            &v(&["a.com", "b.com", "hot.com"]),
            &v(&["a.com", "b.com"]),
            &v(&["a.com", "c.com"]),
        );
        assert_eq!(merged, v(&["a.com", "c.com", "hot.com"]));
        // Un fichier vide coupe tout, sauf les ajouts a chaud.
        assert_eq!(
            merge_reload(&v(&["a.com"]), &v(&["a.com"]), &[]),
            Vec::<String>::new()
        );
        assert_eq!(
            parse_file("a.com\n\n  *.b.com \n"),
            v(&["a.com", "*.b.com"])
        );
    }

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
