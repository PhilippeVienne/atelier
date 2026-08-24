"""Verifie `GitHubProvider.get_issue` contre la vraie API publique GitHub
(depot public bien connu, issue stable dans le temps) — pas de mock.

`post_comment`/`create_branch`/`create_pr`/`merge_pr` ne sont PAS testes de
bout en bout ici : ce sont des operations d'ecriture qui necessiteraient un
jeton avec des droits sur un depot reel, indisponible dans cet
environnement (contrairement a Forgejo, ou une instance de dev complete
existe — voir tests/git_providers/test_forgejo.py, qui couvre le contrat
complet). Skip si le reseau externe n'est pas joignable, jamais de mock en
remplacement.
"""

from __future__ import annotations

import httpx
import pytest

from pm_engine.git_providers import GitHubProvider


@pytest.mark.asyncio
async def test_get_issue_reads_a_real_public_github_issue() -> None:
    provider = GitHubProvider(token="")
    try:
        issue = await provider.get_issue("octocat/Hello-World", 1)
    except httpx.HTTPError as exc:
        pytest.skip(f"api.github.com injoignable pour ce test: {exc}")
    else:
        assert issue.number == 1
        assert issue.url.startswith("https://github.com/octocat/Hello-World/")
        assert issue.author
    finally:
        await provider.aclose()
