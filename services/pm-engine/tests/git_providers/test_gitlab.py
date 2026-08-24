"""Verifie `GitLabProvider.get_issue` contre la vraie API publique GitLab
(depot public bien connu, issue stable dans le temps) — pas de mock.

Memes limites que `test_github.py` : les operations d'ecriture necessitent
un jeton avec des droits sur un projet reel, indisponible dans cet
environnement. Skip si le reseau externe n'est pas joignable.
"""

from __future__ import annotations

import httpx
import pytest

from pm_engine.git_providers import GitLabProvider


@pytest.mark.asyncio
async def test_get_issue_reads_a_real_public_gitlab_issue() -> None:
    provider = GitLabProvider(token="")
    try:
        issue = await provider.get_issue("gitlab-org/gitlab-runner", 1)
    except httpx.HTTPError as exc:
        pytest.skip(f"gitlab.com injoignable pour ce test: {exc}")
    else:
        assert issue.number == 1
        # GitLab a recemment renomme la route web des issues en
        # `/-/work_items/<iid>` ("Work Items") : on verifie le projet et le
        # numero, pas le segment de chemin exact (susceptible de rechanger).
        assert issue.url.startswith("https://gitlab.com/gitlab-org/gitlab-runner/-/")
        assert issue.url.endswith("/1")
        assert issue.author
    finally:
        await provider.aclose()
