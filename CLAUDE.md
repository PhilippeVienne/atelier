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

## Règles Clés de Développement & Suivi Multi-Agents

1. **Feuille de Route & Spécifications** : Avant d'entamer une tâche, consultez [`docs/specs/PLAN-ACTION-GLOBAL.md`](docs/specs/PLAN-ACTION-GLOBAL.md) pour identifier la prochaine tâche `[ ]` et lisez la spécification technique correspondante dans `docs/specs/` (voir cartographie dans `AGENTS.md`).
2. **Verrouillage Nominatif de Tâche (`[-/<agent_family>/<session_id>]`)** :
   - Dès que vous commencez à travailler sur une tâche, **marquez-la immédiatement `[-/claude-code/<session_id>]` dans `PLAN-ACTION-GLOBAL.md`**.
   - Vérifiez systématiquement qu'aucune tâche antérieure n'est restée en cours (`[-/...]`).
3. **Validation & Documentation (`[x]`)** :
   - Dès qu'une tâche est validée par des tests réels (`cargo test`, `cargo clippy`), remplacez le marqueur par `[x]` dans `PLAN-ACTION-GLOBAL.md`.
   - Consignez immédiatement une entrée datée avec sa preuve empirique dans [`docs/PROGRESS.md`](docs/PROGRESS.md).
4. **Architecture** : Ne déplacez pas la logique métier hors de sa crate dédiée.
5. **Qualité & Sécurité** : 0 `unsafe` en production, pas de `.unwrap()` dans le code opérationnel.
6. **Commits Git** : Ne JAMAIS inclure la ligne `Co-authored-by: Claude` dans les messages de commit git.
