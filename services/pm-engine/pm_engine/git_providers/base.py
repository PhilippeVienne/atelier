"""Interface generique multi-forges (Jalon M5, tache 5.4.1).

Conforme au principe de substituabilite de
`docs/specs/00-architecture-principles-substitutability.md` ("Forge Git
[...] entierement substituable") : le graphe LangGraph (taches 5.2.x) ne
doit jamais dependre directement de Forgejo/GitHub/GitLab, seulement de
cette interface. Les trois implementations concretes (`crate::forgejo`,
`crate::github`, `crate::gitlab` — tache 5.4.2) exposent exactement les
memes cinq operations, avec les memes types de retour (`Issue`,
`PullRequest`), quelle que soit la forge sous-jacente.
"""

from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass


@dataclass(frozen=True, slots=True)
class Issue:
    """Ticket source d'un cycle DevFactory (`AnalyzeIssue`, voir
    `docs/specs/05-devfactory-pm-engine.md`, section 8.2 du plan)."""

    number: int
    title: str
    body: str
    author: str
    url: str


@dataclass(frozen=True, slots=True)
class PullRequest:
    """Pull/Merge Request ouverte par le PM (`OpenPullRequest`)."""

    number: int
    url: str
    head_branch: str
    base_branch: str
    state: str


class BaseGitProvider(ABC):
    """Repo identifie de facon uniforme par `"owner/repo"` (une seule
    chaine, meme convention Forgejo/GitHub ; `GitLabProvider` l'encode lui
    meme en identifiant de projet GitLab, voir `gitlab.py`)."""

    @abstractmethod
    async def get_issue(self, repo: str, issue_number: int) -> Issue:
        """Lit un ticket (`AnalyzeIssue`)."""

    @abstractmethod
    async def post_comment(self, repo: str, issue_number: int, body: str) -> None:
        """Poste un commentaire sur un ticket ou une PR (rapport de
        progression, `SuspendWhileWaitingReview`)."""

    @abstractmethod
    async def create_branch(self, repo: str, branch_name: str, base_branch: str) -> None:
        """Cree une branche de travail (`ProvisionWorkshop`, une branche par
        sous-tache paralleliste, ex: `feature/task-<id>`)."""

    @abstractmethod
    async def create_pr(
        self,
        repo: str,
        title: str,
        body: str,
        head_branch: str,
        base_branch: str,
    ) -> PullRequest:
        """Ouvre une Pull/Merge Request (`OpenPullRequest`, signee par
        `atelier-pm-bot` — l'identite du jeton fourni au provider, pas un
        parametre de cette methode)."""

    @abstractmethod
    async def merge_pr(self, repo: str, pr_number: int) -> None:
        """Fusionne une Pull/Merge Request deja approuvee
        (`MergeAndClose`, apres `AwaitHitlApproval`)."""

    async def changed_file_count(self, repo: str, pr_number: int) -> int | None:
        """Nombre de fichiers modifies par une PR, ou `None` si le provider
        ne sait pas repondre.

        Volontairement NON abstraite, et `None` plutot que `0` par defaut :
        une PR vide et une PR dont on ignore le contenu sont deux choses
        differentes, et faire passer la seconde pour la premiere declencherait
        de fausses alertes. `OpenPullRequest` s'en sert uniquement pour
        avertir — une PR a 0 fichier est presque toujours le signe que le
        travail de l'agent n'a pas atteint la branche. Ce symptome a ete
        produit par trois causes distinctes sans que rien ne le signale (voir
        `docs/architecture/pieges.md`), d'ou ce garde-fou."""
        return None
