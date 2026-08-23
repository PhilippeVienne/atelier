# Directives pour les Agents IA de Code (Claude Code, Gemini CLI, Antigravity)

Ce document régit les règles de développement et de collaboration applicables à tous les agents IA de code (Claude Code, Gemini CLI, Antigravity, Cursor, etc.) travaillant sur le dépôt **Atelier**.

---

## 🎯 Principes Fondamentaux

1. **Vérification Empirique Obligatoire** :
   - Ne déclarez **JAMAIS** une tâche terminée sans avoir exécuté et vérifié les commandes de compilation et de test (`cargo test --workspace` et `cargo clippy`).
   - L'édition d'un fichier ne constitue pas une tâche accomplie.

2. **Éthos du Projet : Tests Réels sans Mocks** :
   - Atelier s'appuie sur des tests d'intégration réels contre un cluster `kind` local ou de vraies microVMs Firecracker.
   - Ne remplacez pas les échecs de test par des mocks factices ou des try/catch silencieux.

3. **Collaboration Multi-Agents Concurrente** :
   - Plusieurs agents peuvent travailler simultanément sur le dépôt.
   - Inspectez systématiquement `git status` et `git diff` avant toute modification ou commit pour ne pas écraser les contributions d'un autre agent.

4. **Acceptation du CLA** :
   - Toute contribution produite par ou avec l'assistance d'un agent IA et soumise au dépôt est régie par les termes du [Contributor License Agreement (`cla.md`)](cla.md).
