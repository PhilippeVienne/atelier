# Spécification Technique : Passerelle d'Inférence IA (LiteLLM Proxy)

> **Statut** : Validé suite aux sessions d'itération et stress-test d'architecture (Grill-Me)  
> **Date** : 2026-08-23  
> **Auteur** : Équipe Atelier  
> **Contexte** : Centralisation, routage, observabilité, budgets stricts et cycle de vie des Virtual Keys à TTL court (Workshops, Builds & Reprise post-snapshot).

---

## 1. Objectifs & Rôle de LiteLLM Proxy

LiteLLM Proxy sert de point d'entrée unique et sécurisé pour tous les appels LLM émis par les agents IA :

1. **Isolation, Budgets Stricts & Clés Éphémères à TTL Court** :
   - Le `controller` Atelier appelle l'API de LiteLLM (`POST /key/generate`) lors du provisioning d'un Workshop pour créer une **Virtual Key dédiée**.
   - **Gestion du Cycle Suspend / Resume (Mise en Veille Prolongée)** :
     Pour éviter les risques de clés compromises ou de fuites lors de suspensions de longue durée (ex: mise en veille de plusieurs semaines), les Virtual Keys sont créées avec un **TTL court (1-2 heures)**. Lors de la reprise (`resume_workshop`), le `controller` génère automatiquement à chaud une nouvelle Virtual Key dérivée ré-injectée dans le Pod.
   - **Gestion des Builds (`image-builder`)** : Virtual Key temporaire dédiée révoquée à la fin du Job.
   - Dès que le budget global alloué dans `WorkshopSpec.resources.maxLlmBudgetUsd` est atteint, LiteLLM bloque les requêtes de l'agent (HTTP 429 / 403 Budget Exceeded).
2. **Cycle de Vie & Nettoyage Automatique** :
   - Lors de la suppression d'un Workshop ou de la fin d'un Job de build, le `controller` Atelier invoque `POST /key/delete` dans le cadre de l'exécution du finalizer `atelier.dev/cleanup`.
3. **Sécurité et Masquage des Clés Fournisseurs** :
   - Les clés d'API commerciales restent masquées dans LiteLLM et OpenBao.
