import { defineConfig } from "@playwright/test";
import path from "node:path";

const executablePath = process.env.AFC_CHROMIUM_EXECUTABLE || undefined;
const outputDir = path.resolve(
  process.cwd(),
  process.env.AFC_QA_OUTPUT_DIR || "target/qa/web/playwright-results",
);

export default defineConfig({
  testDir: ".",
  testMatch: "*.spec.mjs",
  fullyParallel: false,
  workers: 1,
  timeout: 300_000,
  expect: { timeout: 20_000 },
  outputDir,
  reporter: [["line"]],
  use: {
    baseURL: process.env.AFC_WEB_BASE_URL || "http://127.0.0.1:8000",
    headless: process.env.AFC_QA_HEADFUL !== "1",
    viewport: { width: 1280, height: 800 },
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
    launchOptions: {
      executablePath,
      args: [
        "--autoplay-policy=no-user-gesture-required",
        "--disable-background-timer-throttling",
        "--disable-backgrounding-occluded-windows",
        "--disable-renderer-backgrounding",
        "--enable-webgpu",
        "--ignore-gpu-blocklist",
      ],
    },
  },
});
