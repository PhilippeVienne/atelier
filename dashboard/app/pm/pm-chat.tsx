"use client";

import Link from "next/link";
import { useEffect, useRef, useState } from "react";
import { MarkdownLite } from "@/app/components/markdown-lite";

interface ToolCall {
  id: string;
  name: string;
  arguments: Record<string, unknown>;
  status: "running" | "done" | "failed";
  result?: { status?: string; message?: string } & Record<string, unknown>;
}

interface ChatEntry {
  role: "user" | "assistant";
  text: string;
  // Cartes d'appel d'outil (Jalon M5, "elements interactifs") : rendues
  // EN PLUS du texte, jamais persistees (voir `pm_engine.main::chat`,
  // uniquement portees par les evenements SSE `tool_call`/`tool_result` —
  // disparaissent donc a un rechargement de page, comme le texte brut de
  // DeepSeek disparaissait deja avant elles).
  toolCalls?: ToolCall[];
}

// Description humaine du seul outil expose au LLM aujourd'hui
// (`setup_mirror_project`, voir `pm_engine.main.SETUP_MIRROR_PROJECT_TOOL`) :
// un dictionnaire nomme plutot qu'un texte generique, pour que la carte
// affiche "Import du projet acme/widgets" plutot que le nom brut de la
// fonction et un blob JSON.
const TOOL_LABELS: Record<string, (args: Record<string, unknown>) => string> = {
  setup_mirror_project: (args) => `Import du projet ${args.name ?? "?"}`,
};

const EXAMPLE_PROMPTS = [
  "Résume les tickets ouverts cette semaine",
  "Quelles PR attendent une revue humaine ?",
  "Explique la dernière décision prise sur ce dépôt",
  "Importe https://github.com/acme/widgets comme nouveau projet",
];

// Consomme directement le flux SSE relaye par `/api/pm/chat`
// (`lib/pm-engine.ts::proxyChat`, lui-meme un pont vers `POST /chat` de
// `services/pm-engine`) : `fetch` + lecture manuelle du `ReadableStream`,
// pas `EventSource` (qui ne supporte que GET, alors que la requete porte
// un corps JSON).
export function PmChat({ projects }: { projects: string[] }) {
  // Un seul projet importe : pas de choix a faire, on le preselectionne.
  const [repo, setRepo] = useState(projects.length === 1 ? projects[0] : "");
  const [query, setQuery] = useState("");
  const [entries, setEntries] = useState<ChatEntry[]>([]);
  // Depot pour lequel `entries` reflete deja l'historique charge (ou
  // `null` avant le tout premier chargement) : compare a `repo` au rendu
  // plutot qu'un simple booleen reinitialise en tete d'effet — un
  // `setState` synchrone en debut de corps d'effet declenche un rendu en
  // cascade evitable (`react-hooks/set-state-in-effect`).
  const [loadedForRepo, setLoadedForRepo] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const abortRef = useRef<AbortController | null>(null);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);

  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight });
  }, [entries]);

  // Recharge l'historique persiste (`GET /api/pm/chat/history`) a chaque
  // changement de depot cible : sans ca, la conversation disparaissait a
  // chaque rechargement de page (`entries` n'etait qu'un `useState`,
  // jamais relu depuis le backend).
  useEffect(() => {
    let cancelled = false;
    fetch(`/api/pm/chat/history?repo=${encodeURIComponent(repo)}`)
      .then((res) => (res.ok ? res.json() : []))
      .then((rows: Array<{ role: "user" | "assistant"; content: string }>) => {
        if (cancelled) return;
        setEntries(rows.map((r) => ({ role: r.role, text: r.content })));
      })
      .catch(() => {
        // Silencieux : l'utilisateur peut toujours discuter, il perd juste
        // la reprise de son historique pour ce chargement.
      })
      .finally(() => {
        if (!cancelled) setLoadedForRepo(repo);
      });
    return () => {
      cancelled = true;
    };
  }, [repo]);

  async function send(overrideQuery?: string) {
    const userQuery = (overrideQuery ?? query).trim();
    if (!userQuery || pending) return;
    setError(null);
    // Tours precedents affiches AVANT d'ajouter celui-ci : le PM Engine
    // n'a sinon aucune memoire d'un message a l'autre (bug constate en
    // pratique — voir `services/pm-engine/pm_engine/main.py::ChatRequest`).
    // Un message assistant encore vide (reponse en cours d'un tour
    // precedent jamais arrivee, ex: onglet ferme puis rouvert) est exclu :
    // un tour "vide" ne veut rien dire pour le LLM.
    const history = entries.filter((e) => e.text);
    setEntries((prev) => [...prev, { role: "user", text: userQuery }, { role: "assistant", text: "" }]);
    setQuery("");
    if (textareaRef.current) textareaRef.current.style.height = "auto";
    setPending(true);

    const controller = new AbortController();
    abortRef.current = controller;
    try {
      const res = await fetch("/api/pm/chat", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          repo,
          query: userQuery,
          history: history.map((e) => ({ role: e.role, content: e.text })),
        }),
        signal: controller.signal,
      });
      if (!res.ok || !res.body) {
        const body = await res.json().catch(() => null);
        throw new Error(body?.message ?? `erreur ${res.status}`);
      }

      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buffer = "";
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split("\n\n");
        buffer = lines.pop() ?? "";
        for (const line of lines) {
          if (!line.startsWith("data: ")) continue;
          const payload = line.slice("data: ".length);
          if (payload === "[DONE]") continue;
          const parsed = JSON.parse(payload) as {
            delta?: string;
            error?: string;
            tool_call?: { id: string; name: string; arguments: Record<string, unknown> };
            tool_result?: { id: string; name: string; result: Record<string, unknown> };
          };
          if (parsed.error) {
            setError(parsed.error);
            failRunningToolCalls();
            continue;
          }
          if (parsed.delta) {
            setEntries((prev) => {
              const next = [...prev];
              const last = next[next.length - 1];
              if (last?.role === "assistant") {
                next[next.length - 1] = { ...last, text: last.text + parsed.delta };
              }
              return next;
            });
          }
          if (parsed.tool_call) {
            const { id, name, arguments: args } = parsed.tool_call;
            setEntries((prev) => {
              const next = [...prev];
              const last = next[next.length - 1];
              if (last?.role === "assistant") {
                const toolCalls = [
                  ...(last.toolCalls ?? []),
                  { id, name, arguments: args, status: "running" as const },
                ];
                next[next.length - 1] = { ...last, toolCalls };
              }
              return next;
            });
          }
          if (parsed.tool_result) {
            const { id, result } = parsed.tool_result;
            setEntries((prev) => {
              const next = [...prev];
              const last = next[next.length - 1];
              if (last?.role === "assistant" && last.toolCalls) {
                const idx = last.toolCalls.findIndex((c) => c.id === id);
                if (idx !== -1) {
                  const toolCalls = [...last.toolCalls];
                  toolCalls[idx] = { ...toolCalls[idx], status: "done", result };
                  next[next.length - 1] = { ...last, toolCalls };
                }
              }
              return next;
            });
          }
        }
      }
    } catch (err) {
      if (!(err instanceof DOMException && err.name === "AbortError")) {
        setError(err instanceof Error ? err.message : "erreur inattendue");
        failRunningToolCalls();
      }
    } finally {
      setPending(false);
      abortRef.current = null;
    }
  }

  // Une erreur SSE ou une coupure reseau en plein appel d'outil laissait
  // sa carte bloquee en "running" indefiniment (spinner fige) : le tour
  // n'aura jamais de `tool_result` puisque le flux vient de s'arreter.
  function failRunningToolCalls() {
    setEntries((prev) => {
      const next = [...prev];
      const last = next[next.length - 1];
      if (last?.role === "assistant" && last.toolCalls?.some((c) => c.status === "running")) {
        const toolCalls = last.toolCalls.map((c) =>
          c.status === "running" ? { ...c, status: "failed" as const } : c,
        );
        next[next.length - 1] = { ...last, toolCalls };
      }
      return next;
    });
  }

  // Le projet n'est PAS requis : sans lui le PM repond sur le general
  // (roles, fonctionnement, comment importer un projet...). L'exiger rendait
  // le bouton d'envoi inerte sans que rien n'explique pourquoi.
  const canSend = query.trim().length > 0 && !pending;

  return (
    <div className="flex-1 min-h-0 flex flex-col">
      {/* Barre superieure compacte : le depot cible du PM, pas une bulle
          de conversation - reste visible en permanence pendant que la
          liste de messages defile en dessous. */}
      <div className="border-b border-border px-4 sm:px-6 py-3 flex items-center gap-3 flex-wrap">
        <label htmlFor="pm-project" className="text-xs uppercase tracking-wide text-muted shrink-0">
          Projet
        </label>
        {projects.length > 0 ? (
          <select
            id="pm-project"
            value={repo}
            onChange={(e) => setRepo(e.target.value)}
            className="flex-1 max-w-xs rounded-lg border border-border bg-background px-3 py-1.5 text-sm"
          >
            <option value="">Aucun — question générale</option>
            {projects.map((p) => (
              <option key={p} value={p}>
                {p}
              </option>
            ))}
          </select>
        ) : (
          <span className="text-sm text-muted">
            Aucun projet importe —{" "}
            <Link href="/projects/new" className="underline hover:no-underline">
              en importer un
            </Link>{" "}
            pour que le PM puisse travailler dessus.
          </span>
        )}
        <span className="text-xs text-muted">
          {repo
            ? "Les questions et actions portent sur ce projet."
            : "Réponses générales : choisissez un projet pour cibler ses tickets et PR."}
        </span>
      </div>

      <div ref={scrollRef} className="flex-1 min-h-0 overflow-y-auto">
        <div className="max-w-3xl w-full mx-auto px-4 sm:px-6 py-6 flex flex-col gap-5">
          {loadedForRepo !== repo ? null : entries.length === 0 ? (
            <div className="flex-1 flex flex-col items-center justify-center text-center gap-4 py-16">
              <span className="inline-flex h-12 w-12 items-center justify-center rounded-2xl bg-accent text-accent-foreground text-xl font-bold">
                PM
              </span>
              <div className="flex flex-col gap-1">
                <h1 className="text-xl font-semibold tracking-tight">Project Manager</h1>
                <p className="text-sm text-muted max-w-sm">
                  {projects.length === 0
                    ? "Importe un projet pour que le PM puisse analyser ses tickets, ouvrir des PR et piloter des Workshops. Tu peux aussi lui poser une question des maintenant."
                    : "Posez une question sur les tickets et PR d'un projet, ou demandez-lui d'en importer un nouveau."}
                </p>
              </div>
              <div className="flex flex-col gap-2 w-full max-w-md">
                {(projects.length === 0 ? EXAMPLE_PROMPTS.slice(-1) : EXAMPLE_PROMPTS).map((prompt) => (
                  <button
                    key={prompt}
                    type="button"
                    onClick={() => {
                      setQuery(prompt);
                      textareaRef.current?.focus();
                    }}
                    className="text-left text-sm rounded-lg border border-border bg-surface px-4 py-2.5 hover:bg-surface-hover transition-colors"
                  >
                    {prompt}
                  </button>
                ))}
              </div>
            </div>
          ) : (
            entries.map((entry, i) => (
              <div
                key={i}
                className={`flex ${entry.role === "user" ? "justify-end" : "justify-start"}`}
              >
                <div className="flex flex-col gap-2 max-w-[85%] sm:max-w-[75%]">
                  {entry.toolCalls?.map((call, j) => <ToolCallCard key={j} call={call} />)}
                  {entry.text || !entry.toolCalls?.length ? (
                    <div
                      className={`rounded-2xl px-4 py-3 text-sm ${
                        entry.role === "user"
                          ? "bg-accent text-accent-foreground"
                          : "bg-surface border border-border shadow-sm"
                      }`}
                    >
                      {entry.text ? (
                        entry.role === "assistant" ? (
                          <MarkdownLite text={entry.text} />
                        ) : (
                          <p className="whitespace-pre-wrap leading-relaxed">{entry.text}</p>
                        )
                      ) : pending && i === entries.length - 1 ? (
                        <span className="inline-flex gap-1 py-1">
                          <span className="h-1.5 w-1.5 rounded-full bg-current opacity-40 animate-bounce [animation-delay:-0.3s]" />
                          <span className="h-1.5 w-1.5 rounded-full bg-current opacity-40 animate-bounce [animation-delay:-0.15s]" />
                          <span className="h-1.5 w-1.5 rounded-full bg-current opacity-40 animate-bounce" />
                        </span>
                      ) : null}
                    </div>
                  ) : null}
                </div>
              </div>
            ))
          )}
          {error && (
            <p className="text-sm text-red-600 dark:text-red-400 text-center">{error}</p>
          )}
        </div>
      </div>

      <div className="border-t border-border px-4 sm:px-6 py-4">
        <div className="max-w-3xl w-full mx-auto flex items-end gap-2">
          <textarea
            ref={textareaRef}
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              e.target.style.height = "auto";
              e.target.style.height = `${Math.min(e.target.scrollHeight, 160)}px`;
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                void send();
              }
            }}
            rows={1}
            placeholder={repo ? `Votre question sur ${repo}…` : "Votre question…"}
            disabled={pending}
            className="flex-1 resize-none rounded-2xl border border-border bg-background px-4 py-2.5 text-sm leading-relaxed max-h-40 disabled:opacity-60"
          />
          <button
            onClick={() => void send()}
            disabled={!canSend}
            aria-label="Envoyer"
            className="shrink-0 rounded-full bg-accent text-accent-foreground h-10 w-10 flex items-center justify-center hover:bg-accent-hover transition-colors disabled:opacity-40"
          >
            <svg viewBox="0 0 20 20" fill="currentColor" className="h-4 w-4">
              <path d="M3.478 2.404a.75.75 0 0 0-.926.94l2.432 7.905H13.5a.75.75 0 0 1 0 1.5H4.984l-2.432 7.905a.75.75 0 0 0 .926.94 60.519 60.519 0 0 0 18.445-8.986.75.75 0 0 0 0-1.218A60.517 60.517 0 0 0 3.478 2.404Z" />
            </svg>
          </button>
        </div>
        <p className="text-[11px] text-muted text-center mt-2">
          Entrée pour envoyer, Maj+Entrée pour un saut de ligne. Pour un dépôt
          privé, le jeton d&apos;accès transite par ce chat (donc par le
          modèle) : préférez{" "}
          <a href="/projects/new" className="underline hover:no-underline">
            le formulaire dédié
          </a>{" "}
          si vous préférez l&apos;éviter.
        </p>
      </div>
    </div>
  );
}

// Carte d'appel d'outil (Jalon M5, "elements interactifs") : affiche en
// direct un `tool_call` du PM (aujourd'hui uniquement `setup_mirror_project`)
// pendant et apres son execution, plutot que de le laisser invisible
// jusqu'a ce que le texte final du LLM le mentionne — ou pire, que le
// modele le retranscrive lui-meme en texte brut (le bug DeepSeek
// "< MOTS | params >" que cette carte remplace).
function ToolCallCard({ call }: { call: ToolCall }) {
  const label = TOOL_LABELS[call.name]?.(call.arguments) ?? call.name;
  const ok = call.status === "done" && call.result?.status !== "error";
  return (
    <div className="rounded-xl border border-border bg-surface px-4 py-2.5 text-xs flex items-center gap-2.5">
      {call.status === "running" ? (
        <span className="h-3 w-3 shrink-0 rounded-full border-2 border-current border-t-transparent animate-spin opacity-60" />
      ) : (
        <span
          className={`h-3 w-3 shrink-0 rounded-full ${ok ? "bg-green-500" : "bg-red-500"}`}
          aria-hidden
        />
      )}
      <span className="flex-1 min-w-0">
        <span className="font-medium">{label}</span>
        {call.status === "failed" ? (
          <span className="block text-red-600 dark:text-red-400 mt-0.5">
            Interrompu avant la fin (connexion perdue)
          </span>
        ) : call.status === "done" && !ok && call.result?.message ? (
          <span className="block text-red-600 dark:text-red-400 mt-0.5">{call.result.message}</span>
        ) : null}
        {call.status === "done" && ok && typeof call.result?.clone_url === "string" ? (
          <a
            href={call.result.clone_url}
            target="_blank"
            rel="noreferrer"
            className="block text-muted underline hover:no-underline mt-0.5 truncate"
          >
            {call.result.clone_url}
          </a>
        ) : null}
      </span>
    </div>
  );
}
