"use client";

import { useRef, useState } from "react";

interface ChatEntry {
  role: "user" | "assistant";
  text: string;
}

// Consomme directement le flux SSE relaye par `/api/pm/chat`
// (`lib/pm-engine.ts::proxyChat`, lui-meme un pont vers `POST /chat` de
// `services/pm-engine`) : `fetch` + lecture manuelle du `ReadableStream`,
// pas `EventSource` (qui ne supporte que GET, alors que la requete porte
// un corps JSON).
export function PmChat() {
  const [repo, setRepo] = useState("");
  const [query, setQuery] = useState("");
  const [entries, setEntries] = useState<ChatEntry[]>([]);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const abortRef = useRef<AbortController | null>(null);

  async function send() {
    if (!repo.trim() || !query.trim() || pending) return;
    setError(null);
    const userQuery = query;
    setEntries((prev) => [...prev, { role: "user", text: userQuery }, { role: "assistant", text: "" }]);
    setQuery("");
    setPending(true);

    const controller = new AbortController();
    abortRef.current = controller;
    try {
      const res = await fetch("/api/pm/chat", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ repo, query: userQuery }),
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
          const parsed = JSON.parse(payload) as { delta?: string; error?: string };
          if (parsed.error) {
            setError(parsed.error);
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
        }
      }
    } catch (err) {
      if (!(err instanceof DOMException && err.name === "AbortError")) {
        setError(err instanceof Error ? err.message : "erreur inattendue");
      }
    } finally {
      setPending(false);
      abortRef.current = null;
    }
  }

  return (
    <div className="flex flex-col gap-3 rounded-xl border border-border bg-surface p-5 shadow-sm">
      <h2 className="text-sm font-medium text-muted uppercase tracking-wide">
        Ask Project Manager
      </h2>

      <input
        value={repo}
        onChange={(e) => setRepo(e.target.value)}
        placeholder="depot (ex: acme/widgets)"
        className="rounded-lg border border-border bg-background px-3 py-2 text-sm font-mono"
      />

      <div className="flex flex-col gap-2 max-h-96 overflow-y-auto pr-1">
        {entries.length === 0 && (
          <p className="text-sm text-muted">
            Pose une question sur les tickets/PR resolus par le PM pour ce depot.
          </p>
        )}
        {entries.map((entry, i) => (
          <div
            key={i}
            className={`rounded-lg px-3 py-2 text-sm whitespace-pre-wrap ${
              entry.role === "user"
                ? "bg-accent text-accent-foreground self-end"
                : "bg-background border border-border"
            }`}
          >
            {entry.text || (pending && i === entries.length - 1 ? "…" : "")}
          </div>
        ))}
      </div>

      {error && <p className="text-sm text-red-600 dark:text-red-400">{error}</p>}

      <div className="flex gap-2">
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              void send();
            }
          }}
          placeholder="Ta question..."
          className="flex-1 rounded-lg border border-border bg-background px-3 py-2 text-sm"
          disabled={pending}
        />
        <button
          onClick={() => void send()}
          disabled={pending || !repo.trim() || !query.trim()}
          className="rounded-full bg-accent text-accent-foreground px-4 py-2 text-sm font-medium hover:bg-accent-hover transition-colors disabled:opacity-50"
        >
          Envoyer
        </button>
      </div>
    </div>
  );
}
