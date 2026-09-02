"""Implementation Forgejo de `BaseGitProvider` (Jalon M5, tache 5.4.2).

API REST Gitea/Forgejo standard (`/api/v1/...`), authentification par
jeton d'acces personnel (en-tete `Authorization: token <PAT>`) — voir
`deploy/dev/forgejo/README.md` pour la procedure de generation d'un jeton
de test. C'est la forge par defaut de ce depot (`crates/api-server`,
`crate::git_identity` cote atelier), donc l'implementation la plus
testee empiriquement (`tests/git_providers/test_forgejo.py`, contre
l'instance de dev reelle).
"""

from __future__ import annotations

from dataclasses import dataclass
from urllib.parse import urlparse

import httpx

from .base import BaseGitProvider, Issue, PullRequest


@dataclass
class MirrorProject:
    name: str
    full_name: str
    owner: str
    clone_url: str
    original_url: str | None
    private: bool


class ForgejoProvider(BaseGitProvider):
    def __init__(self, base_url: str, token: str) -> None:
        self._client = httpx.AsyncClient(
            base_url=f"{base_url.rstrip('/')}/api/v1",
            headers={"Authorization": f"token {token}"},
            timeout=30.0,
        )
        self._owner: str | None = None
        # Conserve pour `git_push_credential` : le meme jeton qui autorise
        # ce provider a ouvrir des PR/creer des branches donne acces en
        # ecriture au depot, exactement ce dont l'agent delegue a besoin
        # pour son propre `git push` depuis le Workshop.
        self._token = token

    async def aclose(self) -> None:
        await self._client.aclose()

    async def _whoami(self) -> str:
        """Identite Forgejo du jeton `FORGEJO_TOKEN` de ce service — resolue
        dynamiquement (pas de nom d'utilisateur code en dur) : le proprietaire
        des depots crees par `create_mirror` doit correspondre a ce jeton,
        qu'il s'agisse de l'admin de dev (`atelier_admin`) ou d'un futur bot
        dedie (`atelier-pm-bot`), voir cache sur `self._owner` (evite un
        aller-retour reseau supplementaire a chaque import de projet)."""
        if self._owner is None:
            response = await self._client.get("/user")
            response.raise_for_status()
            self._owner = response.json()["login"]
        return self._owner

    @staticmethod
    def _migration_auth(source_url: str, token: str | None) -> dict[str, str]:
        """Meme convention que `dashboard/lib/forgejo.ts::migrationAuth`
        (cote TypeScript) : GitHub attend un jeton nu, GitLab (SaaS ou
        instance privee auto-hebergee, d'ou le simple test de sous-chaine
        plutot qu'un hote fixe) une authentification basique avec `oauth2`
        comme utilisateur — convention documentee de l'authentification
        HTTPS par jeton de GitLab."""
        host = urlparse(source_url).netloc.lower()
        if host == "github.com" or host.endswith(".github.com"):
            return {"service": "github", **({"auth_token": token} if token else {})}
        if "gitlab" in host:
            return {
                "service": "gitlab",
                **({"auth_username": "oauth2", "auth_password": token} if token else {}),
            }
        return {"service": "git", **({"auth_username": "git", "auth_password": token} if token else {})}

    async def create_mirror(
        self, name: str, source_url: str, private: bool, token: str | None = None
    ) -> MirrorProject:
        """Miroir en lecture d'un depot externe (Jalon M5, "Projets" —
        `dashboard/lib/forgejo.ts::createMirrorProject`, meme appel cote
        Dashboard) : ici declenchable depuis le chat PM lui-meme via l'outil
        `setup_mirror_project` (`pm_engine.main`). Le jeton externe transite
        par le message utilisateur puis le fournisseur LLM (contrainte
        assumee du choix "conversationnel" — jamais journalise ni persiste
        par ce module) et n'est utilise qu'une fois ici : Forgejo le
        conserve lui-meme pour la resynchronisation periodique du miroir."""
        owner = await self._whoami()
        response = await self._client.post(
            "/repos/migrate",
            json={
                "clone_addr": source_url,
                "repo_name": name,
                "repo_owner": owner,
                "mirror": True,
                "private": private,
                "mirror_interval": "10m0s",
                **self._migration_auth(source_url, token),
            },
        )
        response.raise_for_status()
        data = response.json()
        return MirrorProject(
            name=data["name"],
            full_name=data["full_name"],
            owner=data["owner"]["login"],
            clone_url=data["clone_url"],
            original_url=data.get("original_url"),
            private=data["private"],
        )

    async def get_issue(self, repo: str, issue_number: int) -> Issue:
        response = await self._client.get(f"/repos/{repo}/issues/{issue_number}")
        response.raise_for_status()
        data = response.json()
        return Issue(
            number=data["number"],
            title=data["title"],
            body=data.get("body") or "",
            author=data["user"]["login"],
            url=data["html_url"],
        )

    async def post_comment(self, repo: str, issue_number: int, body: str) -> None:
        response = await self._client.post(
            f"/repos/{repo}/issues/{issue_number}/comments",
            json={"body": body},
        )
        response.raise_for_status()

    async def create_branch(self, repo: str, branch_name: str, base_branch: str) -> None:
        response = await self._client.post(
            f"/repos/{repo}/branches",
            json={"new_branch_name": branch_name, "old_branch_name": base_branch},
        )
        response.raise_for_status()

    async def create_pr(
        self,
        repo: str,
        title: str,
        body: str,
        head_branch: str,
        base_branch: str,
    ) -> PullRequest:
        response = await self._client.post(
            f"/repos/{repo}/pulls",
            json={
                "title": title,
                "body": body,
                "head": head_branch,
                "base": base_branch,
            },
        )
        response.raise_for_status()
        data = response.json()
        return PullRequest(
            number=data["number"],
            url=data["html_url"],
            head_branch=head_branch,
            base_branch=base_branch,
            state=data["state"],
        )

    async def list_root_entries(self, repo: str, ref: str) -> list[str] | None:
        response = await self._client.get(f"/repos/{repo}/contents", params={"ref": ref})
        # 404 sur un depot sans commit initial : c'est un depot VIDE, pas une
        # panne — repondre `[]` et non `None`, la distinction compte (voir
        # `BaseGitProvider.list_root_entries`).
        if response.status_code == 404:
            return []
        response.raise_for_status()
        return [entry["name"] for entry in response.json()]

    async def changed_file_count(self, repo: str, pr_number: int) -> int | None:
        response = await self._client.get(f"/repos/{repo}/pulls/{pr_number}/files")
        response.raise_for_status()
        return len(response.json())

    async def merge_pr(self, repo: str, pr_number: int) -> None:
        response = await self._client.post(
            f"/repos/{repo}/pulls/{pr_number}/merge",
            json={"Do": "merge"},
        )
        response.raise_for_status()

    async def get_diff(self, repo: str, base_branch: str, head_branch: str) -> str | None:
        # `.../compare/{base}...{head}.diff` renvoie 404 sur cette version de
        # Forgejo (9.0.3+gitea-1.22.0, verifie contre l'instance de dev
        # reelle le 2026-09-02) — seul le JSON de `/compare/{base}...{head}`
        # est disponible (liste de commits/fichiers, jamais le texte du
        # diff, quel que soit l'en-tete Accept). En revanche
        # `/git/commits/{sha}.diff` fonctionne (200, text/plain, diff
        # unifie). On recupere donc les commits de la comparaison puis on
        # concatene leur diff individuel, dans l'ordre.
        response = await self._client.get(f"/repos/{repo}/compare/{base_branch}...{head_branch}")
        if response.status_code == 404:
            return None
        response.raise_for_status()
        commits = response.json().get("commits") or []
        diffs: list[str] = []
        for commit in commits:
            sha = commit["sha"]
            diff_response = await self._client.get(
                f"/repos/{repo}/git/commits/{sha}.diff",
                headers={"Accept": "text/plain"},
            )
            diff_response.raise_for_status()
            diffs.append(diff_response.text)
        return "\n".join(diffs)

    def git_push_credential(self) -> tuple[str, str] | None:
        # Convention Forgejo/Gitea (identique a GitHub) : un jeton d'acces
        # personnel s'utilise comme mot de passe HTTP Basic, avec n'importe
        # quel nom d'utilisateur non vide — `x-access-token` est celui deja
        # utilise comme defaut cote `crates/image-builder` (voir
        # `resolve_git_credentials`), reutilise ici pour la coherence.
        return ("x-access-token", self._token)
