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

    async def list_root_entries(self, repo: str, ref: str) -> list[str] | None:
        """Noms des entrees a la RACINE du depot sur `ref`, ou `None` si le
        provider ne sait pas repondre.

        Sert au decoupage en sous-taches (`plan_parallel_tasks`) : sans cette
        information, le planificateur decoupe a l'aveugle un depot dont il
        ignore le contenu, et decide de repartir du travail entre plusieurs
        agents alors qu'aucun d'eux n'a de socle commun sur lequel s'appuyer.
        C'est ainsi qu'on obtient deux points d'entree concurrents pour la
        meme application.

        Meme convention que [`changed_file_count`] : non abstraite, et `None`
        (« je ne sais pas ») distinct de `[]` (« le depot est vide »). Les
        confondre ferait passer un provider muet pour un depot vierge, et
        brimerait le decoupage sur des depots parfaitement fournis.
        """
        return None

    async def changed_file_count(self, repo: str, pr_number: int) -> int | None:
        """Nombre de fichiers modifies par une PR, ou `None` si le provider
        ne sait pas repondre.

        Volontairement NON abstraite, et `None` plutot que `0` par defaut :
        une PR vide et une PR dont on ignore le contenu sont deux choses
        differentes, et faire passer la seconde pour la premiere declencherait
        de fausses alertes. `OpenPullRequest` en fait desormais un ECHEC DUR
        (`RuntimeError`), pas un simple avertissement (2026-09-02) : une PR a
        0 fichier signifie que la sous-tache n'a rien produit — voir
        `docs/architecture/pieges.md` pour l'historique des causes qui ont
        produit ce symptome sans que rien ne le signale."""
        return None

    def git_push_credential(self) -> tuple[str, str] | None:
        """`(username, password)` a deposer dans le Workshop pour que
        l'agent delegue puisse authentifier son propre `git push` vers ce
        depot — ou `None` si le provider ne peut/veut pas en fournir un.

        Volontairement NON abstraite, meme convention que
        [`changed_file_count`]/[`list_root_entries`] : un provider qui ne
        sait pas repondre degrade la fonctionnalite (l'agent devra alors
        s'authentifier lui-meme, ou le push echouera avec un message clair),
        il ne casse pas le reste du graphe.

        Ce provider detient DEJA le jeton qui lui sert a ouvrir des PR/creer
        des branches — c'est le MEME jeton qui donne acces en ecriture au
        depot, exactement ce dont l'agent a besoin pour son propre `push`.
        Sans cette methode, RIEN ne relie ce jeton, deja present cote
        pm-engine, au Workshop ou tourne l'agent : `create_workshop` n'a
        aucun parametre de credential, et le seul mecanisme d'ecriture cote
        `atelier-api-server` (`crates/api-server/src/credentials.rs`) est
        concu pour etre pilote par un humain depuis le dashboard, jamais
        appele par pm-engine. Un `git push` depuis le guest echouait alors
        avec `fatal: could not read Username ... No such device or address`
        (constate en Workshop reel le 2026-09-02) — pas un bug d'infra a
        proprement parler, plutot une piece manquante entre deux mecanismes
        qui existaient deja chacun de leur cote."""
        return None
