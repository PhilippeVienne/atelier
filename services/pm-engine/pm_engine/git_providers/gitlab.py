"""Implementation GitLab de `BaseGitProvider` (Jalon M5, tache 5.4.2).

API REST v4 standard (`https://gitlab.com/api/v4`, ou une instance
GitLab self-hosted via `base_url`), authentification par jeton (en-tete
`PRIVATE-TOKEN`). GitLab identifie un projet par son chemin
`namespace/projet` URL-encode (`owner%2Frepo`) — `repo` (forme
`"owner/repo"`, meme convention que les deux autres providers) est encode
ici, jamais expose tel quel a l'appelant. Les tickets/PR sont identifies
par leur `iid` (numero visible dans l'UI, scope au projet), pas leur `id`
global — c'est bien ce que ce module attend en entree/sortie (`number`),
coherent avec Forgejo/GitHub.
"""

from __future__ import annotations

from urllib.parse import quote

import httpx

from .base import BaseGitProvider, Issue, PullRequest

DEFAULT_BASE_URL = "https://gitlab.com/api/v4"


def _encode_project(repo: str) -> str:
    return quote(repo, safe="")


class GitLabProvider(BaseGitProvider):
    def __init__(self, token: str, base_url: str = DEFAULT_BASE_URL) -> None:
        self._client = httpx.AsyncClient(
            base_url=base_url.rstrip("/"),
            headers={"PRIVATE-TOKEN": token},
            timeout=30.0,
        )

    async def aclose(self) -> None:
        await self._client.aclose()

    async def get_issue(self, repo: str, issue_number: int) -> Issue:
        project = _encode_project(repo)
        response = await self._client.get(f"/projects/{project}/issues/{issue_number}")
        response.raise_for_status()
        data = response.json()
        return Issue(
            number=data["iid"],
            title=data["title"],
            body=data.get("description") or "",
            author=data["author"]["username"],
            url=data["web_url"],
        )

    async def post_comment(self, repo: str, issue_number: int, body: str) -> None:
        project = _encode_project(repo)
        response = await self._client.post(
            f"/projects/{project}/issues/{issue_number}/notes",
            json={"body": body},
        )
        response.raise_for_status()

    async def create_branch(self, repo: str, branch_name: str, base_branch: str) -> None:
        project = _encode_project(repo)
        response = await self._client.post(
            f"/projects/{project}/repository/branches",
            params={"branch": branch_name, "ref": base_branch},
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
        project = _encode_project(repo)
        response = await self._client.post(
            f"/projects/{project}/merge_requests",
            json={
                "title": title,
                "description": body,
                "source_branch": head_branch,
                "target_branch": base_branch,
            },
        )
        response.raise_for_status()
        data = response.json()
        return PullRequest(
            number=data["iid"],
            url=data["web_url"],
            head_branch=head_branch,
            base_branch=base_branch,
            state=data["state"],
        )

    async def list_root_entries(self, repo: str, ref: str) -> list[str] | None:
        project = _encode_project(repo)
        response = await self._client.get(
            f"/projects/{project}/repository/tree", params={"ref": ref}
        )
        if response.status_code == 404:
            return []
        response.raise_for_status()
        return [entry["name"] for entry in response.json()]

    async def merge_pr(self, repo: str, pr_number: int) -> None:
        project = _encode_project(repo)
        response = await self._client.put(
            f"/projects/{project}/merge_requests/{pr_number}/merge"
        )
        response.raise_for_status()
