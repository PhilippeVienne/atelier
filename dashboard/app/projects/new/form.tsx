"use client";

import { useActionState, useState } from "react";
import { createMirrorProjectAction, type CreateMirrorProjectState } from "@/app/actions";

const initialState: CreateMirrorProjectState = {};

export function NewProjectForm() {
  const [state, action, pending] = useActionState(createMirrorProjectAction, initialState);
  const [isPrivate, setIsPrivate] = useState(false);

  return (
    <form action={action} className="flex flex-col gap-4">
      <label className="flex flex-col gap-1 text-sm">
        <span className="font-medium">Nom</span>
        <input
          name="name"
          placeholder="widgets"
          required
          className="rounded-lg border border-border bg-background px-3 py-2 text-sm outline-none focus:border-accent focus:ring-2 focus:ring-accent/20 transition-colors"
        />
      </label>

      <label className="flex flex-col gap-1 text-sm">
        <span className="font-medium">URL source</span>
        <input
          name="sourceUrl"
          placeholder="https://github.com/acme/widgets ou https://gitlab.exemple.com/acme/widgets"
          required
          className="rounded-lg border border-border bg-background px-3 py-2 text-sm outline-none focus:border-accent focus:ring-2 focus:ring-accent/20 transition-colors"
        />
      </label>

      <label className="flex items-center gap-2 text-sm">
        <input
          type="checkbox"
          name="private"
          checked={isPrivate}
          onChange={(e) => setIsPrivate(e.target.checked)}
          className="rounded border-border"
        />
        <span className="font-medium">Depot prive</span>
      </label>

      {isPrivate && (
        <label className="flex flex-col gap-1 text-sm">
          <span className="font-medium">Jeton d&apos;acces (PAT)</span>
          <input
            name="token"
            type="password"
            placeholder="ghp_... ou glpat-..."
            autoComplete="off"
            className="rounded-lg border border-border bg-background px-3 py-2 text-sm font-mono outline-none focus:border-accent focus:ring-2 focus:ring-accent/20 transition-colors"
          />
          <span className="text-xs text-muted">
            Transmis une seule fois a Forgejo, qui le conserve pour la resynchronisation
            periodique du miroir — jamais stocke par le dashboard.
          </span>
        </label>
      )}

      {state.error && (
        <p className="text-sm text-red-600 dark:text-red-400 border border-red-500/30 bg-red-500/10 rounded-lg px-3 py-2">
          {state.error}
        </p>
      )}

      <button
        type="submit"
        disabled={pending}
        className="rounded-full bg-accent text-accent-foreground px-6 py-2.5 font-medium hover:bg-accent-hover transition-colors disabled:opacity-50"
      >
        {pending ? "Import..." : "Importer"}
      </button>
    </form>
  );
}
