import Link from "next/link";
import type { ReactNode } from "react";

export function TopNav({ children }: { children?: ReactNode }) {
  return (
    <header className="border-b border-border bg-surface/80 backdrop-blur supports-[backdrop-filter]:bg-surface/60 sticky top-0 z-10">
      <div className="max-w-5xl mx-auto px-6 py-4 flex items-center justify-between">
        <Link href="/" className="flex items-center gap-2 font-semibold tracking-tight">
          <span className="inline-flex h-7 w-7 items-center justify-center rounded-lg bg-accent text-accent-foreground text-sm font-bold">
            A
          </span>
          Atelier
        </Link>
        <div className="flex items-center gap-3">{children}</div>
      </div>
    </header>
  );
}
