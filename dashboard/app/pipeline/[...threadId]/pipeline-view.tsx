"use client";

import { useEffect, useState } from "react";
import { PhaseBadge } from "@/app/components/phase-badge";
import type { WorkshopPhase } from "@/lib/api-server";

// Vue « mission control » d'un workflow PM : le pipeline complet rendu
// visible pendant qu'il tourne (~12 min en pratique).
//
// Tout ce qui est affiche ici est RELU depuis le checkpoint LangGraph et
// depuis les Workshops eux-memes — rien n'est estime ni extrapole. En
// particulier : aucun pourcentage de progression invente, car le graphe ne
// sait pas combien de temps prendra un agent. On montre ce qui est vrai
// (l'etape courante, la phase de chaque microVM, le temps ecoule) plutot
// qu'une barre qui avancerait toute seule.

interface WorkshopLive {
  name: string;
  /** Chaine libre cote transport (le pm-engine relaie ce que dit
   *  l'api-server) ; `PhaseBadge` n'accepte que les phases connues, d'ou la
   *  verification avant affichage plutot qu'une conversion de type aveugle. */
  phase: string | null;
  podName: string | null;
}

const KNOWN_PHASES: WorkshopPhase[] = [
  "Pending",
  "BuildingImage",
  "Provisioning",
  "Running",
  "Suspending",
  "Suspended",
  "Resuming",
  "Terminating",
  "Failed",
];

function asPhase(phase: string | null): WorkshopPhase | null {
  return KNOWN_PHASES.includes(phase as WorkshopPhase) ? (phase as WorkshopPhase) : null;
}

interface SubTask {
  id: string;
  title: string;
  scope: string[];
  workshopName: string;
  branchName: string;
}

interface Workflow {
  threadId: string;
  startedAt: string | null;
  updatedAt: string | null;
  repo: string | null;
  issueNumber: number | null;
  issueTitle: string | null;
  issueUrl: string | null;
  phase: string | null;
  phaseIndex: number;
  phases: string[];
  pendingNodes: string[];
  plan: SubTask[];
  correctionAttempts: number;
  maxCorrectionAttempts: number;
  testPassed: boolean | null;
  testOutput: string | null;
  integrationConflicts: string[];
  prNumber: number | null;
  prUrl: string | null;
  prChangedFiles: number | null;
  status: string | null;
  workshops: WorkshopLive[];
}

/** Libelles courts, en francais : les noms de noeuds LangGraph
 *  (`RunDevcontainerTests`) sont des identifiants de code, pas une langue
 *  d'interface. */
const PHASE_LABELS: Record<string, string> = {
  AnalyzeIssue: "Analyse du ticket",
  PlanParallelTasks: "Découpage en sous-tâches",
  ProvisionWorkshop: "Provisionnement des microVM",
  DelegateToClaudeCode: "Développement par les agents",
  IntegrateSubTasks: "Intégration des branches",
  RunDevcontainerTests: "Exécution des tests",
  OpenPullRequest: "Ouverture de la Pull Request",
  SuspendWhileWaitingReview: "Mise en veille des microVM",
  AwaitHitlApproval: "Attente de revue humaine",
  MergeAndClose: "Fusion et clôture",
  IndexKnowledge: "Indexation des connaissances",
};

/** Durée du workflow, mesurée depuis son DÉPART (premier checkpoint) et non
 *  depuis l'ouverture de la page : sur un run démarré il y a dix minutes, un
 *  compteur partant de zéro afficherait une durée fausse.
 *
 *  Pour un run terminé, la borne haute est le dernier checkpoint, pas
 *  l'heure courante — sans quoi un run de douze minutes finirait par
 *  afficher « 85:55 » simplement parce qu'on rouvre la page plus tard. */
function duration(startedAt: string | null, endedAt: string | null): string {
  if (!startedAt) return "—";
  const end = endedAt ? Date.parse(endedAt) : Date.now();
  const s = Math.max(0, Math.floor((end - Date.parse(startedAt)) / 1000));
  return `${String(Math.floor(s / 60)).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}`;
}

function Stat({ label, value, tone }: { label: string; value: string; tone?: string }) {
  return (
    <div className="flex flex-col gap-0.5">
      <span className="text-[11px] uppercase tracking-wide text-muted">{label}</span>
      <span className={`text-sm font-medium tabular-nums ${tone ?? ""}`}>{value}</span>
    </div>
  );
}

export function PipelineView({ threadId, initial }: { threadId: string; initial: Workflow }) {
  const [wf, setWf] = useState<Workflow>(initial);
  const [tick, setTick] = useState(0);
  const [error, setError] = useState<string | null>(null);

  // Le workflow est termine quand le graphe n'a plus rien a executer ET
  // qu'il n'attend pas une decision humaine. On arrete alors de sonder :
  // laisser une page ouverte marteler l'API pour un run fini n'apporte rien.
  const waitingForHuman = wf.pendingNodes.includes("AwaitHitlApproval");
  const finished = wf.pendingNodes.length === 0 || waitingForHuman;

  useEffect(() => {
    if (finished) return;
    const id = setInterval(async () => {
      try {
        const path = threadId.split("/").map(encodeURIComponent).join("/");
        const res = await fetch(`/api/pm/workflows/${path}`, { cache: "no-store" });
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        setWf(await res.json());
        setError(null);
      } catch (err) {
        // Un sondage rate n'efface pas ce qui est affiche : on garde le
        // dernier etat connu et on le signale, plutot que de vider l'ecran
        // au premier hoquet reseau — en pleine demo, un ecran vide est bien
        // pire qu'une donnee d'il y a trois secondes.
        setError(err instanceof Error ? err.message : "erreur");
      }
    }, 3000);
    return () => clearInterval(id);
  }, [threadId, finished]);

  // Horloge separee du sondage : le temps ecoule doit avancer meme si l'API
  // ne repond pas.
  useEffect(() => {
    if (finished) return;
    const id = setInterval(() => setTick((t) => t + 1), 1000);
    return () => clearInterval(id);
  }, [finished]);
  void tick;

  const current = wf.phaseIndex;
  const correcting = wf.correctionAttempts > 0 && wf.testPassed !== true;

  return (
    <div className="mx-auto w-full max-w-5xl px-4 py-6 flex flex-col gap-6">
      {/* En-tete : le ticket, l'etat global, le temps */}
      <header className="rounded-xl border border-border bg-surface/70 backdrop-blur p-5">
        <div className="flex flex-wrap items-start justify-between gap-4">
          <div className="min-w-0">
            <div className="flex items-center gap-2 text-xs text-muted">
              <span className="font-mono">{wf.repo}</span>
              {wf.issueNumber != null && (
                <a
                  href={wf.issueUrl ?? undefined}
                  className="rounded-full bg-accent/10 text-accent px-2 py-0.5 font-medium"
                >
                  #{wf.issueNumber}
                </a>
              )}
            </div>
            <h1 className="mt-1 text-xl font-semibold truncate">
              {wf.issueTitle ?? "Workflow"}
            </h1>
          </div>
          <div className="flex items-center gap-6">
            <Stat
              label="Étape"
              value={current >= 0 ? `${current + 1} / ${wf.phases.length}` : "—"}
            />
            <Stat
              label={finished ? "Durée" : "Écoulé"}
              value={duration(wf.startedAt, finished ? wf.updatedAt : null)}
            />
            <div
              className={`rounded-full px-3 py-1 text-sm font-medium ${
                waitingForHuman
                  ? "bg-amber-500/10 text-amber-700 dark:text-amber-400"
                  : finished
                    ? "bg-emerald-500/10 text-emerald-700 dark:text-emerald-400"
                    : "bg-accent/10 text-accent"
              }`}
            >
              <span className="inline-flex items-center gap-1.5">
                <span
                  className={`h-1.5 w-1.5 rounded-full bg-current ${finished ? "" : "animate-pulse"}`}
                />
                {waitingForHuman ? "Revue attendue" : finished ? "Terminé" : "En cours"}
              </span>
            </div>
          </div>
        </div>
        {error && (
          <p className="mt-3 text-xs text-amber-600 dark:text-amber-400">
            Dernière mise à jour indisponible ({error}) — affichage du dernier état connu.
          </p>
        )}
      </header>

      {/* Le pipeline, etape par etape */}
      <section className="rounded-xl border border-border bg-surface/70 backdrop-blur p-5">
        <ol className="flex flex-col gap-0">
          {wf.phases.map((phase, i) => {
            const done = current > i;
            const active = current === i;
            return (
              <li key={phase} className="flex items-stretch gap-3">
                <div className="flex flex-col items-center">
                  <span
                    className={`mt-1 flex h-5 w-5 shrink-0 items-center justify-center rounded-full border text-[10px] font-semibold transition-colors ${
                      done
                        ? "border-emerald-500/40 bg-emerald-500/15 text-emerald-600 dark:text-emerald-400"
                        : active
                          ? "border-accent bg-accent text-accent-foreground"
                          : "border-border text-muted"
                    }`}
                  >
                    {done ? "✓" : i + 1}
                  </span>
                  {i < wf.phases.length - 1 && (
                    <span
                      className={`w-px flex-1 ${done ? "bg-emerald-500/30" : "bg-border"}`}
                    />
                  )}
                </div>
                <div className="pb-4 pt-0.5 min-w-0">
                  <p
                    className={`text-sm ${
                      active ? "font-semibold" : done ? "" : "text-muted"
                    }`}
                  >
                    {PHASE_LABELS[phase] ?? phase}
                    {active && !finished && (
                      <span className="ml-2 inline-flex gap-0.5 align-middle">
                        <span className="h-1 w-1 rounded-full bg-accent animate-bounce [animation-delay:0ms]" />
                        <span className="h-1 w-1 rounded-full bg-accent animate-bounce [animation-delay:150ms]" />
                        <span className="h-1 w-1 rounded-full bg-accent animate-bounce [animation-delay:300ms]" />
                      </span>
                    )}
                  </p>
                  {active && correcting && (
                    <p className="text-xs text-amber-600 dark:text-amber-400">
                      Tentative de correction {wf.correctionAttempts} /{" "}
                      {wf.maxCorrectionAttempts}
                    </p>
                  )}
                </div>
              </li>
            );
          })}
        </ol>
      </section>

      {/* Les sous-taches, avec la phase reelle de leur microVM */}
      {wf.plan.length > 0 && (
        <section className="flex flex-col gap-3">
          <h2 className="text-sm font-semibold text-muted">
            {wf.plan.length} sous-tâche{wf.plan.length > 1 ? "s" : ""} en parallèle
          </h2>
          <div className="grid gap-3 sm:grid-cols-2">
            {wf.plan.map((task) => {
              const ws = wf.workshops.find((w) => w.name === task.workshopName);
              return (
                <article
                  key={task.id}
                  className="rounded-xl border border-border bg-surface/70 backdrop-blur p-4 flex flex-col gap-3"
                >
                  <div className="flex items-start justify-between gap-3">
                    <p className="text-sm font-medium leading-snug">{task.title}</p>
                    {asPhase(ws?.phase ?? null) ? (
                      <PhaseBadge phase={asPhase(ws?.phase ?? null) as WorkshopPhase} />
                    ) : (
                      <span className="shrink-0 rounded-full bg-neutral-500/10 px-2.5 py-0.5 text-xs text-muted">
                        —
                      </span>
                    )}
                  </div>
                  <dl className="flex flex-col gap-1 text-xs text-muted">
                    <div className="flex gap-2">
                      <dt className="shrink-0">microVM</dt>
                      <dd className="font-mono truncate">{task.workshopName}</dd>
                    </div>
                    <div className="flex gap-2">
                      <dt className="shrink-0">branche</dt>
                      <dd className="font-mono truncate">{task.branchName}</dd>
                    </div>
                  </dl>
                  <div className="flex flex-wrap gap-1">
                    {task.scope.map((s) => (
                      <code
                        key={s}
                        className="rounded bg-surface-hover px-1.5 py-0.5 text-[11px] text-muted"
                      >
                        {s}
                      </code>
                    ))}
                  </div>
                </article>
              );
            })}
          </div>
        </section>
      )}

      {/* Le resultat : integration, tests, PR */}
      <section className="grid gap-3 sm:grid-cols-3">
        <div className="rounded-xl border border-border bg-surface/70 backdrop-blur p-4">
          <p className="text-[11px] uppercase tracking-wide text-muted">Intégration</p>
          {wf.integrationConflicts.length > 0 ? (
            <p className="mt-1 text-sm text-red-600 dark:text-red-400">
              {wf.integrationConflicts.length} branche(s) en conflit
            </p>
          ) : current > wf.phases.indexOf("IntegrateSubTasks") ? (
            <p className="mt-1 text-sm text-emerald-600 dark:text-emerald-400">
              Branches réunies
            </p>
          ) : (
            <p className="mt-1 text-sm text-muted">En attente</p>
          )}
        </div>

        <div className="rounded-xl border border-border bg-surface/70 backdrop-blur p-4">
          <p className="text-[11px] uppercase tracking-wide text-muted">Tests</p>
          {wf.testPassed === true ? (
            <p className="mt-1 text-sm text-emerald-600 dark:text-emerald-400">Verts</p>
          ) : wf.testPassed === false ? (
            <p className="mt-1 text-sm text-red-600 dark:text-red-400">En échec</p>
          ) : (
            <p className="mt-1 text-sm text-muted">Pas encore exécutés</p>
          )}
        </div>

        <div className="rounded-xl border border-border bg-surface/70 backdrop-blur p-4">
          <p className="text-[11px] uppercase tracking-wide text-muted">Pull Request</p>
          {wf.prUrl ? (
            <a
              href={wf.prUrl}
              target="_blank"
              rel="noreferrer"
              className="mt-1 block text-sm font-medium text-accent hover:underline"
            >
              #{wf.prNumber}
              {wf.prChangedFiles != null && (
                <span className="ml-1.5 font-normal text-muted">
                  {wf.prChangedFiles} fichier{wf.prChangedFiles > 1 ? "s" : ""}
                </span>
              )}
            </a>
          ) : (
            <p className="mt-1 text-sm text-muted">Pas encore ouverte</p>
          )}
          {/* Le garde-fou d'`OpenPullRequest` : une PR sans aucun fichier
              signale presque toujours du travail qui n'a pas atteint la
              branche. On le dit ici plutot que de laisser croire au succes. */}
          {wf.prChangedFiles === 0 && (
            <p className="mt-1 text-xs text-red-600 dark:text-red-400">
              Aucun fichier modifié — travail non poussé ?
            </p>
          )}
        </div>
      </section>

      {/* La sortie de tests brute, repliee par defaut : precieuse quand ca
          echoue, encombrante le reste du temps. */}
      {wf.testOutput && (
        <details className="rounded-xl border border-border bg-surface/70 backdrop-blur p-4">
          <summary className="cursor-pointer text-sm font-medium">
            Sortie des tests
          </summary>
          <pre className="mt-3 max-h-80 overflow-auto rounded-lg bg-surface-hover p-3 text-xs leading-relaxed">
            {wf.testOutput}
          </pre>
        </details>
      )}
    </div>
  );
}
