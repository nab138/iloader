export type AnisetteServer = {
  value: string;
  label: string;
};

export type AnisetteSpeedGrade =
  | "very_fast"
  | "fast"
  | "good"
  | "normal"
  | "slow"
  | "very_slow"
  | "no_response";

export type AnisetteMeasurement = {
  value: string;
  ttfbMs: number | null;
  grade: AnisetteSpeedGrade;
};

export const anisetteServers: AnisetteServer[] = [
  { value: "ani.sidestore.io", label: "SideStore" },
  { value: "ani.stikstore.app", label: "StikStore" },
  { value: "ani.sidestore.app", label: "SideStore" },
  { value: "ani.sidestore.zip", label: "SideStore" },
  { value: "ani.846969.xyz", label: "SideStore" },
  { value: "ani.neoarz.xyz", label: "neoarz" },
  { value: "ani.xu30.top", label: "SteX" },
  { value: "anisette.wedotstud.io", label: "WE. Studio" },
  { value: "ani.waterwave.space", label: "waterwave" },
];

const timeoutMs = 5000;

export const normalizeAnisetteUrl = (server: string) => {
  return server.startsWith("http://") || server.startsWith("https://")
    ? server
    : `https://${server}`;
};

export const getAnisetteSpeedGrade = (
  ttfbMs: number | null,
): AnisetteSpeedGrade => {
  if (ttfbMs === null) return "no_response";
  if (ttfbMs <= 50) return "very_fast";
  if (ttfbMs <= 120) return "fast";
  if (ttfbMs <= 250) return "good";
  if (ttfbMs <= 500) return "normal";
  if (ttfbMs <= 1000) return "slow";
  return "very_slow";
};

export const measureAnisetteServer = async (
  server: AnisetteServer,
): Promise<AnisetteMeasurement> => {
  const controller = new AbortController();
  const timeout = window.setTimeout(() => controller.abort(), timeoutMs);
  const startedAt = performance.now();

  try {
    await fetch(normalizeAnisetteUrl(server.value), {
      cache: "no-store",
      mode: "no-cors",
      signal: controller.signal,
    });
    const ttfbMs = Math.round(performance.now() - startedAt);
    return {
      value: server.value,
      ttfbMs,
      grade: getAnisetteSpeedGrade(ttfbMs),
    };
  } catch {
    return {
      value: server.value,
      ttfbMs: null,
      grade: "no_response",
    };
  } finally {
    window.clearTimeout(timeout);
  }
};

export const measureAnisetteServers = async () => {
  return Promise.all(anisetteServers.map(measureAnisetteServer));
};
