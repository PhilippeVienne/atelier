# Directives Claude Code pour Atelier

Ce fichier contient les consignes de développement spécifiques pour **Claude Code**.

Voir également [`AGENTS.md`](AGENTS.md) pour les règles générales de développement et de collaboration multi-agents.

## Commandes Principales

```bash
# Vérifier la compilation et le lint Rust
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check

# Exécuter l'ensemble des tests
cargo test --workspace

# Développement Dashboard
cd dashboard && npm run dev
cd dashboard && npm run build
```

## Règles Clés de Développement

1. **Architecture** : Ne déplacez pas la logique métier hors de sa crate dédiée (ex: la logique Firecracker dans `crates/firecracker`, le routage réseau dans `crates/net-proxy`).
2. **Qualité & Sécurité** : 0 `unsafe` en production, pas de `.unwrap()` dans le code opérationnel.
3. **Multi-agents** : Exécutez `git status` avant de commiter pour préserver les modifications apportées par d'autres agents travaillant en parallèle.
