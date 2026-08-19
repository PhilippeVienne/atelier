import { NextRequest, NextResponse } from "next/server";

// Verification optimiste seulement (presence du cookie de session, pas de
// validation du JWT) : la validation reelle a lieu cote atelier-api-server
// a chaque appel (cf. lib/api-server.ts). Suffit a eviter un aller-retour
// inutile vers une page qui redirigera de toute facon.
const PUBLIC_PATHS = ["/login"];

export default function proxy(request: NextRequest) {
  const { pathname } = request.nextUrl;

  if (pathname.startsWith("/api/auth/") || pathname.startsWith("/_next")) {
    return NextResponse.next();
  }

  const hasSession = request.cookies.has("atelier_session");
  const isPublic = PUBLIC_PATHS.includes(pathname);

  if (!hasSession && !isPublic) {
    return NextResponse.redirect(new URL("/login", request.url));
  }
  if (hasSession && isPublic) {
    return NextResponse.redirect(new URL("/", request.url));
  }

  return NextResponse.next();
}

export const config = {
  matcher: ["/((?!_next/static|_next/image|favicon.ico).*)"],
};
