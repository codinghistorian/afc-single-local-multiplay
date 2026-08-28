import { expect, test } from "@playwright/test";
import { writeFile } from "node:fs/promises";

const clientCount = Number(process.env.AFC_QA_CLIENTS || "2");
const serverOrigin = process.env.AFC_WEB_SERVER_ORIGIN || "http://127.0.0.1:18080";

if (![2, 3, 4].includes(clientCount)) {
  throw new Error(`AFC_QA_CLIENTS must be 2, 3, or 4; received ${clientCount}`);
}

const snapshot = (page) =>
  page.evaluate(() => window.AFC_LOBBY_BRIDGE?.snapshot?.() || null);

const waitForScreen = (page, screen, timeout = 60_000) =>
  page.waitForFunction(
    (expected) => window.AFC_LOBBY_BRIDGE?.snapshot?.()?.screen === expected,
    screen,
    { timeout },
  );

async function waitForClientPhase(page, phase, timeout = 90_000) {
  await page.waitForFunction(
    (expected) => {
      const state = window.AFC_LOBBY_BRIDGE?.snapshot?.();
      return state?.client?.phase === expected || state?.screen === "error";
    },
    phase,
    { timeout },
  );
  const state = await snapshot(page);
  expect(state?.screen, JSON.stringify(state?.client || null)).not.toBe("error");
  expect(state?.client?.phase, JSON.stringify(state?.client || null)).toBe(phase);
}

async function reopenOnlineAfterRefresh(page) {
  await page.reload({ waitUntil: "domcontentloaded", timeout: 60_000 });
  await page.waitForFunction(() => Boolean(window.AFC_LOBBY_BRIDGE?.snapshot), null, {
    timeout: 30_000,
  });
  await page.locator("#bevy-canvas").click({ position: { x: 640, y: 400 } });
  await page.keyboard.press("ArrowDown");
  await page.waitForTimeout(180);
  await page.keyboard.press("ArrowDown");
  await page.waitForTimeout(180);
  await page.keyboard.press("Enter");
  await waitForScreen(page, "match", 90_000);
  await waitForClientPhase(page, "fighting", 120_000);
  await page.waitForTimeout(3_000);
  const state = await snapshot(page);
  expect(state?.screen, JSON.stringify(state?.client || null)).toBe("match");
  expect(state?.client?.phase, JSON.stringify(state?.client || null)).toBe("fighting");
}

function requireHealthyMatchStates(states) {
  for (const [index, state] of states.entries()) {
    expect(
      state?.screen,
      `client ${index + 1} left the match: ${JSON.stringify(state?.client || null)}`,
    ).not.toBe("error");
  }
}

function observeBattleStates(states, maximumTicks, evidence) {
  requireHealthyMatchStates(states);
  evidence.latest_battle_states = states;
  states.forEach((state, index) => {
    maximumTicks[index] = Math.max(
      maximumTicks[index],
      state?.room?.worker?.simulation_tick || 0,
    );
    const resultId = state?.room?.result?.result_id;
    if (resultId && !evidence.result_ids.includes(resultId)) {
      evidence.result_ids.push(resultId);
    }
    if (state?.client?.phase === "fighting" && state?.client?.confirmed_tick != null) {
      const confirmationLag = Math.max(
        0,
        (state?.room?.worker?.simulation_tick || 0) - state.client.confirmed_tick,
      );
      evidence.maximum_confirmation_lag ||= Array(clientCount).fill(0);
      evidence.maximum_confirmation_lag[index] = Math.max(
        evidence.maximum_confirmation_lag[index],
        confirmationLag,
      );
      expect(
        confirmationLag,
        `client ${index + 1} authority confirmation stalled: ${JSON.stringify({
          worker: state?.room?.worker,
          client: state?.client,
        })}`,
      ).toBeLessThan(180);
    }
  });
}

async function releaseKeys(pages, keys) {
  await Promise.all(
    pages.map((page, index) =>
      keys[index] ? page.keyboard.up(keys[index]).catch(() => {}) : Promise.resolve(),
    ),
  );
}

async function driveBattleToRoom(pages, evidence) {
  const startingStates = await Promise.all(pages.map(snapshot));
  const startingTicks = startingStates.map((state) => state.room.worker.simulation_tick);
  const maximumTicks = [...startingTicks];
  const towardCenter = ["ArrowRight", "ArrowLeft", "ArrowRight", "ArrowLeft"];
  const outward = [null, "ArrowRight", "ArrowLeft", "ArrowRight"];

  await Promise.all(
    pages.map((page) => page.locator("#bevy-canvas").click({ position: { x: 640, y: 400 } })),
  );
  await Promise.all(pages.map((page, index) => page.keyboard.down(towardCenter[index])));

  // Begin every acceptance match with controlled close combat. This exercises
  // movement, attacks, jump edges, prediction, rollback, and presentation on
  // every browser before the guests deliberately walk off the open test court.
  for (let round = 0; round < 14; round += 1) {
    const states = await Promise.all(pages.map(snapshot));
    observeBattleStates(states, maximumTicks, evidence);
    if (states.every((state) => state?.screen === "room")) {
      await releaseKeys(pages, towardCenter);
      return { startingTicks, maximumTicks };
    }
    await Promise.all(
      pages.map(async (page, index) => {
        if (states[index]?.client?.phase !== "fighting") return;
        await page.keyboard.press(round % 2 === 0 ? "c" : "x");
        if ((round + index) % 4 === 0) await page.keyboard.press("v");
      }),
    );
    await pages[0].waitForTimeout(500);
  }
  await releaseKeys(pages, towardCenter);

  for (let index = 1; index < pages.length; index += 1) {
    await pages[index].keyboard.down(outward[index]);
  }
  const returnDeadline = Date.now() + 75_000;
  while (Date.now() < returnDeadline) {
    const states = await Promise.all(pages.map(snapshot));
    observeBattleStates(states, maximumTicks, evidence);
    if (states.every((state) => state?.screen === "room")) break;
    await Promise.all(
      pages.map(async (page, index) => {
        if (states[index]?.client?.phase !== "fighting") return;
        if (index > 0) await page.keyboard.press("v");
        await page.keyboard.press(index % 2 === 0 ? "c" : "x");
      }),
    );
    await pages[0].waitForTimeout(500);
  }
  await releaseKeys(pages, outward);
  await Promise.all(pages.map((page) => waitForScreen(page, "room", 20_000)));
  return { startingTicks, maximumTicks };
}

async function enterOnline(page, nickname) {
  await page.goto(`/?afc_server=${encodeURIComponent(serverOrigin)}`, {
    waitUntil: "domcontentloaded",
    timeout: 60_000,
  });
  await expect(page.locator("#bevy-canvas")).toBeVisible();
  await page.waitForFunction(() => Boolean(window.AFC_LOBBY_BRIDGE?.snapshot), null, {
    timeout: 30_000,
  });

  for (let attempt = 0; attempt < 3; attempt += 1) {
    if ((await snapshot(page))?.screen === "identity") break;
    if (attempt > 0) {
      await page.reload({ waitUntil: "domcontentloaded" });
      await page.waitForFunction(() => Boolean(window.AFC_LOBBY_BRIDGE?.snapshot), null, {
        timeout: 30_000,
      });
    }
    await page.locator("#bevy-canvas").click({ position: { x: 640, y: 400 } });
    await page.keyboard.press("ArrowDown");
    await page.waitForTimeout(180);
    await page.keyboard.press("ArrowDown");
    await page.waitForTimeout(180);
    await page.keyboard.press("Enter");
    await page
      .waitForFunction(() => window.AFC_LOBBY_BRIDGE?.snapshot?.()?.screen === "identity", null, {
        timeout: 8_000,
      })
      .catch(() => {});
  }

  await waitForScreen(page, "identity");
  await page.locator('[data-qa="nickname-input"]').fill(nickname);
  await page.locator('[data-qa="nickname-form"]').evaluate((form) => form.requestSubmit());
  await waitForScreen(page, "lobby");
}

async function waitForRoomMembers(page, count) {
  await page.waitForFunction(
    (expected) => window.AFC_LOBBY_BRIDGE?.snapshot?.()?.room?.member_count === expected,
    count,
    { timeout: 30_000 },
  );
}

async function setReady(page) {
  await page.locator('[data-qa="ready-button"]').click();
  await expect(page.locator('[data-qa="ready-button"]')).toHaveText("Not ready");
}

test(`${clientCount}-client battle and rematch return every guest to the same room`, async ({
  browser,
  baseURL,
}, testInfo) => {
  const contexts = [];
  const pages = [];
  const fatalBrowserErrors = [];
  const browserLogs = [];
  const evidence = { client_count: clientCount, milestones: [], result_ids: [] };
  try {
    for (let index = 0; index < clientCount; index += 1) {
      const context = await browser.newContext({ baseURL });
      const page = await context.newPage();
      page.on("pageerror", (error) => {
        const entry = `client ${index + 1} pageerror: ${error.stack || error}`;
        fatalBrowserErrors.push(entry);
        browserLogs.push(entry);
      });
      page.on("console", (message) => {
        browserLogs.push(`client ${index + 1} console ${message.type()}: ${message.text()}`);
        if (message.type() === "error") {
          fatalBrowserErrors.push(`client ${index + 1} console: ${message.text()}`);
        }
      });
      page.on("requestfailed", (request) => {
        browserLogs.push(
          `client ${index + 1} request failed: ${request.method()} ${request.url()} ${request.failure()?.errorText || "unknown"}`,
        );
      });
      page.on("response", (response) => {
        if (response.status() >= 400 && !response.url().endsWith("/favicon.ico")) {
          browserLogs.push(
            `client ${index + 1} response ${response.status()}: ${response.request().method()} ${response.url()}`,
          );
        }
      });
      page.on("websocket", (socket) => {
        browserLogs.push(`client ${index + 1} websocket opened: ${socket.url()}`);
        socket.on("framesent", (event) => {
          const length = typeof event.payload === "string" ? event.payload.length : event.payload.byteLength;
          browserLogs.push(`client ${index + 1} websocket sent: ${length} bytes`);
        });
        socket.on("framereceived", (event) => {
          const length = typeof event.payload === "string" ? event.payload.length : event.payload.byteLength;
          browserLogs.push(`client ${index + 1} websocket received: ${length} bytes`);
        });
        socket.on("close", () => browserLogs.push(`client ${index + 1} websocket closed`));
      });
      contexts.push(context);
      pages.push(page);
    }

    await Promise.all(
      pages.map((page, index) => enterOnline(page, `QA Fighter ${index + 1}`)),
    );
    evidence.milestones.push({ name: "lobby", states: await Promise.all(pages.map(snapshot)) });
    await Promise.all(
      pages.map((page) =>
        page.waitForFunction(
          (expected) => window.AFC_LOBBY_BRIDGE?.snapshot?.()?.online_guests === expected,
          clientCount,
          { timeout: 30_000 },
        ),
      ),
    );

    const host = pages[0];
    const guest = pages[1];

    await host.locator('[data-qa="global-chat-input"]').fill("draft survives a remote update");
    await guest.locator('[data-qa="global-chat-input"]').fill("hello from another browser");
    await guest.locator('[data-qa="global-chat-form"]').evaluate((form) => form.requestSubmit());
    await expect(host.locator('[data-qa="global-chat-log"]')).toContainText(
      "hello from another browser",
    );
    await expect(host.locator('[data-qa="global-chat-input"]')).toHaveValue(
      "draft survives a remote update",
    );

    await host.locator('select[name="maximum-players"]').selectOption(String(clientCount));
    await host.locator('select[name="visibility"]').selectOption("public");
    await host.locator('[data-qa="create-room-form"]').evaluate((form) => form.requestSubmit());
    await waitForScreen(host, "room");
    const publicCode = (await snapshot(host)).room.room_code;
    evidence.public_room_code = publicCode;
    await expect(guest.locator(`[data-room-code="${publicCode}"]`)).toBeVisible();
    await host.getByRole("button", { name: "Leave room" }).click();
    await waitForScreen(host, "lobby");
    await expect(guest.locator(`[data-room-code="${publicCode}"]`)).toHaveCount(0);

    await host.locator('select[name="maximum-players"]').selectOption(String(clientCount));
    await host.locator('select[name="visibility"]').selectOption("private");
    await host.locator('[data-qa="create-room-form"]').evaluate((form) => form.requestSubmit());
    await waitForScreen(host, "room");
    const roomCode = (await snapshot(host)).room.room_code;
    evidence.private_room_code = roomCode;
    await expect(guest.locator(`[data-room-code="${roomCode}"]`)).toHaveCount(0);

    for (let index = 1; index < pages.length; index += 1) {
      const page = pages[index];
      await page.locator('[data-qa="room-code-input"]').fill(roomCode);
      await page.locator('[data-qa="join-room-form"]').evaluate((form) => form.requestSubmit());
      await waitForScreen(page, "room");
      await waitForRoomMembers(host, index + 1);
    }
    await Promise.all(pages.map((page) => waitForRoomMembers(page, clientCount)));
    evidence.milestones.push({ name: "private_room_joined", states: await Promise.all(pages.map(snapshot)) });

    await pages.at(-1).locator('[data-qa="room-chat-input"]').fill("room chat is live");
    await pages.at(-1).locator('[data-qa="room-chat-form"]').evaluate((form) => form.requestSubmit());
    await expect(host.locator('[data-qa="room-chat-log"]')).toContainText("room chat is live");

    await host.locator('select[name="arena"]').selectOption("8");
    await host.locator('select[name="rules"]').selectOption("2");
    await host.locator('[data-qa="room-settings-form"]').evaluate((form) => form.requestSubmit());
    await host.waitForFunction(
      () => {
        const room = window.AFC_LOBBY_BRIDGE?.snapshot?.()?.room;
        return room?.arena_index === 8 && room?.rule_index === 2;
      },
      null,
      { timeout: 20_000 },
    );

    for (let index = 0; index < pages.length; index += 1) {
      const selector = pages[index].locator('[data-qa="character-select"]');
      const options = await selector.locator("option").count();
      await selector.selectOption({ index: index % options });
      await pages[index].waitForTimeout(500);
      await setReady(pages[index]);
      await host.waitForFunction(
        (expected) =>
          window.AFC_LOBBY_BRIDGE
            ?.snapshot?.()
            ?.room?.members.filter((member) => member.ready).length === expected,
        index + 1,
        { timeout: 20_000 },
      );
    }

    const startButton = host.locator('[data-qa="start-match-button"]');
    await expect(startButton).toBeEnabled();
    evidence.milestones.push({ name: "ready", states: await Promise.all(pages.map(snapshot)) });
    await host.screenshot({ path: testInfo.outputPath(`room-${clientCount}.png`) });
    await startButton.click();

    await Promise.all(pages.map((page) => waitForScreen(page, "match", 90_000)));
    await Promise.all(pages.map((page) => waitForClientPhase(page, "fighting", 120_000)));
    await reopenOnlineAfterRefresh(guest);
    const restoredGuest = await snapshot(guest);
    expect(restoredGuest.guest.display_name).toBe("QA Fighter 2");
    evidence.milestones.push({ name: "guest_reconnected_after_refresh", state: restoredGuest });
    await pages[0].screenshot({ path: testInfo.outputPath(`battle-${clientCount}.png`) });
    const { startingTicks, maximumTicks } = await driveBattleToRoom(pages, evidence);

    const returnedStates = await Promise.all(pages.map(snapshot));
    evidence.milestones.push({ name: "returned", states: returnedStates });
    evidence.starting_ticks = startingTicks;
    evidence.maximum_ticks = maximumTicks;
    for (let index = 0; index < returnedStates.length; index += 1) {
      expect(returnedStates[index].room.room_code).toBe(roomCode);
      expect(returnedStates[index].room.state).toBe("open");
      expect(returnedStates[index].room.members).toHaveLength(clientCount);
      expect(returnedStates[index].room.members.every((member) => member.ready === false)).toBe(true);
      expect(returnedStates[index].client).toBeNull();
      expect(returnedStates[index].guest.display_name).toBe(`QA Fighter ${index + 1}`);
    }

    expect(startingTicks.every((tick) => tick > 0)).toBe(true);
    expect(maximumTicks.every((tick, index) => tick > startingTicks[index] + 60)).toBe(true);
    await host.screenshot({ path: testInfo.outputPath(`returned-room-${clientCount}.png`) });

    const firstEpoch = returnedStates[0].room.match_epoch;
    for (let index = 0; index < pages.length; index += 1) {
      await setReady(pages[index]);
      await host.waitForFunction(
        (expected) =>
          window.AFC_LOBBY_BRIDGE
            ?.snapshot?.()
            ?.room?.members.filter((member) => member.ready).length === expected,
        index + 1,
        { timeout: 20_000 },
      );
    }
    await expect(startButton).toBeEnabled();
    await startButton.click();
    await Promise.all(pages.map((page) => waitForScreen(page, "match", 90_000)));
    await Promise.all(pages.map((page) => waitForClientPhase(page, "fighting", 120_000)));
    const rematchStartStates = await Promise.all(pages.map(snapshot));
    evidence.milestones.push({ name: "rematch_fighting", states: rematchStartStates });

    const {
      startingTicks: rematchTicks,
      maximumTicks: rematchMaximumTicks,
    } = await driveBattleToRoom(pages, evidence);
    const rematchReturnedStates = await Promise.all(pages.map(snapshot));
    evidence.milestones.push({ name: "rematch_returned", states: rematchReturnedStates });
    evidence.rematch_starting_ticks = rematchTicks;
    evidence.rematch_maximum_ticks = rematchMaximumTicks;
    expect(rematchReturnedStates.every((state) => state.room.room_code === roomCode)).toBe(true);
    expect(rematchReturnedStates.every((state) => state.room.state === "open")).toBe(true);
    expect(rematchReturnedStates.every((state) => state.room.match_epoch > firstEpoch)).toBe(true);
    expect(rematchMaximumTicks.every((tick, index) => tick > rematchTicks[index] + 60)).toBe(true);
    expect(evidence.result_ids).toHaveLength(2);
    await host.screenshot({ path: testInfo.outputPath(`rematch-returned-room-${clientCount}.png`) });

    expect(fatalBrowserErrors, fatalBrowserErrors.join("\n")).toEqual([]);
  } finally {
    await Promise.all(contexts.map((context) => context.close().catch(() => {})));
    try {
      const metricsResponse = await fetch(`${serverOrigin}/metrics`);
      evidence.metrics = await metricsResponse.text();
    } catch (error) {
      evidence.metrics_error = String(error);
    }
    const browserLog = `${browserLogs.join("\n")}\n`;
    const evidenceJson = `${JSON.stringify(evidence, null, 2)}\n`;
    await writeFile(testInfo.outputPath("browser-network-console.log"), browserLog);
    await writeFile(testInfo.outputPath("multiplayer-evidence.json"), evidenceJson);
    await testInfo.attach("browser-network-console.log", {
      body: Buffer.from(browserLog),
      contentType: "text/plain",
    });
    await testInfo.attach("multiplayer-evidence.json", {
      body: Buffer.from(evidenceJson),
      contentType: "application/json",
    });
  }
});
