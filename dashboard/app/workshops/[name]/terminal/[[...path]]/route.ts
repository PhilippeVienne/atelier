import "server-only";
import { guestProxy } from "@/lib/guest-proxy";

async function proxy(req: Request, { params }: { params: Promise<{ name: string; path?: string[] }> }) {
  const { name, path } = await params;
  return guestProxy(req, name, path, "terminal");
}

export const GET = proxy;
export const POST = proxy;
export const PUT = proxy;
export const PATCH = proxy;
export const DELETE = proxy;
export const HEAD = proxy;
export const OPTIONS = proxy;
