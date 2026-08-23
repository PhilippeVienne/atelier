"use client";

import { useEffect, useState } from "react";
import type { WorkshopEvent } from "@/lib/api-server";

function formatTime(iso: string | null): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  return d.toLocaleTimeString();
}

export function EventsLog({
  name,
  initialEvents,
  live,
}: {
  name: string;
  initialEvents: WorkshopEvent[];
  live: boolean;
}) {
  const [events, setEvents] = useState(initialEvents);

  useEffect(() => {
    if (!live) return;
    let cancelled = false;
    const tick = async () => {
      try {
        const res = await fetch(`/workshops/${encodeURIComponent(name)}/events`, {
          cache: "no-store",
        });
        if (!res.ok || cancelled) return;
        const data = (await res.json()) as WorkshopEvent[];
        if (!cancelled) setEvents(data);
      } catch {
        // reseau transitoire : on reessaiera au prochain tick, rien a afficher
      }
    };
    const id = setInterval(tick, 2500);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [name, live]);

  if (events.length === 0) {
    return <p className="text-sm text-muted px-1">Aucun evenement pour le moment.</p>;
  }

  return (
    <ol className="flex flex-col-reverse gap-2 max-h-80 overflow-y-auto pr-1">
      {events.map((ev, i) => (
        <li
          key={`${ev.involvedObject}-${ev.reason}-${ev.timestamp}-${i}`}
          className="flex items-start gap-3 rounded-lg border border-border bg-surface px-3 py-2 text-sm"
        >
          <span
            className={`mt-1 h-1.5 w-1.5 shrink-0 rounded-full ${
              ev.type === "Warning" ? "bg-red-500" : "bg-emerald-500"
            }`}
          />
          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2 flex-wrap">
              <span className="font-mono text-xs text-muted">{formatTime(ev.timestamp)}</span>
              <span className="font-medium">{ev.reason}</span>
              <span className="text-xs text-muted font-mono">{ev.involvedObject}</span>
              {ev.count > 1 && (
                <span className="text-xs text-muted">×{ev.count}</span>
              )}
            </div>
            <p className="text-muted break-words">{ev.message}</p>
          </div>
        </li>
      ))}
    </ol>
  );
}
