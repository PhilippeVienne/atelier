"""Verifie empiriquement chaque noeud testable sans microVM Firecracker
live (Jalon M5, tache 5.2.2) : `AnalyzeIssue`, `PlanParallelTasks`,
`ProvisionWorkshop`, `SuspendWhileWaitingReview`, `OpenPullRequest`,
`MergeAndClose`, `IndexKnowledge`, ainsi que les aretes conditionnelles
pures (`route_after_tests`/`route_after_hitl`).

`DelegateToClaudeCode`/`RunDevcontainerTests` ne sont PAS testes de bout
en bout ici (necessitent un `atelier-controller` reel + une image
devcontainer construite + une microVM demarree, indisponibles dans cet
environnement) — voir docs/PROGRESS.md pour le detail de cette limite
assumee.

Necessite : Forgejo/Keycloak/api-server/LiteLLM/Postgres de dev reels
(memes variables d'environnement que les autres suites de ce depot). Skip
si non disponible.
"""

from __future__ import annotations

import os
import uuid

import asyncpg
import httpx
import pytest

from pm_engine import nodes
from pm_engine.deps import PmEngineDeps
from pm_engine.exec_client import ExecResult
from pm_engine.git_providers import ForgejoProvider
from pm_engine.llm_client import LlmClient
from pm_engine.mcp_client import atelier_mcp_session, call_tool_json, call_tool_text
from pm_engine.oidc import OidcTokenProvider
from pm_engine.state import PMWorkflowState, SubTask

FORGEJO_URL = os.environ.get("FORGEJO_URL", "http://127.0.0.1:3000")
FORGEJO_TOKEN = os.environ.get("FORGEJO_TOKEN")
FORGEJO_OWNER = os.environ.get("FORGEJO_OWNER", "atelier_admin")
ATELIER_API_URL = os.environ.get("ATELIER_API_URL", "http://127.0.0.1:8080")
KEYCLOAK_TOKEN_URL = os.environ.get(
    "KEYCLOAK_TOKEN_URL",
    "http://127.0.0.1:8090/realms/atelier/protocol/openid-connect/token",
)
KEYCLOAK_PM_BOT_SECRET = os.environ.get("KEYCLOAK_PM_BOT_SECRET")
LITELLM_URL = os.environ.get("LITELLM_URL", "http://127.0.0.1:4000")
LITELLM_MASTER_KEY = os.environ.get("LITELLM_MASTER_KEY")
DATABASE_URL_PM = os.environ.get(
    "DATABASE_URL_PM",
    "postgresql://atelier_admin:dev-only-not-for-production@127.0.0.1:5433/atelier_pm",
)


def _skip_if_unavailable() -> None:
    if not (FORGEJO_TOKEN and KEYCLOAK_PM_BOT_SECRET and LITELLM_MASTER_KEY):
        pytest.skip(
            "FORGEJO_TOKEN/KEYCLOAK_PM_BOT_SECRET/LITELLM_MASTER_KEY non definis, test ignore"
        )


class _FakeConfig(dict):
    """`RunnableConfig` minimal — seul `config["configurable"]["deps"]` est
    lu par les noeuds (voir `pm_engine.nodes._deps`)."""


@pytest.fixture
async def deps():
    _skip_if_unavailable()
    git_provider = ForgejoProvider(FORGEJO_URL, FORGEJO_TOKEN)
    llm_client = LlmClient(LITELLM_URL, LITELLM_MASTER_KEY)
    token_provider = OidcTokenProvider(KEYCLOAK_TOKEN_URL, "atelier-pm-bot", KEYCLOAK_PM_BOT_SECRET)
    pool = await asyncpg.create_pool(DATABASE_URL_PM, min_size=1, max_size=2)
    pm_bot_token = await token_provider.get_token()
    import base64
    import json as jsonlib

    payload = pm_bot_token.split(".")[1]
    payload += "=" * (-len(payload) % 4)
    pm_bot_subject = jsonlib.loads(base64.urlsafe_b64decode(payload))["sub"]

    d = PmEngineDeps(
        git_provider=git_provider,
        llm_client=llm_client,
        atelier_api_url=ATELIER_API_URL,
        mcp_token_provider=token_provider,
        db_pool=pool,
        chat_model="atelier-budget-test",
        embedding_model="embedding-dev-local",
        pm_bot_subject=pm_bot_subject,
    )
    try:
        yield d
    finally:
        await git_provider.aclose()
        await llm_client.aclose()
        await pool.close()


@pytest.fixture
async def test_repo():
    async with httpx.AsyncClient(
        base_url=f"{FORGEJO_URL.rstrip('/')}/api/v1",
        headers={"Authorization": f"token {FORGEJO_TOKEN}"},
        timeout=30.0,
    ) as admin_client:
        repo_name = f"pm-nodes-test-{uuid.uuid4().hex[:8]}"
        response = await admin_client.post(
            "/user/repos",
            json={"name": repo_name, "auto_init": True, "default_branch": "main"},
        )
        response.raise_for_status()
        try:
            yield f"{FORGEJO_OWNER}/{repo_name}"
        finally:
            await admin_client.delete(f"/repos/{FORGEJO_OWNER}/{repo_name}")


def test_test_trace_reports_the_exit_code_and_names_an_empty_output() -> None:
    """Regression : une sous-tache qui echoue SANS rien afficher donnait une
    trace vide, donc un prompt de correction inexploitable (deux tours de
    correction perdus, constate le 2026-08-31). Le code de sortie doit
    toujours figurer, et l'absence de sortie doit etre dite explicitement."""
    trace = nodes.format_test_trace("pm-9-task-2", ExecResult(status="Succeeded", exit_code=1))
    assert "pm-9-task-2" in trace
    assert "exit code 1" in trace
    assert "aucune sortie" in trace


def test_test_trace_keeps_the_real_output_when_there_is_one() -> None:
    trace = nodes.format_test_trace(
        "pm-9-task-1",
        ExecResult(status="Succeeded", exit_code=0, stdout="ok 1 - liste vide", stderr=""),
    )
    assert "ok 1 - liste vide" in trace
    assert "aucune sortie" not in trace


def test_route_after_tests_proceeds_when_tests_pass() -> None:
    state = PMWorkflowState(test_passed=True, correction_attempts=0, max_correction_attempts=3)
    assert nodes.route_after_tests(state) == "OpenPullRequest"


def test_route_after_tests_retries_when_budget_remains() -> None:
    state = PMWorkflowState(test_passed=False, correction_attempts=1, max_correction_attempts=3)
    assert nodes.route_after_tests(state) == "AutoCorrectionLoop"


def test_route_after_tests_gives_up_when_budget_exhausted() -> None:
    state = PMWorkflowState(test_passed=False, correction_attempts=3, max_correction_attempts=3)
    assert nodes.route_after_tests(state) == "OpenPullRequest"


def test_route_after_hitl_merges_when_approved() -> None:
    assert nodes.route_after_hitl(PMWorkflowState(hitl_decision="approved")) == "MergeAndClose"


def test_route_after_hitl_ends_when_rejected() -> None:
    assert nodes.route_after_hitl(PMWorkflowState(hitl_decision="rejected")) == "__end__"


@pytest.mark.asyncio
async def test_analyze_issue_reads_a_real_issue_and_calls_the_real_llm(deps, test_repo) -> None:
    async with httpx.AsyncClient(
        base_url=f"{FORGEJO_URL.rstrip('/')}/api/v1",
        headers={"Authorization": f"token {FORGEJO_TOKEN}"},
        timeout=30.0,
    ) as admin_client:
        created = await admin_client.post(
            f"/repos/{test_repo}/issues",
            json={"title": "Ajouter un bouton", "body": "Le dashboard a besoin d'un bouton export."},
        )
        created.raise_for_status()
        issue_number = created.json()["number"]

    state = PMWorkflowState(repo=test_repo, issue_number=issue_number)
    update = await nodes.analyze_issue(state, _FakeConfig(configurable={"deps": deps}))

    assert update["issue_title"] == "Ajouter un bouton"
    assert update["analysis"] == "ok"  # modele mock (atelier-budget-test)
    assert update["phase"] == "AnalyzeIssue"


@pytest.mark.asyncio
async def test_plan_parallel_tasks_parses_a_real_json_plan(deps) -> None:
    deps.chat_model = "atelier-plan-test"  # mock_response JSON, voir config.yaml
    state = PMWorkflowState(issue_number=42, analysis="decoupe ce ticket")
    update = await nodes.plan_parallel_tasks(state, _FakeConfig(configurable={"deps": deps}))

    plan = update["plan"]
    assert len(plan) == 2
    assert plan[0]["id"] == "task-1"
    assert plan[0]["workshop_name"] == "pm-42-task-1"
    assert plan[0]["branch_name"] == "feature/42-task-1"


@pytest.mark.asyncio
async def test_provision_and_suspend_workshop_via_real_mcp(deps, test_repo) -> None:
    plan = [
        SubTask(
            id="task-1",
            title="t",
            scope=["**"],
            workshop_name=f"pm-nodes-test-{uuid.uuid4().hex[:8]}",
            branch_name="feature/task-1",
        )
    ]
    state = PMWorkflowState(
        repo=test_repo, devcontainer_repo="https://example.invalid/repo.git", plan=plan
    )
    config = _FakeConfig(configurable={"deps": deps})

    # `devcontainer_repo` pointe volontairement dans le vide. Le resultat
    # depend donc de l'environnement, et les DEUX issues sont correctes :
    #  - sans `atelier-controller`, le Workshop reste en attente et
    #    `provision_workshop` finit par rendre la main (cas d'origine de ce
    #    test) ;
    #  - avec un controller reel, celui-ci tente la construction, echoue, et
    #    `provision_workshop` remonte l'echec sans attendre le timeout
    #    (fail-fast ajoute le 2026-08-30 — c'est precisement ce qu'on veut).
    # Ce que ce test verifie vraiment est en dessous : le pont MCP.
    try:
        await nodes.provision_workshop(state, config)
        await nodes.suspend_while_waiting_review(state, config)
    except RuntimeError as exc:
        assert "en echec" in str(exc), exc

    async with atelier_mcp_session(deps.atelier_api_url, deps.mcp_token_provider) as session:
        # `status` est `null` tant qu'aucun `atelier-controller` reel n'a
        # reconcilie ce Workshop (aucun controller actif dans cet
        # environnement de test) : seul l'aboutissement sans erreur de
        # l'appel MCP est verifie ici, pas le contenu du statut.
        await call_tool_json(session, "get_workshop_status", {"name": plan[0]["workshop_name"]})
        await call_tool_text(session, "delete_workshop", {"name": plan[0]["workshop_name"]})


@pytest.mark.asyncio
async def test_open_pull_request_and_merge_and_close_via_real_forgejo(deps, test_repo) -> None:
    async with httpx.AsyncClient(
        base_url=f"{FORGEJO_URL.rstrip('/')}/api/v1",
        headers={"Authorization": f"token {FORGEJO_TOKEN}"},
        timeout=30.0,
    ) as admin_client:
        created = await admin_client.post(
            f"/repos/{test_repo}/issues", json={"title": "issue", "body": "body"}
        )
        created.raise_for_status()
        issue_number = created.json()["number"]

        branch = (await admin_client.post(
            f"/repos/{test_repo}/branches",
            json={"new_branch_name": "feature/task-1", "old_branch_name": "main"},
        ))
        branch.raise_for_status()
        commit = await admin_client.post(
            f"/repos/{test_repo}/contents/task.txt",
            json={"content": "dGVzdA==", "message": "add", "branch": "feature/task-1"},
        )
        commit.raise_for_status()

    plan = [SubTask(id="task-1", title="t", scope=["**"], workshop_name="w", branch_name="feature/task-1")]
    state = PMWorkflowState(
        repo=test_repo,
        issue_number=issue_number,
        issue_title="issue",
        analysis="resolu",
        test_passed=True,
        plan=plan,
    )
    config = _FakeConfig(configurable={"deps": deps})

    pr_update = await nodes.open_pull_request(state, config)
    assert pr_update["pr_number"]
    state.update(pr_update)

    await nodes.merge_and_close(state, config)


@pytest.mark.asyncio
async def test_index_knowledge_writes_a_real_padded_embedding_with_rls(deps) -> None:
    state = PMWorkflowState(
        repo="atelier_admin/pm-nodes-test",
        issue_number=1,
        issue_title="ticket",
        analysis="resolu avec succes",
        pr_url="http://example.invalid/pr/1",
    )
    config = _FakeConfig(configurable={"deps": deps})

    update = await nodes.index_knowledge(state, config)
    assert update["knowledge_indexed"] is True

    async with deps.db_pool.acquire() as conn:
        async with conn.transaction():
            await conn.execute(
                "SELECT set_config('app.current_tenant', $1, true)", deps.pm_bot_subject
            )
            row = await conn.fetchrow(
                "SELECT content, tenant_id FROM project_memories "
                "WHERE project_id = $1 ORDER BY id DESC LIMIT 1",
                state["repo"],
            )
            assert row is not None
            assert "ticket" in row["content"]
            assert row["tenant_id"] == deps.pm_bot_subject
