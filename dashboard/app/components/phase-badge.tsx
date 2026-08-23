import type { WorkshopPhase } from "@/lib/api-server";

const PHASE_STYLES: Record<WorkshopPhase, string> = {
  Pending: "bg-neutral-500/10 text-neutral-600 dark:text-neutral-300",
  BuildingImage: "bg-amber-500/10 text-amber-700 dark:text-amber-400",
  Provisioning: "bg-amber-500/10 text-amber-700 dark:text-amber-400",
  Running: "bg-emerald-500/10 text-emerald-700 dark:text-emerald-400",
  Suspending: "bg-amber-500/10 text-amber-700 dark:text-amber-400",
  Suspended: "bg-neutral-500/10 text-neutral-600 dark:text-neutral-300",
  Resuming: "bg-amber-500/10 text-amber-700 dark:text-amber-400",
  Terminating: "bg-red-500/10 text-red-700 dark:text-red-400",
  Failed: "bg-red-500/10 text-red-700 dark:text-red-400",
};

const PULSING: WorkshopPhase[] = ["BuildingImage", "Provisioning", "Suspending", "Resuming", "Terminating"];

export function PhaseBadge({ phase, size = "sm" }: { phase: WorkshopPhase; size?: "sm" | "md" }) {
  const padding = size === "md" ? "px-3 py-1 text-sm" : "px-2.5 py-0.5 text-xs";
  return (
    <span
      className={`inline-flex items-center gap-1.5 rounded-full font-medium ${padding} ${PHASE_STYLES[phase]}`}
    >
      <span
        className={`h-1.5 w-1.5 rounded-full bg-current ${PULSING.includes(phase) ? "animate-pulse" : ""}`}
      />
      {phase}
    </span>
  );
}
