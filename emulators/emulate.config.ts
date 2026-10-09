// Local stand-ins for Google sign-in and the YouTube Data API.
//   npm start   (or scripts/emulators.sh from the repository root)
// Ports default to 4002 (Google) and 4020 (YouTube); tests override them through env.
// GOOGLE_EMULATOR_URL makes the YouTube emulator accept tokens of another Google emulator.
import { defineConfig } from "emulate";
import { createYoutubeEmulator } from "./youtube.ts";

const host = "127.0.0.1";
const googlePort = Number(process.env.GOOGLE_EMULATOR_PORT ?? 4002);
const youtubePort = Number(process.env.YOUTUBE_EMULATOR_PORT ?? 4020);
const googleUrl = `http://${host}:${googlePort}`;

export default defineConfig({
  services: {
    // No oauth_clients on purpose: the emulator then accepts any client_id and any
    // redirect_uri, which the desktop loopback flow (random port) needs.
    google: {
      emulator: "google",
      port: googlePort,
      baseUrl: googleUrl,
      seed: {
        users: [
          { email: "creator@example.com", name: "Demo Creator" },
          { email: "editor@example.com", name: "Second Channel" },
        ],
      },
    },
    youtube: {
      emulator: createYoutubeEmulator({ googleUrl: process.env.GOOGLE_EMULATOR_URL || googleUrl }),
      port: youtubePort,
      baseUrl: `http://${host}:${youtubePort}`,
    },
  },
});
