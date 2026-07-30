export const SYSTEM_TIME_ZONE = "";

type DateInput = string | number | Date;
type DateTimePrecision = "minute" | "second";

const formatterCache = new Map<string, Intl.DateTimeFormat>();

export function normalizeTimeZone(value: string | null | undefined): string {
  const timeZone = value?.trim() ?? SYSTEM_TIME_ZONE;
  if (!timeZone) return SYSTEM_TIME_ZONE;

  try {
    new Intl.DateTimeFormat("en-US", { timeZone }).format(0);
    return timeZone;
  } catch {
    return SYSTEM_TIME_ZONE;
  }
}

export function getSystemTimeZone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
}

export function getSupportedTimeZones(): string[] {
  const supportedValuesOf = (
    Intl as typeof Intl & {
      supportedValuesOf?: (key: "timeZone") => string[];
    }
  ).supportedValuesOf;
  const zones = supportedValuesOf
    ? supportedValuesOf("timeZone")
    : ["Asia/Shanghai", "Asia/Singapore", "Europe/London", "America/New_York"];

  return Array.from(new Set(["UTC", getSystemTimeZone(), ...zones])).sort((a, b) =>
    a.localeCompare(b),
  );
}

function toValidDate(value: DateInput): Date | null {
  const date = value instanceof Date ? value : new Date(value);
  return Number.isNaN(date.getTime()) ? null : date;
}

function getFormatter(
  timeZone: string,
  precision: DateTimePrecision | "date",
): Intl.DateTimeFormat {
  const normalized = normalizeTimeZone(timeZone);
  const key = `${normalized || "system"}:${precision}`;
  const cached = formatterCache.get(key);
  if (cached) return cached;

  const options: Intl.DateTimeFormatOptions = {
    timeZone: normalized || undefined,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
  };
  if (precision !== "date") {
    options.hour = "2-digit";
    options.minute = "2-digit";
    options.hourCycle = "h23";
    if (precision === "second") options.second = "2-digit";
  }

  const formatter = new Intl.DateTimeFormat("en-US", options);
  formatterCache.set(key, formatter);
  return formatter;
}

function getParts(
  value: DateInput,
  timeZone: string,
  precision: DateTimePrecision | "date",
): Record<string, string> | null {
  const date = toValidDate(value);
  if (!date) return null;

  return Object.fromEntries(
    getFormatter(timeZone, precision)
      .formatToParts(date)
      .map((part) => [part.type, part.value]),
  );
}

function fallback(value: DateInput): string {
  return typeof value === "string" ? value : String(value);
}

export function formatDateOnly(value: DateInput, timeZone = SYSTEM_TIME_ZONE): string {
  const parts = getParts(value, timeZone, "date");
  return parts ? `${parts.year}-${parts.month}-${parts.day}` : fallback(value);
}

export function formatDateTime(
  value: DateInput,
  timeZone = SYSTEM_TIME_ZONE,
  precision: DateTimePrecision = "second",
): string {
  const parts = getParts(value, timeZone, precision);
  if (!parts) return fallback(value);

  const seconds = precision === "second" ? `:${parts.second}` : "";
  return `${parts.year}-${parts.month}-${parts.day} ${parts.hour}:${parts.minute}${seconds}`;
}

export function formatShortDateTime(
  value: DateInput,
  timeZone = SYSTEM_TIME_ZONE,
): string {
  const parts = getParts(value, timeZone, "second");
  return parts
    ? `${parts.month}-${parts.day} ${parts.hour}:${parts.minute}:${parts.second}`
    : fallback(value);
}
