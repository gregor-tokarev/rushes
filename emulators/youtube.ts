// YouTube Data API v3 emulator: the subset Rushes uses (channels, resumable video
// uploads, videos, thumbnails, uploads playlist) with Google-style error bodies, plus
// the parts of the YouTube Analytics API (daily views and watch time of a video) and
// the YouTube Reporting API (reach reports) that A/B tests read.
//
// Bearer tokens are validated against the Google emulator's userinfo endpoint. Video
// bytes never enter the emulator state: in-flight uploads are spooled to a temp dir and
// only their size and SHA-256 are recorded.

import { createHash, randomBytes } from "node:crypto";
import { createReadStream, existsSync, mkdtempSync, rmSync } from "node:fs";
import { open, readFile, rm, truncate, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { crc32, deflateSync } from "node:zlib";
import { defineEmulator, type EmulatorContext, type EmulatorDefinition } from "emulate";

export interface YoutubeEmulatorOptions {
  /** Google emulator that issued the access tokens. Env `GOOGLE_EMULATOR_URL`, default `http://127.0.0.1:4002`. */
  googleUrl?: string;
  /** Time from upload completion until processing succeeds. Env `YOUTUBE_EMULATOR_PROCESSING_MS`, default 3000. */
  processingMs?: number;
  /** Quota units per Pacific-time day. Env `YOUTUBE_EMULATOR_DAILY_QUOTA`, default 10000 (the real default). */
  dailyQuota?: number;
  /** How long a token validation result is reused. Default 5000 ms. */
  authCacheMs?: number;
}

type Privacy = "private" | "unlisted" | "public";

interface Snippet {
  title: string;
  description: string;
  tags: string[];
  categoryId: string;
  defaultLanguage: string | null;
}

interface Status {
  privacyStatus: Privacy;
  publishAt: string | null;
  selfDeclaredMadeForKids: boolean;
  embeddable: boolean;
  license: string;
  publicStatsViewable: boolean;
}

interface ChannelRecord {
  id: string;
  ownerEmail: string;
  title: string;
  customUrl: string;
  thumbnailUrl: string | null;
  publishedAt: string;
}

interface ThumbnailRecord {
  contentType: string;
  bytes: number;
  sha256: string;
}

/** What a video showed from `at` on: its title and the SHA-256 of its custom thumbnail. */
interface Showing {
  at: number;
  title: string;
  thumbnail: string | null;
}

interface VideoRecord {
  id: string;
  seq: number;
  channelId: string;
  snippet: Snippet;
  status: Status;
  publishedAt: string;
  uploadedAtMs: number;
  thumbnail: ThumbnailRecord | null;
  /** Every title and thumbnail the video showed, oldest first. */
  history: Showing[];
}

/** What one video did on one Pacific-time day, as YouTube Analytics and the reach report count it. */
interface DayNumbers {
  views: number;
  minutesWatched: number;
  impressions: number;
  /** Clicks per impression, 0 to 1. */
  ctr: number;
}

/** A YouTube Reporting API job: YouTube makes one report of its type a day. */
interface ReportingJob {
  id: string;
  channelId: string;
  reportTypeId: string;
  name: string;
  createTime: string;
}

interface SessionRecord {
  id: string;
  channelId: string;
  parts: string[];
  snippet: Snippet;
  status: Status;
  contentType: string;
  total: number;
  received: number;
  videoId: string | null;
  createdAt: string;
}

/** A completed upload as reported by `GET /_emulator/uploads`. */
interface UploadRecord {
  videoId: string;
  channelId: string;
  bytes: number;
  sha256: string;
  contentType: string;
  snippet: Snippet;
  status: Status;
  completedAt: string;
}

/**
 * Injected failure for the next `remaining` requests matching `method` and `path`.
 * Either fails with an error body (`status`) or, for upload session PUTs, persists at
 * most `persistBytes` of the chunk and answers 308 like a server that lost the rest.
 */
interface Fault {
  method: string | null;
  path: string;
  remaining: number;
  status: number | null;
  reason: string | null;
  message: string | null;
  persistBytes: number | null;
}

export interface YoutubeState {
  channels: Record<string, ChannelRecord>;
  videos: Record<string, VideoRecord>;
  sessions: Record<string, SessionRecord>;
  uploads: UploadRecord[];
  nextSeq: number;
  quota: { day: string; used: number };
  faults: Fault[];
  /** Numbers set through `POST /_emulator/analytics`, by video id and Pacific date (YYYY-MM-DD). */
  analytics: Record<string, Record<string, DayNumbers>>;
  /**
   * When numbers were set, by channel id and Pacific date, oldest first: each time
   * publishes that day's reach report again, as YouTube does with corrected data.
   */
  analyticsVersions: Record<string, Record<string, number[]>>;
  jobs: Record<string, ReportingJob>;
}

interface GoogleUser {
  sub: string;
  email: string;
  name?: string;
  picture?: string | null;
}

type App = EmulatorContext<YoutubeState>["app"];
type Ctx = Parameters<Parameters<App["get"]>[1]>[0];

/** Resumable uploads persist non-final data in multiples of this many bytes. */
const UPLOAD_GRANULARITY = 256 * 1024;
const MAX_VIDEO_BYTES = 256 * 1024 ** 3;
const MAX_THUMBNAIL_BYTES = 2 * 1024 * 1024;
const PRIVACY: readonly Privacy[] = ["private", "unlisted", "public"];
// Assignable categories in the US region.
const CATEGORY_IDS = ["1", "2", "10", "15", "17", "19", "20", "22", "23", "24", "25", "26", "27", "28", "29"];
const DEFAULT_CATEGORY_ID = "22";
// Costs from https://developers.google.com/youtube/v3/determine_quota_cost
const COST = { list: 1, insert: 1600, update: 50, delete: 50, thumbnail: 50 };
// Google's emulator resolves unknown tokens to its first seeded user, so tokens are
// also required to look like the ones it mints.
const GOOGLE_EMULATOR_TOKEN = /^google_[A-Za-z0-9_-]{20,}$/;
const AUTH_ERROR_MESSAGE =
  "Request had invalid authentication credentials. Expected OAuth 2 access token, login cookie or other valid authentication credential. See https://developers.google.com/identity/sign-in/web/devconsole-project.";
const STATUS_NAMES: Record<number, string> = {
  400: "INVALID_ARGUMENT",
  401: "UNAUTHENTICATED",
  403: "PERMISSION_DENIED",
  404: "NOT_FOUND",
  409: "ABORTED",
  429: "RESOURCE_EXHAUSTED",
  500: "INTERNAL",
  503: "UNAVAILABLE",
};
const REASON_DEFAULTS: Record<string, { domain: string; message: string }> = {
  quotaExceeded: {
    domain: "youtube.quota",
    message:
      'The request cannot be completed because you have exceeded your <a href="/youtube/v3/getting-started#quota">quota</a>.',
  },
  uploadLimitExceeded: { domain: "youtube.video", message: "The user has exceeded the number of videos they may upload." },
  backendError: { domain: "global", message: "Backend Error" },
  rateLimitExceeded: { domain: "youtube.quota", message: "The request cannot be completed because you have exceeded your rate limit." },
};

interface ErrorDetails {
  domain?: string;
  location?: string;
  locationType?: string;
  /** Top-level `error.message` when it differs from the per-error message. */
  summary?: string;
}

class YoutubeError extends Error {
  readonly status: number;
  readonly reason: string;
  readonly details: ErrorDetails;

  constructor(status: number, reason: string, message: string, details: ErrorDetails = {}) {
    super(message);
    this.status = status;
    this.reason = reason;
    this.details = details;
  }
}

function errorBody(error: YoutubeError) {
  const domain = error.details.domain ?? REASON_DEFAULTS[error.reason]?.domain ?? "global";
  return {
    error: {
      code: error.status,
      message: error.details.summary ?? error.message,
      errors: [
        {
          message: error.message,
          domain,
          reason: error.reason,
          ...(error.details.location
            ? { location: error.details.location, locationType: error.details.locationType ?? "parameter" }
            : {}),
        },
      ],
      ...(STATUS_NAMES[error.status] ? { status: STATUS_NAMES[error.status] } : {}),
    },
  };
}

function errorResponse(c: Ctx, error: YoutubeError): Response {
  const headers: Record<string, string> = {};
  if (error.status === 401) headers["WWW-Authenticate"] = 'Bearer realm="https://accounts.google.com/", error="invalid_token"';
  return c.json(errorBody(error), error.status, headers);
}

function handle(fn: (c: Ctx) => Promise<Response>) {
  return async (c: Ctx): Promise<Response> => {
    try {
      return await fn(c);
    } catch (error) {
      if (error instanceof YoutubeError) return errorResponse(c, error);
      throw error;
    }
  };
}

function badRequest(reason: string, message: string, details?: ErrorDetails): YoutubeError {
  return new YoutubeError(400, reason, message, details);
}

function videoNotFound(): YoutubeError {
  return new YoutubeError(404, "videoNotFound", "The video identified by the <code>id</code> parameter could not be found.", {
    domain: "youtube.video",
    location: "id",
  });
}

function etag(value: unknown): string {
  return createHash("sha1").update(JSON.stringify(value)).digest("base64url");
}

function pacificDay(now: Date): string {
  return now.toLocaleDateString("en-CA", { timeZone: "America/Los_Angeles" });
}

function numberFromEnv(name: string): number | undefined {
  const value = process.env[name];
  if (value === undefined || value === "") return undefined;
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed < 0) throw new Error(`${name} must be a non-negative number`);
  return parsed;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

async function discardBody(c: Ctx): Promise<void> {
  await c.req.raw.arrayBuffer().catch(() => undefined);
}

async function readJson(c: Ctx): Promise<unknown> {
  const text = await c.req.text();
  if (!text.trim()) return {};
  try {
    return JSON.parse(text);
  } catch {
    throw badRequest("parseError", "Parse Error");
  }
}

function parseParts(c: Ctx, allowed: readonly string[]): string[] {
  const raw = c.req.query("part");
  if (!raw) throw badRequest("required", "Required parameter: part", { location: "part" });
  const parts = raw
    .split(",")
    .map((part) => part.trim())
    .filter(Boolean);
  for (const part of parts) {
    if (!allowed.includes(part)) throw badRequest("unknownPart", `'${part}'`, { domain: "youtube.part", location: "part" });
  }
  return parts;
}

function hasAngleBrackets(value: string): boolean {
  return value.includes("<") || value.includes(">");
}

function validateTitle(value: unknown): string {
  if (typeof value !== "string" || !value.trim() || [...value].length > 100 || hasAngleBrackets(value)) {
    throw badRequest("invalidTitle", "The request metadata specifies an invalid or empty video title.", {
      domain: "youtube.video",
      location: "body.snippet.title",
      locationType: "other",
    });
  }
  return value;
}

function validateDescription(value: unknown): string {
  if (value === undefined || value === null) return "";
  if (typeof value !== "string" || Buffer.byteLength(value, "utf8") > 5000 || hasAngleBrackets(value)) {
    throw badRequest("invalidDescription", "The request metadata specifies an invalid video description.", {
      domain: "youtube.video",
      location: "body.snippet.description",
      locationType: "other",
    });
  }
  return value;
}

function validateTags(value: unknown): string[] {
  if (value === undefined || value === null) return [];
  const invalid = badRequest("invalidTags", "The request metadata specifies invalid video keywords.", {
    domain: "youtube.video",
    location: "body.snippet.tags",
    locationType: "other",
  });
  if (!Array.isArray(value) || value.some((tag) => typeof tag !== "string" || hasAngleBrackets(tag))) throw invalid;
  const tags = value as string[];
  // YouTube counts the comma separators and the quotes around tags containing spaces.
  const length = tags.reduce((sum, tag) => sum + [...tag].length + (tag.includes(" ") ? 2 : 0), Math.max(0, tags.length - 1));
  if (length > 500) throw invalid;
  return tags;
}

function validateCategory(value: unknown, fallback: string | null): string {
  if (value === undefined || value === null) {
    if (fallback !== null) return fallback;
    throw badRequest("invalidCategoryId", "The <code>snippet.categoryId</code> property must be specified.", {
      domain: "youtube.video",
      location: "body.snippet.categoryId",
      locationType: "other",
    });
  }
  if (typeof value !== "string" || !CATEGORY_IDS.includes(value)) {
    throw badRequest("invalidCategoryId", "The <code>snippet.categoryId</code> property specifies an invalid category ID.", {
      domain: "youtube.video",
      location: "body.snippet.categoryId",
      locationType: "other",
    });
  }
  return value;
}

function optionalBoolean(value: unknown, field: string, fallback: boolean): boolean {
  if (value === undefined || value === null) return fallback;
  if (typeof value !== "boolean") throw badRequest("invalidVideoMetadata", `Invalid value for ${field}.`, { domain: "youtube.video" });
  return value;
}

/** Validates the `snippet`/`status` parts of a videos.insert or videos.update body. */
function readVideoInput(
  body: unknown,
  parts: string[],
  current: VideoRecord | null,
  now: Date,
): { snippet: Snippet; status: Status } {
  if (!isRecord(body)) throw badRequest("invalidVideoMetadata", "The request metadata is invalid.", { domain: "youtube.video" });

  let snippet: Snippet;
  if (parts.includes("snippet")) {
    const raw = body.snippet ?? {};
    if (!isRecord(raw)) throw badRequest("invalidVideoMetadata", "The request metadata is invalid.", { domain: "youtube.video" });
    const language = raw.defaultLanguage;
    if (language !== undefined && language !== null && typeof language !== "string") {
      throw badRequest("invalidVideoMetadata", "Invalid value for snippet.defaultLanguage.", { domain: "youtube.video" });
    }
    snippet = {
      title: validateTitle(raw.title),
      description: validateDescription(raw.description),
      tags: validateTags(raw.tags),
      // videos.update replaces the whole snippet, so the category must be restated.
      categoryId: validateCategory(raw.categoryId, current ? null : DEFAULT_CATEGORY_ID),
      defaultLanguage: language || null,
    };
  } else if (current) {
    snippet = current.snippet;
  } else {
    throw badRequest("invalidTitle", "The request metadata specifies an invalid or empty video title.", {
      domain: "youtube.video",
    });
  }

  let status: Status;
  if (parts.includes("status")) {
    const raw = body.status ?? {};
    if (!isRecord(raw)) throw badRequest("invalidVideoMetadata", "The request metadata is invalid.", { domain: "youtube.video" });
    const privacy = raw.privacyStatus ?? current?.status.privacyStatus ?? "public";
    if (typeof privacy !== "string" || !PRIVACY.includes(privacy as Privacy)) {
      throw badRequest("invalidVideoMetadata", "Invalid value for status.privacyStatus.", {
        domain: "youtube.video",
        location: "body.status.privacyStatus",
        locationType: "other",
      });
    }
    let publishAt: string | null = null;
    if (raw.publishAt !== undefined && raw.publishAt !== null) {
      const time = typeof raw.publishAt === "string" ? Date.parse(raw.publishAt) : Number.NaN;
      if (Number.isNaN(time) || privacy !== "private" || time <= now.getTime()) {
        throw badRequest("invalidPublishAt", "The request metadata specifies an invalid scheduled publishing time.", {
          domain: "youtube.video",
          location: "body.status.publishAt",
          locationType: "other",
        });
      }
      publishAt = new Date(time).toISOString();
    }
    const license = raw.license ?? "youtube";
    if (license !== "youtube" && license !== "creativeCommon") {
      throw badRequest("invalidVideoMetadata", "Invalid value for status.license.", { domain: "youtube.video" });
    }
    status = {
      privacyStatus: privacy as Privacy,
      publishAt,
      selfDeclaredMadeForKids: optionalBoolean(
        raw.selfDeclaredMadeForKids,
        "status.selfDeclaredMadeForKids",
        current?.status.selfDeclaredMadeForKids ?? false,
      ),
      embeddable: optionalBoolean(raw.embeddable, "status.embeddable", true),
      license,
      publicStatsViewable: optionalBoolean(raw.publicStatsViewable, "status.publicStatsViewable", true),
    };
  } else {
    status = current?.status ?? {
      privacyStatus: "public",
      publishAt: null,
      selfDeclaredMadeForKids: false,
      embeddable: true,
      license: "youtube",
      publicStatsViewable: true,
    };
  }
  return { snippet, status };
}

type ContentRange =
  | { kind: "whole" }
  | { kind: "query"; total: number | null }
  | { kind: "chunk"; start: number; end: number; total: number | null };

function parseContentRange(header: string | undefined): ContentRange {
  if (header === undefined) return { kind: "whole" };
  const total = (value: string) => (value === "*" ? null : Number(value));
  const query = /^bytes \*\/(\d+|\*)$/.exec(header.trim());
  if (query) return { kind: "query", total: total(query[1]) };
  const chunk = /^bytes (\d+)-(\d+)\/(\d+|\*)$/.exec(header.trim());
  if (chunk && Number(chunk[2]) >= Number(chunk[1])) {
    return { kind: "chunk", start: Number(chunk[1]), end: Number(chunk[2]), total: total(chunk[3]) };
  }
  throw badRequest("badContent", `Invalid Content-Range header: ${header}`, { location: "Content-Range", locationType: "header" });
}

async function sha256OfFile(path: string): Promise<string> {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk as Buffer);
  return hash.digest("hex");
}

function sniffImage(bytes: Uint8Array): string | null {
  if (bytes.length >= 8 && Buffer.from(bytes.subarray(0, 8)).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]))) {
    return "image/png";
  }
  if (bytes.length >= 3 && bytes[0] === 0xff && bytes[1] === 0xd8 && bytes[2] === 0xff) return "image/jpeg";
  return null;
}

/** Solid-color PNG used where real YouTube would serve generated frames or avatars. */
function placeholderPng(width: number, height: number, rgb: [number, number, number]): Buffer {
  const row = Buffer.alloc(1 + width * 3);
  for (let x = 0; x < width; x++) row.set(rgb, 1 + x * 3);
  const chunk = (type: string, data: Buffer) => {
    const length = Buffer.alloc(4);
    length.writeUInt32BE(data.length);
    const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(body));
    return Buffer.concat([length, body, crc]);
  };
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header.set([8, 2, 0, 0, 0], 8);
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", header),
    chunk("IDAT", deflateSync(Buffer.concat(Array.from({ length: height }, () => row)))),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

const DAY_MS = 86_400_000;
const HOUR_MS = 3_600_000;
const ANALYTICS_METRICS = ["views", "estimatedMinutesWatched"];
const REPORT_TYPES = ["channel_reach_basic_a1"];
const REACH_COLUMNS = "date,channel_id,video_id,video_thumbnail_impressions,video_thumbnail_impressions_ctr";

function hashNumber(text: string): number {
  return createHash("sha256").update(text).digest().readUInt32BE(0);
}

function isDate(value: unknown): value is string {
  return typeof value === "string" && /^\d{4}-\d{2}-\d{2}$/.test(value) && !Number.isNaN(Date.parse(`${value}T00:00:00Z`));
}

function addDays(date: string, days: number): string {
  return new Date(Date.parse(`${date}T00:00:00Z`) + days * DAY_MS).toISOString().slice(0, 10);
}

/** Midnight Pacific time, standard or daylight, at the start of `date`. */
function pacificMidnight(date: string): number {
  const utcMidnight = Date.parse(`${date}T00:00:00Z`);
  for (const hours of [7, 8]) {
    const at = utcMidnight + hours * HOUR_MS;
    if (pacificDay(new Date(at)) === date && pacificDay(new Date(at - 1)) !== date) return at;
  }
  return utcMidnight + 8 * HOUR_MS;
}

/**
 * Made-up but steady numbers for a day nobody set: impressions vary by video and day,
 * and the click rate and watch time follow what the video showed at noon that day (by
 * now, today), so a better title or thumbnail does better. `null` before the upload.
 */
function syntheticNumbers(video: VideoRecord, date: string, now: number): DayNumbers | null {
  const start = pacificMidnight(date);
  const end = pacificMidnight(addDays(date, 1));
  if (end <= video.uploadedAtMs || start > now) return null;
  const noon = Math.min(start + (end - start) / 2, now);
  const history = video.history ?? [];
  const showing = [...history].reverse().find((entry) => entry.at <= noon) ??
    history[0] ?? { at: 0, title: video.snippet.title, thumbnail: video.thumbnail?.sha256 ?? null };
  // From 0.6 to 1.4.
  const appeal = 0.6 + (hashNumber(`${showing.title}|${showing.thumbnail ?? ""}`) % 81) / 100;
  const elapsed = Math.min(1, (now - Math.max(start, video.uploadedAtMs)) / (end - start));
  const impressions = Math.round((400 + (hashNumber(`${video.id}:${date}`) % 400)) * elapsed);
  const ctr = Math.round(400 * appeal) / 10000;
  const views = Math.round(impressions * ctr * 1.25);
  return { views, minutesWatched: Math.round(views * (1.5 + appeal) * 10) / 10, impressions, ctr };
}

const VIDEO_THUMBNAILS = { default: [120, 90], medium: [320, 180], high: [480, 360] } as const;
const THUMBNAIL_FILES: Record<string, readonly [number, number]> = {
  "default.jpg": VIDEO_THUMBNAILS.default,
  "mqdefault.jpg": VIDEO_THUMBNAILS.medium,
  "hqdefault.jpg": VIDEO_THUMBNAILS.high,
};

export function createYoutubeEmulator(options: YoutubeEmulatorOptions = {}): EmulatorDefinition<YoutubeState> {
  return defineEmulator({
    name: "youtube",
    state: (): YoutubeState => ({
      channels: {},
      videos: {},
      sessions: {},
      uploads: [],
      nextSeq: 1,
      quota: { day: "", used: 0 },
      faults: [],
      analytics: {},
      analyticsVersions: {},
      jobs: {},
    }),

    setup({ app, state, baseUrl, signal, onDispose }) {
      const googleUrl = (options.googleUrl ?? process.env.GOOGLE_EMULATOR_URL ?? "http://127.0.0.1:4002").replace(/\/$/, "");
      const processingMs = options.processingMs ?? numberFromEnv("YOUTUBE_EMULATOR_PROCESSING_MS") ?? 3000;
      const dailyQuota = options.dailyQuota ?? numberFromEnv("YOUTUBE_EMULATOR_DAILY_QUOTA") ?? 10000;
      const authCacheMs = options.authCacheMs ?? 5000;

      // Upload spool and custom thumbnails; state only records sizes and hashes.
      const spool = mkdtempSync(join(tmpdir(), "rushes-youtube-emulator-"));
      onDispose(() => rmSync(spool, { recursive: true, force: true }));
      const sessionFile = (id: string) => join(spool, `upload-${id}.bin`);
      const thumbnailFile = (id: string) => join(spool, `thumbnail-${id}`);

      const tokenCache = new Map<string, { user: GoogleUser | null; expires: number }>();

      // Requests to one upload session run one at a time, so a status query racing an
      // aborted chunk sees what that chunk persisted.
      const sessionQueues = new Map<string, Promise<void>>();

      function exclusive<T>(id: string, task: () => Promise<T>): Promise<T> {
        const run = (sessionQueues.get(id) ?? Promise.resolve()).then(task);
        const tail = run.then(
          () => undefined,
          () => undefined,
        );
        sessionQueues.set(id, tail);
        void tail.then(() => {
          if (sessionQueues.get(id) === tail) sessionQueues.delete(id);
        });
        return run;
      }

      async function lookupUser(token: string): Promise<GoogleUser | null> {
        const now = Date.now();
        const cached = tokenCache.get(token);
        if (cached && cached.expires > now) return cached.user;
        let response: Response;
        try {
          response = await fetch(`${googleUrl}/oauth2/v2/userinfo`, {
            headers: { Authorization: `Bearer ${token}` },
            signal,
          });
        } catch {
          throw new YoutubeError(503, "backendError", `Cannot reach the Google emulator at ${googleUrl} to validate the access token.`);
        }
        const body: unknown = response.ok ? await response.json().catch(() => undefined) : null;
        if (body === undefined) {
          throw new YoutubeError(503, "backendError", `The Google emulator at ${googleUrl} returned an invalid userinfo response.`);
        }
        const user = isRecord(body) && typeof body.email === "string" ? (body as unknown as GoogleUser) : null;
        for (const [key, entry] of tokenCache) if (entry.expires <= now) tokenCache.delete(key);
        tokenCache.set(token, { user, expires: now + authCacheMs });
        return user;
      }

      async function authenticate(c: Ctx): Promise<ChannelRecord> {
        const match = /^Bearer\s+(\S+)$/i.exec(c.req.header("Authorization") ?? "");
        if (!match) {
          throw new YoutubeError(401, "required", "Login Required.", {
            location: "Authorization",
            locationType: "header",
            summary:
              "Request is missing required authentication credential. Expected OAuth 2 access token, login cookie or other valid authentication credential. See https://developers.google.com/identity/sign-in/web/devconsole-project.",
          });
        }
        const user = GOOGLE_EMULATOR_TOKEN.test(match[1]) ? await lookupUser(match[1]) : null;
        if (!user) {
          throw new YoutubeError(401, "authError", "Invalid Credentials", {
            location: "Authorization",
            locationType: "header",
            summary: AUTH_ERROR_MESSAGE,
          });
        }
        return ensureChannel(user);
      }

      function ensureChannel(user: GoogleUser): ChannelRecord {
        const existing = Object.values(state.channels).find((channel) => channel.ownerEmail === user.email);
        if (existing) return existing;
        // Derived from the email so a restarted emulator hands out the same channel id.
        const suffix = createHash("sha256").update(`channel:${user.email}`).digest("base64url").slice(0, 22);
        const channel: ChannelRecord = {
          id: `UC${suffix}`,
          ownerEmail: user.email,
          title: user.name || user.email.split("@")[0],
          customUrl: `@${user.email.split("@")[0].toLowerCase()}`,
          thumbnailUrl: user.picture || null,
          publishedAt: new Date().toISOString(),
        };
        state.channels[channel.id] = channel;
        return channel;
      }

      function chargeQuota(cost: number): void {
        const day = pacificDay(new Date());
        if (state.quota.day !== day) state.quota = { day, used: 0 };
        if (state.quota.used + cost > dailyQuota) {
          throw new YoutubeError(403, "quotaExceeded", REASON_DEFAULTS.quotaExceeded.message);
        }
        state.quota.used += cost;
      }

      function takeFault(method: string, path: string, kind: "error" | "partial"): Fault | null {
        const index = state.faults.findIndex(
          (fault) =>
            (fault.method === null || fault.method === method) &&
            path.startsWith(fault.path) &&
            (kind === "error" ? fault.status !== null : fault.persistBytes !== null),
        );
        if (index < 0) return null;
        const fault = state.faults[index];
        fault.remaining -= 1;
        if (fault.remaining <= 0) state.faults.splice(index, 1);
        return fault;
      }

      function uploadsPlaylistId(channel: ChannelRecord): string {
        return `UU${channel.id.slice(2)}`;
      }

      function channelOf(video: VideoRecord): ChannelRecord {
        return state.channels[video.channelId];
      }

      /** Notes what the video shows now, when its title or thumbnail changed. */
      function recordShowing(video: VideoRecord, at: number): void {
        const history = (video.history ??= []);
        const showing: Showing = { at, title: video.snippet.title, thumbnail: video.thumbnail?.sha256 ?? null };
        const last = history[history.length - 1];
        if (!last || last.title !== showing.title || last.thumbnail !== showing.thumbnail) history.push(showing);
      }

      /** The numbers a test set for that day, else made-up ones while the video is public. */
      function numbersOf(video: VideoRecord, date: string, now: number): DayNumbers | null {
        const set = state.analytics[video.id]?.[date];
        if (set) return set;
        return effectivePrivacy(video, now) === "public" ? syntheticNumbers(video, date, now) : null;
      }

      function ownJob(id: string, viewer: ChannelRecord): ReportingJob {
        const job = state.jobs[id];
        if (!job || job.channelId !== viewer.id) {
          throw new YoutubeError(404, "notFound", "Requested entity was not found.");
        }
        return job;
      }

      function renderJob(job: ReportingJob) {
        return { id: job.id, reportTypeId: job.reportTypeId, name: job.name, createTime: job.createTime };
      }

      /**
       * The job's daily reports: from 30 days before it was created to yesterday, each
       * made an hour after its day ended, plus a version of a day each time a test set
       * numbers for it.
       */
      function reportsOf(job: ReportingJob, now: number) {
        const firstDay = addDays(pacificDay(new Date(Date.parse(job.createTime))), -30);
        const yesterday = addDays(pacificDay(new Date(now)), -1);
        const days = new Map<string, number[]>();
        for (let date = firstDay; date <= yesterday; date = addDays(date, 1)) {
          days.set(date, [Math.min(now, pacificMidnight(addDays(date, 1)) + HOUR_MS)]);
        }
        for (const [date, versions] of Object.entries(state.analyticsVersions[job.channelId] ?? {})) {
          days.set(date, [...(days.get(date) ?? []), ...versions]);
        }
        return [...days]
          .flatMap(([date, versions]) =>
            versions.map((created, version) => ({
              id: `${job.id}.${date.replaceAll("-", "")}.${version}`,
              jobId: job.id,
              startTime: new Date(pacificMidnight(date)).toISOString(),
              endTime: new Date(pacificMidnight(addDays(date, 1))).toISOString(),
              createTime: new Date(created).toISOString(),
              downloadUrl: `${baseUrl}/youtubereporting/v1/media/${job.id}/${date}?version=${version}`,
            })),
          )
          .sort((a, b) => a.startTime.localeCompare(b.startTime) || a.createTime.localeCompare(b.createTime));
      }

      function effectivePrivacy(video: VideoRecord, now: number): Privacy {
        const { publishAt, privacyStatus } = video.status;
        return publishAt && Date.parse(publishAt) <= now ? "public" : privacyStatus;
      }

      function canView(video: VideoRecord, viewer: ChannelRecord, now: number): boolean {
        return video.channelId === viewer.id || effectivePrivacy(video, now) !== "private";
      }

      function findOwnedVideo(id: string | undefined, viewer: ChannelRecord, action: string): VideoRecord {
        const video = id ? state.videos[id] : undefined;
        if (!video || !canView(video, viewer, Date.now())) throw videoNotFound();
        if (video.channelId !== viewer.id) {
          throw new YoutubeError(403, "forbidden", `The video that you are trying to ${action} cannot be ${action}d. The request might not be properly authorized.`, {
            domain: "youtube.video",
          });
        }
        return video;
      }

      function videoThumbnails(video: VideoRecord) {
        const version = video.thumbnail ? `?v=${video.thumbnail.sha256.slice(0, 12)}` : "";
        const files = { default: "default.jpg", medium: "mqdefault.jpg", high: "hqdefault.jpg" } as const;
        return Object.fromEntries(
          (Object.keys(files) as Array<keyof typeof files>).map((size) => [
            size,
            {
              url: `${baseUrl}/vi/${video.id}/${files[size]}${version}`,
              width: VIDEO_THUMBNAILS[size][0],
              height: VIDEO_THUMBNAILS[size][1],
            },
          ]),
        );
      }

      function renderVideo(video: VideoRecord, parts: string[], viewer: ChannelRecord, now: number) {
        const channel = channelOf(video);
        const elapsed = now - video.uploadedAtMs;
        const processed = elapsed >= processingMs;
        const privacy = effectivePrivacy(video, now);
        const isOwner = video.channelId === viewer.id;
        const resource: Record<string, unknown> = { kind: "youtube#video", etag: "", id: video.id };
        if (parts.includes("snippet")) {
          resource.snippet = {
            publishedAt: video.publishedAt,
            channelId: video.channelId,
            title: video.snippet.title,
            description: video.snippet.description,
            thumbnails: videoThumbnails(video),
            channelTitle: channel.title,
            ...(video.snippet.tags.length ? { tags: video.snippet.tags } : {}),
            categoryId: video.snippet.categoryId,
            liveBroadcastContent: "none",
            ...(video.snippet.defaultLanguage ? { defaultLanguage: video.snippet.defaultLanguage } : {}),
            localized: { title: video.snippet.title, description: video.snippet.description },
          };
        }
        if (parts.includes("status")) {
          resource.status = {
            uploadStatus: processed ? "processed" : "uploaded",
            privacyStatus: privacy,
            ...(video.status.publishAt && privacy === "private" ? { publishAt: video.status.publishAt } : {}),
            license: video.status.license,
            embeddable: video.status.embeddable,
            publicStatsViewable: video.status.publicStatsViewable,
            madeForKids: video.status.selfDeclaredMadeForKids,
            ...(isOwner ? { selfDeclaredMadeForKids: video.status.selfDeclaredMadeForKids } : {}),
          };
        }
        if (parts.includes("processingDetails") && isOwner) {
          const availability = processed ? "available" : "inProgress";
          resource.processingDetails = {
            processingStatus: processed ? "succeeded" : "processing",
            ...(processed
              ? {}
              : {
                  processingProgress: {
                    partsTotal: "1000",
                    partsProcessed: String(Math.floor((1000 * elapsed) / Math.max(1, processingMs))),
                    timeLeftMs: String(processingMs - elapsed),
                  },
                }),
            fileDetailsAvailability: "available",
            processingIssuesAvailability: "available",
            tagSuggestionsAvailability: availability,
            editorSuggestionsAvailability: availability,
            thumbnailsAvailability: availability,
          };
        }
        if (parts.includes("statistics")) {
          resource.statistics = { viewCount: "0", likeCount: "0", favoriteCount: "0", commentCount: "0" };
        }
        resource.etag = etag(resource);
        return resource;
      }

      function renderChannel(channel: ChannelRecord, parts: string[], now: number) {
        const avatar = channel.thumbnailUrl ?? `${baseUrl}/yt3/${channel.id}.png`;
        const resource: Record<string, unknown> = { kind: "youtube#channel", etag: "", id: channel.id };
        if (parts.includes("snippet")) {
          resource.snippet = {
            title: channel.title,
            description: "",
            customUrl: channel.customUrl,
            publishedAt: channel.publishedAt,
            thumbnails: {
              default: { url: avatar, width: 88, height: 88 },
              medium: { url: avatar, width: 240, height: 240 },
              high: { url: avatar, width: 800, height: 800 },
            },
            localized: { title: channel.title, description: "" },
          };
        }
        if (parts.includes("contentDetails")) {
          resource.contentDetails = { relatedPlaylists: { likes: "", uploads: uploadsPlaylistId(channel) } };
        }
        if (parts.includes("statistics")) {
          const publicVideos = Object.values(state.videos).filter(
            (video) => video.channelId === channel.id && effectivePrivacy(video, now) === "public",
          );
          resource.statistics = {
            viewCount: "0",
            subscriberCount: "0",
            hiddenSubscriberCount: false,
            videoCount: String(publicVideos.length),
          };
        }
        resource.etag = etag(resource);
        return resource;
      }

      function listResponse(kind: string, items: unknown[], resultsPerPage: number, extra: Record<string, unknown> = {}) {
        return {
          kind,
          etag: etag(items),
          ...extra,
          pageInfo: { totalResults: items.length, resultsPerPage },
          items,
        };
      }

      async function completeUpload(c: Ctx, session: SessionRecord, viewer: ChannelRecord): Promise<Response> {
        const path = sessionFile(session.id);
        const sha256 = await sha256OfFile(path);
        await rm(path, { force: true });
        const now = new Date();
        const video: VideoRecord = {
          id: randomBytes(8).toString("base64url").slice(0, 11),
          seq: state.nextSeq++,
          channelId: session.channelId,
          snippet: session.snippet,
          status: session.status,
          publishedAt: now.toISOString(),
          uploadedAtMs: now.getTime(),
          thumbnail: null,
          history: [],
        };
        recordShowing(video, now.getTime());
        state.videos[video.id] = video;
        state.uploads.push({
          videoId: video.id,
          channelId: video.channelId,
          bytes: session.total,
          sha256,
          contentType: session.contentType,
          snippet: video.snippet,
          status: video.status,
          completedAt: now.toISOString(),
        });
        session.videoId = video.id;
        return c.json(renderVideo(video, session.parts, viewer, now.getTime()), 200);
      }

      /** 308 "Resume Incomplete" with the persisted byte range, as Google's upload servers answer. */
      function resumeIncomplete(c: Ctx, session: SessionRecord): Response {
        const headers: Record<string, string> = { "X-GUploader-UploadID": session.id };
        if (session.received > 0) headers.Range = `bytes=0-${session.received - 1}`;
        return c.body(null, 308, headers);
      }

      // ---------- Fault injection (applies to every API route) ----------

      app.use(async (c, next) => {
        if (c.req.path.startsWith("/_emulator/")) return next();
        const fault = takeFault(c.req.method, c.req.path, "error");
        if (!fault || fault.status === null) return next();
        await discardBody(c);
        const reason = fault.reason ?? (fault.status >= 500 ? "backendError" : "badRequest");
        const message = fault.message ?? REASON_DEFAULTS[reason]?.message ?? "Injected fault";
        return errorResponse(c, new YoutubeError(fault.status, reason, message));
      });

      // ---------- Channels ----------

      app.get(
        "/youtube/v3/channels",
        handle(async (c) => {
          const viewer = await authenticate(c);
          const parts = parseParts(c, ["id", "snippet", "contentDetails", "statistics"]);
          let channels: ChannelRecord[];
          if (c.req.query("mine") === "true") {
            channels = [viewer];
          } else if (c.req.query("id")) {
            channels = c.req
              .query("id")!
              .split(",")
              .map((id) => state.channels[id.trim()])
              .filter(Boolean);
          } else {
            throw badRequest(
              "missingRequiredParameter",
              "No filter selected. Expected one of: forUsername, managedByMe, categoryId, mine, mySubscribers, id, idParam",
              { domain: "youtube.parameter" },
            );
          }
          chargeQuota(COST.list);
          const now = Date.now();
          return c.json(listResponse("youtube#channelListResponse", channels.map((ch) => renderChannel(ch, parts, now)), 5));
        }),
      );

      // ---------- Resumable video upload ----------

      app.post(
        "/upload/youtube/v3/videos",
        handle(async (c) => {
          const viewer = await authenticate(c);
          if (c.req.query("uploadType") !== "resumable") {
            await discardBody(c);
            throw badRequest("invalidUploadType", "This emulator only supports uploadType=resumable.", { location: "uploadType" });
          }
          const parts = parseParts(c, ["snippet", "status"]);
          const declared = c.req.header("X-Upload-Content-Length");
          const total = declared === undefined ? Number.NaN : Number(declared);
          if (!Number.isSafeInteger(total) || total <= 0 || total > MAX_VIDEO_BYTES) {
            throw badRequest(
              declared === undefined ? "required" : "badContent",
              declared === undefined
                ? "Required header: X-Upload-Content-Length"
                : `Invalid X-Upload-Content-Length: ${declared}`,
              { location: "X-Upload-Content-Length", locationType: "header" },
            );
          }
          const contentType = c.req.header("X-Upload-Content-Type") ?? "application/octet-stream";
          if (!contentType.startsWith("video/") && contentType !== "application/octet-stream") {
            throw badRequest("invalidContentType", `Media type '${contentType}' is not supported. Valid media types: [video/*, application/octet-stream]`, {
              location: "X-Upload-Content-Type",
              locationType: "header",
            });
          }
          const { snippet, status } = readVideoInput(await readJson(c), parts, null, new Date());
          chargeQuota(COST.insert);
          const session: SessionRecord = {
            id: `AE${randomBytes(30).toString("base64url")}`,
            channelId: viewer.id,
            parts,
            snippet,
            status,
            contentType,
            total,
            received: 0,
            videoId: null,
            createdAt: new Date().toISOString(),
          };
          state.sessions[session.id] = session;
          await writeFile(sessionFile(session.id), Buffer.alloc(0));
          const location = `${baseUrl}/upload/youtube/v3/videos?uploadType=resumable&upload_id=${session.id}&part=${parts.join(",")}`;
          return c.body(null, 200, { Location: location, "X-GUploader-UploadID": session.id });
        }),
      );

      // The session URL is the credential for the transfer: no Authorization header needed.
      app.put(
        "/upload/youtube/v3/videos",
        handle(async (c) => {
          const uploadId = c.req.query("upload_id");
          const session = uploadId ? state.sessions[uploadId] : undefined;
          if (!session) {
            await discardBody(c);
            throw new YoutubeError(404, "notFound", "The upload session does not exist or has expired.", { location: "upload_id" });
          }
          return exclusive(session.id, async () => {
            const owner = state.channels[session.channelId];
            if (session.videoId) {
              // Already complete: repeat the final answer, unless the video was deleted since.
              await discardBody(c);
              const video = state.videos[session.videoId];
              if (!video) throw videoNotFound();
              return c.json(renderVideo(video, session.parts, owner, Date.now()), 200);
            }
            // Spooled bytes do not survive a restart or a restored snapshot.
            if (!existsSync(sessionFile(session.id))) {
              await discardBody(c);
              delete state.sessions[session.id];
              throw new YoutubeError(404, "notFound", "The upload session does not exist or has expired.", { location: "upload_id" });
            }

            const range = parseContentRange(c.req.header("Content-Range"));
            if (range.kind !== "whole" && range.total !== null && range.total !== session.total) {
              await discardBody(c);
              throw badRequest("badContent", `Content-Range total ${range.total} does not match X-Upload-Content-Length ${session.total}.`, {
                location: "Content-Range",
                locationType: "header",
              });
            }
            if (range.kind === "query") {
              await discardBody(c);
              return resumeIncomplete(c, session);
            }
            const start = range.kind === "chunk" ? range.start : 0;
            const limit = range.kind === "chunk" ? range.end + 1 : session.total;
            if (start > session.received || limit > session.total) {
              await discardBody(c);
              throw badRequest("badContent", `Content-Range bytes ${start}-${limit - 1} do not continue the ${session.received} bytes received so far.`, {
                location: "Content-Range",
                locationType: "header",
              });
            }

            // Append the part of the body past what is already persisted.
            const path = sessionFile(session.id);
            const before = session.received;
            const file = await open(path, "a");
            let offset = start;
            let appended = 0;
            let interrupted = false;
            try {
              for await (const piece of c.req.raw.body ?? []) {
                const bytes = piece as Uint8Array;
                const skip = Math.min(bytes.length, Math.max(0, before + appended - offset));
                const room = Math.max(0, limit - (before + appended));
                const fresh = bytes.subarray(skip, skip + room);
                offset += bytes.length;
                if (fresh.length) {
                  await file.write(fresh);
                  appended += fresh.length;
                }
              }
            } catch {
              interrupted = true;
            } finally {
              await file.close();
            }

            if (!interrupted && offset !== limit) {
              await truncate(path, before);
              throw badRequest("badContent", `Content-Range declares ${limit - start} bytes but the request body has ${offset - start}.`, {
                location: "Content-Range",
                locationType: "header",
              });
            }

            let persisted = before + appended;
            const partial = takeFault("PUT", c.req.path, "partial");
            if (partial?.persistBytes !== null && partial?.persistBytes !== undefined) {
              persisted = Math.min(persisted, before + partial.persistBytes);
            }
            if (persisted < session.total) {
              persisted = Math.max(before, persisted - (persisted % UPLOAD_GRANULARITY));
            }
            if (persisted !== before + appended) await truncate(path, persisted);
            session.received = persisted;

            if (session.received === session.total) return completeUpload(c, session, owner);
            return resumeIncomplete(c, session);
          });
        }),
      );

      // ---------- Videos ----------

      app.get(
        "/youtube/v3/videos",
        handle(async (c) => {
          const viewer = await authenticate(c);
          const parts = parseParts(c, ["id", "snippet", "status", "processingDetails", "statistics"]);
          const ids = c.req.query("id");
          if (!ids) {
            throw badRequest("missingRequiredParameter", "No filter selected. Expected one of: myRating, chart, id", {
              domain: "youtube.parameter",
            });
          }
          chargeQuota(COST.list);
          const now = Date.now();
          const items = ids
            .split(",")
            .map((id) => state.videos[id.trim()])
            .filter((video): video is VideoRecord => !!video && canView(video, viewer, now))
            .map((video) => renderVideo(video, parts, viewer, now));
          return c.json(listResponse("youtube#videoListResponse", items, items.length));
        }),
      );

      app.put(
        "/youtube/v3/videos",
        handle(async (c) => {
          const viewer = await authenticate(c);
          const parts = parseParts(c, ["snippet", "status"]);
          const body = await readJson(c);
          const id = isRecord(body) && typeof body.id === "string" ? body.id : undefined;
          const video = findOwnedVideo(id, viewer, "update");
          const { snippet, status } = readVideoInput(body, parts, video, new Date());
          chargeQuota(COST.update);
          video.snippet = snippet;
          video.status = status;
          recordShowing(video, Date.now());
          return c.json(renderVideo(video, parts, viewer, Date.now()));
        }),
      );

      app.delete(
        "/youtube/v3/videos",
        handle(async (c) => {
          const viewer = await authenticate(c);
          const video = findOwnedVideo(c.req.query("id"), viewer, "delete");
          chargeQuota(COST.delete);
          delete state.videos[video.id];
          await rm(thumbnailFile(video.id), { force: true });
          return c.body(null, 204);
        }),
      );

      // ---------- Thumbnails ----------

      app.post(
        "/upload/youtube/v3/thumbnails/set",
        handle(async (c) => {
          const viewer = await authenticate(c);
          const videoId = c.req.query("videoId");
          if (!videoId) {
            await discardBody(c);
            throw badRequest("required", "Required parameter: videoId", { location: "videoId" });
          }
          const video = findOwnedVideo(videoId, viewer, "update");
          const declaredType = (c.req.header("Content-Type") ?? "").split(";")[0].trim().toLowerCase();
          const bytes = new Uint8Array(await c.req.arrayBuffer());
          if (bytes.length === 0) {
            throw badRequest("mediaBodyRequired", "The request does not include the image content.", { domain: "youtube.thumbnail" });
          }
          const sniffed = sniffImage(bytes);
          if (
            !["image/png", "image/jpeg", "application/octet-stream"].includes(declaredType) ||
            sniffed === null ||
            bytes.length > MAX_THUMBNAIL_BYTES
          ) {
            throw badRequest(
              "invalidImage",
              bytes.length > MAX_THUMBNAIL_BYTES
                ? "The provided image is larger than the 2MB limit."
                : "The provided image content is invalid.",
              { domain: "youtube.thumbnail" },
            );
          }
          chargeQuota(COST.thumbnail);
          await writeFile(thumbnailFile(video.id), bytes);
          video.thumbnail = {
            contentType: sniffed,
            bytes: bytes.length,
            sha256: createHash("sha256").update(bytes).digest("hex"),
          };
          recordShowing(video, Date.now());
          const items = [videoThumbnails(video)];
          return c.json({ kind: "youtube#thumbnailSetResponse", etag: etag(items), items });
        }),
      );

      // Stand-ins for i.ytimg.com and yt3.ggpht.com.
      app.get("/vi/:id/:file", async (c) => {
        const video = state.videos[c.req.param("id")];
        const size = THUMBNAIL_FILES[c.req.param("file")];
        if (!video || !size) return c.body(null, 404);
        // The image file is gone after a restart or restored snapshot; fall back to the placeholder.
        const custom = video.thumbnail && (await readFile(thumbnailFile(video.id)).catch(() => null));
        if (video.thumbnail && custom) {
          return c.body(new Uint8Array(custom), 200, { "Content-Type": video.thumbnail.contentType });
        }
        return c.body(new Uint8Array(placeholderPng(size[0], size[1], [0x3f, 0x3f, 0x46])), 200, { "Content-Type": "image/png" });
      });

      app.get("/yt3/:file", (c) => {
        if (!state.channels[c.req.param("file").replace(/\.png$/, "")]) return c.body(null, 404);
        return c.body(new Uint8Array(placeholderPng(88, 88, [0xcc, 0x33, 0x33])), 200, { "Content-Type": "image/png" });
      });

      // ---------- Uploads playlist ----------

      app.get(
        "/youtube/v3/playlistItems",
        handle(async (c) => {
          const viewer = await authenticate(c);
          const parts = parseParts(c, ["id", "snippet", "contentDetails", "status"]);
          const playlistId = c.req.query("playlistId");
          if (!playlistId) {
            throw badRequest("missingRequiredParameter", "No filter selected. Expected one of: id, playlistId", {
              domain: "youtube.parameter",
            });
          }
          const channel = Object.values(state.channels).find((ch) => uploadsPlaylistId(ch) === playlistId);
          if (!channel) {
            throw new YoutubeError(404, "playlistNotFound", "The playlist identified with the request's <code>playlistId</code> parameter cannot be found.", {
              domain: "youtube.playlistItem",
              location: "playlistId",
            });
          }
          const maxResults = Number(c.req.query("maxResults") ?? 5);
          if (!Number.isInteger(maxResults) || maxResults < 0 || maxResults > 50) {
            throw badRequest("invalidParameter", `Invalid value '${c.req.query("maxResults")}'. Values must be within the range: [0, 50]`, {
              location: "maxResults",
            });
          }
          const offset = decodePageToken(c.req.query("pageToken"));
          chargeQuota(COST.list);
          const now = Date.now();
          const videos = Object.values(state.videos)
            .filter((video) => video.channelId === channel.id && canView(video, viewer, now))
            .sort((a, b) => b.seq - a.seq);
          const items = videos.slice(offset, offset + maxResults).map((video, index) => {
            const item: Record<string, unknown> = {
              kind: "youtube#playlistItem",
              etag: "",
              id: Buffer.from(`${playlistId}.${video.id}`).toString("base64url"),
            };
            if (parts.includes("snippet")) {
              item.snippet = {
                publishedAt: video.publishedAt,
                channelId: channel.id,
                title: video.snippet.title,
                description: video.snippet.description,
                thumbnails: videoThumbnails(video),
                channelTitle: channel.title,
                playlistId,
                position: offset + index,
                resourceId: { kind: "youtube#video", videoId: video.id },
                videoOwnerChannelTitle: channel.title,
                videoOwnerChannelId: channel.id,
              };
            }
            if (parts.includes("contentDetails")) {
              item.contentDetails = { videoId: video.id, videoPublishedAt: video.publishedAt };
            }
            if (parts.includes("status")) item.status = { privacyStatus: effectivePrivacy(video, now) };
            item.etag = etag(item);
            return item;
          });
          const response = listResponse("youtube#playlistItemListResponse", items, maxResults, {
            ...(offset + maxResults < videos.length && maxResults > 0 ? { nextPageToken: encodePageToken(offset + maxResults) } : {}),
            ...(offset > 0 ? { prevPageToken: encodePageToken(Math.max(0, offset - maxResults)) } : {}),
          });
          response.pageInfo.totalResults = videos.length;
          return c.json(response);
        }),
      );

      // ---------- YouTube Analytics API ----------

      app.get(
        "/youtubeAnalytics/v2/reports",
        handle(async (c) => {
          const viewer = await authenticate(c);
          const ids = c.req.query("ids");
          if (ids !== "channel==MINE" && ids !== `channel==${viewer.id}`) {
            throw new YoutubeError(403, "forbidden", "Forbidden");
          }
          const startDate = c.req.query("startDate");
          const endDate = c.req.query("endDate");
          if (!isDate(startDate) || !isDate(endDate) || endDate < startDate) {
            throw badRequest("badRequest", "The start and end dates must be valid YYYY-MM-DD dates, the end on or after the start.");
          }
          const metrics = (c.req.query("metrics") ?? "")
            .split(",")
            .map((metric) => metric.trim())
            .filter(Boolean);
          const unknown = metrics.find((metric) => !ANALYTICS_METRICS.includes(metric));
          if (!metrics.length || unknown) {
            throw badRequest("badRequest", `Unknown identifier (${unknown ?? ""}) given in field parameters.metrics.`);
          }
          if (c.req.query("dimensions") !== "day") {
            throw badRequest("badRequest", "The query is not supported. This emulator only reports by day.");
          }
          const filter = /^video==([A-Za-z0-9_-]+)$/.exec(c.req.query("filters") ?? "");
          if (!filter) throw badRequest("badRequest", "The query is not supported. This emulator needs filters=video==<id>.");
          const video = state.videos[filter[1]];
          if (!video || video.channelId !== viewer.id) throw new YoutubeError(403, "forbidden", "Forbidden");
          const now = Date.now();
          const rows: Array<Array<string | number>> = [];
          for (let date = startDate; date <= endDate; date = addDays(date, 1)) {
            const numbers = numbersOf(video, date, now);
            if (!numbers) continue;
            rows.push([date, ...metrics.map((metric) => (metric === "views" ? numbers.views : numbers.minutesWatched))]);
          }
          return c.json({
            kind: "youtubeAnalytics#resultTable",
            columnHeaders: [
              { name: "day", columnType: "DIMENSION", dataType: "STRING" },
              ...metrics.map((name) => ({ name, columnType: "METRIC", dataType: name === "views" ? "INTEGER" : "FLOAT" })),
            ],
            rows,
          });
        }),
      );

      // ---------- YouTube Reporting API ----------

      app.get(
        "/youtubereporting/v1/jobs",
        handle(async (c) => {
          const viewer = await authenticate(c);
          const jobs = Object.values(state.jobs)
            .filter((job) => job.channelId === viewer.id)
            .map(renderJob);
          return c.json(jobs.length ? { jobs } : {});
        }),
      );

      app.post(
        "/youtubereporting/v1/jobs",
        handle(async (c) => {
          const viewer = await authenticate(c);
          const body = await readJson(c);
          const type = isRecord(body) ? body.reportTypeId : undefined;
          if (typeof type !== "string" || !REPORT_TYPES.includes(type)) {
            throw badRequest("badRequest", `Unknown report type: ${String(type)}.`);
          }
          if (Object.values(state.jobs).some((job) => job.channelId === viewer.id && job.reportTypeId === type)) {
            throw new YoutubeError(409, "alreadyExists", "A job with this report type already exists.");
          }
          const job: ReportingJob = {
            id: randomBytes(12).toString("hex"),
            channelId: viewer.id,
            reportTypeId: type,
            name: isRecord(body) && typeof body.name === "string" ? body.name : "",
            createTime: new Date().toISOString(),
          };
          state.jobs[job.id] = job;
          return c.json(renderJob(job));
        }),
      );

      app.get(
        "/youtubereporting/v1/jobs/:id/reports",
        handle(async (c) => {
          const viewer = await authenticate(c);
          const job = ownJob(c.req.param("id"), viewer);
          const raw = c.req.query("startTimeAtOrAfter");
          const since = raw === undefined ? Number.NEGATIVE_INFINITY : Date.parse(raw);
          if (Number.isNaN(since)) throw badRequest("badRequest", `Invalid value for startTimeAtOrAfter: ${raw}.`);
          const reports = reportsOf(job, Date.now()).filter((report) => Date.parse(report.startTime) >= since);
          return c.json(reports.length ? { reports } : {});
        }),
      );

      app.get(
        "/youtubereporting/v1/media/:jobId/:date",
        handle(async (c) => {
          const viewer = await authenticate(c);
          ownJob(c.req.param("jobId"), viewer);
          const date = c.req.param("date");
          if (!isDate(date)) throw new YoutubeError(404, "notFound", "Requested entity was not found.");
          const now = Date.now();
          const lines = [REACH_COLUMNS];
          const videos = Object.values(state.videos)
            .filter((video) => video.channelId === viewer.id)
            .sort((a, b) => a.seq - b.seq);
          for (const video of videos) {
            const numbers = numbersOf(video, date, now);
            if (!numbers || numbers.impressions <= 0) continue;
            lines.push([date.replaceAll("-", ""), video.channelId, video.id, numbers.impressions, numbers.ctr.toFixed(4)].join(","));
          }
          return c.body(`${lines.join("\n")}\n`, 200, { "Content-Type": "application/octet-stream" });
        }),
      );

      // ---------- Inspection and fault injection for tests ----------

      app.get("/_emulator/uploads", (c) => c.json({ uploads: state.uploads }));

      // What a video has now and every title and thumbnail it showed.
      app.get("/_emulator/videos/:id", (c) => {
        const video = state.videos[c.req.param("id")];
        if (!video) return c.json({ error: "no such video" }, 404);
        return c.json({ id: video.id, snippet: video.snippet, status: video.status, thumbnail: video.thumbnail, history: video.history ?? [] });
      });

      // Sets what YouTube Analytics and the reach report say about a video on some days.
      app.post(
        "/_emulator/analytics",
        handle(async (c) => {
          const body = await readJson(c);
          if (!isRecord(body) || typeof body.videoId !== "string" || !Array.isArray(body.days)) {
            throw badRequest("invalid", "Send a videoId and a list of days.");
          }
          const video = state.videos[body.videoId];
          if (!video) throw videoNotFound();
          const days = (state.analytics[video.id] ??= {});
          const versions = (state.analyticsVersions[video.channelId] ??= {});
          const now = Date.now();
          for (const day of body.days as unknown[]) {
            if (!isRecord(day) || !isDate(day.date)) throw badRequest("invalid", "Each day needs a date (YYYY-MM-DD).");
            const number = (key: string) => {
              const value = day[key];
              return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : 0;
            };
            days[day.date] = {
              views: number("views"),
              minutesWatched: number("minutesWatched"),
              impressions: number("impressions"),
              ctr: Math.min(1, number("ctr")),
            };
            (versions[day.date] ??= []).push(now);
          }
          return c.json({ videoId: video.id, days }, 201);
        }),
      );

      app.post(
        "/_emulator/faults",
        handle(async (c) => {
          const body = await readJson(c);
          if (!isRecord(body) || typeof body.path !== "string") throw badRequest("invalid", "A fault needs a path.");
          const status = typeof body.status === "number" ? body.status : null;
          const persistBytes = typeof body.persistBytes === "number" ? body.persistBytes : null;
          if ((status === null) === (persistBytes === null)) throw badRequest("invalid", "Set exactly one of status or persistBytes.");
          const times = body.times ?? 1;
          if (typeof times !== "number" || !Number.isInteger(times) || times < 1) throw badRequest("invalid", "times must be a positive integer.");
          state.faults.push({
            method: typeof body.method === "string" ? body.method.toUpperCase() : null,
            path: body.path,
            remaining: times,
            status,
            reason: typeof body.reason === "string" ? body.reason : null,
            message: typeof body.message === "string" ? body.message : null,
            persistBytes,
          });
          return c.json({ faults: state.faults }, 201);
        }),
      );

      app.delete("/_emulator/faults", (c) => {
        state.faults = [];
        return c.body(null, 204);
      });
    },
  });
}

function encodePageToken(offset: number): string {
  return Buffer.from(`rushes:${offset}`).toString("base64url");
}

function decodePageToken(token: string | undefined): number {
  if (!token) return 0;
  const match = /^rushes:(\d+)$/.exec(Buffer.from(token, "base64url").toString());
  if (!match) throw badRequest("invalidPageToken", "The request specifies an invalid page token.", { location: "pageToken" });
  return Number(match[1]);
}

export default createYoutubeEmulator();
