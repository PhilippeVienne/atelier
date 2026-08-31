//! Le chart Helm embarque une COPIE des migrations sqlx
//! (`charts/atelier/files/migrations/`, empaquetees par
//! `templates/jobs/db-migrate-job.yaml` via `.Files.Glob` — Helm ne sait pas
//! lire hors du repertoire du chart). Une copie manuelle derive, et c'est
//! exactement ce qui s'est produit : le 2026-08-31, le chart n'avait qu'UNE
//! des trois migrations de l'api-server. `mcp_exec_commands` manquait depuis
//! sa creation, si bien qu'une instance deployee par Helm n'aurait pas eu la
//! table `exec_commands` du tout — sans que rien ne le signale, la
//! divergence etant invisible tant qu'on developpe avec `sqlx migrate` en
//! local.
//!
//! Ce test ne verifie pas que les migrations sont bonnes : il verifie
//! qu'elles sont LES MEMES des deux cotes. C'est le seul lien entre le code
//! et le chart, et il n'etait tenu que par la discipline.

use std::collections::BTreeMap;
use std::path::Path;

fn read_sql_dir(dir: &Path) -> BTreeMap<String, String> {
    std::fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("lecture de {}: {err}", dir.display()))
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|e| e == "sql"))
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let content = std::fs::read_to_string(entry.path())
                .unwrap_or_else(|err| panic!("lecture de {}: {err}", entry.path().display()));
            (name, content)
        })
        .collect()
}

#[test]
fn chart_embeds_the_same_migrations_as_the_crates() {
    // `CARGO_MANIFEST_DIR` = crates/api-server ; la racine est deux niveaux
    // au-dessus.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("racine du depot");

    for (source, chart) in [
        ("crates/api-server/migrations", "apiserver"),
        ("crates/controller/migrations", "controller"),
    ] {
        let source_dir = root.join(source);
        let chart_dir = root.join("charts/atelier/files/migrations").join(chart);
        let from_source = read_sql_dir(&source_dir);
        let from_chart = read_sql_dir(&chart_dir);

        let missing: Vec<_> = from_source
            .keys()
            .filter(|name| !from_chart.contains_key(*name))
            .collect();
        assert!(
            missing.is_empty(),
            "migrations absentes du chart ({chart}) : {missing:?}\n\
             Copiez-les : cp {source}/*.sql charts/atelier/files/migrations/{chart}/"
        );

        let extra: Vec<_> = from_chart
            .keys()
            .filter(|name| !from_source.contains_key(*name))
            .collect();
        assert!(
            extra.is_empty(),
            "migrations presentes dans le chart ({chart}) mais absentes des sources : {extra:?}"
        );

        for (name, content) in &from_source {
            assert_eq!(
                from_chart.get(name),
                Some(content),
                "la migration {name} differe entre {source} et le chart ({chart})"
            );
        }
    }
}
