import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import { after, before, test } from "node:test";
import { setTimeout as sleep } from "node:timers/promises";
import { createEmulator, type CustomEmulator, type Emulator } from "emulate";
import { createYoutubeEmulator, type YoutubeEmulatorOptions, type YoutubeState } from "./youtube.ts";

const KIB = 1024;
const PROCESSING_MS = 300;
// The emulator checks the signature, not the whole image.
const PNG = Buffer.concat([Buffer.from("89504e470d0a1a0a", "hex"), randomBytes(64)]);

let google: Emulator;
let googleUrl: string;
let creatorToken: string;
let editorToken: string;

before(async () => {
  google = await createEmulator({
    service: "google",
    port: 0,
    seed: {
      google: {
        users: [
          { email: "creator@example.com", name: "Demo Creator" },
          { email: "editor@example.com", name: "Second Channel" },
        ],
      },
    },
  });
  googleUrl = google.url.replace("localhost", "127.0.0.1");
  creatorToken = await signIn("creator@example.com");
  editorToken = await signIn("editor@example.com");
});

after(() => google.close());

/** Runs the consent form + code exchange like a browser and the desktop app would. */
async function signIn(email: string): Promise<string> {
  const redirectUri = "http://127.0.0.1:9/callback";
  const consent = await fetch(`${googleUrl}/o/oauth2/v2/auth/callback`, {
    method: "POST",
    body: new URLSearchParams({ email, redirect_uri: redirectUri, scope: "openid email profile", state: "s", client_id: "test" }),
    redirect: "manual",
  });
  const code = new URL(consent.headers.get("location")!).searchParams.get("code")!;
  const token = await fetch(`${googleUrl}/oauth2/token`, {
    method: "POST",
    body: new URLSearchParams({ code, client_id: "test", redirect_uri: redirectUri, grant_type: "authorization_code" }),
  });
  return ((await token.json()) as { access_token: string }).access_token;
}

async function withYoutube(
  run: (youtube: CustomEmulator<YoutubeState>) => Promise<void>,
  options: YoutubeEmulatorOptions = {},
): Promise<void> {
  const youtube = await createEmulator({
    service: createYoutubeEmulator({ googleUrl, processingMs: PROCESSING_MS, authCacheMs: 0, ...options }),
    listen: false,
  });
  try {
    await run(youtube);
  } finally {
    await youtube.close();
  }
}

function auth(token: string, headers: Record<string, string> = {}): Record<string, string> {
  return { Authorization: `Bearer ${token}`, ...headers };
}

/** Path + query of an absolute emulator URL, for `request()` on an unbound emulator. */
function local(url: string): string {
  const parsed = new URL(url);
  return parsed.pathname + parsed.search;
}

async function startSession(
  youtube: CustomEmulator<YoutubeState>,
  size: number,
  metadata: unknown = { snippet: { title: "Test video" }, status: { privacyStatus: "private" } },
  token = creatorToken,
): Promise<Response> {
  return youtube.request("/upload/youtube/v3/videos?uploadType=resumable&part=snippet,status", {
    method: "POST",
    headers: auth(token, {
      "Content-Type": "application/json; charset=UTF-8",
      "X-Upload-Content-Length": String(size),
      "X-Upload-Content-Type": "video/mp4",
    }),
    body: JSON.stringify(metadata),
  });
}

function putChunk(youtube: CustomEmulator<YoutubeState>, session: string, bytes: Uint8Array<ArrayBuffer>, start: number, total: number) {
  return youtube.request(session, {
    method: "PUT",
    headers: { "Content-Range": `bytes ${start}-${start + bytes.length - 1}/${total}` },
    body: bytes,
  });
}

function queryStatus(youtube: CustomEmulator<YoutubeState>, session: string, total: number) {
  return youtube.request(session, { method: "PUT", headers: { "Content-Range": `bytes */${total}` } });
}

async function uploadWhole(youtube: CustomEmulator<YoutubeState>, title: string, privacy = "private", token = creatorToken) {
  const bytes = randomBytes(1000);
  const init = await startSession(youtube, bytes.length, { snippet: { title }, status: { privacyStatus: privacy } }, token);
  const done = await youtube.request(local(init.headers.get("location")!), { method: "PUT", body: bytes });
  assert.equal(done.status, 200);
  return (await done.json()) as { id: string };
}

async function errorOf(response: Response): Promise<{ code: number; reason: string; domain: string }> {
  const body = (await response.json()) as { error: { code: number; errors: Array<{ reason: string; domain: string }> } };
  return { code: body.error.code, reason: body.error.errors[0].reason, domain: body.error.errors[0].domain };
}

test("rejects requests without a valid bearer token", async () => {
  await withYoutube(async (youtube) => {
    const missing = await youtube.request("/youtube/v3/channels?part=snippet&mine=true");
    assert.equal(missing.status, 401);
    assert.deepEqual(await errorOf(missing), { code: 401, reason: "required", domain: "global" });

    const bogus = await youtube.request("/youtube/v3/channels?part=snippet&mine=true", { headers: auth("not-a-token") });
    assert.equal(bogus.status, 401);
    assert.match(bogus.headers.get("www-authenticate") ?? "", /invalid_token/);
    assert.deepEqual(await errorOf(bogus), { code: 401, reason: "authError", domain: "global" });

    const upload = await startSession(youtube, 10, {}, "not-a-token");
    assert.equal(upload.status, 401);
  });
});

test("creates one channel per Google user", async () => {
  await withYoutube(async (youtube) => {
    const response = await youtube.request("/youtube/v3/channels?part=snippet,contentDetails,statistics&mine=true", {
      headers: auth(creatorToken),
    });
    assert.equal(response.status, 200);
    const body = (await response.json()) as { items: Array<any> };
    assert.equal(body.items.length, 1);
    const [channel] = body.items;
    assert.match(channel.id, /^UC[A-Za-z0-9_-]{22}$/);
    assert.equal(channel.snippet.title, "Demo Creator");
    assert.equal(channel.snippet.customUrl, "@creator");
    assert.equal(channel.contentDetails.relatedPlaylists.uploads, "UU" + channel.id.slice(2));
    assert.equal(channel.statistics.videoCount, "0");

    const other = await youtube.request("/youtube/v3/channels?part=snippet&mine=true", { headers: auth(editorToken) });
    const otherChannel = ((await other.json()) as { items: Array<any> }).items[0];
    assert.notEqual(otherChannel.id, channel.id);
    assert.equal(otherChannel.snippet.title, "Second Channel");

    const avatar = await youtube.request(local(channel.snippet.thumbnails.default.url));
    assert.equal(avatar.headers.get("content-type"), "image/png");
  });
});

test("resumable upload in chunks with status queries", async () => {
  await withYoutube(async (youtube) => {
    const data = randomBytes(700 * KIB);
    const total = data.length;
    const init = await startSession(youtube, total, {
      snippet: { title: "Chunked", description: "Uploaded in pieces", tags: ["rust", "gpui"], categoryId: "28" },
      status: { privacyStatus: "unlisted", selfDeclaredMadeForKids: false },
    });
    assert.equal(init.status, 200);
    const session = local(init.headers.get("location")!);

    const fresh = await queryStatus(youtube, session, total);
    assert.equal(fresh.status, 308);
    assert.equal(fresh.headers.get("range"), null);

    const first = await putChunk(youtube, session, data.subarray(0, 256 * KIB), 0, total);
    assert.equal(first.status, 308);
    assert.equal(first.headers.get("range"), `bytes=0-${256 * KIB - 1}`);

    // A chunk that is not a multiple of 256 KiB is only persisted up to the last boundary.
    const second = await putChunk(youtube, session, data.subarray(256 * KIB, 600 * KIB), 256 * KIB, total);
    assert.equal(second.status, 308);
    assert.equal(second.headers.get("range"), `bytes=0-${512 * KIB - 1}`);

    const status = await queryStatus(youtube, session, total);
    assert.equal(status.status, 308);
    assert.equal(status.headers.get("range"), `bytes=0-${512 * KIB - 1}`);

    // Overlapping bytes the server already has are skipped.
    const last = await putChunk(youtube, session, data.subarray(500 * KIB), 500 * KIB, total);
    assert.equal(last.status, 200);
    const video = (await last.json()) as any;
    assert.match(video.id, /^[A-Za-z0-9_-]{11}$/);
    assert.equal(video.snippet.title, "Chunked");
    assert.deepEqual(video.snippet.tags, ["rust", "gpui"]);
    assert.equal(video.status.uploadStatus, "uploaded");
    assert.equal(video.status.privacyStatus, "unlisted");

    const again = await queryStatus(youtube, session, total);
    assert.equal(again.status, 200);
    assert.equal(((await again.json()) as any).id, video.id);

    const uploads = (await (await youtube.request("/_emulator/uploads")).json()) as { uploads: Array<any> };
    assert.equal(uploads.uploads.length, 1);
    assert.equal(uploads.uploads[0].videoId, video.id);
    assert.equal(uploads.uploads[0].bytes, total);
    assert.equal(uploads.uploads[0].sha256, createHash("sha256").update(data).digest("hex"));
    assert.equal(uploads.uploads[0].contentType, "video/mp4");
    assert.doesNotThrow(() => JSON.stringify(youtube.snapshot()));
  });
});

test("validates upload metadata and Content-Range", async () => {
  await withYoutube(async (youtube) => {
    const cases: Array<[unknown, string]> = [
      [{ snippet: {} }, "invalidTitle"],
      [{ snippet: { title: "x".repeat(101) } }, "invalidTitle"],
      [{ snippet: { title: "<b>bold</b>" } }, "invalidTitle"],
      [{ snippet: { title: "ok", description: "é".repeat(2501) } }, "invalidDescription"],
      [{ snippet: { title: "ok", categoryId: "999" } }, "invalidCategoryId"],
      [{ snippet: { title: "ok" }, status: { privacyStatus: "secret" } }, "invalidVideoMetadata"],
      [{ snippet: { title: "ok" }, status: { privacyStatus: "public", publishAt: "2999-01-01T00:00:00Z" } }, "invalidPublishAt"],
      [{ snippet: { title: "ok" }, status: { privacyStatus: "private", publishAt: "2001-01-01T00:00:00Z" } }, "invalidPublishAt"],
    ];
    for (const [metadata, reason] of cases) {
      const response = await startSession(youtube, 10, metadata);
      assert.equal(response.status, 400, JSON.stringify(metadata));
      assert.equal((await errorOf(response)).reason, reason, JSON.stringify(metadata));
    }
    const scheduled = await startSession(youtube, 10, {
      snippet: { title: "ok" },
      status: { privacyStatus: "private", publishAt: "2999-01-01T00:00:00Z" },
    });
    assert.equal(scheduled.status, 200);

    const noLength = await youtube.request("/upload/youtube/v3/videos?uploadType=resumable&part=snippet,status", {
      method: "POST",
      headers: auth(creatorToken, { "Content-Type": "application/json" }),
      body: JSON.stringify({ snippet: { title: "ok" } }),
    });
    assert.equal(noLength.status, 400);
    assert.equal((await errorOf(noLength)).reason, "required");

    const init = await startSession(youtube, 1000);
    const session = local(init.headers.get("location")!);
    const gap = await putChunk(youtube, session, randomBytes(10), 500, 1000);
    assert.equal(gap.status, 400);
    const wrongTotal = await putChunk(youtube, session, randomBytes(10), 0, 999);
    assert.equal(wrongTotal.status, 400);
    const short = await youtube.request(session, { method: "PUT", headers: { "Content-Range": "bytes 0-99/1000" }, body: randomBytes(50) });
    assert.equal(short.status, 400);
    const status = await queryStatus(youtube, session, 1000);
    assert.equal(status.headers.get("range"), null);

    const unknown = await youtube.request("/upload/youtube/v3/videos?uploadType=resumable&upload_id=nope", { method: "PUT", body: "x" });
    assert.equal(unknown.status, 404);

    // Tags are measured in characters, like the Rust client does.
    const emojiTags = await startSession(youtube, 10, { snippet: { title: "ok", tags: ["🎬".repeat(250), "🎥".repeat(249)] } });
    assert.equal(emojiTags.status, 200);
  });
});

test("processing finishes a few moments after the upload", async () => {
  await withYoutube(async (youtube) => {
    const { id } = await uploadWhole(youtube, "Processing");
    const get = () =>
      youtube
        .request(`/youtube/v3/videos?part=snippet,status,processingDetails,statistics&id=${id},missing`, { headers: auth(creatorToken) })
        .then((response) => response.json() as Promise<{ items: Array<any> }>);
    const early = await get();
    assert.equal(early.items.length, 1);
    assert.equal(early.items[0].processingDetails.processingStatus, "processing");
    assert.equal(early.items[0].status.uploadStatus, "uploaded");
    assert.equal(early.items[0].statistics.viewCount, "0");

    await sleep(PROCESSING_MS + 50);
    const late = await get();
    assert.equal(late.items[0].processingDetails.processingStatus, "succeeded");
    assert.equal(late.items[0].status.uploadStatus, "processed");

    // Private videos are invisible to other users.
    const foreign = await youtube.request(`/youtube/v3/videos?part=snippet&id=${id}`, { headers: auth(editorToken) });
    assert.equal(((await foreign.json()) as { items: unknown[] }).items.length, 0);
  });
});

test("updates, thumbnails and deletes videos", async () => {
  await withYoutube(async (youtube) => {
    const { id } = await uploadWhole(youtube, "Before");
    const update = await youtube.request("/youtube/v3/videos?part=snippet,status", {
      method: "PUT",
      headers: auth(creatorToken, { "Content-Type": "application/json" }),
      body: JSON.stringify({ id, snippet: { title: "After", categoryId: "27" }, status: { privacyStatus: "public" } }),
    });
    assert.equal(update.status, 200);
    const updated = (await update.json()) as any;
    assert.equal(updated.snippet.title, "After");
    assert.equal(updated.status.privacyStatus, "public");

    const missingCategory = await youtube.request("/youtube/v3/videos?part=snippet", {
      method: "PUT",
      headers: auth(creatorToken, { "Content-Type": "application/json" }),
      body: JSON.stringify({ id, snippet: { title: "No category" } }),
    });
    assert.equal((await errorOf(missingCategory)).reason, "invalidCategoryId");

    const foreignUpdate = await youtube.request("/youtube/v3/videos?part=snippet", {
      method: "PUT",
      headers: auth(editorToken, { "Content-Type": "application/json" }),
      body: JSON.stringify({ id, snippet: { title: "Hijack", categoryId: "22" } }),
    });
    assert.equal(foreignUpdate.status, 403);

    const thumbnail = await youtube.request(`/upload/youtube/v3/thumbnails/set?videoId=${id}`, {
      method: "POST",
      headers: auth(creatorToken, { "Content-Type": "image/png" }),
      body: PNG,
    });
    assert.equal(thumbnail.status, 200);
    const set = (await thumbnail.json()) as { kind: string; items: Array<any> };
    assert.equal(set.kind, "youtube#thumbnailSetResponse");
    const served = await youtube.request(local(set.items[0].default.url));
    assert.deepEqual(Buffer.from(await served.arrayBuffer()), PNG);

    const notImage = await youtube.request(`/upload/youtube/v3/thumbnails/set?videoId=${id}`, {
      method: "POST",
      headers: auth(creatorToken, { "Content-Type": "image/png" }),
      body: "definitely not a png",
    });
    assert.equal((await errorOf(notImage)).reason, "invalidImage");
    const tooLarge = await youtube.request(`/upload/youtube/v3/thumbnails/set?videoId=${id}`, {
      method: "POST",
      headers: auth(creatorToken, { "Content-Type": "image/png" }),
      body: Buffer.concat([PNG, Buffer.alloc(2 * 1024 * KIB)]),
    });
    assert.equal((await errorOf(tooLarge)).reason, "invalidImage");

    const removed = await youtube.request(`/youtube/v3/videos?id=${id}`, { method: "DELETE", headers: auth(creatorToken) });
    assert.equal(removed.status, 204);
    const again = await youtube.request(`/youtube/v3/videos?id=${id}`, { method: "DELETE", headers: auth(creatorToken) });
    assert.equal(again.status, 404);
    assert.equal((await errorOf(again)).reason, "videoNotFound");
  });
});

test("lists the uploads playlist newest first with pagination", async () => {
  await withYoutube(async (youtube) => {
    const first = await uploadWhole(youtube, "First", "public");
    const second = await uploadWhole(youtube, "Second", "private");
    const third = await uploadWhole(youtube, "Third", "unlisted");
    const channel = (await (
      await youtube.request("/youtube/v3/channels?part=contentDetails&mine=true", { headers: auth(creatorToken) })
    ).json()) as { items: Array<any> };
    const playlist = channel.items[0].contentDetails.relatedPlaylists.uploads;
    const list = (token: string, query: string) =>
      youtube
        .request(`/youtube/v3/playlistItems?part=snippet,contentDetails&playlistId=${playlist}&${query}`, { headers: auth(token) })
        .then((response) => response.json() as Promise<any>);

    const page1 = await list(creatorToken, "maxResults=2");
    assert.deepEqual(
      page1.items.map((item: any) => item.contentDetails.videoId),
      [third.id, second.id],
    );
    assert.equal(page1.pageInfo.totalResults, 3);
    assert.ok(page1.nextPageToken);
    const page2 = await list(creatorToken, `maxResults=2&pageToken=${page1.nextPageToken}`);
    assert.deepEqual(
      page2.items.map((item: any) => item.snippet.resourceId.videoId),
      [first.id],
    );
    assert.equal(page2.items[0].snippet.position, 2);
    assert.equal(page2.nextPageToken, undefined);

    const asOther = await list(editorToken, "maxResults=50");
    assert.deepEqual(
      asOther.items.map((item: any) => item.contentDetails.videoId),
      [third.id, first.id],
    );

    const missing = await youtube.request("/youtube/v3/playlistItems?part=snippet&playlistId=UUnope", { headers: auth(creatorToken) });
    assert.equal((await errorOf(missing)).reason, "playlistNotFound");
  });
});

test("injected faults and quota exhaustion", async () => {
  await withYoutube(
    async (youtube) => {
      const data = randomBytes(768 * KIB);
      const init = await startSession(youtube, data.length);
      const session = local(init.headers.get("location")!);

      const invalidFault = await youtube.request("/_emulator/faults", {
        method: "POST",
        body: JSON.stringify({ path: "/upload/youtube/v3/videos", status: 503, times: 0 }),
      });
      assert.equal(invalidFault.status, 400);

      const fault = await youtube.request("/_emulator/faults", {
        method: "POST",
        body: JSON.stringify({ method: "PUT", path: "/upload/youtube/v3/videos", status: 503 }),
      });
      assert.equal(fault.status, 201);
      const failed = await putChunk(youtube, session, data.subarray(0, 256 * KIB), 0, data.length);
      assert.equal(failed.status, 503);
      assert.deepEqual(await errorOf(failed), { code: 503, reason: "backendError", domain: "global" });
      assert.equal((await queryStatus(youtube, session, data.length)).headers.get("range"), null);

      await youtube.request("/_emulator/faults", {
        method: "POST",
        body: JSON.stringify({ path: "/upload/youtube/v3/videos", persistBytes: 256 * KIB }),
      });
      const partial = await putChunk(youtube, session, data.subarray(0, 512 * KIB), 0, data.length);
      assert.equal(partial.status, 308);
      assert.equal(partial.headers.get("range"), `bytes=0-${256 * KIB - 1}`);
      const rest = await putChunk(youtube, session, data.subarray(256 * KIB), 256 * KIB, data.length);
      assert.equal(rest.status, 200);

      // dailyQuota 3300 = two inserts (2 × 1600) plus a few list calls.
      const second = await startSession(youtube, 10);
      assert.equal(second.status, 200);
      const third = await startSession(youtube, 10);
      assert.equal(third.status, 403);
      assert.deepEqual(await errorOf(third), { code: 403, reason: "quotaExceeded", domain: "youtube.quota" });

      await youtube.request("/_emulator/faults", {
        method: "POST",
        body: JSON.stringify({ path: "/youtube/v3/channels", status: 403, reason: "uploadLimitExceeded" }),
      });
      const limited = await youtube.request("/youtube/v3/channels?part=id&mine=true", { headers: auth(creatorToken) });
      assert.deepEqual(await errorOf(limited), { code: 403, reason: "uploadLimitExceeded", domain: "youtube.video" });
    },
    { dailyQuota: 3300 },
  );
});
