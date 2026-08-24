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

import httpx

from .base import BaseGitProvider, Issue, PullRequest


class ForgejoProvider(BaseGitProvider):
    def __init__(self, base_url: str, token: str) -> None:
        self._client = httpx.AsyncClient(
            base_url=f"{base_url.rstrip('/')}/api/v1",
            headers={"Authorization": f"token {token}"},
            timeout=30.0,
        )

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
