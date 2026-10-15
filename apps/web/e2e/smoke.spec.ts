import { expect, test } from "@playwright/test";

/**
 * Smoke: the SPA loads, the login form is present, and the rooms page
 * redirects to sign-in when unauthenticated. Runs against the production
 * build served by `vite preview` (no backend required).
 */

test("loads the login page", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "RVP" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Sign in" })).toBeVisible();
  await expect(page.getByLabel("User ID")).toBeVisible();
  await expect(page.getByLabel("Nest URL")).toBeVisible();
  await expect(page.getByLabel("WebTransport URL")).toBeVisible();
  await expect(page.getByLabel(/WT cert hash/)).toBeVisible();
});

test("rooms page redirects to sign-in when unauthenticated", async ({ page }) => {
  await page.goto("/rooms");
  await expect(page.getByRole("link", { name: /Sign in first/ })).toBeVisible();
});

test("room page redirects to sign-in when unauthenticated", async ({ page }) => {
  await page.goto("/rooms/some-room-id");
  await expect(page.getByRole("link", { name: /Sign in first/ })).toBeVisible();
});
