import type { TFunction } from "i18next";

export const errorSuggestionKeys = {
  underage: ["error.suggestions.underage"],
  account_locked: ["error.suggestions.account_locked"],
  developer: [],
  auth: ["error.suggestions.auth"],
  download: ["error.suggestions.download"],
  house_arrest: [
    "error.suggestions.house_arrest",
    "error.suggestions.trust",
    "error.suggestions.device_coms",
  ],
  remote_pairing: ["error.suggestions.trust", "error.suggestions.pairing"],
  lockdown_pairing: ["error.suggestions.trust", "error.suggestions.pairing"],
  canceled: [],
  operation_update: [],
  device_coms: ["error.suggestions.device_coms", "error.suggestions.trust"],
  device_coms_with_message: [
    "error.suggestions.device_coms",
    "error.suggestions.trust",
  ],
  usbmuxd: [
    "error.suggestions.usbmuxd",
    "error.suggestions.device_coms",
    "error.suggestions.trust",
  ],
  not_logged_in: ["error.suggestions.not_logged_in"],
  no_device_selected: ["error.suggestions.no_device_selected"],
  anisette: ["error.suggestions.anisette"],
  keyring: ["error.suggestions.keyring", "error.suggestions.admin"],
  keyring_with_message: [
    "error.suggestions.keyring",
    "error.suggestions.admin",
  ],
  storage: [
    "error.suggestions.keyring",
    "error.suggestions.admin",
    "error.suggestions.filesystem",
  ],
  misc: ["error.suggestions.misc"],
  filesystem: ["error.suggestions.filesystem", "error.suggestions.admin"],
  not_enough_app_ids: ["error.suggestions.not_enough_app_ids"],
  max_apps: ["error.suggestions.max_apps"],
} as const;

export type ErrorVariant = keyof typeof errorSuggestionKeys;

export type AppError = {
  type: ErrorVariant;
  message: string;
};

export const isErrorVariant = (value: string): value is ErrorVariant => {
  return value in errorSuggestionKeys;
};

const normalizeAnisetteServer = (anisetteServer: string) => {
  return anisetteServer.startsWith("http://") ||
    anisetteServer.startsWith("https://")
    ? anisetteServer
    : `https://${anisetteServer}`;
};

const dedupeSuggestions = (suggestions: string[]) => {
  return [...new Set(suggestions)];
};

const getSuggestionBlock = (
  t: TFunction,
  key: string,
  platform: "mac" | "windows" | "linux",
  anisetteServer: string,
) => {
  const rawSuggestions = t(key, {
    returnObjects: true,
    defaultValue: [],
    anisetteServerUrl: normalizeAnisetteServer(anisetteServer),
  }) as unknown;

  if (!Array.isArray(rawSuggestions)) {
    return [];
  }

  return rawSuggestions
    .filter((suggestion): suggestion is string => {
      if (typeof suggestion !== "string") {
        return false;
      }
      if (suggestion.startsWith("[platform::")) {
        const platformEnd = suggestion.indexOf("]");
        if (platformEnd !== -1) {
          const suggestionPlatform = suggestion.substring(11, platformEnd);
          console.log(
            "suggestion platform:",
            suggestionPlatform,
            "current platform:",
            platform,
          );
          if (suggestionPlatform === platform) {
            return true;
          }
        }
        return false;
      }
      return true;
    })
    .map((s) =>
      s
        .replace(/^\[platform::.*?\]/, "")
        // TODO: actually check ios version
        .replace(/\[ios::.*?\]/g, "")
        .trim(),
    );
};

export const getErrorSuggestions = (
  t: TFunction,
  type: ErrorVariant,
  platform: "mac" | "windows" | "linux",
  anisetteServer: string,
): string[] => {
  return dedupeSuggestions(
    errorSuggestionKeys[type].flatMap((key) =>
      getSuggestionBlock(t, key, platform, anisetteServer),
    ),
  );
};

export const parseLinkToken = (
  token: string,
): { url: string; text: string } | null => {
  const doubleColonMatch = token.match(/^\(\(link::([^)]+)\)\)$/);
  if (doubleColonMatch) {
    const url = doubleColonMatch[1].trim();
    return url ? { url, text: url } : null;
  }

  const singleColonMatch = token.match(/^\(\(link:([^)]+)\)\)$/);
  if (!singleColonMatch) {
    return null;
  }

  const payload = singleColonMatch[1].trim();
  if (!payload) {
    return null;
  }

  const lastColon = payload.lastIndexOf(":");
  if (lastColon > 0) {
    const possibleUrl = payload.slice(0, lastColon).trim();
    const possibleText = payload.slice(lastColon + 1).trim();
    if (possibleText && /^[a-z][a-z0-9+.-]*:\/\//i.test(possibleUrl)) {
      return { url: possibleUrl, text: possibleText };
    }
  }

  return { url: payload, text: payload };
};
