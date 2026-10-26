import { describe, expect, it } from "vitest";
import { formatBytes, formatDate, validateConfig } from "../src/validation";
import type { RuntimeConfig } from "../src/types";

function fixture(): RuntimeConfig {
  return {
    assistant: {
      name: "刘嘉卉",
      system_prompt: "",
      max_concurrent_runs: 2,
      timeout_seconds: 120,
    },
    model: {
      base_url: "https://api.deepseek.com",
      model: "deepseek-chat",
      credential_ref: "model-key",
    },
    qq: {
      enabled: false,
      app_id: "",
      credential_ref: null,
      transport: "websocket",
    },
  };
}

describe("configuration form guardrails", () => {
  it("accepts a disabled, unconfigured QQ channel without blocking model setup", () => {
    expect(validateConfig(fixture())).toEqual([]);
  });
  it("requires QQ identity and a credential before enabling the channel", () => {
    const value = fixture();
    value.qq.enabled = true;
    expect(validateConfig(value)).toContainEqual(
      expect.objectContaining({ field: "qq" }),
    );
  });
  it("prevents an accidental resource increase on the 8 GB baseline", () => {
    const value = fixture();
    value.assistant.max_concurrent_runs = 8;
    expect(validateConfig(value)).toContainEqual(
      expect.objectContaining({ field: "assistant.max_concurrent_runs" }),
    );
  });
  it("rejects model URLs embedding credentials or executable schemes", () => {
    for (const base_url of [
      "javascript:alert(1)",
      "https://user:password@example.com",
    ]) {
      const value = fixture();
      value.model.base_url = base_url;
      expect(validateConfig(value)).toContainEqual(
        expect.objectContaining({ field: "model.base_url" }),
      );
    }
  });
  it("distinguishes missing host measurements from a real zero value", () => {
    expect(formatBytes(undefined)).toBe("等待采样");
    expect(formatBytes(0)).toBe("0 MB");
  });
  it("renders Unix seconds from the Rust API as the correct calendar year", () => {
    expect(formatDate(1792800000)).toContain("2026");
  });
});
