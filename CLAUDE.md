# Directives Claude Code pour Atelier

Ce fichier contient les consignes de développement spécifiques pour **Claude Code**.

Voir également [`AGENTS.md`](AGENTS.md) pour les règles générales de développement, le plan d'action global et la matrice de lecture conditionnelle des spécifications techniques.

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

## Règles Clés de Développement & Suivi

1. **Feuille de Route & Spécifications** : Avant d'entamer une tâche, consultez [`docs/specs/PLAN-ACTION-GLOBAL.md`](docs/specs/PLAN-ACTION-GLOBAL.md) pour identifier la prochaine tâche `[ ]` et lisez la spécification technique correspondante dans `docs/specs/` (voir cartographie dans `AGENTS.md`).
2. **Architecture** : Ne déplacez pas la logique métier hors de sa crate dédiée (ex: la logique Firecracker dans `crates/firecracker`, le routage réseau dans `crates/net-proxy`, le MCP externe dans `crates/api-server`).
3. **Qualité & Sécurité** : 0 `unsafe` en production, pas de `.unwrap()` dans le code opérationnel.
4. **Commits Git** : Ne JAMAIS inclure la ligne `Co-authored-by: Claude` dans les messages de commit git.
5. **Multi-agents & Traçabilité** : Dès qu'une tâche est validée par des tests réels, cochez la case `[x]` dans `PLAN-ACTION-GLOBAL.md` et consignez une entrée datée avec sa preuve empirique dans [`docs/PROGRESS.md`](docs/PROGRESS.md).
