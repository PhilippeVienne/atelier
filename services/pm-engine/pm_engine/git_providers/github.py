"""Implementation GitHub de `BaseGitProvider` (Jalon M5, tache 5.4.2).

API REST v3 standard (`https://api.github.com`, ou une instance GitHub
Enterprise via `base_url`), authentification par jeton (en-tete
`Authorization: Bearer <token>`). Contrairement a Forgejo/GitLab, l'API
GitHub n'offre pas d'endpoint "creer une branche" direct : une branche est
une simple reference Git (`refs/heads/<nom>`), creee en deux temps (lire le
SHA de la branche de base, puis creer la reference) — voir
`create_branch`.
"""

from __future__ import annotations

import httpx

from .base import BaseGitProvider, Issue, PullRequest

DEFAULT_BASE_URL = "https://api.github.com"


class GitHubProvider(BaseGitProvider):
    def __init__(self, token: str, base_url: str = DEFAULT_BASE_URL) -> None:
        headers = {
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
        }
        # Un en-tete `Authorization: Bearer` VIDE est explicitement refuse
        # par l'API GitHub (401), contrairement a son absence totale
        # (autorisee pour les lectures sur des depots publics, avec des
        # limites de debit plus basses) — verifie empiriquement. `token`
        # vide n'est donc pas equivalent a l'omettre.
        if token:
            headers["Authorization"] = f"Bearer {token}"
        self._client = httpx.AsyncClient(
            base_url=base_url.rstrip("/"),
            headers=headers,
            timeout=30.0,
        )
        self._token = token

    async def aclose(self) -> None:
        await self._client.aclose()

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
        # L'API GitHub ne cree pas de branche directement : une branche EST
        # une reference Git (`refs/heads/<nom>`) pointant sur un SHA de
        # commit — il faut donc d'abord lire le SHA courant de la branche de
        # base avant de creer la nouvelle reference sur ce meme SHA.
        base_ref = await self._client.get(f"/repos/{repo}/git/ref/heads/{base_branch}")
        base_ref.raise_for_status()
        base_sha = base_ref.json()["object"]["sha"]

        response = await self._client.post(
            f"/repos/{repo}/git/refs",
            json={"ref": f"refs/heads/{branch_name}", "sha": base_sha},
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
        response = await self._client.get(f"/repos/{repo}/contents/", params={"ref": ref})
        if response.status_code == 404:
            return []
        response.raise_for_status()
        return [entry["name"] for entry in response.json()]

    async def merge_pr(self, repo: str, pr_number: int) -> None:
        response = await self._client.put(f"/repos/{repo}/pulls/{pr_number}/merge")
        response.raise_for_status()

    async def get_diff(self, repo: str, base_branch: str, head_branch: str) -> str | None:
        # Verifie contre l'API publique reelle (`api.github.com`, depot
        # `octocat/Hello-World`) le 2026-09-02 : `Accept:
        # application/vnd.github.v3.diff` fait directement repondre un
        # diff unifie complet, contrairement a Forgejo qui ne l'offre pas
        # sur une comparaison de branches (voir `ForgejoProvider.get_diff`).
        response = await self._client.get(
            f"/repos/{repo}/compare/{base_branch}...{head_branch}",
            headers={"Accept": "application/vnd.github.v3.diff"},
        )
        if response.status_code == 404:
            return None
        response.raise_for_status()
        return response.text

    def git_push_credential(self) -> tuple[str, str] | None:
        if not self._token:
            return None
        # Convention GitHub : un jeton s'utilise comme mot de passe HTTP
        # Basic, avec n'importe quel nom d'utilisateur non vide.
        return ("x-access-token", self._token)
