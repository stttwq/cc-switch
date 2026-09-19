import { describe, it, expect } from "vitest";
import { http, HttpResponse } from "msw";

import { settingsApi } from "@/lib/api";
import { server } from "../msw/server";

const TAURI_ENDPOINT = "http://tauri.local";

/** 捕获某个命令收到的 IPC 载荷。 */
function capturePayload(command: string): Array<Record<string, unknown>> {
  const calls: Array<Record<string, unknown>> = [];
  server.use(
    http.post(`${TAURI_ENDPOINT}/${command}`, async ({ request }) => {
      const body = await request.text();
      calls.push(body ? (JSON.parse(body) as Record<string, unknown>) : {});
      return HttpResponse.json({ success: true });
    }),
  );
  return calls;
}

// P0-1 回归：WebDAV / S3 凭据必须作为独立 IPC 入参送出。
// 曾经的实现把密码塞进 settings 对象，而后端结构体已删该字段 → serde 静默丢弃，
// 命令的 password 形参恒为 None，凭据永远进不了凭据管理器。
describe("settingsApi sync credential payload", () => {
  it("sends the WebDAV password as its own argument", async () => {
    const calls = capturePayload("webdav_sync_save_settings");

    await settingsApi.webdavSyncSaveSettings(
      { baseUrl: "https://dav.example.com/dav/", username: "alice" },
      "secret",
    );

    expect(calls).toHaveLength(1);
    expect(calls[0].password).toBe("secret");
    expect(calls[0].settings).toEqual({
      baseUrl: "https://dav.example.com/dav/",
      username: "alice",
    });
  });

  it("omits the WebDAV password key when untouched", async () => {
    const calls = capturePayload("webdav_sync_save_settings");

    await settingsApi.webdavSyncSaveSettings({
      baseUrl: "https://dav.example.com/dav/",
      username: "alice",
    });

    expect(calls[0]).not.toHaveProperty("password");
  });

  it("sends an empty WebDAV password when the field was cleared", async () => {
    const calls = capturePayload("webdav_sync_save_settings");

    await settingsApi.webdavSyncSaveSettings(
      { baseUrl: "https://dav.example.com/dav/", username: "alice" },
      "",
    );

    expect(calls[0].password).toBe("");
  });

  it("sends the S3 credentials as their own arguments", async () => {
    const calls = capturePayload("s3_sync_save_settings");

    await settingsApi.s3SyncSaveSettings(
      { region: "us-east-1", bucket: "my-bucket" },
      "AKIAEXAMPLE",
      "secret-key",
    );

    expect(calls[0].accessKeyId).toBe("AKIAEXAMPLE");
    expect(calls[0].secretAccessKey).toBe("secret-key");
    expect(calls[0].settings).toEqual({
      region: "us-east-1",
      bucket: "my-bucket",
    });
  });

  it("omits S3 credential keys when untouched", async () => {
    const calls = capturePayload("s3_sync_save_settings");

    await settingsApi.s3SyncSaveSettings({
      region: "us-east-1",
      bucket: "my-bucket",
    });

    expect(calls[0]).not.toHaveProperty("accessKeyId");
    expect(calls[0]).not.toHaveProperty("secretAccessKey");
  });

  it("passes a typed-but-unsaved WebDAV password to the connection test", async () => {
    const calls = capturePayload("webdav_test_connection");

    await settingsApi.webdavTestConnection(
      { baseUrl: "https://dav.example.com/dav/", username: "alice" },
      "secret",
    );

    expect(calls[0].password).toBe("secret");
  });
});
