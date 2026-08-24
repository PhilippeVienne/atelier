# Politique de Sécurité — Atelier

La sécurité et l'isolation étanche des environnements d'exécution d'agents IA sont au cœur même de la mission d'**Atelier**. Nous prenons très au sérieux la découverte et la résolution de vulnérabilités.

---

## 🛡️ Versions Supportées

| Version | Supportée | Statut |
| :--- | :---: | :--- |
| `0.1.x` (main) | ✅ | Version de développement actif |
| `< 0.1.0` | ❌ | Versions expérimentales obsolètes |

---

## 🚨 Signalement d'une Vulnérabilité (Divulgation Responsable)

Si vous découvrez une vulnérabilité de sécurité dans Atelier (évasion de microVM, fuite de secrets OpenBao, contournement d'allowlist réseau `net-proxy`, usurpation OIDC JWT, etc.) :

> [!IMPORTANT]
> **Ne créez PAS d'Issue GitHub publique pour signaler une faille de sécurité.**

Veuillez utiliser l'un des canaux privés suivants :

1. **GitHub Private Vulnerability Reporting** *(Recommandé)* :
   - Rendez-vous sur l'onglet **Security** du dépôt GitHub : `https://github.com/PhilippeVienne/atelier/security/advisories/new`
   - Remplissez le formulaire de signalement sécurisé.

2. **Email direct au Mainteneur** :
   - Envoyez un email chiffré ou standard à : **philippe@vienne.me**.
   - Indiquez dans l'objet : `[SECURITY] Vulnérabilité Atelier - <Composant>`.
   - Fournissez une description détaillée du vecteur d'attaque, un scénario de reproduction (PoC) et l'impact estimé.

---

## ⏱️ Délais de Prise en Charge & Engagements

- **Accusé de réception** : Sous **48 heures ouvrées**.
- **Évaluation initiale & Confirmation** : Sous **7 jours ouvrés**.
- **Publication du correctif & Avis de sécurité (Advisory)** :
  - Dès qu'un correctif est validé et testé, une nouvelle version sera publiée avec un avis de sécurité officiel et attribution au chercheur (si souhaité).
  - Un délai de divulgation coordonnée de 30 à 90 jours est appliqué pour permettre aux utilisateurs de mettre à jour leurs clusters.

---

## 🏰 Modèle de Sécurité en Profondeur d'Atelier

Pour rappel, l'architecture d'Atelier implémente une défense en profondeur multi-niveaux :
1. **Isolation MicroVM Firecracker** : Virtualisation matérielle KVM dédiée par agent (espace mémoire et kernel Linux distincts de l'hôte).
2. **Conteneurs Kubernetes Non Privilégiés** : Allocation des périphériques `/dev/kvm` et `/dev/net/tun` via le DaemonSet `kvm-device-plugin` sans exiger `securityContext.privileged: true`.
3. **Médiation Egress Stricte (`net-proxy`)** : Confinement réseau avec allowlist de domaines et tunnels transparents HTTP/CONNECT.
4. **Zéro Secret en Clair (`identity-proxy` & `openbao`)** : Les agents ne détiennent aucun jeton permanent ; les en-têtes sont injectés à la volée sur les connexions internes.
