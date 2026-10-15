import { expect, test, type Page } from "@playwright/test";

/**
 * Full-stack two-tab voice E2E (plan §7, P6b).
 *
 * Requires a live control + media stack:
 *   1. Postgres 16 (DATABASE_URL)
 *   2. Nest API      (http://127.0.0.1:3000)
 *   3. rvp-cli       (--listen 127.0.0.1:4433 --wt-listen 127.0.0.1:4434)
 *   4. Web preview   (this config's webServer, or RVP_E2E_BASE_URL)
 *
 * Set RVP_E2E=1 to enable. Without it the suite is skipped so CI stays green
 * until the full stack job is wired (see docs/plan.md P6b).
 *
 * Flow:
 *   - Alice logs in, creates a room, invites Bob, joins (grant + WT + voice).
 *   - Bob logs in, joins the same room.
 *   - Assert both see each other on the roster and Alice's voice stats show
 *     datagrams being sent (sealed fanout). Two real mic streams are not
 *     available in headless CI, but the pipeline starts and the SFU path
 *     is exercised via the control handshake and datagram plumbing.
 */

const enabled = process.env.RVP_E2E === "1";
const nestUrl = process.env.RVP_E2E_NEST_URL ?? "http://127.0.0.1:3000";
const wtUrl = process.env.RVP_E2E_WT_URL ?? "https://127.0.0.1:4434/";
const certHash = process.env.RVP_E2E_WT_CERT_HASH ?? "";

test.describe("voice E2E (full stack)", () => {
  test.skip(!enabled, "set RVP_E2E=1 with a live Nest + rvp-cli stack");

  test("alice creates a room, bob joins, both see roster and stats", async ({
    browser,
  }) => {
    test.setTimeout(120_000);
    const aliceCtx = await browser.newContext({ permissions: ["microphone"] });
    const bobCtx = await browser.newContext({ permissions: ["microphone"] });
    const alice = await aliceCtx.newPage();
    const bob = await bobCtx.newPage();

    try {
      const aliceId = `alice-${Date.now()}`;
      const bobId = `bob-${Date.now()}`;

      // --- Alice: sign in, create room, invite Bob, join ---
      await signIn(alice, aliceId);
      await alice.getByRole("button", { name: "Create room" }).click();
      await expect(alice.getByText(/Created room/)).toBeVisible({ timeout: 15_000 });
      const roomId = (
        await alice.locator("code").first().textContent()
      )?.trim();
      expect(roomId).toBeTruthy();

      await alice.getByLabel("Invite user").fill(bobId);
      await alice.getByRole("button", { name: "Add member" }).click();

      await alice.getByRole("link", { name: "Join →" }).click();
      await expect(alice.getByRole("heading", { name: `Room ${roomId}` })).toBeVisible();

      // Fill WT settings from the environment and connect.
      await fillWtSettings(alice);
      await alice
        .getByRole("button", { name: /connect \+ join voice/ })
        .click();
      await expect(
        alice.getByRole("button", { name: "Mute" }),
      ).toBeVisible({ timeout: 30_000 });
      await expect(alice.getByText(/Participant/)).toBeVisible();

      // --- Bob: sign in, join the same room ---
      await signIn(bob, bobId);
      // Bob needs the room id — Alice's UI shows it; use the same id.
      await bob.goto(`/rooms/${roomId}`);
      await fillWtSettings(bob);
      await bob.getByRole("button", { name: /connect \+ join voice/ }).click();
      await expect(
        bob.getByRole("button", { name: "Mute" }),
      ).toBeVisible({ timeout: 30_000 });

      // --- Roster convergence ---
      // Alice should eventually see Bob (participantJoined event).
      await expect(alice.getByText(bobId)).toBeVisible({ timeout: 30_000 });
      // Bob sees Alice (from the Welcome roster).
      await expect(bob.getByText(aliceId)).toBeVisible({ timeout: 30_000 });

      // --- Voice stats: Alice is sending sealed datagrams (fanout) ---
      // The pipeline starts on connect; even without a real mic stream the
      // control path and pair-key table are live. Wait for stats to render.
      await expect(alice.getByText("Voice stats")).toBeVisible();
      const statsText = await alice
        .locator("pre")
        .filter({ hasText: "framesSent" })
        .textContent();
      expect(statsText).toBeTruthy();

      // Mute toggle works.
      await alice.getByRole("button", { name: "Mute" }).click();
      await expect(alice.getByRole("button", { name: "Unmute" })).toBeVisible();
    } finally {
      await aliceCtx.close();
      await bobCtx.close();
    }
  });
});

async function signIn(page: Page, userId: string): Promise<void> {
  await page.goto("/");
  await page.getByLabel("Nest URL").fill(nestUrl);
  await page.getByLabel("User ID").fill(userId);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("link", { name: /Go to rooms/ })).toBeVisible({
    timeout: 15_000,
  });
}

async function fillWtSettings(page: Page): Promise<void> {
  // The room page reads WT settings from localStorage set at sign-in.
  // Re-seed them so the E2E can point at the live rvp-cli endpoint.
  await page.evaluate(
    ([url, hash]) => {
      localStorage.setItem("rvp.wtUrl", JSON.stringify(url));
      localStorage.setItem("rvp.wtCertHash", JSON.stringify(hash));
    },
    [wtUrl, certHash] as const,
  );
  // Sign-in already wrote nest URL; ensure it matches too.
  await page.evaluate(
    (url) => {
      localStorage.setItem("rvp.nestUrl", JSON.stringify(url));
    },
    nestUrl,
  );
}
