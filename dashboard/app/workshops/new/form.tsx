"use client";

import { useActionState } from "react";
import { createWorkshopAction, type CreateWorkshopState } from "@/app/actions";
import { DEV_EGRESS_ALLOWLIST } from "@/lib/dev-allowlist";

const initialState: CreateWorkshopState = {};

function Field({
  label,
  name,
  placeholder,
  defaultValue,
  required,
}: {
  label: string;
  name: string;
  placeholder?: string;
  defaultValue?: string;
  required?: boolean;
}) {
  return (
    <label className="flex flex-col gap-1 text-sm">
      <span className="font-medium">{label}</span>
      <input
        name={name}
        placeholder={placeholder}
        defaultValue={defaultValue}
        required={required}
        className="rounded-lg border border-border bg-background px-3 py-2 text-sm outline-none focus:border-accent focus:ring-2 focus:ring-accent/20 transition-colors"
      />
    </label>
  );
}

export function NewWorkshopForm({
  defaults,
}: {
  defaults?: { repo: string; revision: string; configPath: string; egressAllowlist?: string };
}) {
  const [state, action, pending] = useActionState(createWorkshopAction, initialState);

  return (
    <form action={action} className="flex flex-col gap-4">
      <Field label="Nom" name="name" placeholder="mon-workshop" required />
      <Field
        label="Depot devcontainer"
        name="repo"
        placeholder="https://github.com/org/repo"
        defaultValue={defaults?.repo}
        required
      />
      <Field
        label="Revision"
        name="revision"
        placeholder="HEAD"
        defaultValue={defaults?.revision ?? "HEAD"}
      />
      <Field
        label="Chemin devcontainer.json"
        name="configPath"
        placeholder=".devcontainer/devcontainer.json"
        defaultValue={defaults?.configPath ?? ".devcontainer/devcontainer.json"}
      />
      <div className="grid grid-cols-2 gap-4">
        <Field label="CPU" name="cpu" placeholder="1" defaultValue="1" />
        <Field label="Memoire" name="memory" placeholder="512Mi" defaultValue="512Mi" />
      </div>
      <label className="flex flex-col gap-1 text-sm">
        <span className="font-medium">Allowlist egress (separee par des virgules)</span>
        <textarea
          name="egressAllowlist"
          rows={4}
          defaultValue={defaults?.egressAllowlist ?? DEV_EGRESS_ALLOWLIST.join(", ")}
          className="rounded-lg border border-border bg-background px-3 py-2 text-sm font-mono outline-none focus:border-accent focus:ring-2 focus:ring-accent/20 transition-colors resize-y"
        />
        <span className="text-xs text-muted">
          Preremplie avec une allowlist &quot;dev&quot; large (github, ghcr.io,
          mcr.microsoft.com, apt, docker, npm/pip) : a restreindre pour un
          usage plus scope.
        </span>
      </label>

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
        {pending ? "Creation..." : "Creer"}
      </button>
    </form>
  );
}
