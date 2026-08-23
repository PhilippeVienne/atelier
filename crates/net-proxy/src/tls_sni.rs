//! Extraction du SNI (Server Name Indication) d'un `ClientHello` TLS, sans
//! jamais dechiffrer quoi que ce soit : le SNI est un champ en clair du tout
//! premier message du handshake TLS (RFC 6066 §3), justement concu pour
//! permettre a un routeur/reverse-proxy de savoir vers quel service router
//! une connexion HTTPS sans en voir le contenu — meme principe que
//! `ssl_preread` (nginx) ou `req.ssl_sni` (HAProxy). Utilise par le port TLS
//! transparent de `net-proxy` (redirection iptables sur le port 443, voir
//! `crate::proxy::handle_transparent_tls_connection`) : la microVM ne sait
//! pas qu'elle parle a un proxy, donc il n'y a pas de `CONNECT host:port` a
//! lire — le seul moyen de connaitre la destination visee est de lire ce
//! champ avant de relayer les octets tels quels vers la vraie destination.

/// Tente d'extraire le nom d'hote SNI d'un buffer contenant (au moins) le
/// debut d'un `ClientHello` TLS. Retourne `None` si le buffer n'est pas un
/// `ClientHello` reconnaissable, s'il ne contient pas d'extension SNI, ou
/// s'il est trop court pour conclure — utiliser [`is_incomplete`] pour
/// distinguer "pas encore assez de donnees" de "definitivement invalide".
pub fn parse_sni(buf: &[u8]) -> Option<String> {
    // En-tete d'enregistrement TLS : type (1, 0x16 = Handshake) + version
    // (2) + longueur (2).
    if buf.len() < 5 || buf[0] != 0x16 {
        return None;
    }
    let record_len = u16::from_be_bytes([buf[3], buf[4]]) as usize;
    let record_end = 5 + record_len;
    if buf.len() < record_end {
        return None;
    }
    let body = &buf[5..record_end];

    // En-tete de handshake : type (1, doit etre 0x01 = ClientHello) +
    // longueur u24 (3, gros-boutiste).
    if body.len() < 4 || body[0] != 0x01 {
        return None;
    }
    let hs_len = u32::from_be_bytes([0, body[1], body[2], body[3]]) as usize;
    if body.len() < 4 + hs_len {
        return None;
    }
    let hs = &body[4..4 + hs_len];

    // client_version (2) + random (32).
    let mut pos = 34usize;
    if hs.len() < pos + 1 {
        return None;
    }
    let session_id_len = hs[pos] as usize;
    pos += 1 + session_id_len;

    if hs.len() < pos + 2 {
        return None;
    }
    let cipher_suites_len = u16::from_be_bytes([hs[pos], hs[pos + 1]]) as usize;
    pos += 2 + cipher_suites_len;

    if hs.len() < pos + 1 {
        return None;
    }
    let compression_len = hs[pos] as usize;
    pos += 1 + compression_len;

    if hs.len() < pos + 2 {
        // Pas d'extensions du tout (ClientHello minimal, tres ancien) :
        // aucun SNI exploitable.
        return None;
    }
    let extensions_len = u16::from_be_bytes([hs[pos], hs[pos + 1]]) as usize;
    pos += 2;
    let extensions = hs.get(pos..pos + extensions_len)?;

    let mut epos = 0usize;
    while epos + 4 <= extensions.len() {
        let ext_type = u16::from_be_bytes([extensions[epos], extensions[epos + 1]]);
        let ext_len = u16::from_be_bytes([extensions[epos + 2], extensions[epos + 3]]) as usize;
        epos += 4;
        let ext_data = extensions.get(epos..epos + ext_len)?;
        if ext_type == 0x0000 {
            return parse_server_name_extension(ext_data);
        }
        epos += ext_len;
    }
    None
}

/// Format de l'extension `server_name` (RFC 6066 §3) : une liste (u16
/// longueur totale) d'entrees `(type: u8, longueur: u16, nom)` — un seul
/// nom de type `host_name` (0) en pratique, mais la boucle reste generale.
fn parse_server_name_extension(data: &[u8]) -> Option<String> {
    let list_len = u16::from_be_bytes([*data.first()?, *data.get(1)?]) as usize;
    let list = data.get(2..2 + list_len)?;

    let mut pos = 0usize;
    while pos + 3 <= list.len() {
        let name_type = list[pos];
        let name_len = u16::from_be_bytes([list[pos + 1], list[pos + 2]]) as usize;
        pos += 3;
        let name = list.get(pos..pos + name_len)?;
        pos += name_len;
        if name_type == 0 {
            return std::str::from_utf8(name).ok().map(str::to_string);
        }
    }
    None
}

/// Distingue "le `ClientHello` n'est pas encore entierement arrive" (le
/// buffer, obtenu via `TcpStream::peek`, doit etre relu plus tard une fois
/// que davantage d'octets seront disponibles) de "ce n'est definitivement
/// pas un `ClientHello` TLS exploitable" (inutile de reessayer). Se base
/// uniquement sur la longueur d'enregistrement annoncee dans les 5 premiers
/// octets, disponible independamment du reste du parsing.
pub fn is_incomplete(buf: &[u8]) -> bool {
    if buf.len() < 5 {
        return true;
    }
    if buf[0] != 0x16 {
        return false;
    }
    let record_len = u16::from_be_bytes([buf[3], buf[4]]) as usize;
    buf.len() < 5 + record_len
}

/// Construit un `ClientHello` minimal mais structurellement valide, portant
/// un SNI donne — plus maintenable qu'une capture binaire figee, tout en
/// exercant le meme format de bytes qu'un vrai client TLS (curl, git,
/// envbuilder...). `pub(crate)` (pas seulement prive a ce module) pour etre
/// reutilisable par les tests d'integration de `crate::proxy`.
#[cfg(test)]
pub(crate) fn build_client_hello(sni: &str) -> Vec<u8> {
    let mut server_name_entry = vec![0u8]; // name_type = host_name
    server_name_entry.extend((sni.len() as u16).to_be_bytes());
    server_name_entry.extend(sni.as_bytes());

    let mut server_name_list = (server_name_entry.len() as u16).to_be_bytes().to_vec();
    server_name_list.extend(server_name_entry);

    let mut sni_extension = vec![0x00, 0x00]; // extension type = server_name
    sni_extension.extend((server_name_list.len() as u16).to_be_bytes());
    sni_extension.extend(server_name_list);

    let extensions = sni_extension;

    let mut hs_body = Vec::new();
    hs_body.extend([0x03, 0x03]); // client_version (TLS 1.2 "legacy")
    hs_body.extend([0u8; 32]); // random
    hs_body.push(0); // session_id vide
    hs_body.extend((2u16).to_be_bytes()); // cipher_suites: 1 entree
    hs_body.extend([0x00, 0x2f]);
    hs_body.push(1); // compression_methods: 1 entree
    hs_body.push(0);
    hs_body.extend((extensions.len() as u16).to_be_bytes());
    hs_body.extend(extensions);

    let mut handshake = vec![0x01]; // ClientHello
    let hs_len = (hs_body.len() as u32).to_be_bytes();
    handshake.extend(&hs_len[1..]); // u24
    handshake.extend(hs_body);

    let mut record = vec![0x16, 0x03, 0x01]; // Handshake, "version" du record
    record.extend((handshake.len() as u16).to_be_bytes());
    record.extend(handshake);
    record
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_sni_from_a_well_formed_client_hello() {
        let hello = build_client_hello("example.com");
        assert_eq!(parse_sni(&hello).as_deref(), Some("example.com"));
    }

    #[test]
    fn rejects_non_tls_traffic() {
        assert_eq!(parse_sni(b"GET / HTTP/1.1\r\n\r\n"), None);
    }

    #[test]
    fn reports_truncated_record_as_incomplete_not_invalid() {
        let hello = build_client_hello("example.com");
        let truncated = &hello[..hello.len() - 5];
        assert_eq!(parse_sni(truncated), None);
        assert!(is_incomplete(truncated));
    }

    #[test]
    fn a_full_but_unparseable_record_is_not_incomplete() {
        // Enregistrement complet (longueur annoncee correcte) mais dont le
        // contenu n'a pas de sens comme ClientHello : ne doit jamais
        // declencher une nouvelle tentative de lecture.
        let mut garbage = vec![0x16, 0x03, 0x01, 0x00, 0x02];
        garbage.extend([0xffu8, 0xff]);
        assert_eq!(parse_sni(&garbage), None);
        assert!(!is_incomplete(&garbage));
    }

    #[test]
    fn plain_http_is_not_incomplete_tls() {
        assert!(!is_incomplete(b"GET / HTTP/1.1\r\n\r\n"));
    }
}
