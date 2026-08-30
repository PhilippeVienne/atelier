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

    async def merge_pr(self, repo: str, pr_number: int) -> None:
        response = await self._client.post(
            f"/repos/{repo}/pulls/{pr_number}/merge",
            json={"Do": "merge"},
        )
        response.raise_for_status()
