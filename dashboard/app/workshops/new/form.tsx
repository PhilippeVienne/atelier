"use client";

import { useActionState } from "react";
import { createWorkshopAction, type CreateWorkshopState } from "@/app/actions";

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
        className="rounded border border-neutral-300 px-3 py-2 text-sm"
      />
    </label>
  );
}

export function NewWorkshopForm() {
  const [state, action, pending] = useActionState(createWorkshopAction, initialState);

  return (
    <form action={action} className="flex flex-col gap-4">
      <Field label="Nom" name="name" placeholder="mon-workshop" required />
      <Field
        label="Depot devcontainer"
        name="repo"
        placeholder="https://github.com/org/repo"
        required
      />
      <Field label="Revision" name="revision" placeholder="HEAD" defaultValue="HEAD" />
      <Field
        label="Chemin devcontainer.json"
        name="configPath"
        placeholder=".devcontainer/devcontainer.json"
        defaultValue=".devcontainer/devcontainer.json"
      />
      <div className="grid grid-cols-2 gap-4">
        <Field label="CPU" name="cpu" placeholder="1" defaultValue="1" />
        <Field label="Memoire" name="memory" placeholder="512Mi" defaultValue="512Mi" />
      </div>
      <Field
        label="Allowlist egress (separee par des virgules)"
        name="egressAllowlist"
        placeholder="registry.npmjs.org, pypi.org"
      />

      {state.error && (
        <p className="text-sm text-red-600 border border-red-200 bg-red-50 rounded px-3 py-2">
          {state.error}
        </p>
      )}

      <button
        type="submit"
        disabled={pending}
        className="rounded-full bg-foreground text-background px-6 py-2.5 font-medium hover:opacity-90 transition-opacity disabled:opacity-50"
      >
        {pending ? "Creation..." : "Creer"}
      </button>
    </form>
  );
}
