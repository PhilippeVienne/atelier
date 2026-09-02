"""Verifie empiriquement chaque noeud testable sans microVM Firecracker
live (Jalon M5, tache 5.2.2) : `AnalyzeIssue`, `PlanParallelTasks`,
`ProvisionWorkshop`, `SuspendWhileWaitingReview`, `OpenPullRequest`,
`MergeAndClose`, `IndexKnowledge`, ainsi que les aretes conditionnelles
pures (`route_after_tests`/`route_after_hitl`).

`DelegateToOpencode`/`RunDevcontainerTests` ne sont PAS testes de bout
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


_AUTH_DIFF = (
    "diff --git a/src/auth.js b/src/auth.js\n"
    "new file mode 100644\n"
    "--- /dev/null\n"
    "+++ b/src/auth.js\n"
    "@@ -0,0 +1,3 @@\n"
    "+module.exports = {};\n"
)
_ROUTES_DIFF = (
    "diff --git a/src/routes.js b/src/routes.js\n"
    "--- a/src/routes.js\n"
    "+++ b/src/routes.js\n"
    "@@ -1,2 +1,3 @@\n"
    "+// no security-relevant change here\n"
)
_TERRAFORM_DIFF = (
    "diff --git a/infra/main.tf b/infra/main.tf\n"
    "new file mode 100644\n"
    "--- /dev/null\n"
    "+++ b/infra/main.tf\n"
    "@@ -0,0 +1,1 @@\n"
    "+resource \"null_resource\" \"x\" {}\n"
)
_ROOT_TERRAFORM_DIFF = (
    "diff --git a/main.tf b/main.tf\n"
    "new file mode 100644\n"
    "--- /dev/null\n"
    "+++ b/main.tf\n"
    "@@ -0,0 +1,1 @@\n"
    "+resource \"null_resource\" \"y\" {}\n"
)


def test_diff_matches_any_pattern_detects_a_generic_auth_path() -> None:
    """Motifs volontairement generiques (docs/specs/08-...) : un depot
    CIBLE quelconque (jamais le code d'Atelier) qui ajoute un module
    d'authentification doit declencher ReviewSecurity."""
    assert nodes.diff_matches_any_pattern(_AUTH_DIFF, nodes.SECURITY_SENSITIVE_PATTERNS)


def test_diff_matches_any_pattern_ignores_unrelated_paths() -> None:
    """Preuve que la detection n'est pas un simple 'toujours vrai' : un
    diff sans rapport ne declenche ni ReviewSecurity ni ReviewOps."""
    assert not nodes.diff_matches_any_pattern(_ROUTES_DIFF, nodes.SECURITY_SENSITIVE_PATTERNS)
    assert not nodes.diff_matches_any_pattern(_ROUTES_DIFF, nodes.OPS_SENSITIVE_PATTERNS)


def test_diff_matches_any_pattern_detects_terraform_nested_and_at_root() -> None:
    """`**/*.tf` doit matcher un chemin imbrique (`infra/main.tf`) ET un
    chemin a la racine (`main.tf`, sans aucun `/`) — voir la docstring de
    `_path_matches` sur la semantique non intuitive de `fnmatch` avec `**`."""
    assert nodes.diff_matches_any_pattern(_TERRAFORM_DIFF, nodes.OPS_SENSITIVE_PATTERNS)
    assert nodes.diff_matches_any_pattern(_ROOT_TERRAFORM_DIFF, nodes.OPS_SENSITIVE_PATTERNS)


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


# `interrupt()` (LangGraph) ne peut pas s'invoquer hors de l'execution reelle
# du graphe (leve une erreur sans le runtime Pregel) : `await_hitl_approval`
# lui-meme n'est donc pas testable en isolation. `_outstanding_review_concerns`
# porte toute la logique interessante (docs/specs/08-..., section 4.5) et
# reste une fonction pure — la tester directement, malgre le prefixe prive,
# est le seul moyen de la verifier sans un run de bout en bout complet.
def test_outstanding_review_concerns_empty_when_everything_approved() -> None:
    approve = {"verdict": "approve", "comments": []}
    state = PMWorkflowState(
        architecture_review=approve, code_review=approve, security_review=None, ops_review=None
    )
    assert nodes._outstanding_review_concerns(state) == []


def test_outstanding_review_concerns_surfaces_a_forced_pass() -> None:
    """Le seul chemin qui laisse un verdict a `request_changes` dans l'etat
    final EST un passage en force par epuisement de budget (voir la
    docstring de la fonction) : le relecteur humain doit le voir."""
    state = PMWorkflowState(
        architecture_review={"verdict": "approve", "comments": []},
        code_review={"verdict": "request_changes", "comments": ["code mort"]},
        security_review={"verdict": "request_changes", "comments": ["jeton en clair"]},
        ops_review=None,
    )
    concerns = nodes._outstanding_review_concerns(state)
    assert "[Code] code mort" in concerns
    assert "[Securite] jeton en clair" in concerns
    assert len(concerns) == 2


def test_qa_workshop_name_is_disjoint_from_task_workshops() -> None:
    assert nodes._qa_workshop_name(27) == "pm-27-qa"


def test_parse_qa_verdict_extracts_the_last_json_block_from_an_agent_transcript() -> None:
    """`opencode run` produit une transcription complete (raisonnement,
    appels d'outils) avant le verdict final — contrairement a un appel de
    completion direct, la reponse ENTIERE n'est jamais que le JSON."""
    transcript = (
        "Je vais d'abord lire le code...\n"
        "$ cat package.json\n"
        '{"name": "url-shortener"}\n'
        "Maintenant je demarre le serveur et teste l'API.\n\n"
        '{"verdict": "pass", "comments": [], "evidence_files": [".qa-evidence/get.txt"]}'
    )
    verdict = nodes._parse_qa_verdict(transcript)
    assert verdict == {
        "verdict": "pass",
        "comments": [],
        "evidence_files": [".qa-evidence/get.txt"],
    }


def test_parse_qa_verdict_ignores_an_earlier_unrelated_json_object() -> None:
    """Le `{"name": "url-shortener"}` (sortie de `cat package.json` dans la
    transcription) ne doit jamais etre pris pour le verdict — seul le
    DERNIER bloc portant `"verdict"` compte."""
    transcript = '{"name": "url-shortener"}\n\n{"verdict": "fail", "comments": ["500 au lieu de 404"]}'
    verdict = nodes._parse_qa_verdict(transcript)
    assert verdict["verdict"] == "fail"
    assert verdict["comments"] == ["500 au lieu de 404"]
    assert verdict["evidence_files"] == []  # absent du JSON -> repli sur une liste vide


def test_parse_qa_verdict_handles_literal_braces_inside_a_comment() -> None:
    """Regression REELLE (2026-09-02, run de validation, ticket #29) : un
    commentaire qui cite litteralement une reponse JSON de l'application
    testee (`{"status":"ok"}`) faisait rater l'objet entier a une
    precedente version naive (regex interdisant toute accolade interne) —
    l'agent avait pourtant produit un verdict parfaitement valide."""
    transcript = (
        "```json\n"
        '{"verdict": "pass", "comments": ["corps exact {\\"status\\":\\"ok\\"} recu"], '
        '"evidence_files": [".qa-evidence/health_evidence.txt"]}\n'
        "```"
    )
    verdict = nodes._parse_qa_verdict(transcript)
    assert verdict["verdict"] == "pass"
    assert verdict["comments"] == ['corps exact {"status":"ok"} recu']
    assert verdict["evidence_files"] == [".qa-evidence/health_evidence.txt"]


def test_parse_qa_verdict_falls_back_to_fail_when_no_json_found() -> None:
    """Repli INVERSE de `_parse_review_verdict` : `"fail"`, jamais
    `"pass"` — ce noeud terminal ne bloque plus rien, un repli optimiste
    masquerait une incertitude reelle."""
    verdict = nodes._parse_qa_verdict("l'agent a plante avant de produire quoi que ce soit")
    assert verdict["verdict"] == "fail"
    assert verdict["evidence_files"] == []


def test_parse_qa_verdict_falls_back_to_fail_on_an_unexpected_verdict_value() -> None:
    verdict = nodes._parse_qa_verdict('{"verdict": "maybe", "comments": []}')
    assert verdict["verdict"] == "fail"


def test_route_after_plan_expands_spec_on_greenfield() -> None:
    assert nodes.route_after_plan(PMWorkflowState(greenfield=True)) == "ExpandGreenfieldSpec"


def test_route_after_plan_skips_spec_on_existing_repo() -> None:
    assert nodes.route_after_plan(PMWorkflowState(greenfield=False)) == "ReviewArchitecture"


def test_route_after_plan_skips_spec_when_absent() -> None:
    # Cle absente == depot deja pourvu : c'est le cas des tests plus anciens
    # qui appellent `plan_parallel_tasks` sans `test_repo` (`root_entries`
    # reste `None`, donc `is_greenfield_repo` vaut `False`).
    assert nodes.route_after_plan(PMWorkflowState()) == "ReviewArchitecture"


def test_route_after_architecture_review_proceeds_on_approve() -> None:
    state = PMWorkflowState(architecture_review={"verdict": "approve", "comments": []})
    assert nodes.route_after_architecture_review(state) == "ProvisionWorkshop"


def test_route_after_architecture_review_proceeds_when_absent() -> None:
    # Aucune revue enregistree (etat construit a la main sans passer par le
    # noeud) : ne doit jamais bloquer un run par defaut.
    assert nodes.route_after_architecture_review(PMWorkflowState()) == "ProvisionWorkshop"


def test_route_after_architecture_review_reconsiders_when_budget_remains() -> None:
    state = PMWorkflowState(
        architecture_review={"verdict": "request_changes", "comments": ["x"]},
        architecture_review_attempts=0,
        max_architecture_review_attempts=3,
    )
    assert nodes.route_after_architecture_review(state) == "ArchitectureReconsideration"


def test_route_after_architecture_review_gives_up_when_budget_exhausted() -> None:
    state = PMWorkflowState(
        architecture_review={"verdict": "request_changes", "comments": ["x"]},
        architecture_review_attempts=2,
        max_architecture_review_attempts=3,
    )
    assert nodes.route_after_architecture_review(state) == "ProvisionWorkshop"


@pytest.mark.asyncio
async def test_prepare_architecture_reconsideration_injects_comments() -> None:
    state = PMWorkflowState(
        analysis="ticket initial",
        architecture_review={"verdict": "request_changes", "comments": ["chevauchement de scope"]},
        architecture_review_attempts=0,
    )
    update = await nodes.prepare_architecture_reconsideration(state, _FakeConfig())

    assert update["architecture_review_attempts"] == 1
    assert "ticket initial" in update["analysis"]
    assert "chevauchement de scope" in update["analysis"]
    assert update["phase"] == "ReviewArchitecture"


@pytest.mark.asyncio
async def test_review_architecture_parses_a_real_request_changes_verdict(deps) -> None:
    deps.chat_model = "atelier-review-test"  # mock_response JSON, voir config.yaml
    state = PMWorkflowState(
        analysis="decoupe ce ticket",
        plan=[
            SubTask(
                id="task-1",
                title="Backend",
                scope=["src/**"],
                workshop_name="pm-1-task-1",
                branch_name="feature/1-task-1",
            )
        ],
    )
    update = await nodes.review_architecture(state, _FakeConfig(configurable={"deps": deps}))

    assert update["architecture_review"]["verdict"] == "request_changes"
    assert update["architecture_review"]["comments"] == ["scope de task-2 chevauche task-1"]
    assert update["phase"] == "ReviewArchitecture"


@pytest.mark.asyncio
async def test_review_architecture_falls_back_to_approve_on_unparsable_reply(deps) -> None:
    deps.chat_model = "atelier-budget-test"  # mock_response "ok", pas du JSON
    state = PMWorkflowState(analysis="decoupe ce ticket", plan=[])
    update = await nodes.review_architecture(state, _FakeConfig(configurable={"deps": deps}))

    assert update["architecture_review"] == {"verdict": "approve", "comments": []}


def test_route_after_code_review_skips_straight_to_gate_when_nothing_flagged() -> None:
    state = PMWorkflowState(security_review_needed=False, ops_review_needed=False)
    assert nodes.route_after_code_review(state) == ["ReviewGate"]


def test_route_after_code_review_fans_out_to_both_when_both_flagged() -> None:
    state = PMWorkflowState(security_review_needed=True, ops_review_needed=True)
    assert set(nodes.route_after_code_review(state)) == {"ReviewSecurity", "ReviewOps"}


def test_route_after_code_review_fans_out_to_security_only() -> None:
    state = PMWorkflowState(security_review_needed=True, ops_review_needed=False)
    assert nodes.route_after_code_review(state) == ["ReviewSecurity"]


def test_route_after_review_proceeds_when_everything_approves() -> None:
    approve = {"verdict": "approve", "comments": []}
    state = PMWorkflowState(code_review=approve, security_review=None, ops_review=None)
    assert nodes.route_after_review(state) == "OpenPullRequest"


def test_route_after_review_reconsiders_when_any_role_rejects() -> None:
    approve = {"verdict": "approve", "comments": []}
    reject = {"verdict": "request_changes", "comments": ["x"]}
    state = PMWorkflowState(
        code_review=approve,
        security_review=reject,
        ops_review=None,
        review_attempts=0,
        max_review_attempts=3,
    )
    assert nodes.route_after_review(state) == "ReviewReconsideration"


def test_route_after_review_gives_up_when_budget_exhausted() -> None:
    reject = {"verdict": "request_changes", "comments": ["x"]}
    state = PMWorkflowState(
        code_review=reject,
        security_review=None,
        ops_review=None,
        review_attempts=2,
        max_review_attempts=3,
    )
    assert nodes.route_after_review(state) == "OpenPullRequest"


@pytest.mark.asyncio
async def test_prepare_review_reconsideration_aggregates_comments_from_every_role() -> None:
    state = PMWorkflowState(
        analysis="ticket initial",
        code_review={"verdict": "request_changes", "comments": ["code mort dans routes.js"]},
        security_review={"verdict": "request_changes", "comments": ["jeton en clair"]},
        ops_review={"verdict": "approve", "comments": []},
        review_attempts=0,
    )
    update = await nodes.prepare_review_reconsideration(state, _FakeConfig())

    assert update["review_attempts"] == 1
    assert "[Code] code mort dans routes.js" in update["analysis"]
    assert "[Securite] jeton en clair" in update["analysis"]
    assert "[Ops]" not in update["analysis"]  # Ops a approuve, rien a signaler
    assert update["phase"] == "ReviewCode"


@pytest.mark.asyncio
async def test_review_code_reads_a_real_diff_and_flags_a_sensitive_path(deps, test_repo) -> None:
    """Verifie de bout en bout, contre la vraie Forgejo de dev : `ReviewCode`
    lit le diff (5.6.1), obtient un verdict via un vrai aller-retour LiteLLM,
    et calcule correctement `security_review_needed` a partir d'un chemin
    generique (`src/auth.js`) — jamais un nom de composant Atelier."""
    await deps.git_provider.create_branch(test_repo, "feature/review-code-1", "main")
    async with httpx.AsyncClient(
        base_url=f"{FORGEJO_URL.rstrip('/')}/api/v1",
        headers={"Authorization": f"token {FORGEJO_TOKEN}"},
        timeout=30.0,
    ) as admin_client:
        commit = await admin_client.post(
            f"/repos/{test_repo}/contents/src/auth.js",
            json={
                "content": "bW9kdWxlLmV4cG9ydHMgPSB7fTs=",  # "module.exports = {};"
                "message": "ajoute l'authentification",
                "branch": "feature/review-code-1",
            },
        )
        commit.raise_for_status()

    deps.chat_model = "atelier-review-test"  # mock_response request_changes
    state = PMWorkflowState(
        repo=test_repo,
        analysis="ajoute l'authentification",
        plan=[
            SubTask(
                id="task-1",
                title="Auth",
                scope=["src/**"],
                workshop_name="pm-1-task-1",
                branch_name="feature/review-code-1",
            )
        ],
    )
    update = await nodes.review_code(state, _FakeConfig(configurable={"deps": deps}))

    assert update["code_review"]["verdict"] == "request_changes"
    assert update["security_review_needed"] is True
    assert update["ops_review_needed"] is False
    assert update["phase"] == "ReviewCode"


@pytest.mark.asyncio
async def test_review_security_and_review_gate_via_real_llm(deps) -> None:
    deps.chat_model = "atelier-review-test"
    state = PMWorkflowState(repo="owner/repo-inexistant", plan=[], analysis="ticket")

    security_update = await nodes.review_security(state, _FakeConfig(configurable={"deps": deps}))
    assert security_update["security_review"]["verdict"] == "request_changes"
    # Pas de "phase" ici : ReviewSecurity/ReviewOps peuvent tourner en
    # parallele, et deux ecritures concurrentes differentes sur "phase"
    # font planter LangGraph (voir le commentaire dans review_security) —
    # reproduit reellement lors du run de validation du ticket #27.
    assert "phase" not in security_update

    gate_update = await nodes.review_gate(state, _FakeConfig())
    assert gate_update["phase"] == "ReviewGate"


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
async def test_plan_parallel_tasks_falls_back_on_a_well_formed_but_wrong_shaped_plan(
    deps,
) -> None:
    """Regression reelle (2026-09-02, ticket #29 de validation) : un JSON
    PARFAITEMENT valide mais de mauvaise forme (une liste de chaines,
    `["task-1"]`, plutot que d'objets `{"id":...}`) faisait planter le
    noeud (`TypeError: string indices must be integers`) avant l'ajout du
    garde-fou de forme — constate avec le vrai modele
    (`claude-3-5-sonnet-20241022`) sur un ticket tres simple."""
    deps.chat_model = "atelier-plan-malformed-test"  # mock_response '["task-1"]'
    state = PMWorkflowState(issue_number=29, issue_title="ticket simple", analysis="fais un truc")
    update = await nodes.plan_parallel_tasks(state, _FakeConfig(configurable={"deps": deps}))

    plan = update["plan"]
    assert len(plan) == 1
    assert plan[0]["id"] == "task-1"
    assert plan[0]["title"] == "ticket simple"
    assert plan[0]["scope"] == ["**"]


@pytest.mark.asyncio
async def test_plan_parallel_tasks_flags_a_greenfield_repo(deps, test_repo) -> None:
    # `test_repo` est cree avec `auto_init=True` : sa racine ne contient que
    # README.md, donc `_is_greenfield` vaut vrai. Le plan mock (2 sous-taches)
    # doit alors etre replie a une seule ET le flag `greenfield` doit
    # apparaitre a `True` — c'est ce flag, pas la longueur du plan, que
    # `route_after_plan` consulte pour declencher `ExpandGreenfieldSpec`.
    deps.chat_model = "atelier-plan-test"
    state = PMWorkflowState(repo=test_repo, issue_number=1, analysis="decoupe ce ticket")
    update = await nodes.plan_parallel_tasks(state, _FakeConfig(configurable={"deps": deps}))

    assert update["greenfield"] is True
    assert len(update["plan"]) == 1


@pytest.mark.asyncio
async def test_plan_parallel_tasks_does_not_flag_an_existing_repo(deps, test_repo) -> None:
    async with httpx.AsyncClient(
        base_url=f"{FORGEJO_URL.rstrip('/')}/api/v1",
        headers={"Authorization": f"token {FORGEJO_TOKEN}"},
        timeout=30.0,
    ) as admin_client:
        commit = await admin_client.post(
            f"/repos/{test_repo}/contents/package.json",
            json={"content": "e30=", "message": "scaffold"},  # "{}" en base64
        )
        commit.raise_for_status()

    deps.chat_model = "atelier-plan-test"
    state = PMWorkflowState(repo=test_repo, issue_number=1, analysis="decoupe ce ticket")
    update = await nodes.plan_parallel_tasks(state, _FakeConfig(configurable={"deps": deps}))

    assert update["greenfield"] is False
    assert len(update["plan"]) == 2


@pytest.mark.asyncio
async def test_expand_greenfield_spec_calls_the_real_llm(deps) -> None:
    deps.chat_model = "atelier-budget-test"
    state = PMWorkflowState(analysis="Application de suivi de depenses, depot vierge")
    update = await nodes.expand_greenfield_spec(state, _FakeConfig(configurable={"deps": deps}))

    assert update["greenfield_spec"] == "ok"  # modele mock (atelier-budget-test)
    assert update["phase"] == "ExpandGreenfieldSpec"


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
async def test_open_pull_request_fails_loudly_on_an_empty_diff(deps, test_repo) -> None:
    """Regression (2026-09-02) : un run entierement propre par ailleurs
    (planificateur, tests 5/5) a neanmoins abouti a une PR au diff vide —
    l'agent avait tout fait sauf `git commit`/`git push`. Depuis que
    `delegate_to_opencode` garantit lui-meme le commit ET le push (voir
    `test_delegate_auto_commit.py`), une PR encore vide ICI ne peut plus
    signifier "l'agent a oublie" : elle signifie que la sous-tache n'a rien
    produit du tout, un vrai echec — `open_pull_request` doit echouer
    bruyamment plutot que de laisser passer une revue humaine sur du vide."""
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

        # Branche creee depuis `main`, SANS aucun commit supplementaire :
        # exactement l'etat d'une sous-tache dont l'agent n'a rien produit.
        branch = await admin_client.post(
            f"/repos/{test_repo}/branches",
            json={"new_branch_name": "feature/task-1", "old_branch_name": "main"},
        )
        branch.raise_for_status()

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

    with pytest.raises(RuntimeError, match="AUCUN fichier"):
        await nodes.open_pull_request(state, config)


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
